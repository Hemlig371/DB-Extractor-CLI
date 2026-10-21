//! Приведение строки подключения к виду, который понимает connectorx.

/// `redshift://...` превращается в `postgresql://...` с протоколом `cursor` (так делает и Python-обёртка
/// connectorx); `redshift-iam://` и остальные схемы не меняются.
pub fn rewrite_conn(conn: &str) -> String {
    let (backend, details) = match conn.split_once(':') {
        Some(parts) => parts,
        None => return conn.to_string(),
    };
    if !backend.to_lowercase().contains("redshift") || backend.to_lowercase().contains("-iam") {
        return conn.to_string();
    }
    let mut out = format!("postgresql:{}", details);
    if !out.contains("cxprotocol=") {
        out.push(if out.contains('?') { '&' } else { '?' });
        out.push_str("cxprotocol=cursor");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redshift_becomes_postgres_with_cursor() {
        assert_eq!(
            rewrite_conn("redshift://u:p@host:5439/db"),
            "postgresql://u:p@host:5439/db?cxprotocol=cursor"
        );
        assert_eq!(
            rewrite_conn("redshift://u:p@host/db?sslmode=require"),
            "postgresql://u:p@host/db?sslmode=require&cxprotocol=cursor"
        );
        assert_eq!(
            rewrite_conn("redshift://u:p@host/db?cxprotocol=binary"),
            "postgresql://u:p@host/db?cxprotocol=binary"
        );
    }

    #[test]
    fn other_schemes_are_untouched() {
        for conn in [
            "postgresql://u:p@h/db",
            "redshift-iam://u@h/db",
            "oracle://u:p@h:1521/ORCL",
            "sqlite:///tmp/a.db",
            "not a url",
        ] {
            assert_eq!(rewrite_conn(conn), conn);
        }
    }
}
