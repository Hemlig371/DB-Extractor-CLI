//! Выбор формата вывода и сжатия. Порт `parse_compression`, `_resolve_format`,
//! `_resolve_compression` из Python-версии.

pub const SUPPORTED_FORMATS: [&str; 4] = ["csv", "parquet", "arrow", "jsonl"];
pub const PARQUET_COMPRESSIONS: [&str; 6] = ["uncompressed", "snappy", "gzip", "brotli", "lz4", "zstd"];
pub const ARROW_COMPRESSIONS: [&str; 3] = ["uncompressed", "lz4", "zstd"];

fn alias(name: &str) -> &str {
    match name {
        "gz" => "gzip",
        "zst" => "zstd",
        "none" => "uncompressed",
        other => other,
    }
}

/// `'ZSTD:10'` -> `(Some("zstd"), Some(10), true)`. Третье значение false, если уровень не число.
pub fn parse_compression(value: &str) -> (Option<String>, Option<u32>, bool) {
    if value.is_empty() {
        return (None, None, true);
    }
    let (algo_raw, level_raw) = match value.split_once(':') {
        Some((a, l)) => (a, l),
        None => (value, ""),
    };
    let algo = algo_raw.trim().to_lowercase();
    let algo = alias(&algo).to_string();
    let algo = if algo.is_empty() { None } else { Some(algo) };
    let level = level_raw.trim();
    if level.is_empty() {
        return (algo, None, true);
    }
    if level.chars().all(|c| c.is_ascii_digit()) {
        return match level.parse::<u32>() {
            Ok(n) => (algo, Some(n), true),
            Err(_) => (algo, None, false),
        };
    }
    (algo, None, false)
}

/// Определяет формат по `--format`/`@DX_FORMAT` или по расширению файла.
pub fn resolve_format(format: Option<&str>, out: &str) -> Result<String, String> {
    if let Some(f) = format.filter(|f| !f.is_empty()) {
        let f = f.to_lowercase();
        if !SUPPORTED_FORMATS.contains(&f.as_str()) {
            return Err(format!(
                "Unsupported format '{}' (supported: {}).",
                f,
                SUPPORTED_FORMATS.join(", ")
            ));
        }
        return Ok(f);
    }
    let lower = out.to_lowercase();
    let base = strip_compression_suffix(&lower);
    let ext = match base.rfind('.') {
        // как os.path.splitext: точка в начале имени файла не считается расширением
        Some(pos) if !base[..pos].is_empty() && !base[..pos].ends_with(['/', '\\']) => &base[pos..],
        _ => "",
    };
    match ext {
        ".csv" => Ok("csv".into()),
        ".parquet" => Ok("parquet".into()),
        ".arrow" | ".ipc" => Ok("arrow".into()),
        ".jsonl" | ".ndjson" => Ok("jsonl".into()),
        _ => Err(format!("Could not infer format from output file '{}'.", out)),
    }
}

fn strip_compression_suffix(lower: &str) -> &str {
    for suffix in [".gz", ".zstd", ".zst", ".zip"] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped;
        }
    }
    lower
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCompression {
    /// None: без сжатия. Для parquet и arrow всегда заполнено (`uncompressed` вместо None у arrow).
    pub algo: Option<String>,
    pub level: Option<u32>,
    pub warnings: Vec<String>,
}

/// Допустимый диапазон уровня сжатия для алгоритма (None: уровни не поддерживаются).
pub fn level_range(algo: &str) -> Option<(u32, u32)> {
    match algo {
        "gzip" => Some((0, 9)),
        "zstd" => Some((1, 22)),
        "brotli" => Some((0, 11)),
        _ => None,
    }
}

/// Возвращает алгоритм и уровень для формата. Не поддерживаемое форматом сжатие отключается
/// с предупреждением; для parquet неизвестный алгоритм это ошибка.
pub fn resolve_compression(
    comp: Option<&str>,
    level: Option<u32>,
    out: &str,
    fmt: &str,
) -> Result<ResolvedCompression, String> {
    let mut warnings = Vec::new();
    let mut comp: Option<String> = comp.filter(|c| !c.is_empty()).map(|c| c.to_string());
    let lower_out = out.to_lowercase();
    if comp.is_none() {
        if lower_out.ends_with(".gz") {
            comp = Some("gzip".into());
        } else if lower_out.ends_with(".zst") || lower_out.ends_with(".zstd") {
            comp = Some("zstd".into());
        } else if lower_out.ends_with(".zip") {
            warnings.push(format!(
                "zip compression is not supported; '{}' is written uncompressed.",
                out
            ));
        }
    }

    let resolved: Option<String> = match fmt {
        "parquet" => {
            let c = comp.unwrap_or_else(|| "zstd".into());
            if !PARQUET_COMPRESSIONS.contains(&c.as_str()) {
                return Err(format!(
                    "Unsupported compression '{}' for parquet (supported: {}).",
                    c,
                    PARQUET_COMPRESSIONS.join(", ")
                ));
            }
            Some(c)
        }
        "csv" => match comp.as_deref() {
            None | Some("uncompressed") => None,
            Some("gzip") | Some("zstd") => comp,
            Some(other) => {
                warnings.push(format!(
                    "CSV compression '{}' is not supported (use gzip or zstd); writing uncompressed.",
                    other
                ));
                None
            }
        },
        "arrow" => match comp.as_deref() {
            None | Some("uncompressed") => Some("uncompressed".into()),
            Some("lz4") | Some("zstd") => comp,
            Some(other) => {
                warnings.push(format!(
                    "Arrow compression '{}' is not supported (use lz4 or zstd); writing uncompressed.",
                    other
                ));
                Some("uncompressed".into())
            }
        },
        _ => {
            if let Some(c) = comp.as_deref() {
                if c != "uncompressed" {
                    warnings.push(format!("JSONL compression '{}' is not supported and is ignored.", c));
                }
            }
            None
        }
    };

    // Уровень: проверяем диапазон; для алгоритмов без уровней он игнорируется.
    let mut level = level;
    if let (Some(algo), Some(l)) = (resolved.as_deref(), level) {
        match level_range(algo) {
            Some((lo, hi)) => {
                if l < lo || l > hi {
                    return Err(format!(
                        "Compression level {} is out of range for {} ({}-{}).",
                        l, algo, lo, hi
                    ));
                }
            }
            None => {
                warnings.push(format!("Compression level is ignored: '{}' has no levels.", algo));
                level = None;
            }
        }
    }
    if resolved.is_none() {
        level = None;
    }

    Ok(ResolvedCompression {
        algo: resolved,
        level,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_compression_cases() {
        assert_eq!(parse_compression(""), (None, None, true));
        assert_eq!(parse_compression("zstd"), (Some("zstd".into()), None, true));
        assert_eq!(parse_compression("ZSTD:10"), (Some("zstd".into()), Some(10), true));
        assert_eq!(parse_compression("gz:6"), (Some("gzip".into()), Some(6), true));
        assert_eq!(parse_compression("zst"), (Some("zstd".into()), None, true));
        assert_eq!(parse_compression("none"), (Some("uncompressed".into()), None, true));
        assert_eq!(parse_compression("zstd:abc"), (Some("zstd".into()), None, false));
        assert_eq!(parse_compression("zstd:"), (Some("zstd".into()), None, true));
        assert_eq!(parse_compression(":5"), (None, Some(5), true));
        assert_eq!(
            parse_compression("zstd:99999999999999999999"),
            (Some("zstd".into()), None, false)
        );
    }

    #[test]
    fn format_from_flag_and_extension() {
        assert_eq!(resolve_format(Some("CSV"), "x.bin").unwrap(), "csv");
        assert!(resolve_format(Some("xlsx"), "x.xlsx")
            .unwrap_err()
            .contains("Unsupported format"));
        assert_eq!(resolve_format(None, "a.parquet").unwrap(), "parquet");
        assert_eq!(resolve_format(None, "A.CSV.GZ").unwrap(), "csv");
        assert_eq!(resolve_format(None, "a.csv.zst").unwrap(), "csv");
        assert_eq!(resolve_format(None, "a.ipc").unwrap(), "arrow");
        assert_eq!(resolve_format(None, "a.arrow").unwrap(), "arrow");
        assert_eq!(resolve_format(None, "a.ndjson").unwrap(), "jsonl");
        assert!(resolve_format(None, "a.txt").is_err());
        assert!(resolve_format(None, "noext").is_err());
        assert!(resolve_format(None, ".csv").is_err()); // как splitext: ".csv" без имени не имеет расширения
    }

    #[test]
    fn parquet_defaults_and_validation() {
        let r = resolve_compression(None, None, "a.parquet", "parquet").unwrap();
        assert_eq!(r.algo.as_deref(), Some("zstd"));
        let r = resolve_compression(Some("snappy"), None, "a.parquet", "parquet").unwrap();
        assert_eq!(r.algo.as_deref(), Some("snappy"));
        assert!(resolve_compression(Some("bogus"), None, "a.parquet", "parquet").is_err());
        assert!(resolve_compression(Some("zstd"), Some(99), "a.parquet", "parquet").is_err());
    }

    #[test]
    fn level_is_dropped_for_algorithms_without_levels() {
        let r = resolve_compression(Some("snappy"), Some(3), "a.parquet", "parquet").unwrap();
        assert_eq!(r.level, None);
        assert!(!r.warnings.is_empty());
    }

    #[test]
    fn csv_compression_from_extension_and_unsupported() {
        let r = resolve_compression(None, None, "a.csv.gz", "csv").unwrap();
        assert_eq!(r.algo.as_deref(), Some("gzip"));
        let r = resolve_compression(None, None, "a.csv.zst", "csv").unwrap();
        assert_eq!(r.algo.as_deref(), Some("zstd"));
        let r = resolve_compression(None, None, "a.csv.zip", "csv").unwrap();
        assert_eq!(r.algo, None);
        assert!(!r.warnings.is_empty());
        let r = resolve_compression(Some("snappy"), None, "a.csv", "csv").unwrap();
        assert_eq!(r.algo, None);
        assert!(!r.warnings.is_empty());
        // пример из README: users_active.csv + gzip:6
        let r = resolve_compression(Some("gzip"), Some(6), "users_active.csv", "csv").unwrap();
        assert_eq!((r.algo.as_deref(), r.level), (Some("gzip"), Some(6)));
    }

    #[test]
    fn arrow_and_jsonl() {
        let r = resolve_compression(None, None, "a.arrow", "arrow").unwrap();
        assert_eq!(r.algo.as_deref(), Some("uncompressed"));
        let r = resolve_compression(Some("lz4"), None, "a.arrow", "arrow").unwrap();
        assert_eq!(r.algo.as_deref(), Some("lz4"));
        let r = resolve_compression(Some("gzip"), None, "a.arrow", "arrow").unwrap();
        assert_eq!(r.algo.as_deref(), Some("uncompressed"));
        assert!(!r.warnings.is_empty());
        let r = resolve_compression(Some("gzip"), None, "a.jsonl", "jsonl").unwrap();
        assert_eq!(r.algo, None);
        assert!(!r.warnings.is_empty());
    }
}
