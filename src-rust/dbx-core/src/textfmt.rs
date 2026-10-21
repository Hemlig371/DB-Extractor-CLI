//! Текстовое представление значений в CSV и JSONL. Форматы совпадают с тем, что пишет polars
//! в сборке на Python, поэтому обе сборки дают одинаковые файлы.

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Бинарные данные как строчный шестнадцатеричный текст.
pub fn push_hex(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.reserve(bytes.len() * 2);
    for b in bytes {
        buf.push(HEX[(b >> 4) as usize]);
        buf.push(HEX[(b & 0x0f) as usize]);
    }
}

/// Десятичное число с фиксированным количеством знаков после точки (хвостовые нули сохраняются).
pub fn push_decimal(buf: &mut Vec<u8>, value: i128, scale: usize) {
    let mut stack = [0u8; 40];
    let mut start = stack.len();
    let mut rest = value.unsigned_abs();
    loop {
        start -= 1;
        stack[start] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    let digits = &stack[start..];
    if value < 0 {
        buf.push(b'-');
    }
    if scale == 0 {
        buf.extend_from_slice(digits);
        return;
    }
    if digits.len() > scale {
        let whole = digits.len() - scale;
        buf.extend_from_slice(&digits[..whole]);
        buf.push(b'.');
        buf.extend_from_slice(&digits[whole..]);
    } else {
        buf.extend_from_slice(b"0.");
        buf.resize(buf.len() + (scale - digits.len()), b'0');
        buf.extend_from_slice(digits);
    }
}

/// Строка в CSV: в кавычках, если содержит запятую, перевод строки или кавычку; пустая строка пишется
/// как `""`, чтобы отличаться от NULL (он пишется пустым полем).
pub fn push_csv_str(buf: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        buf.extend_from_slice(b"\"\"");
        return;
    }
    let quoted = s.bytes().any(|b| matches!(b, b',' | b'\n' | b'\r' | b'"'));
    if !quoted {
        buf.extend_from_slice(s.as_bytes());
        return;
    }
    buf.push(b'"');
    for b in s.bytes() {
        if b == b'"' {
            buf.push(b'"');
        }
        buf.push(b);
    }
    buf.push(b'"');
}

/// Строка в JSON: экранируются кавычка, обратная косая черта и управляющие символы; остальное как есть (UTF-8).
pub fn push_json_str(buf: &mut Vec<u8>, s: &str) {
    buf.push(b'"');
    for &b in s.as_bytes() {
        match b {
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            0x08 => buf.extend_from_slice(b"\\b"),
            0x09 => buf.extend_from_slice(b"\\t"),
            0x0a => buf.extend_from_slice(b"\\n"),
            0x0c => buf.extend_from_slice(b"\\f"),
            0x0d => buf.extend_from_slice(b"\\r"),
            0x00..=0x1f => {
                buf.extend_from_slice(b"\\u00");
                buf.push(HEX[(b >> 4) as usize]);
                buf.push(HEX[(b & 0x0f) as usize]);
            }
            _ => buf.push(b),
        }
    }
    buf.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut b = Vec::new();
        f(&mut b);
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(s(|b| push_hex(b, &[0x00, 0xab, 0xff, 0x10])), "00abff10");
        assert_eq!(s(|b| push_hex(b, &[])), "");
    }

    #[test]
    fn decimal_keeps_scale() {
        assert_eq!(s(|b| push_decimal(b, 123456780000, 10)), "12.3456780000");
        assert_eq!(s(|b| push_decimal(b, 5, 3)), "0.005");
        assert_eq!(s(|b| push_decimal(b, -5, 3)), "-0.005");
        assert_eq!(s(|b| push_decimal(b, 0, 2)), "0.00");
        assert_eq!(s(|b| push_decimal(b, 12345, 0)), "12345");
        assert_eq!(s(|b| push_decimal(b, -12345, 0)), "-12345");
        assert_eq!(s(|b| push_decimal(b, 1234, 2)), "12.34");
        assert_eq!(s(|b| push_decimal(b, 100, 2)), "1.00");
        assert_eq!(s(|b| push_decimal(b, i128::MIN, 0)), "-170141183460469231731687303715884105728");
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        assert_eq!(s(|b| push_csv_str(b, "abc")), "abc");
        assert_eq!(s(|b| push_csv_str(b, "a b")), "a b");
        assert_eq!(s(|b| push_csv_str(b, "")), "\"\"");
        assert_eq!(s(|b| push_csv_str(b, "a,b")), "\"a,b\"");
        assert_eq!(s(|b| push_csv_str(b, "a\nb")), "\"a\nb\"");
        assert_eq!(s(|b| push_csv_str(b, "a\rb")), "\"a\rb\"");
        assert_eq!(s(|b| push_csv_str(b, "say \"hi\"")), "\"say \"\"hi\"\"\"");
        assert_eq!(s(|b| push_csv_str(b, "a;b\tc")), "a;b\tc");
        assert_eq!(s(|b| push_csv_str(b, "привет")), "привет");
    }

    #[test]
    fn json_string_escapes_like_serde() {
        assert_eq!(s(|b| push_json_str(b, "a\"b\\c")), r#""a\"b\\c""#);
        assert_eq!(s(|b| push_json_str(b, "\n\r\t\u{8}\u{c}")), r#""\n\r\t\b\f""#);
        assert_eq!(s(|b| push_json_str(b, "\u{1}\u{1f}")), r#""\u0001\u001f""#);
        assert_eq!(s(|b| push_json_str(b, "/ é 😀 \u{7f}")), "\"/ é 😀 \u{7f}\"");
        assert_eq!(s(|b| push_json_str(b, "")), "\"\"");
    }
}
