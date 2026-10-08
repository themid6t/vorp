use std::{collections::HashMap, fmt::Display, str::FromStr};

use super::ConfigError;

/// A snapshot of the `VORP_*` environment variables. Values may be secrets
/// (`VORP_TOKEN`), so this type is deliberately not `Debug`.
pub(crate) struct Env(HashMap<String, String>);

impl Env {
    pub(crate) fn from_process() -> Result<Self, ConfigError> {
        let mut vars = HashMap::new();
        for (key, value) in std::env::vars_os() {
            // A non-UTF-8 name cannot be one of ours.
            let Some(key) = key.to_str().filter(|key| key.starts_with("VORP_")) else {
                continue;
            };
            let value = value.into_string().map_err(|_| ConfigError::InvalidValue {
                key: key.to_owned(),
                message: "not valid UTF-8".to_owned(),
            })?;
            vars.insert(key.to_owned(), value);
        }
        Ok(Self(vars))
    }

    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }

    /// The typed value of `name`. An empty variable counts as unset, which is
    /// how container and systemd templates usually "remove" one.
    pub(crate) fn get<T>(&self, name: &str) -> Result<Option<T>, ConfigError>
    where
        T: FromStr,
        T::Err: Display,
    {
        let Some(raw) = self.0.get(name).filter(|raw| !raw.is_empty()) else {
            return Ok(None);
        };
        raw.parse()
            .map(Some)
            .map_err(|error| ConfigError::InvalidValue {
                key: name.to_owned(),
                message: format!("{error}"),
            })
    }

    /// A comma-separated list, such as `VORP_SUBDOMAINS=web,api`.
    pub(crate) fn list(&self, name: &str) -> Option<Vec<String>> {
        let raw = self.0.get(name).filter(|raw| !raw.is_empty())?;
        Some(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values_empty_values_and_lists() {
        let env = Env::from_pairs(&[
            ("VORP_LISTEN", "127.0.0.1:8443"),
            ("VORP_EMPTY", ""),
            ("VORP_BAD", "nope"),
            ("VORP_SUBDOMAINS", "web, api,,"),
        ]);
        assert_eq!(
            env.get::<std::net::SocketAddr>("VORP_LISTEN")
                .ok()
                .flatten(),
            Some("127.0.0.1:8443".parse().expect("valid address"))
        );
        assert!(matches!(env.get::<u64>("VORP_EMPTY"), Ok(None)));
        assert!(matches!(env.get::<u64>("VORP_MISSING"), Ok(None)));
        assert!(matches!(
            env.get::<bool>("VORP_BAD"),
            Err(ConfigError::InvalidValue { key, .. }) if key == "VORP_BAD"
        ));
        assert_eq!(
            env.list("VORP_SUBDOMAINS"),
            Some(vec!["web".to_owned(), "api".to_owned()])
        );
        assert_eq!(env.list("VORP_EMPTY"), None);
    }
}
