//! Формат строк `export_log.txt`.

use crate::sqlsplit::strip_sql_comments;

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Текст запроса для журнала: без комментариев, в одну строку, до 100 символов.
pub fn clean_query(query: &str) -> String {
    let mut plain = collapse_ws(&strip_sql_comments(query));
    if plain.is_empty() {
        plain = collapse_ws(query);
    }
    let count = plain.chars().count();
    if count > 100 {
        let head: String = plain.chars().take(100).collect();
        format!("{}...", head)
    } else {
        plain
    }
}

pub fn format_success(timestamp: &str, duration_secs: f64, rows: u64, size_mb: f64, file: &str, query: &str) -> String {
    format!(
        "[{}] SUCCESS | Time: {:.2}s | Rows: {} | Size: {:.2} MB | File: {} | Query: {}\n",
        timestamp,
        duration_secs,
        rows,
        size_mb,
        file,
        clean_query(query)
    )
}

pub fn format_error(timestamp: &str, duration_secs: f64, error: &str, query: &str) -> String {
    format!(
        "[{}] ERROR | Time: {:.2}s | Error: {} | Query: {}\n",
        timestamp,
        duration_secs,
        collapse_ws(error),
        clean_query(query)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_cleaned_and_truncated() {
        assert_eq!(clean_query("-- @DX_OUT: a\nselect  1\n from t"), "select 1 from t");
        let long = format!("select {}", "x".repeat(200));
        let c = clean_query(&long);
        assert_eq!(c.chars().count(), 103);
        assert!(c.ends_with("..."));
    }

    #[test]
    fn comment_only_query_falls_back_to_original_text() {
        assert_eq!(clean_query("-- just a comment"), "-- just a comment");
    }

    #[test]
    fn success_line_format() {
        let l = format_success("2026-01-02 03:04:05", 1.234, 10, 0.5, "a.parquet", "select 1");
        assert_eq!(
            l,
            "[2026-01-02 03:04:05] SUCCESS | Time: 1.23s | Rows: 10 | Size: 0.50 MB | File: a.parquet | Query: select 1\n"
        );
    }

    #[test]
    fn error_line_is_single_line() {
        let l = format_error("2026-01-02 03:04:05", 0.0, "bad\nthing\r\nhappened", "select 1");
        assert_eq!(
            l,
            "[2026-01-02 03:04:05] ERROR | Time: 0.00s | Error: bad thing happened | Query: select 1\n"
        );
    }
}
