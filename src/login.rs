//! `vorp login <relay>`: store the agent token and point the agent config at
//! the relay, in one step.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{AgentLayer, AgentSettings, ConfigError, Env, Setting};

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
    #[error("{}: relay_host is set, but not on a line of its own; edit the file by hand", path.display())]
    NoRelayHostLine { path: PathBuf },
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
    if valid {
        Ok(())
    } else {
        Err(LoginError::InvalidHost(host.to_owned()))
    }
}

/// The config text with `relay_host` set to `host`, or `None` when it already
/// is. Only that one line is touched (or appended), so comments and the other
/// keys survive byte for byte.
pub(crate) fn set_relay_host(
    path: &Path,
    text: &str,
    host: &str,
    force: bool,
) -> Result<Option<String>, LoginError> {
    validate_host(host)?;
    let current = AgentLayer::parse(path, text)?.relay_host;
    let new_line = format!("relay_host: {host}");
    let Some(current) = current else {
        let separator = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return Ok(Some(format!("{text}{separator}{new_line}\n")));
    };
    if current == host {
        return Ok(None);
    }
    if !force {
        return Err(LoginError::OtherRelay {
            path: path.to_owned(),
            current,
            requested: host.to_owned(),
        });
    }
    let mut replaced = false;
    let lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| {
            let Some(rest) = line.strip_prefix("relay_host") else {
                return line.to_owned();
            };
            if replaced || !rest.trim_start().starts_with(':') {
                return line.to_owned();
            }
            replaced = true;
            // Keep a trailing comment; YAML needs whitespace before `#`.
            let comment = rest.find(" #").map_or("", |at| {
                rest[rest[..at].trim_end().len()..].trim_end_matches(['\r', '\n'])
            });
            let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
            format!("{new_line}{comment}{ending}")
        })
        .collect();
    if !replaced {
        return Err(LoginError::NoRelayHostLine {
            path: path.to_owned(),
        });
    }
    Ok(Some(lines.concat()))
}

/// The whole command, with the token source injected so tests need no
/// terminal. Refusals happen before the token is read or anything is written.
pub(crate) fn login(
    config: &Setting<PathBuf>,
    env: &Env,
    host: &str,
    force: bool,
    read_token: impl FnOnce() -> Result<String>,
) -> Result<()> {
    let path = &config.value;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("read config file {}", path.display()));
        }
    };
    let updated = set_relay_host(path, &text, host, force)?;

    let file = AgentLayer::parse(path, &text)?;
    let default_token = path.with_file_name("authtoken");
    let token_file = AgentSettings::merge(
        AgentLayer::default(),
        AgentLayer::from_env(env)?,
        file,
        Some(default_token),
    )
    .token_file
    .context("no token file path")?;

    let token = read_token()?;
    let token = crate::token::validate(&token)?;
    crate::token::save(&token_file.value, token)?;

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
    use crate::config::Source;

    const PATH: &str = "/home/u/.config/vorp/config.yaml";

    #[test]
    fn set_relay_host_cases() {
        let path = Path::new(PATH);
        let cases: &[(&str, &str, bool, Option<&str>)] = &[
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
        for (text, host, force, expected) in cases {
            let got = set_relay_host(path, text, host, *force).expect(text);
            assert_eq!(got.as_deref(), *expected, "{text:?}");
        }
    }

    #[test]
    fn set_relay_host_refusals() {
        let path = Path::new(PATH);
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
        assert!(matches!(
            set_relay_host(path, "{relay_host: a.example.com}\n", "b.example.com", true),
            Err(LoginError::NoRelayHostLine { .. })
        ));
        assert!(matches!(
            set_relay_host(path, "relay_hots: a\n", "b.example.com", true),
            Err(LoginError::Config(ConfigError::UnknownKey { .. }))
        ));
    }

    fn named(path: PathBuf) -> Setting<PathBuf> {
        Setting::new(path, Source::Flag)
    }

    #[test]
    fn fresh_directory_gets_token_and_config() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let config = dir.path().join("vorp").join("config.yaml");
        login(
            &named(config.clone()),
            &Env::from_pairs(&[]),
            "tunnels.example.com",
            false,
            || Ok("vorp_1_secret\n".to_owned()),
        )
        .expect("login succeeds");

        let token_path = dir.path().join("vorp").join("authtoken");
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
    fn existing_config_keeps_comments_and_other_keys() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let config = dir.path().join("config.yaml");
        let original = "# my agent\nupstream: http://127.0.0.1:3000  # app\n\n# subdomains: [x]\n";
        std::fs::write(&config, original).expect("write config");
        login(
            &named(config.clone()),
            &Env::from_pairs(&[]),
            "tunnels.example.com",
            false,
            || Ok("t".to_owned()),
        )
        .expect("login succeeds");
        assert_eq!(
            std::fs::read_to_string(&config).expect("config exists"),
            format!("{original}relay_host: tunnels.example.com\n")
        );
    }

    #[test]
    fn other_relay_without_force_changes_nothing() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let config = dir.path().join("config.yaml");
        let original = "relay_host: old.example.com # keep\n";
        std::fs::write(&config, original).expect("write config");
        let error = login(
            &named(config.clone()),
            &Env::from_pairs(&[]),
            "new.example.com",
            false,
            || panic!("the token must not be read before the refusal"),
        )
        .expect_err("refused");
        assert!(error.to_string().contains("old.example.com"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&config).expect("config exists"),
            original
        );
        assert!(!dir.path().join("authtoken").exists());

        login(
            &named(config.clone()),
            &Env::from_pairs(&[]),
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
        let dir = tempfile::tempdir().expect("create temp dir");
        let config = dir.path().join("config.yaml");
        let result = login(
            &named(config.clone()),
            &Env::from_pairs(&[]),
            "tunnels.example.com",
            false,
            || Ok("  \n".to_owned()),
        );
        assert!(result.is_err());
        assert!(!config.exists());
        assert!(!dir.path().join("authtoken").exists());
    }

    #[test]
    fn token_goes_to_the_configured_token_file() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let config = dir.path().join("config.yaml");
        let token_file = dir.path().join("secrets").join("vorp-token");
        std::fs::write(&config, format!("token_file: {}\n", token_file.display()))
            .expect("write config");
        login(
            &named(config),
            &Env::from_pairs(&[]),
            "tunnels.example.com",
            false,
            || Ok("t".to_owned()),
        )
        .expect("login succeeds");
        assert_eq!(
            std::fs::read_to_string(token_file).expect("token written"),
            "t"
        );
    }
}
