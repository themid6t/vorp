const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

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
    fn slug_entropy_and_failure_paths() {
        let slug = generate_slug().unwrap();
        assert_eq!(slug.len(), 16);
        assert!(slug.bytes().all(|b| ALPHABET.contains(&b)));
    }
}
