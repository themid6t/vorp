//! `vorp login <relay>`: store the agent token and point the agent config at
//! the relay, in one step.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{self, AgentLayer, ConfigError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum LoginError {
    #[error("relay name {0:?} is not a host name")]
    InvalidHost(String),
    #[error(
        "{} already uses relay_host {current}; pass --force to replace it with {requested}",
        path.display()
    )]
    OtherRelay {
        path: PathBuf,
        current: String,
        requested: String,
    },
    #[error("{}: cannot set relay_host by editing one line; edit the file by hand", path.display())]
    CannotEdit { path: PathBuf },
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// A DNS name or IPv4 address. Restricting the characters also keeps the
/// value from changing the meaning of the YAML line it is written into.
fn validate_host(host: &str) -> Result<(), LoginError> {
    let valid = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    valid
        .then_some(())
        .ok_or_else(|| LoginError::InvalidHost(host.to_owned()))
}

/// The config text with `relay_host` set to `host`, or `None` when it already
/// is. Only that one line changes (or is appended), so comments and the other
/// keys survive byte for byte. The result is parsed again, so an edit that
/// would not mean `relay_host: host` is refused instead of written.
pub(crate) fn set_relay_host(
    path: &Path,
    text: &str,
    host: &str,
    force: bool,
) -> Result<Option<String>, LoginError> {
    validate_host(host)?;
    let updated = match config::parse::<AgentLayer>(path, text, true)?.relay_host {
        Some(current) if current == host => return Ok(None),
        Some(current) if !force => {
            return Err(LoginError::OtherRelay {
                path: path.to_owned(),
                current,
                requested: host.to_owned(),
            });
        }
        Some(_) => replace_line(text, host),
        None if text.is_empty() || text.ends_with('\n') => format!("{text}relay_host: {host}\n"),
        None => format!("{text}\nrelay_host: {host}\n"),
    };
    match config::parse::<AgentLayer>(path, &updated, true) {
        Ok(layer) if layer.relay_host.as_deref() == Some(host) => Ok(Some(updated)),
        _ => Err(LoginError::CannotEdit {
            path: path.to_owned(),
        }),
    }
}

/// Rewrites the first top-level `relay_host:` line, keeping its trailing
/// comment and line ending.
fn replace_line(text: &str, host: &str) -> String {
    let mut done = false;
    text.split_inclusive('\n')
        .map(|line| {
            let body = line.trim_end_matches(['\r', '\n']);
            let Some(rest) = body.strip_prefix("relay_host") else {
                return line.to_owned();
            };
            if done || !rest.trim_start().starts_with(':') {
                return line.to_owned();
            }
            done = true;
            // YAML needs whitespace before `#`; keep all of it.
            let comment = rest
                .find(" #")
                .map_or("", |at| &rest[rest[..at].trim_end().len()..]);
            format!("relay_host: {host}{comment}{}", &line[body.len()..])
        })
        .collect()
}

/// The whole command, with the token source injected so tests need no
/// terminal. Refusals happen before the token is read or anything is written.
/// The token goes to `token_file` (a flag or `VORP_TOKEN_FILE`), else the
/// file's `token_file`, else `authtoken` next to the config.
pub(crate) fn login(
    path: &Path,
    token_file: Option<PathBuf>,
    host: &str,
    force: bool,
    read_token: impl FnOnce() -> Result<String>,
) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("read config file {}", path.display()));
        }
    };
    let updated = set_relay_host(path, &text, host, force)?;
    let token_file = token_file
        .or(config::parse::<AgentLayer>(path, &text, true)?.token_file)
        .unwrap_or_else(|| path.with_file_name("authtoken"));

    let token = read_token()?;
    crate::token::save(&token_file, crate::token::validate(&token)?)?;

    if let Some(updated) = updated {
        if let Some(directory) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(directory)
                .with_context(|| format!("create config directory {}", directory.display()))?;
        }
        std::fs::write(path, updated)
            .with_context(|| format!("write config file {}", path.display()))?;
    }
    tracing::info!(config = %path.display(), relay_host = %host, "agent config saved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TempDir;

    #[test]
    fn set_relay_host_cases() {
        let path = Path::new("/home/u/.config/vorp/config.yaml");
        // (existing text, host, --force, the new text or `None` for unchanged)
        let ok: &[(&str, &str, bool, Option<&str>)] = &[
            (
                "",
                "a.example.com",
                false,
                Some("relay_host: a.example.com\n"),
            ),
            (
                "# mine\nupstream: http://127.0.0.1:3000",
                "a.example.com",
                false,
                Some("# mine\nupstream: http://127.0.0.1:3000\nrelay_host: a.example.com\n"),
            ),
            ("relay_host: a.example.com\n", "a.example.com", false, None),
            (
                "# relay\nrelay_host: a.example.com  # prod\n# end\nsubdomains: [x]\n",
                "b.example.com",
                true,
                Some("# relay\nrelay_host: b.example.com  # prod\n# end\nsubdomains: [x]\n"),
            ),
            (
                "relay_host : \"a.example.com\"\r\nupstream: http://localhost:1\r\n",
                "b.example.com",
                true,
                Some("relay_host: b.example.com\r\nupstream: http://localhost:1\r\n"),
            ),
        ];
        for (text, host, force, expected) in ok {
            let got = set_relay_host(path, text, host, *force).expect(text);
            assert_eq!(got.as_deref(), *expected, "{text:?}");
        }

        assert!(matches!(
            set_relay_host(path, "relay_host: a.example.com\n", "b.example.com", false),
            Err(LoginError::OtherRelay { current, .. }) if current == "a.example.com"
        ));
        for host in ["", "a b", "a\ntoken: x", "a#b", "a:443", "\"a\""] {
            assert!(
                matches!(
                    set_relay_host(path, "", host, true),
                    Err(LoginError::InvalidHost(_))
                ),
                "{host:?}"
            );
        }
        // Edits a single line cannot make: a flow mapping, a value on the next
        // line (which would become `b.example.com a.example.com`), and a null
        // value (appending would duplicate the key).
        for text in [
            "{relay_host: a.example.com}\n",
            "relay_host:\n  a.example.com\n",
            "relay_host: ~\n",
        ] {
            assert!(
                matches!(
                    set_relay_host(path, text, "b.example.com", true),
                    Err(LoginError::CannotEdit { .. })
                ),
                "{text:?}"
            );
        }
        assert!(matches!(
            set_relay_host(path, "relay_hots: a\n", "b.example.com", true),
            Err(LoginError::Config(ConfigError::UnknownKey { .. }))
        ));
    }

    #[test]
    fn fresh_directory_gets_token_and_config() {
        let dir = TempDir::new("login-fresh");
        let config = dir.0.join("vorp").join("config.yaml");
        login(&config, None, "tunnels.example.com", false, || {
            Ok("vorp_1_secret\n".to_owned())
        })
        .expect("login succeeds");

        let token_path = dir.0.join("vorp").join("authtoken");
        assert_eq!(
            std::fs::read_to_string(&token_path).expect("token written"),
            "vorp_1_secret"
        );
        assert_eq!(
            std::fs::read_to_string(&config).expect("config written"),
            "relay_host: tunnels.example.com\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&token_path)
                .expect("token exists")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn existing_config_keeps_comments_and_token_file() {
        let dir = TempDir::new("login-existing");
        let config = dir.0.join("config.yaml");
        let token_file = dir.0.join("secrets").join("vorp-token");
        let original = format!(
            "# my agent\nupstream: http://127.0.0.1:3000  # app\ntoken_file: {}\n\n# subdomains: [x]\n",
            token_file.display()
        );
        std::fs::write(&config, &original).expect("write config");
        login(&config, None, "tunnels.example.com", false, || {
            Ok("t".to_owned())
        })
        .expect("login succeeds");
        assert_eq!(
            std::fs::read_to_string(&config).expect("config exists"),
            format!("{original}relay_host: tunnels.example.com\n")
        );
        assert_eq!(
            std::fs::read_to_string(token_file).expect("token written"),
            "t"
        );
    }

    #[test]
    fn other_relay_without_force_changes_nothing() {
        let dir = TempDir::new("login-other");
        let config = dir.0.join("config.yaml");
        let original = "relay_host: old.example.com # keep\n";
        std::fs::write(&config, original).expect("write config");
        let error = login(&config, None, "new.example.com", false, || {
            panic!("the token must not be read before the refusal")
        })
        .expect_err("refused");
        assert!(error.to_string().contains("old.example.com"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&config).expect("config exists"),
            original
        );
        assert!(!dir.0.join("authtoken").exists());

        login(
            &config,
            None,
            "new.example.com",
            true,
            || Ok("t".to_owned()),
        )
        .expect("--force replaces it");
        assert_eq!(
            std::fs::read_to_string(&config).expect("config exists"),
            "relay_host: new.example.com # keep\n"
        );
    }

    #[test]
    fn empty_token_is_refused_and_nothing_is_written() {
        let dir = TempDir::new("login-empty");
        let config = dir.0.join("config.yaml");
        let result = login(&config, None, "tunnels.example.com", false, || {
            Ok("  \n".to_owned())
        });
        assert!(result.is_err());
        assert!(!config.exists());
        assert!(!dir.0.join("authtoken").exists());
    }
}
