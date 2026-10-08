use std::path::{Path, PathBuf};

use super::{ConfigError, Env, Setting, Source, setting::pick};

const RELAY_CONFIG: &str = "/etc/vorp/vorp.yaml";

/// The agent's per-user directory: `$XDG_CONFIG_HOME/vorp`, else
/// `%APPDATA%\vorp`, else `~/.config/vorp`. It holds `config.yaml` and
/// `authtoken`.
pub(crate) fn vorp_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join("vorp"))
}

/// `--config`, else `VORP_CONFIG`, else the per-user default. `None` when
/// there is no home directory to put a default in (e.g. a bare container).
pub(crate) fn agent_config_path(
    flag: Option<PathBuf>,
    env: &Env,
) -> Result<Option<Setting<PathBuf>>, ConfigError> {
    let default =
        vorp_config_dir().map(|dir| Setting::new(dir.join("config.yaml"), Source::Default));
    Ok(pick(flag, env.get("VORP_CONFIG")?, None).or(default))
}

/// `vorp serve --config`, else `VORP_CONFIG`, else `/etc/vorp/vorp.yaml`.
pub(crate) fn relay_config_path(
    flag: Option<PathBuf>,
    env: &Env,
) -> Result<Setting<PathBuf>, ConfigError> {
    Ok(pick(flag, env.get("VORP_CONFIG")?, None)
        .unwrap_or_else(|| Setting::new(PathBuf::from(RELAY_CONFIG), Source::Default)))
}

/// Reads and parses the config file. A missing default file yields an empty
/// layer, so flag-only setups behave as before; a missing file the user named
/// is an error.
pub(crate) fn read_layer<T: Default>(
    path: Option<&Setting<PathBuf>>,
    parse: fn(&Path, &str) -> Result<T, ConfigError>,
) -> Result<T, ConfigError> {
    let Some(path) = path else {
        return Ok(T::default());
    };
    match std::fs::read_to_string(&path.value) {
        Ok(text) => {
            let layer = parse(&path.value, &text)?;
            tracing::info!(path = %path.value.display(), "config file loaded");
            Ok(layer)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match path.source {
            Source::Default => Ok(T::default()),
            _ => Err(ConfigError::NotFound {
                path: path.value.clone(),
            }),
        },
        Err(source) => Err(ConfigError::Read {
            path: path.value.clone(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(_: &Path, text: &str) -> Result<String, ConfigError> {
        Ok(text.to_owned())
    }

    #[test]
    fn missing_default_file_is_empty_but_missing_named_file_is_an_error() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("absent.yaml");

        let default = Setting::new(missing.clone(), Source::Default);
        assert_eq!(
            read_layer(Some(&default), parse_text).ok(),
            Some(String::new())
        );

        for source in [Source::Flag, Source::Env] {
            let named = Setting::new(missing.clone(), source);
            assert!(matches!(
                read_layer(Some(&named), parse_text),
                Err(ConfigError::NotFound { path }) if path == missing
            ));
        }

        assert_eq!(read_layer(None, parse_text).ok(), Some(String::new()));
    }

    #[test]
    fn present_file_is_parsed() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("vorp.yaml");
        std::fs::write(&path, "listen: 127.0.0.1:1\n").expect("write config");
        let named = Setting::new(path, Source::Flag);
        assert_eq!(
            read_layer(Some(&named), parse_text).ok(),
            Some("listen: 127.0.0.1:1\n".to_owned())
        );
    }

    #[test]
    fn config_path_prefers_flag_then_env() {
        let env = Env::from_pairs(&[("VORP_CONFIG", "/env.yaml")]);
        let flag = relay_config_path(Some("/flag.yaml".into()), &env).expect("valid path");
        assert_eq!(
            flag,
            Setting::new(PathBuf::from("/flag.yaml"), Source::Flag)
        );
        let from_env = relay_config_path(None, &env).expect("valid path");
        assert_eq!(
            from_env,
            Setting::new(PathBuf::from("/env.yaml"), Source::Env)
        );
        let default = relay_config_path(None, &Env::from_pairs(&[])).expect("valid path");
        assert_eq!(
            default,
            Setting::new(PathBuf::from(RELAY_CONFIG), Source::Default)
        );
    }
}
