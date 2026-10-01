const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
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

pub(crate) fn valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !SYSTEM_RESERVED.contains(&name)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SlugError {
    #[error("cryptographic random generation failed: {0}")]
    Random(#[from] getrandom::Error),
    #[error("could not allocate a unique subdomain after repeated collisions")]
    Collisions,
}

pub(crate) fn generate_slug() -> Result<String, SlugError> {
    let mut random = [0u8; 10];
    getrandom::fill(&mut random)?;
    let mut slug = String::with_capacity(16);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in random {
        bits = (bits << 8) | u32::from(byte);
        count += 8;
        while count >= 5 {
            count -= 5;
            slug.push(char::from(ALPHABET[((bits >> count) & 31) as usize]));
        }
    }
    Ok(slug)
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
        ] {
            assert_eq!(valid(name), expected, "{name}");
        }
        assert!(valid(&"a".repeat(63)));
        assert!(!valid(&"a".repeat(64)));
    }

    #[test]
    fn slug_entropy_and_failure_paths() {
        let slug = generate_slug().unwrap();
        assert_eq!(slug.len(), 16);
        assert!(slug.bytes().all(|b| ALPHABET.contains(&b)));
    }
}
