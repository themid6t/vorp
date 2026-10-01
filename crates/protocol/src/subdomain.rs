const SYSTEM_RESERVED: &[&str] = &[
    "www",
    "api",
    "mail",
    "smtp",
    "ftp",
    "admin",
    "dash",
    "dashboard",
    "vorpd",
];

/// Shared validation for names carried by RegisterTunnel and persisted by the store.
pub fn valid_subdomain(name: &str) -> bool {
    let bytes = name.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !SYSTEM_RESERVED.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_table() {
        for (name, expected) in [
            ("app", true),
            ("a-b", true),
            ("123", true),
            ("admin", false),
            ("api", false),
            ("apis", true),
            ("ab", false),
            ("-abc", false),
            ("abc-", false),
            ("Aaa", false),
            ("a_b", false),
            ("a.b", false),
            ("dashboard", false),
        ] {
            assert_eq!(valid_subdomain(name), expected, "{name}");
        }
        assert!(valid_subdomain(&"a".repeat(63)));
        assert!(!valid_subdomain(&"a".repeat(64)));
    }
}
