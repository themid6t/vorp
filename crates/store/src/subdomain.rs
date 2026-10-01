pub(crate) fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && ![
            "www",
            "api",
            "mail",
            "smtp",
            "ftp",
            "admin",
            "dash",
            "dashboard",
            "vorpd",
        ]
        .contains(&name)
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn names_reject_reserved_and_invalid_values() {
        for name in [
            "a",
            "-foo",
            "foo-",
            "Foo",
            "www",
            "api",
            "dashboard",
            "bad.name",
        ] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(valid_name("good-name"));
    }
}
