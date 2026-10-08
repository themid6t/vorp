use std::path::Path;

use serde::{Deserialize, de::DeserializeOwned, de::IgnoredAny};
use serde_saphyr::Spanned;

use super::ConfigError;

/// Keys with a reserved meaning, found before the strict parse so they get
/// their own message instead of "unknown key".
#[derive(Deserialize)]
struct Reserved {
    token: Option<Spanned<IgnoredAny>>,
    dev_token: Option<Spanned<IgnoredAny>>,
    tunnels: Option<Spanned<IgnoredAny>>,
}

const TOKEN_HINT: &str = "save the agent token with `vorp login` or `vorp authtoken`, and set `token_file` to its path if it is not in the default place";
const DEV_TOKEN_HINT: &str = "the development token is set only with --dev-token or VORP_DEV_TOKEN";

/// Parses a config document strictly: reserved secret keys, then unknown
/// keys and wrong types are errors. `tunnels` is reserved only for the agent;
/// for the relay it is an ordinary unknown key.
pub(super) fn parse<T: DeserializeOwned>(
    path: &Path,
    text: &str,
    reserve_tunnels: bool,
) -> Result<T, ConfigError> {
    let reserved: Reserved = serde_saphyr::from_str(text).map_err(|e| yaml_error(path, &e))?;
    if let Some(token) = reserved.token {
        return Err(secret(path, &token, "token", TOKEN_HINT));
    }
    if let Some(token) = reserved.dev_token {
        return Err(secret(path, &token, "dev_token", DEV_TOKEN_HINT));
    }
    if let (true, Some(tunnels)) = (reserve_tunnels, reserved.tunnels) {
        return Err(ConfigError::UnsupportedTunnels {
            path: path.to_owned(),
            line: tunnels.referenced.line(),
        });
    }
    serde_saphyr::from_str(text).map_err(|e| yaml_error(path, &e))
}

fn secret(
    path: &Path,
    value: &Spanned<IgnoredAny>,
    key: &'static str,
    hint: &'static str,
) -> ConfigError {
    ConfigError::SecretKey {
        path: path.to_owned(),
        line: value.referenced.line(),
        key,
        hint,
    }
}

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
