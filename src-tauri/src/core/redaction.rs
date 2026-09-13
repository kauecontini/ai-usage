use regex::Regex;
use std::sync::OnceLock;

pub fn redact(input: &str) -> String {
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static SENSITIVE_FIELD: OnceLock<Regex> = OnceLock::new();
    static LONG_TOKEN: OnceLock<Regex> = OnceLock::new();

    let bearer = BEARER.get_or_init(|| {
        Regex::new(
            r"(?i)(authorization\s*[:=]\s*bearer\s+)[^\s,;]+|\bbearer\s+[A-Za-z0-9._~+/=-]{12,}",
        )
        .unwrap()
    });
    let sensitive = SENSITIVE_FIELD.get_or_init(|| Regex::new(r#"(?i)(access[_-]?token|refresh[_-]?token|api[_-]?key|cookie|authorization)(\"?\s*[:=]\s*\"?)[^\"\s,}]+"#).unwrap());
    let long = LONG_TOKEN.get_or_init(|| Regex::new(r"\b[A-Za-z0-9_-]{48,}\b").unwrap());

    let s = bearer.replace_all(input, "[REDACTED]");
    let s = sensitive.replace_all(&s, "$1$2[REDACTED]");
    long.replace_all(&s, "[REDACTED]").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_secret_shapes() {
        assert!(
            !redact("Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456")
                .contains("abcdefghijkl")
        );
        assert!(
            !redact(r#"access_token":"abcdefghijklmnopqrstuvwxyz1234567890abcdefghijklmnop"#)
                .contains("abcdefghijklmnopqrstuvwxyz")
        );
    }
}
