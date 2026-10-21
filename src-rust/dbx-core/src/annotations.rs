//! Аннотации `-- @DX_*` в тексте запроса и их объединение с аргументами командной строки.
//!
//! Аннотации ищутся только в комментариях (не в строковых литералах); значение берётся из строки
//! самой аннотации; при повторе побеждает первая.

use crate::compression::parse_compression;
use crate::sqlsplit::comment_text;

/// Значения по умолчанию из командной строки.
#[derive(Debug, Clone, Default)]
pub struct BaseOptions {
    pub out: Option<String>,
    pub format: Option<String>,
    pub compression: Option<String>,
    pub partition_on: Option<String>,
    pub partitions: Option<i64>,
    pub fill_nulls: Option<Vec<String>>,
    pub drop_nulls: Option<Vec<String>>,
    pub utc: bool,
    pub categorize: Option<Vec<String>>,
    pub hash_columns: Option<Vec<String>>,
    pub cast_universal: bool,
    pub decimal_as: Option<String>,
}

/// Итоговые параметры одного запроса.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryParams {
    pub query: String,
    pub out: Option<String>,
    pub format: Option<String>,
    pub compression: Option<String>,
    pub compression_level: Option<u32>,
    pub partition_on: Option<String>,
    pub partitions: Option<i64>,
    /// None: не задано; Some(пусто): все колонки.
    pub fill_nulls: Option<Vec<String>>,
    pub drop_nulls: Option<Vec<String>>,
    pub utc: bool,
    pub categorize: Option<Vec<String>>,
    pub hash_columns: Option<Vec<String>>,
    pub cast_universal: bool,
    /// `float`, `decimal` или `string`; None: `float` при `cast_universal`, иначе `decimal`.
    pub decimal_as: Option<String>,
    pub warnings: Vec<String>,
}

/// Допустимые значения `--decimal-as` и `@DX_DECIMAL_AS`.
pub const DECIMAL_MODES: [&str; 3] = ["float", "decimal", "string"];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Совпадает ли `line[pos..]` с `needle` без учёта регистра ASCII; возвращает длину в символах.
fn starts_with_ci(line: &[char], pos: usize, needle: &str) -> bool {
    let nc: Vec<char> = needle.chars().collect();
    if pos + nc.len() > line.len() {
        return false;
    }
    line[pos..pos + nc.len()]
        .iter()
        .zip(nc.iter())
        .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

/// Ищет в строке `--[ \t]*@DX_<NAME>(?![A-Za-z0-9_])` и возвращает индекс символа сразу за именем.
fn match_marker(line: &[char], from: usize, name: &str) -> Option<usize> {
    let mut p = from;
    if !(p + 1 < line.len() && line[p] == '-' && line[p + 1] == '-') {
        return None;
    }
    p += 2;
    while p < line.len() && (line[p] == ' ' || line[p] == '\t') {
        p += 1;
    }
    let marker = format!("@DX_{}", name);
    if !starts_with_ci(line, p, &marker) {
        return None;
    }
    p += marker.chars().count();
    if p < line.len() && is_word(line[p]) {
        return None;
    }
    Some(p)
}

/// Значение аннотации (в пределах её строки), None если аннотации нет.
fn annotation(comments: &str, name: &str, colon_required: bool) -> Option<String> {
    for raw_line in comments.split('\n') {
        let line: Vec<char> = raw_line.chars().collect();
        for start in 0..line.len() {
            if let Some(mut p) = match_marker(&line, start, name) {
                while p < line.len() && (line[p] == ' ' || line[p] == '\t') {
                    p += 1;
                }
                if p < line.len() && line[p] == ':' {
                    p += 1;
                } else if colon_required {
                    continue;
                }
                while p < line.len() && (line[p] == ' ' || line[p] == '\t') {
                    p += 1;
                }
                let rest: String = line[p..].iter().collect();
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// Есть ли флаг `-- @DX_<NAME>`.
fn flag(comments: &str, name: &str) -> bool {
    for raw_line in comments.split('\n') {
        let line: Vec<char> = raw_line.chars().collect();
        for start in 0..line.len() {
            if match_marker(&line, start, name).is_some() {
                return true;
            }
        }
    }
    false
}

/// Делит список имён по запятым и пробелам.
pub fn name_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn first_token(text: &str) -> Option<&str> {
    text.split_whitespace().next()
}

fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

pub fn extract_dx_params(query: &str, base: &BaseOptions) -> QueryParams {
    let mut warnings: Vec<String> = Vec::new();

    let base_compression = base.compression.as_deref().filter(|s| !s.is_empty());
    let (comp_val, comp_lvl, level_ok) = parse_compression(base_compression.unwrap_or(""));
    if !level_ok {
        warnings.push(format!(
            "Ignoring non-numeric compression level in --compression '{}'.",
            base_compression.unwrap_or("")
        ));
    }

    let nonempty = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());

    let mut params = QueryParams {
        query: query.to_string(),
        out: nonempty(&base.out),
        format: nonempty(&base.format),
        compression: comp_val,
        compression_level: comp_lvl,
        partition_on: nonempty(&base.partition_on),
        partitions: base.partitions,
        fill_nulls: base.fill_nulls.clone(),
        drop_nulls: base.drop_nulls.clone(),
        utc: base.utc,
        categorize: base.categorize.clone(),
        hash_columns: base.hash_columns.clone(),
        cast_universal: base.cast_universal,
        decimal_as: base.decimal_as.clone(),
        warnings: Vec::new(),
    };

    let comments = comment_text(query);

    // Непустое значение аннотации или None (пустое значение игнорируется с предупреждением).
    let value_of = |name: &str, warnings: &mut Vec<String>| -> Option<String> {
        let value = annotation(&comments, name, true)?;
        if value.is_empty() {
            warnings.push(format!("Ignoring @DX_{}: the value is empty.", name));
            return None;
        }
        Some(value)
    };

    if let Some(v) = value_of("OUT", &mut warnings) {
        params.out = Some(v);
    }

    if let Some(v) = value_of("FORMAT", &mut warnings) {
        if let Some(tok) = first_token(&v) {
            params.format = Some(tok.to_lowercase());
        }
    }

    if let Some(v) = value_of("COMPRESSION", &mut warnings) {
        if let Some(tok) = first_token(&v) {
            let (ann_algo, mut ann_lvl, ann_ok) = parse_compression(tok);
            if !ann_ok {
                warnings.push(format!(
                    "Ignoring non-numeric compression level in @DX_COMPRESSION '{}'.",
                    v
                ));
            }
            if let Some(algo) = ann_algo {
                if ann_lvl.is_none() && params.compression.as_deref() == Some(algo.as_str()) {
                    ann_lvl = params.compression_level;
                }
                params.compression = Some(algo);
                params.compression_level = ann_lvl;
            }
        }
    }

    if let Some(v) = value_of("PARTITION_ON", &mut warnings) {
        if let Some(tok) = first_token(&v) {
            params.partition_on = Some(tok.to_string());
        }
    }

    if let Some(v) = value_of("PARTITIONS", &mut warnings) {
        match v.parse::<i64>() {
            Ok(n) if is_ascii_digits(&v) => params.partitions = Some(n),
            _ => warnings.push(format!("Ignoring @DX_PARTITIONS: '{}' is not an integer.", v)),
        }
    }

    // Пустой список означает «все колонки».
    if let Some(v) = annotation(&comments, "FILL_NULLS", false) {
        params.fill_nulls = Some(name_list(&v));
    }
    if let Some(v) = annotation(&comments, "DROP_NULLS", false) {
        params.drop_nulls = Some(name_list(&v));
    }

    if flag(&comments, "UTC") {
        params.utc = true;
    }

    if let Some(v) = value_of("CATEGORIZE", &mut warnings) {
        params.categorize = Some(name_list(&v));
    }
    if let Some(v) = value_of("HASH_COLUMNS", &mut warnings) {
        params.hash_columns = Some(name_list(&v));
    }

    if flag(&comments, "CAST_UNIVERSAL") {
        params.cast_universal = true;
    }

    if let Some(v) = value_of("DECIMAL_AS", &mut warnings) {
        let mode = first_token(&v).unwrap_or("").to_lowercase();
        if DECIMAL_MODES.contains(&mode.as_str()) {
            params.decimal_as = Some(mode);
        } else {
            warnings.push(format!(
                "Ignoring @DX_DECIMAL_AS: '{}' is not one of {}.",
                v,
                DECIMAL_MODES.join(", ")
            ));
        }
    }

    params.warnings = warnings;
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlsplit::split_sql_queries;

    fn ex(sql: &str) -> QueryParams {
        extract_dx_params(sql, &BaseOptions::default())
    }

    fn ex_with(sql: &str, base: BaseOptions) -> QueryParams {
        extract_dx_params(sql, &base)
    }

    fn comp(p: &QueryParams) -> (Option<&str>, Option<u32>) {
        (p.compression.as_deref(), p.compression_level)
    }

    #[test]
    fn defaults_come_from_cli() {
        let base = BaseOptions {
            out: Some("o.csv".into()),
            format: Some("csv".into()),
            partition_on: Some("id".into()),
            partitions: Some(4),
            fill_nulls: Some(vec!["a".into()]),
            drop_nulls: Some(vec![]),
            utc: true,
            categorize: Some(vec!["c".into()]),
            hash_columns: Some(vec!["h".into()]),
            cast_universal: true,
            decimal_as: Some("string".into()),
            ..Default::default()
        };
        let p = ex_with("SELECT 1", base);
        assert_eq!(p.out.as_deref(), Some("o.csv"));
        assert_eq!(p.format.as_deref(), Some("csv"));
        assert_eq!((p.partition_on.as_deref(), p.partitions), (Some("id"), Some(4)));
        assert_eq!(p.fill_nulls, Some(vec!["a".to_string()]));
        assert_eq!(p.drop_nulls, Some(vec![]));
        assert!(p.utc && p.cast_universal);
        assert_eq!(p.decimal_as.as_deref(), Some("string"));
        assert_eq!(p.categorize, Some(vec!["c".to_string()]));
        assert_eq!(p.hash_columns, Some(vec!["h".to_string()]));
    }

    #[test]
    fn readme_example_one() {
        let p = ex(
            "-- @DX_OUT: reports_2023.parquet\n-- @DX_PARTITION_ON: report_id\n-- @DX_PARTITIONS: 4\n\
                    -- @DX_COMPRESSION: zstd:10\n-- @DX_CATEGORIZE: status, type\n\
                    -- @DX_FILL_NULLS: status, category\nSELECT * FROM reports WHERE year = 2023",
        );
        assert_eq!(p.out.as_deref(), Some("reports_2023.parquet"));
        assert_eq!((p.partition_on.as_deref(), p.partitions), (Some("report_id"), Some(4)));
        assert_eq!(comp(&p), (Some("zstd"), Some(10)));
        assert_eq!(p.categorize, Some(vec!["status".to_string(), "type".to_string()]));
        assert_eq!(p.fill_nulls, Some(vec!["status".to_string(), "category".to_string()]));
    }

    #[test]
    fn readme_example_two() {
        let p = ex("-- @DX_OUT: users_active.csv\n-- @DX_COMPRESSION: gzip:6\n\
                    -- @DX_HASH_COLUMNS: email, phone\n-- @DX_UTC\nSELECT * FROM users WHERE active = true");
        assert_eq!(p.out.as_deref(), Some("users_active.csv"));
        assert_eq!(comp(&p), (Some("gzip"), Some(6)));
        assert_eq!(p.hash_columns, Some(vec!["email".to_string(), "phone".to_string()]));
        assert!(p.utc);
    }

    #[test]
    fn annotation_beats_cli_and_first_duplicate_wins() {
        let base = BaseOptions {
            out: Some("cli.csv".into()),
            ..Default::default()
        };
        assert_eq!(
            ex_with("-- @DX_OUT: ann.csv\nSELECT 1", base).out.as_deref(),
            Some("ann.csv")
        );
        assert_eq!(
            ex("-- @DX_OUT: a.parquet\n-- @DX_OUT: b.parquet\nSELECT 1")
                .out
                .as_deref(),
            Some("a.parquet")
        );
    }

    #[test]
    fn names_are_case_insensitive_and_format_is_lowercased() {
        let p = ex("-- @dx_out: A.CSV\n-- @DX_Format: CSV\nSELECT 1");
        assert_eq!((p.out.as_deref(), p.format.as_deref()), (Some("A.CSV"), Some("csv")));
    }

    #[test]
    fn marker_spacing_variants() {
        assert_eq!(ex("--@DX_OUT: z.csv\nSELECT 1").out.as_deref(), Some("z.csv"));
        assert_eq!(ex("--\t@DX_OUT: t.csv\nSELECT 1").out.as_deref(), Some("t.csv"));
    }

    #[test]
    fn fill_nulls_with_list_and_without_colon() {
        let want = Some(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(ex("-- @DX_FILL_NULLS: a, b\nSELECT 1").fill_nulls, want);
        assert_eq!(ex("-- @DX_FILL_NULLS a, b\nSELECT 1").fill_nulls, want);
        assert_eq!(ex("-- @DX_FILL_NULLS: a b\nSELECT 1").fill_nulls, want);
    }

    #[test]
    fn cli_compression_variants() {
        let b = |c: &str| BaseOptions {
            compression: Some(c.into()),
            ..Default::default()
        };
        assert_eq!(comp(&ex_with("SELECT 1", b("zstd:10"))), (Some("zstd"), Some(10)));
        assert_eq!(comp(&ex_with("SELECT 1", b("gz:6"))), (Some("gzip"), Some(6)));
        for (cli, want) in [("ZSTD", "zstd"), ("GZ", "gzip"), ("GZIP", "gzip"), ("Zstd:5", "zstd")] {
            assert_eq!(ex_with("SELECT 1", b(cli)).compression.as_deref(), Some(want));
        }
    }

    #[test]
    fn annotation_compression_interactions() {
        let cli = || BaseOptions {
            compression: Some("zstd:10".into()),
            ..Default::default()
        };
        let p = ex_with("-- @DX_COMPRESSION: zstd:3\nSELECT 1", cli());
        assert_eq!(comp(&p), (Some("zstd"), Some(3)));
        let p = ex_with("-- @DX_COMPRESSION: zstd\nSELECT 1", cli());
        assert_eq!(comp(&p), (Some("zstd"), Some(10)));
        let p = ex_with("-- @DX_COMPRESSION: gzip\nSELECT 1", cli());
        assert_eq!(comp(&p), (Some("gzip"), None));
        let p = ex("-- @DX_COMPRESSION: ZSTD:10\nSELECT 1");
        assert_eq!(comp(&p), (Some("zstd"), Some(10)));
    }

    #[test]
    fn invalid_level_is_ignored_with_warning() {
        let base = BaseOptions {
            compression: Some("zstd:abc".into()),
            ..Default::default()
        };
        let p = ex_with("SELECT 1", base);
        assert_eq!(comp(&p), (Some("zstd"), None));
        assert!(!p.warnings.is_empty());
    }

    #[test]
    fn annotation_after_semicolon_belongs_to_next_query() {
        let qs = split_sql_queries("select 1; -- @DX_OUT: b.parquet\nselect 2");
        assert_eq!(ex(&qs[0]).out, None);
        assert_eq!(ex(&qs[1]).out.as_deref(), Some("b.parquet"));
    }

    #[test]
    fn bare_null_annotations_mean_all_columns() {
        assert_eq!(ex("-- @DX_FILL_NULLS\nSELECT a, b FROM t").fill_nulls, Some(vec![]));
        assert_eq!(ex("-- @DX_DROP_NULLS\nSELECT a FROM t").drop_nulls, Some(vec![]));
        let p = ex("-- @DX_FILL_NULLS\n-- @DX_UTC\nSELECT a FROM t");
        assert_eq!(p.fill_nulls, Some(vec![]));
        assert!(p.utc);
        assert_eq!(ex("SELECT a FROM t -- @DX_FILL_NULLS").fill_nulls, Some(vec![]));
    }

    #[test]
    fn empty_values_are_ignored_not_taken_from_next_line() {
        for name in [
            "OUT",
            "FORMAT",
            "COMPRESSION",
            "PARTITION_ON",
            "CATEGORIZE",
            "HASH_COLUMNS",
            "PARTITIONS",
        ] {
            let p = ex(&format!("-- @DX_{}:\nSELECT a FROM t", name));
            assert!(
                p.out.is_none() && p.format.is_none() && p.compression.is_none(),
                "{}",
                name
            );
            assert!(p.partition_on.is_none() && p.partitions.is_none(), "{}", name);
            assert!(p.categorize.is_none() && p.hash_columns.is_none(), "{}", name);
            assert!(!p.warnings.is_empty(), "{}: ignored empty value must be reported", name);
        }
    }

    #[test]
    fn out_path_with_spaces_and_crlf() {
        let p = ex("-- @DX_OUT: C:\\Users\\me\\my data\\a.parquet\nSELECT 1");
        assert_eq!(p.out.as_deref(), Some("C:\\Users\\me\\my data\\a.parquet"));
        assert_eq!(
            ex("-- @DX_OUT: a b.parquet\r\nSELECT 1").out.as_deref(),
            Some("a b.parquet")
        );
    }

    #[test]
    fn annotation_in_string_literal_is_ignored() {
        assert_eq!(ex("SELECT '-- @DX_OUT: evil.parquet' AS x FROM t").out, None);
    }

    #[test]
    fn annotation_name_must_end_at_word_boundary() {
        let p = ex("-- @DX_UTC_FOO\n-- @DX_CAST_UNIVERSAL_X\nSELECT 1");
        assert!(!p.utc && !p.cast_universal);
    }

    #[test]
    fn annotation_inside_block_comment_works() {
        assert_eq!(ex("/*\n-- @DX_OUT: m.csv\n*/ SELECT 1").out.as_deref(), Some("m.csv"));
    }

    #[test]
    fn partitions_must_be_integer() {
        let p = ex("-- @DX_PARTITIONS: four\nSELECT 1");
        assert_eq!(p.partitions, None);
        assert!(!p.warnings.is_empty());
        assert_eq!(ex("-- @DX_PARTITIONS: 8\nSELECT 1").partitions, Some(8));
    }

    #[test]
    fn decimal_as_annotation() {
        assert_eq!(
            ex("-- @DX_DECIMAL_AS: String\nSELECT 1").decimal_as.as_deref(),
            Some("string")
        );
        assert_eq!(ex("SELECT 1").decimal_as, None);
        let base = BaseOptions {
            decimal_as: Some("string".into()),
            ..Default::default()
        };
        let p = ex_with("-- @DX_DECIMAL_AS: float\nSELECT 1", base);
        assert_eq!(p.decimal_as.as_deref(), Some("float"));
    }

    #[test]
    fn decimal_as_unknown_value_is_ignored_with_warning() {
        let base = BaseOptions {
            decimal_as: Some("float".into()),
            ..Default::default()
        };
        let p = ex_with("-- @DX_DECIMAL_AS: number\nSELECT 1", base);
        assert_eq!(p.decimal_as.as_deref(), Some("float"));
        assert_eq!(
            p.warnings,
            vec!["Ignoring @DX_DECIMAL_AS: 'number' is not one of float, decimal, string.".to_string()]
        );
    }
}
