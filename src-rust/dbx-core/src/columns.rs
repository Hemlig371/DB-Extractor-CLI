//! Сопоставление имён колонок без учёта регистра (Oracle возвращает имена в верхнем регистре).

use std::collections::HashMap;

/// Сопоставляет запрошенные имена с фактическими. Точное совпадение приоритетнее;
/// неоднозначное совпадение по регистру считается ненайденным.
/// Возвращает (найденные фактические имена без повторов, ненайденные).
pub fn resolve_columns(requested: &[String], available: &[String]) -> (Vec<String>, Vec<String>) {
    let mut by_lower: HashMap<String, Vec<&String>> = HashMap::new();
    for name in available {
        by_lower.entry(name.to_lowercase()).or_default().push(name);
    }
    let mut found: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for name in requested {
        let actual: Option<String> = if available.iter().any(|a| a == name) {
            Some(name.clone())
        } else {
            match by_lower.get(&name.to_lowercase()) {
                Some(v) if v.len() == 1 => Some(v[0].clone()),
                _ => None,
            }
        };
        match actual {
            Some(a) => {
                if !found.contains(&a) {
                    found.push(a);
                }
            }
            None => missing.push(name.clone()),
        }
    }
    (found, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn case_insensitive_match() {
        let (found, missing) = resolve_columns(&v(&["email", "Phone"]), &v(&["EMAIL", "PHONE", "ID"]));
        assert_eq!(found, v(&["EMAIL", "PHONE"]));
        assert!(missing.is_empty());
    }

    #[test]
    fn exact_match_preferred_and_ambiguity_is_missing() {
        let (found, missing) = resolve_columns(&v(&["a"]), &v(&["a", "A"]));
        assert_eq!(found, v(&["a"]));
        assert!(missing.is_empty());
        let (found, missing) = resolve_columns(&v(&["b"]), &v(&["B", "b".to_uppercase().as_str(), "x"]));
        // "B" встречается дважды (дубликат), но точного "b" нет и совпадение по регистру неоднозначно
        assert!(found.is_empty());
        assert_eq!(missing, v(&["b"]));
    }

    #[test]
    fn duplicates_are_removed_and_missing_reported() {
        let (found, missing) = resolve_columns(&v(&["id", "ID", "nope"]), &v(&["ID"]));
        assert_eq!(found, v(&["ID"]));
        assert_eq!(missing, v(&["nope"]));
    }
}
