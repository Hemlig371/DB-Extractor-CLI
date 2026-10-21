//! Разбиение SQL на запросы по `;` с учётом строк, идентификаторов и комментариев.
//!
//! Порт `_scan_sql` / `split_sql_queries` из Python-версии. Понимает:
//! `'...'` (с удвоением кавычки), `"..."`, `` `...` ``, `-- ...`, `/* ... */`,
//! PostgreSQL `$$...$$` и `$tag$...$tag$`, Oracle `q'[...]'` и `nq'{...}'`.
//! Не поддерживает PL/SQL-блоки и `GO` из T-SQL (как и Python-версия).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Code,
    Str,
    LineComment,
    BlockComment,
    Sep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub kind: Kind,
    pub text: String,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '#')
}

fn find_char(s: &[char], from: usize, c: char) -> Option<usize> {
    if from >= s.len() {
        return None;
    }
    s[from..].iter().position(|&x| x == c).map(|p| p + from)
}

fn find_seq(s: &[char], from: usize, pat: &[char]) -> Option<usize> {
    if pat.is_empty() || from + pat.len() > s.len() {
        return None;
    }
    (from..=s.len() - pat.len()).find(|&i| &s[i..i + pat.len()] == pat)
}

/// Индекс сразу за закрывающей кавычкой; удвоенная кавычка это экранирование.
fn quoted_end(s: &[char], start: usize, quote: char) -> usize {
    let n = s.len();
    let mut pos = start + 1;
    loop {
        match find_char(s, pos, quote) {
            None => return n,
            Some(k) => {
                if s.get(k + 1) == Some(&quote) {
                    pos = k + 2;
                    continue;
                }
                return k + 1;
            }
        }
    }
}

/// Oracle `q'[...]'` / `nq'{...}'`. Возвращает конец литерала или None, если это не он.
/// `i` указывает на открывающий апостроф.
fn q_literal_end(s: &[char], i: usize) -> Option<usize> {
    let n = s.len();
    if i == 0 || !matches!(s[i - 1], 'q' | 'Q') || i + 1 >= n {
        return None;
    }
    if i > 1 {
        let before = s[i - 2];
        if is_ident_char(before) {
            // допустим только префикс nq'...', перед которым нет идентификатора
            if !matches!(before, 'n' | 'N') || (i > 2 && is_ident_char(s[i - 3])) {
                return None;
            }
        }
    }
    let delimiter = s[i + 1];
    if delimiter.is_whitespace() || delimiter == '\'' {
        return None;
    }
    let closing_char = match delimiter {
        '[' => ']',
        '{' => '}',
        '(' => ')',
        '<' => '>',
        other => other,
    };
    match find_seq(s, i + 2, &[closing_char, '\'']) {
        None => Some(n),
        Some(k) => Some(k + 2),
    }
}

/// PostgreSQL `$$...$$` / `$tag$...$tag$`. Возвращает конец литерала или None.
fn dollar_end(s: &[char], i: usize) -> Option<usize> {
    let n = s.len();
    if i > 0 && is_ident_char(s[i - 1]) {
        return None;
    }
    if s.get(i) != Some(&'$') {
        return None;
    }
    let mut j = i + 1;
    if j < n && (s[j].is_ascii_alphabetic() || s[j] == '_') {
        j += 1;
        while j < n && (s[j].is_ascii_alphanumeric() || s[j] == '_') {
            j += 1;
        }
    }
    if j >= n || s[j] != '$' {
        return None;
    }
    let tag: Vec<char> = s[i..=j].to_vec();
    let after_tag = j + 1;
    match find_seq(s, after_tag, &tag) {
        None => Some(n),
        Some(k) => Some(k + tag.len()),
    }
}

/// Разбивает SQL на сегменты. Конкатенация текстов всех сегментов даёт исходную строку.
/// `;` вне строк и комментариев это отдельный сегмент `Sep`.
pub fn scan_sql(sql: &str) -> Vec<Segment> {
    let s: Vec<char> = sql.chars().collect();
    let n = s.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut start = 0usize;
    let text_of = |a: usize, b: usize| -> String { s[a..b].iter().collect() };

    while i < n {
        let ch = s[i];
        let nxt = s.get(i + 1).copied();
        let mut kind: Option<Kind> = None;
        let mut end = 0usize;

        if ch == '-' && nxt == Some('-') {
            end = find_char(&s, i, '\n').unwrap_or(n);
            kind = Some(Kind::LineComment);
        } else if ch == '/' && nxt == Some('*') {
            end = match find_seq(&s, i + 2, &['*', '/']) {
                Some(close) => close + 2,
                None => n,
            };
            kind = Some(Kind::BlockComment);
        } else if ch == '\'' {
            end = q_literal_end(&s, i).unwrap_or_else(|| quoted_end(&s, i, '\''));
            kind = Some(Kind::Str);
        } else if ch == '"' || ch == '`' {
            end = quoted_end(&s, i, ch);
            kind = Some(Kind::Str);
        } else if ch == '$' {
            if let Some(e) = dollar_end(&s, i) {
                end = e;
                kind = Some(Kind::Str);
            }
        } else if ch == ';' {
            end = i + 1;
            kind = Some(Kind::Sep);
        }

        match kind {
            None => i += 1,
            Some(k) => {
                if start < i {
                    out.push(Segment {
                        kind: Kind::Code,
                        text: text_of(start, i),
                    });
                }
                out.push(Segment {
                    kind: k,
                    text: text_of(i, end),
                });
                i = end;
                start = end;
            }
        }
    }
    if start < n {
        out.push(Segment {
            kind: Kind::Code,
            text: text_of(start, n),
        });
    }
    out
}

/// Делит SQL по `;`, не трогая `;` внутри строк, идентификаторов и комментариев.
/// Фрагменты без SQL (пустые или состоящие только из комментариев) отбрасываются.
pub fn split_sql_queries(sql: &str) -> Vec<String> {
    let mut queries = Vec::new();
    let mut current = String::new();
    let mut has_code = false;
    for seg in scan_sql(sql) {
        if seg.kind == Kind::Sep {
            let q = current.trim();
            if !q.is_empty() && has_code {
                queries.push(q.to_string());
            }
            current.clear();
            has_code = false;
            continue;
        }
        current.push_str(&seg.text);
        if seg.kind == Kind::Str || (seg.kind == Kind::Code && !seg.text.trim().is_empty()) {
            has_code = true;
        }
    }
    let q = current.trim();
    if !q.is_empty() && has_code {
        queries.push(q.to_string());
    }
    queries
}

/// SQL без комментариев (каждый комментарий заменяется пробелом).
pub fn strip_sql_comments(sql: &str) -> String {
    scan_sql(sql)
        .into_iter()
        .map(|seg| match seg.kind {
            Kind::LineComment | Kind::BlockComment => " ".to_string(),
            _ => seg.text,
        })
        .collect()
}

/// Текст всех комментариев запроса, по одному фрагменту на строку.
pub fn comment_text(sql: &str) -> String {
    let mut chunks: Vec<String> = Vec::new();
    for seg in scan_sql(sql) {
        match seg.kind {
            Kind::LineComment => chunks.push(seg.text),
            Kind::BlockComment => {
                let chars: Vec<char> = seg.text.chars().collect();
                let inner: String = if chars.len() >= 4 && seg.text.ends_with("*/") {
                    chars[2..chars.len() - 2].iter().collect()
                } else {
                    chars[2..].iter().collect()
                };
                chunks.push(inner);
            }
            _ => {}
        }
    }
    chunks.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(sql: &str, expected: &[&str]) {
        let got = split_sql_queries(sql);
        let want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(got, want, "sql = {:?}", sql);
    }

    #[test]
    fn basic() {
        check("select 1; select 2;", &["select 1", "select 2"]);
        check("select 1; select 2", &["select 1", "select 2"]);
    }

    #[test]
    fn empty_and_only_separators() {
        check("", &[]);
        check("   \n ", &[]);
        check(";;;", &[]);
    }

    #[test]
    fn quotes_and_identifiers() {
        check("select 'a;b' from t; select 2", &["select 'a;b' from t", "select 2"]);
        check(
            "select 'it''s;x' from t; select 2",
            &["select 'it''s;x' from t", "select 2"],
        );
        check(
            "select 1 as \"a;b\" from t; select 2",
            &["select 1 as \"a;b\" from t", "select 2"],
        );
        check("select `a;b` from t; select 2", &["select `a;b` from t", "select 2"]);
    }

    #[test]
    fn comments() {
        check("select 1 -- a;b\n, 2; select 3", &["select 1 -- a;b\n, 2", "select 3"]);
        check("select 1 /* ; */, 2; select 3", &["select 1 /* ; */, 2", "select 3"]);
        check(
            "select 'a -- b; c' from t; select 2",
            &["select 'a -- b; c' from t", "select 2"],
        );
        check("/* c */ select 1", &["/* c */ select 1"]);
    }

    #[test]
    fn leading_annotations_stay_with_their_query() {
        check(
            "-- @DX_OUT: a.parquet\nselect 1; -- @DX_OUT: b.parquet\nselect 2",
            &["-- @DX_OUT: a.parquet\nselect 1", "-- @DX_OUT: b.parquet\nselect 2"],
        );
    }

    #[test]
    fn comment_only_fragments_are_dropped() {
        check("select 1;\n-- конец файла\n", &["select 1"]);
        check("select 1;\n/* x */", &["select 1"]);
        check("-- nothing here\n/* nor here */", &[]);
        check("select 1;\n-- middle\n;select 2", &["select 1", "select 2"]);
    }

    #[test]
    fn dollar_quoting() {
        check("select $$a;b$$; select 2", &["select $$a;b$$", "select 2"]);
        check("select $fn$a;b$fn$; select 2", &["select $fn$a;b$fn$", "select 2"]);
        check(
            "select * from v$session; select 2",
            &["select * from v$session", "select 2"],
        );
        check("select $1; select 2", &["select $1", "select 2"]);
    }

    #[test]
    fn oracle_q_literals() {
        check(
            "select q'[it's; x]' from dual; select 2",
            &["select q'[it's; x]' from dual", "select 2"],
        );
        check(
            "select q'{a;b}' from dual; select 2",
            &["select q'{a;b}' from dual", "select 2"],
        );
        check(
            "select q'!a;b!' from dual; select 2",
            &["select q'!a;b!' from dual", "select 2"],
        );
        check(
            "select nq'[a;b]' from dual; select 2",
            &["select nq'[a;b]' from dual", "select 2"],
        );
        // идентификатор, оканчивающийся на q, не открывает q-литерал
        check(
            "select seq'a;b' from t; select 2",
            &["select seq'a;b' from t", "select 2"],
        );
    }

    #[test]
    fn unterminated_constructs_run_to_the_end() {
        check("select 'abc; select 2", &["select 'abc; select 2"]);
        check("select 1 /* open; select 2", &["select 1 /* open; select 2"]);
    }

    #[test]
    fn non_ascii_text_is_preserved() {
        check(
            "select 'привет;мир' as x; select 2",
            &["select 'привет;мир' as x", "select 2"],
        );
    }

    #[test]
    fn strip_comments_keeps_code_and_strings() {
        let stripped = strip_sql_comments("-- @DX_OUT: a\nselect 'x -- y' /* c */ from t");
        let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(collapsed, "select 'x -- y' from t");
    }

    #[test]
    fn comment_text_collects_line_and_block_comments() {
        let t = comment_text("/* a */ select 1 -- b\n");
        assert_eq!(t, " a \n-- b");
    }
}
