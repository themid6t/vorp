//! Reading and storing the agent token. The token never goes in the config
//! file; it lives in its own `0600` file.

use std::{
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::config::agent_dir;

const MAX_TOKEN_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenError {
    #[error("agent token must contain 1 to {} bytes", MAX_TOKEN_BYTES - 1)]
    BadLength,
}

/// Trims surrounding whitespace and rejects an empty or oversized token.
pub(crate) fn validate(token: &str) -> Result<&str, TokenError> {
    let token = token.trim();
    if token.is_empty() || token.len() >= MAX_TOKEN_BYTES {
        return Err(TokenError::BadLength);
    }
    Ok(token)
}

/// `authtoken` in the agent's config directory.
pub(crate) fn default_path() -> Result<PathBuf> {
    agent_dir(|name| std::env::var_os(name))
        .map(|dir| dir.join("authtoken"))
        .context("no config directory; pass --token-file or set VORP_TOKEN")
}

/// Reads the token from standard input: a hidden prompt on a terminal, the
/// piped bytes otherwise. It is never a command-line argument, which other
/// local users could read from the process list.
pub(crate) fn read_from_stdin(prompt: bool) -> Result<String> {
    if prompt && std::io::stdin().is_terminal() {
        return rpassword::prompt_password("Agent token: ").context("read agent token");
    }
    let mut token = String::new();
    std::io::stdin()
        .take(MAX_TOKEN_BYTES as u64)
        .read_to_string(&mut token)
        .context("read agent token from standard input")?;
    Ok(token)
}

/// Writes `token` to `path` with mode `0600`, creating the directory.
pub(crate) fn save(path: &Path, token: &str) -> Result<()> {
    let directory = path
        .parent()
        .context("token file has no parent directory")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create token directory {}", directory.display()))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("open token file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure token file {}", path.display()))?;
    }
    file.write_all(token.as_bytes())
        .with_context(|| format!("write token file {}", path.display()))?;
    tracing::info!(path = %path.display(), "agent token stored");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_trims_and_bounds_the_token() {
        assert_eq!(validate("  vorp_1_abc\n").ok(), Some("vorp_1_abc"));
        for bad in ["", " \n\t", &"x".repeat(MAX_TOKEN_BYTES)] {
            assert!(matches!(validate(bad), Err(TokenError::BadLength)));
        }
    }
}
