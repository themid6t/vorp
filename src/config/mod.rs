//! Layered settings: flag > `VORP_*` environment > YAML file > default.
//!
//! clap resolves the flag, the environment and the built-in default; the
//! config file slots in under the first two. Parsing and merging are pure;
//! only [`read`] touches the file system.

mod agent;
mod relay;

use std::{
    ffi::OsString,
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};
use serde_saphyr::Spanned;

pub(crate) use agent::AgentLayer;
pub(crate) use relay::{DevArgs, RelayLayer};

pub(crate) const RELAY_CONFIG: &str = "/etc/vorp/vorp.yaml";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// Only for a path the user named; a missing default file is not an error.
    #[error("config file {} does not exist", path.display())]
    NotFound { path: PathBuf },
    #[error("read config file {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Malformed YAML or a value of the wrong type. `message` ends with the
    /// line and column.
    #[error("{}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("{}:{line}: unknown key `{key}`, expected one of: {expected}", path.display())]
    UnknownKey {
        path: PathBuf,
        line: u64,
        key: String,
        expected: String,
    },
    #[error("{}:{line}: `{key}` is a secret and is not allowed in the config file; {hint}", path.display())]
    SecretKey {
        path: PathBuf,
        line: u64,
        key: &'static str,
        hint: &'static str,
    },
    #[error("{}:{line}: `tunnels`: multi-tunnel config is not supported yet", path.display())]
    UnsupportedTunnels { path: PathBuf, line: u64 },
    #[error("tls: {0}")]
    Tls(&'static str),
    #[error("{key} is not set: pass --{flag}, set {env}, or add `{key}` to the config file")]
    Missing {
        key: &'static str,
        flag: String,
        env: String,
    },
}

/// The value of a setting that has no default, or a [`ConfigError::Missing`]
/// naming the three ways to set it.
fn required<T>(value: Option<T>, key: &'static str) -> Result<T, ConfigError> {
    value.ok_or_else(|| ConfigError::Missing {
        key,
        flag: arg_id(key).replace('_', "-"),
        env: format!("VORP_{}", key.replace('.', "_").to_uppercase()),
    })
}

/// The clap id of a config key: its last segment, so `limits.max_connections`
/// is the `--max-connections` flag.
fn arg_id(key: &str) -> &str {
    key.rsplit('.').next().unwrap_or(key)
}

/// The agent's per-user directory, holding `config.yaml` and `authtoken`:
/// `$XDG_CONFIG_HOME/vorp`, else `%APPDATA%\vorp`, else `~/.config/vorp`. An
/// empty variable counts as unset, as the XDG spec says.
pub(crate) fn agent_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |name| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    var("XDG_CONFIG_HOME")
        .or_else(|| var("APPDATA"))
        .or_else(|| var("HOME").map(|home| home.join(".config")))
        .map(|dir| dir.join("vorp"))
}

/// Reads and parses a config file. A missing file is `None` unless the user
/// named it with `--config` or `VORP_CONFIG`.
pub(crate) fn read<T: DeserializeOwned>(
    path: &Path,
    named: bool,
    reserve_tunnels: bool,
) -> Result<Option<T>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text, reserve_tunnels).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !named => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(ConfigError::NotFound {
            path: path.to_owned(),
        }),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

/// The directory a config file's relative paths are relative to.
pub(crate) fn file_dir(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new(""))
}

/// Makes a relative path from the config file relative to the file's
/// directory `dir`. Flag and environment paths stay relative to the working
/// directory; they never pass through here.
fn resolve(dir: &Path, path: &mut Option<PathBuf>) {
    if let Some(path) = path.as_mut().filter(|path| path.is_relative()) {
        *path = dir.join(&*path);
    }
}

/// Keys with a reserved meaning, found before the strict parse so they get
/// their own message instead of "unknown key".
#[derive(Deserialize)]
struct Reserved {
    token: Option<Spanned<IgnoredAny>>,
    dev_token: Option<Spanned<IgnoredAny>>,
    tunnels: Option<Spanned<IgnoredAny>>,
}

/// Parses a config document strictly: secret keys, unknown keys and wrong
/// types are errors. `tunnels` is reserved for the agent's v0.2 multi-tunnel
/// config; for the relay it is an ordinary unknown key.
pub(crate) fn parse<T: DeserializeOwned>(
    path: &Path,
    text: &str,
    reserve_tunnels: bool,
) -> Result<T, ConfigError> {
    let reserved: Reserved = serde_saphyr::from_str(text).map_err(|e| yaml_error(path, &e))?;
    let secret = |value: Spanned<IgnoredAny>, key, hint| ConfigError::SecretKey {
        path: path.to_owned(),
        line: value.referenced.line(),
        key,
        hint,
    };
    if let Some(value) = reserved.token {
        return Err(secret(
            value,
            "token",
            "save the agent token with `vorp login` or `vorp authtoken`, and set `token_file` to its path if it is not in the default place",
        ));
    }
    if let Some(value) = reserved.dev_token {
        return Err(secret(
            value,
            "dev_token",
            "the development token is set only with --dev-token or VORP_DEV_TOKEN",
        ));
    }
    if let (true, Some(tunnels)) = (reserve_tunnels, reserved.tunnels) {
        return Err(ConfigError::UnsupportedTunnels {
            path: path.to_owned(),
            line: tunnels.referenced.line(),
        });
    }
    serde_saphyr::from_str(text).map_err(|e| yaml_error(path, &e))
}

/// The message without a source snippet, which could echo a secret value.
fn yaml_error(path: &Path, error: &serde_saphyr::Error) -> ConfigError {
    match error.without_snippet() {
        serde_saphyr::Error::SerdeUnknownField {
            field, expected, ..
        } => ConfigError::UnknownKey {
            path: path.to_owned(),
            line: error.location().map_or(0, |location| location.line()),
            key: field.clone(),
            expected: expected.join(", "),
        },
        other => ConfigError::Parse {
            path: path.to_owned(),
            message: other.to_string(),
        },
    }
}

/// The layer an effective setting came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Flag,
    Env,
    File,
    Default,
    /// Unset, so it falls back on the setting named here.
    Fallback(&'static str),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flag => f.write_str("flag"),
            Self::Env => f.write_str("env"),
            Self::File => f.write_str("file"),
            Self::Default => f.write_str("default"),
            Self::Fallback(key) => write!(f, "default: {key}"),
        }
    }
}

/// One line of `vorp config show`: the key, then the value and its source,
/// or `None` when the setting is unset.
pub(crate) type Row = (&'static str, Option<(String, Source)>);

/// How a value is printed by `vorp config show`.
pub(crate) trait Show {
    fn show(&self) -> String;
}

macro_rules! show_with_display {
    ($($type:ty),*) => {$(
        impl Show for $type {
            fn show(&self) -> String {
                self.to_string()
            }
        }
    )*};
}
show_with_display!(String, SocketAddr, bool, usize, u64);

impl Show for PathBuf {
    fn show(&self) -> String {
        self.display().to_string()
    }
}

impl Show for Vec<String> {
    fn show(&self) -> String {
        self.join(",")
    }
}

/// Merges the file layer under the clap layer, recording a [`Row`] for each
/// setting in the order merged.
pub(crate) struct Merge<'a> {
    /// `Flag` or `Env` when the user set the clap id; `None` when clap holds
    /// a built-in default or nothing.
    explicit: &'a dyn Fn(&str) -> Option<Source>,
    pub(crate) rows: Vec<Row>,
}

impl<'a> Merge<'a> {
    pub(crate) fn new(explicit: &'a dyn Fn(&str) -> Option<Source>) -> Self {
        Self {
            explicit,
            rows: Vec::new(),
        }
    }

    pub(crate) fn pick<T: Show>(
        &mut self,
        key: &'static str,
        clap: Option<T>,
        file: Option<T>,
    ) -> Option<T> {
        let (value, source) = match ((self.explicit)(arg_id(key)), file) {
            (Some(source), _) => (clap, source),
            (None, Some(file)) => (Some(file), Source::File),
            (None, None) => (clap, Source::Default),
        };
        self.rows
            .push((key, value.as_ref().map(|value| (value.show(), source))));
        value
    }

    /// Shows `value` for the unset setting `key`, which falls back on the
    /// setting `from`.
    pub(crate) fn fallback(&mut self, key: &str, from: &'static str, value: Option<String>) {
        if let Some(row) = self.rows.iter_mut().find(|(k, v)| *k == key && v.is_none()) {
            row.1 = value.map(|value| (value, Source::Fallback(from)));
        }
    }
}

/// A directory under the system temp dir, removed on drop.
#[cfg(test)]
pub(crate) struct TempDir(pub(crate) PathBuf);

#[cfg(test)]
impl TempDir {
    pub(crate) fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vorp-{name}-{}", std::process::id()));
        // A leftover from an earlier, aborted run; absence is the normal case.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

#[cfg(test)]
impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort cleanup; a leftover temp dir is harmless.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (relay?, YAML, expected message after the path, check of the variant)
    type ParseCase = (bool, &'static str, &'static str, fn(&ConfigError) -> bool);

    #[test]
    fn parse_failures() {
        let path = Path::new("/etc/vorp/vorp.yaml");
        let cases: &[ParseCase] = &[
            (
                false,
                "relay_host: a\nrelay_hots: b\n",
                ":2: unknown key `relay_hots`",
                |e| matches!(e, ConfigError::UnknownKey { line: 2, key, .. } if key == "relay_hots"),
            ),
            (
                true,
                "limits:\n  max_conections: 1\n",
                ":2: unknown key `max_conections`",
                |e| matches!(e, ConfigError::UnknownKey { line: 2, .. }),
            ),
            (true, "dev_self_signed: true\n", ":1: unknown key", |e| {
                matches!(e, ConfigError::UnknownKey { .. })
            }),
            (true, "tunnels: {}\n", ":1: unknown key `tunnels`", |e| {
                matches!(e, ConfigError::UnknownKey { .. })
            }),
            (
                false,
                "tunnels: {web: {upstream: x}}\n",
                ":1: `tunnels`: multi-tunnel",
                |e| matches!(e, ConfigError::UnsupportedTunnels { line: 1, .. }),
            ),
            (
                false,
                "relay_host: a\ntoken: hunter2\n",
                ":2: `token` is a secret",
                |e| {
                    matches!(
                        e,
                        ConfigError::SecretKey {
                            line: 2,
                            key: "token",
                            ..
                        }
                    )
                },
            ),
            (true, "token: hunter2\n", ":1: `token` is a secret", |e| {
                matches!(e, ConfigError::SecretKey { key: "token", .. })
            }),
            (
                true,
                "base_domain: a\ndev_token: hunter2\n",
                ":2: `dev_token` is a secret",
                |e| {
                    matches!(
                        e,
                        ConfigError::SecretKey {
                            line: 2,
                            key: "dev_token",
                            ..
                        }
                    )
                },
            ),
            (true, "listen: yes\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            (true, "limits:\n  max_connections: -1\n", "line 2", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            (true, "signup: maybe\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            (false, "allow_remote_targets: maybe\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            (false, "subdomains: myapp\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            (true, "- base_domain\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            (false, "relay_addr: [\n", "", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
        ];
        for (relay, text, message, is_expected) in cases {
            let error = if *relay {
                parse::<RelayLayer>(path, text, false).err()
            } else {
                parse::<AgentLayer>(path, text, true).err()
            }
            .expect(text);
            assert!(is_expected(&error), "{text:?}: {error:?}");
            let shown = error.to_string();
            assert!(shown.starts_with("/etc/vorp/vorp.yaml"), "{shown}");
            assert!(shown.contains(message), "{text:?}: {shown}");
            assert!(!shown.contains("hunter2"), "secret echoed: {shown}");
        }
    }

    #[test]
    fn empty_file_is_an_empty_layer() {
        for text in ["", "# nothing yet\n"] {
            let layer: AgentLayer = parse(Path::new("c.yaml"), text, true).expect("empty is valid");
            assert_eq!(layer, AgentLayer::default());
        }
    }

    #[test]
    fn missing_file_is_empty_only_when_not_named() {
        let dir = TempDir::new("config-read");
        let missing = dir.0.join("absent.yaml");
        assert!(matches!(
            read::<AgentLayer>(&missing, false, true),
            Ok(None)
        ));
        assert!(matches!(
            read::<AgentLayer>(&missing, true, true),
            Err(ConfigError::NotFound { path }) if path == missing
        ));
    }

    #[test]
    fn agent_dir_order_and_empty_values() {
        // (XDG_CONFIG_HOME, APPDATA, HOME) -> directory
        let cases = [
            ([Some("/x"), Some("/a"), Some("/h")], Some("/x/vorp")),
            ([Some(""), Some("/a"), Some("/h")], Some("/a/vorp")),
            ([Some(""), None, Some("/h")], Some("/h/.config/vorp")),
            ([None, None, Some("")], None),
        ];
        for (vars, expected) in cases {
            let got = agent_dir(|name| {
                let at = ["XDG_CONFIG_HOME", "APPDATA", "HOME"]
                    .iter()
                    .position(|n| *n == name)?;
                vars[at].map(OsString::from)
            });
            assert_eq!(got, expected.map(PathBuf::from), "{vars:?}");
        }
    }

    /// A relative path from the config file is relative to the file's
    /// directory; one from a flag or the environment stays relative to the
    /// working directory. Checked for every path key of both modes.
    #[test]
    fn relative_paths() {
        let dir = std::env::temp_dir().join("etc-vorp");
        let elsewhere = std::env::temp_dir().join("elsewhere");
        // (clap's source, clap's value, the file's value) -> effective path
        let cases = [
            (None, None, PathBuf::from("p"), dir.join("p")),
            (None, None, PathBuf::from("sub/p"), dir.join("sub/p")),
            (None, None, elsewhere.clone(), elsewhere),
            (
                Some(Source::Flag),
                Some(PathBuf::from("p")),
                "f".into(),
                "p".into(),
            ),
            (
                Some(Source::Env),
                Some(PathBuf::from("p")),
                "f".into(),
                "p".into(),
            ),
        ];
        for (clap_source, clap, file, expected) in cases {
            let explicit = move |_: &str| clap_source;
            let mut merge = Merge::new(&explicit);
            let agent = |path: Option<PathBuf>| AgentLayer {
                ca_cert: path.clone(),
                token_file: path,
                ..AgentLayer::default()
            };
            let relay = |path: Option<PathBuf>| {
                let mut layer = RelayLayer {
                    database_path: path.clone(),
                    ..RelayLayer::default()
                };
                layer.tls.cert = path.clone();
                layer.tls.key = path;
                layer
            };
            let a =
                agent(clap.clone()).merge(agent(Some(file.clone())).relative_to(&dir), &mut merge);
            let r = relay(clap).merge(relay(Some(file)).relative_to(&dir), &mut merge);
            for got in [
                a.ca_cert,
                a.token_file,
                r.database_path,
                r.tls.cert,
                r.tls.key,
            ] {
                assert_eq!(got.as_ref(), Some(&expected), "{clap_source:?}");
            }
        }
    }

    #[test]
    fn fallback_names_the_setting() {
        let explicit = |_: &str| None;
        let mut merge = Merge::new(&explicit);
        let layer = RelayLayer {
            base_domain: Some("example.com".into()),
            ..RelayLayer::default()
        };
        RelayLayer::default().merge(layer, &mut merge);
        let row = merge.rows.iter().find(|(key, _)| *key == "dashboard_host");
        assert_eq!(
            row.and_then(|(_, value)| value.as_ref())
                .map(|(value, source)| format!("{value}  ({source})")),
            Some("example.com  (default: base_domain)".to_owned())
        );
    }

    /// Flag > env > file > default for a scalar (`relay_host`), a list
    /// (`subdomains`) and a nested key (`limits.max_connections`), through
    /// the real merge functions. clap itself puts the flag over the
    /// environment; here `explicit` stands in for what clap reports.
    #[test]
    fn precedence() {
        // (clap's source, clap holds a value, the file sets it) -> expected
        let cases = [
            (Some(Source::Flag), true, true, Some(Source::Flag)),
            (Some(Source::Flag), true, false, Some(Source::Flag)),
            (Some(Source::Env), true, true, Some(Source::Env)),
            (Some(Source::Env), true, false, Some(Source::Env)),
            (None, true, true, Some(Source::File)),
            (None, false, true, Some(Source::File)),
            (None, true, false, Some(Source::Default)),
            (None, false, false, None),
        ];
        for (clap_source, in_clap, in_file, expected) in cases {
            let agent = |set: bool, name: &str| AgentLayer {
                relay_host: set.then(|| name.to_owned()),
                subdomains: set.then(|| vec![name.to_owned()]),
                ..AgentLayer::default()
            };
            let relay = |set: bool, value: usize| {
                let mut layer = RelayLayer::default();
                layer.limits.max_connections = set.then_some(value);
                layer
            };
            let explicit = move |_: &str| clap_source;
            let mut merge = Merge::new(&explicit);
            let merged = agent(in_clap, "clap").merge(agent(in_file, "file"), &mut merge);
            let limits = relay(in_clap, 1)
                .merge(relay(in_file, 2), &mut merge)
                .limits;

            let (name, number) = match expected {
                Some(Source::File) => (Some("file"), Some(2)),
                Some(_) => (Some("clap"), Some(1)),
                None => (None, None),
            };
            assert_eq!(merged.relay_host.as_deref(), name);
            assert_eq!(merged.subdomains, name.map(|name| vec![name.to_owned()]));
            assert_eq!(limits.max_connections, number);
            for key in ["relay_host", "subdomains", "limits.max_connections"] {
                let row = merge.rows.iter().find(|(k, _)| *k == key).expect(key);
                assert_eq!(row.1.as_ref().map(|(_, source)| *source), expected, "{key}");
            }
        }
    }
}
