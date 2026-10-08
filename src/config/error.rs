use std::path::PathBuf;

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
    /// Malformed YAML or a value of the wrong type. `message` carries the
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
    #[error("{key}: {message}")]
    InvalidValue { key: String, message: String },
    #[error("{key} is not set: pass {flag}, set {env}, or add `{key}` to the config file")]
    Missing {
        key: &'static str,
        flag: &'static str,
        env: &'static str,
    },
}
