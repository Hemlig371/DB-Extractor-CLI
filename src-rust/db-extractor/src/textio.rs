//! Чтение текстовых файлов (запросы, строка подключения) с указанной кодировкой.

use std::fs;
use std::path::Path;

fn normalize_label(label: &str) -> String {
    label.trim().to_lowercase().replace('_', "-")
}

/// Декодирует байты в текст. Некорректные последовательности это ошибка (как в Python-версии).
pub fn decode(bytes: &[u8], encoding: &str) -> Result<String, String> {
    let label = normalize_label(encoding);
    match label.as_str() {
        // UTF-8 с необязательным BOM (значение по умолчанию)
        "utf-8-sig" | "utf8-sig" => {
            let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
            return String::from_utf8(body.to_vec()).map_err(|e| format!("invalid UTF-8: {}", e));
        }
        // UTF-16 определяется по BOM
        "utf-16" | "utf16" => {
            let (enc, bom_len) = encoding_rs::Encoding::for_bom(bytes)
                .ok_or_else(|| "UTF-16 text must start with a byte order mark".to_string())?;
            return decode_with(enc, &bytes[bom_len..]);
        }
        _ => {}
    }
    let enc =
        encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| format!("unknown encoding '{}'", encoding))?;
    decode_with(enc, bytes)
}

fn decode_with(enc: &'static encoding_rs::Encoding, bytes: &[u8]) -> Result<String, String> {
    enc.decode_without_bom_handling_and_without_replacement(bytes)
        .map(|s| s.into_owned())
        .ok_or_else(|| format!("the data is not valid {}", enc.name()))
}

pub fn read_text(path: &Path, encoding: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    decode(&bytes, encoding)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_sig_strips_bom() {
        assert_eq!(decode(b"\xEF\xBB\xBFselect 1", "utf-8-sig").unwrap(), "select 1");
        assert_eq!(decode(b"select 1", "utf-8-sig").unwrap(), "select 1");
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        assert!(decode(b"\xFF\xFE\xFA", "utf-8-sig").is_err());
    }

    #[test]
    fn legacy_encodings() {
        // «Привет» в windows-1251
        let bytes = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        assert_eq!(decode(&bytes, "cp1251").unwrap(), "Привет");
        assert_eq!(decode(&bytes, "windows-1251").unwrap(), "Привет");
        assert!(decode(b"x", "no-such-encoding").is_err());
    }

    #[test]
    fn utf16_with_bom() {
        let bytes = [0xFF, 0xFE, b'h', 0, b'i', 0];
        assert_eq!(decode(&bytes, "utf-16").unwrap(), "hi");
    }
}
