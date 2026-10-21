//! Маскирование пароля из строки подключения в тексте ошибок, журнале и на консоли.
//! Порт `mask_secrets` из Python-версии (без регулярных выражений).

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = (bytes[i + 1] as char).to_digit(16);
            let l = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (h, l) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Пароль из `scheme://user:password@host/...` (как `urllib.parse.urlsplit(...).password`).
pub fn password_of(conn: &str) -> Option<String> {
    let after_scheme = conn.find("://")? + 3;
    let rest = &conn[after_scheme..];
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..netloc_end];
    let at = netloc.rfind('@')?;
    let userinfo = &netloc[..at];
    let colon = userinfo.find(':')?;
    Some(userinfo[colon + 1..].to_string())
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `(://[^/\s:@]*):[^/\s@]*@` -> `\1:***@`
fn mask_url_userinfo(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        if chars[i] == ':' && i + 2 < n && chars[i + 1] == '/' && chars[i + 2] == '/' {
            let mut j = i + 3;
            while j < n && !matches!(chars[j], '/' | ':' | '@') && !chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && chars[j] == ':' {
                let mut k = j + 1;
                while k < n && !matches!(chars[k], '/' | '@') && !chars[k].is_whitespace() {
                    k += 1;
                }
                if k < n && chars[k] == '@' {
                    out.extend(chars[i..=j].iter());
                    out.push_str("***@");
                    i = k + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `(?i)\b(password|pwd)=[^&\s;]*` -> `\1=***`
fn mask_key_values(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        let boundary = i == 0 || !is_word_char(chars[i - 1]);
        if boundary {
            let mut matched: Option<usize> = None;
            for key in ["password", "pwd"] {
                let kc: Vec<char> = key.chars().collect();
                if i + kc.len() < n
                    && chars[i..i + kc.len()]
                        .iter()
                        .zip(kc.iter())
                        .all(|(a, b)| a.to_ascii_lowercase() == *b)
                    && chars[i + kc.len()] == '='
                {
                    matched = Some(i + kc.len());
                    break;
                }
            }
            if let Some(eq) = matched {
                out.extend(chars[i..=eq].iter());
                out.push_str("***");
                let mut k = eq + 1;
                while k < n && chars[k] != '&' && chars[k] != ';' && !chars[k].is_whitespace() {
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Убирает пароль из текста: и по строке подключения, и по общим шаблонам.
pub fn mask_secrets(text: &str, conn: &str) -> String {
    let mut text = text.to_string();
    if !conn.is_empty() {
        if let Some(pw) = password_of(conn) {
            let mut secrets: Vec<String> = vec![pw.clone(), percent_decode(&pw)];
            secrets.retain(|s| !s.is_empty());
            secrets.sort();
            secrets.dedup();
            secrets.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
            for s in secrets {
                text = text.replace(&s, "***");
            }
        }
    }
    mask_key_values(&mask_url_userinfo(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_is_removed_from_connection_string_in_text() {
        let conn = "postgresql://user:s3cret@host:5432/db";
        let msg = "connection failed for postgresql://user:s3cret@host:5432/db: refused";
        let masked = mask_secrets(msg, conn);
        assert!(!masked.contains("s3cret"), "{}", masked);
        assert!(masked.contains("user:***@host"), "{}", masked);
    }

    #[test]
    fn password_with_special_characters() {
        let conn = "oracle://scott:p%40ss:w%2Frd@db:1521/svc";
        assert_eq!(password_of(conn).as_deref(), Some("p%40ss:w%2Frd"));
        let masked = mask_secrets("login failed: p@ss:w/rd and p%40ss:w%2Frd", conn);
        assert!(!masked.contains("p@ss"), "{}", masked);
        assert!(!masked.contains("p%40ss"), "{}", masked);
    }

    #[test]
    fn password_param_and_pwd_param_are_masked() {
        let masked = mask_secrets("bad: Password=abc123&x=1 and PWD=zzz;next", "sqlite://x");
        assert_eq!(masked, "bad: Password=***&x=1 and PWD=***;next");
    }

    #[test]
    fn word_boundary_for_key_values() {
        let masked = mask_secrets("mypassword=keep", "");
        assert_eq!(masked, "mypassword=keep");
    }

    #[test]
    fn text_without_secrets_is_unchanged() {
        let msg = "ORA-00942: table or view does not exist";
        assert_eq!(mask_secrets(msg, "oracle://u:p@h:1521/s"), msg);
    }

    #[test]
    fn userinfo_pattern_masks_unknown_urls() {
        let masked = mask_secrets("failed at mysql://root:hunter2@10.0.0.1/db", "");
        assert_eq!(masked, "failed at mysql://root:***@10.0.0.1/db");
    }

    #[test]
    fn no_password_means_nothing_to_mask() {
        assert_eq!(password_of("sqlite:///tmp/a.db"), None);
        assert_eq!(password_of("postgresql://user@host/db"), None);
    }
}
