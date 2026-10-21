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

/// Добавляет подсказку к известным причинам сбоя.
pub fn with_hint(message: &str) -> String {
    let lower = message.to_lowercase();
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
    fn oracle_client_hint() {
        let m = with_hint("DPI-1047: Cannot locate a 64-bit Oracle Client library");
        assert!(m.contains("Instant Client"));
    }
}
