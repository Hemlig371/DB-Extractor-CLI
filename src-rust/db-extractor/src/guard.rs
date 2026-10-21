//! Превращение паник (connectorx использует `unimplemented!` и `panic!` для неподдержанных
//! типов и ошибок потока) в обычные ошибки с подсказкой.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Отключает вывод паник в stderr: все паники ловит `guard` и записывает в журнал.
pub fn install_quiet_panic_hook() {
    std::panic::set_hook(Box::new(|_| {}));
}

fn payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "internal error".to_string()
    }
}

/// Текст ошибки вместе с цепочкой причин (`tokio-postgres` и другие драйверы кладут сообщение сервера в `source`).
pub fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut current = err.source();
    while let Some(cause) = current {
        let part = cause.to_string();
        if !part.is_empty() && !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        current = cause.source();
    }
    text
}

/// Текст ошибки connectorx с цепочкой причин и подсказкой.
pub fn describe(err: &(dyn std::error::Error + 'static)) -> String {
    with_hint(&error_chain(err))
}

/// Добавляет подсказку к известным причинам сбоя.
pub fn with_hint(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("timed out waiting for connection") {
        return format!(
            "{} -- could not connect: check host, port, user, password and network. The connection pool retries for \
             30 seconds and hides the exact cause.",
            message
        );
    }
    if message.contains("Source Unknown not supported") {
        return format!(
            "{} -- unknown connection scheme. Supported: postgresql, redshift, mysql, mssql, oracle, sqlite, \
             bigquery, clickhouse, trino.",
            message
        );
    }
    if message.contains("DPI-1047") || lower.contains("oracle client library") {
        return format!(
            "{} -- Oracle Instant Client was not found. Install it (same bitness as db-extractor) and add its \
             folder to PATH (Windows) or LD_LIBRARY_PATH (Linux).",
            message
        );
    }
    if lower.contains("not implemented") || lower.contains("unimplemented") {
        return format!(
            "{} -- the query returns a column type that connectorx cannot read. Convert that column in the \
             query itself, for example TO_CHAR(col) in Oracle or CAST(col AS VARCHAR/TEXT) elsewhere.",
            message
        );
    }
    message.to_string()
}

/// Выполняет `f`, превращая панику в `Err` с понятным текстом.
pub fn guard<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| with_hint(&payload_message(&*payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_becomes_error() {
        install_quiet_panic_hook();
        let r: Result<(), String> = guard(|| panic!("boom"));
        assert_eq!(r.unwrap_err(), "boom");
    }

    #[test]
    fn string_payload_and_ok_value() {
        install_quiet_panic_hook();
        let r = guard(|| -> i32 { panic!("{}", String::from("formatted")) });
        assert_eq!(r.unwrap_err(), "formatted");
        assert_eq!(guard(|| 5).unwrap(), 5);
    }

    #[test]
    fn unimplemented_gets_a_hint() {
        install_quiet_panic_hook();
        let r: Result<(), String> = guard(|| unimplemented!("oracle type"));
        let e = r.unwrap_err();
        assert!(e.contains("not implemented"), "{}", e);
        assert!(e.contains("TO_CHAR"), "{}", e);
    }

    #[test]
    fn error_chain_joins_causes_without_repeats() {
        use std::fmt;
        #[derive(Debug)]
        struct Outer(Inner);
        #[derive(Debug)]
        struct Inner(&'static str);
        impl fmt::Display for Inner {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.0)
            }
        }
        impl fmt::Display for Outer {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("db error")
            }
        }
        impl std::error::Error for Inner {}
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        assert_eq!(
            error_chain(&Outer(Inner("ERROR: relation \"x\" does not exist"))),
            "db error: ERROR: relation \"x\" does not exist"
        );
        assert_eq!(error_chain(&Inner("db error")), "db error");
        assert_eq!(error_chain(&Outer(Inner("db error"))), "db error");
    }

    #[test]
    fn connection_and_scheme_hints() {
        assert!(with_hint("timed out waiting for connection: db error").contains("check host"));
        assert!(with_hint("Source Unknown not supported.").contains("Supported:"));
    }

    #[test]
    fn oracle_client_hint() {
        let m = with_hint("DPI-1047: Cannot locate a 64-bit Oracle Client library");
        assert!(m.contains("Instant Client"));
    }
}
