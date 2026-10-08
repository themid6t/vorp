use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use vorp_agent::AgentConfig;

use super::{
    ConfigError, Env, Setting,
    setting::{Row, optional_row, pick, pick_or, row},
};

const DEFAULT_RELAY_HOST: &str = "localhost";

/// One layer of agent settings. The YAML file deserializes into it directly;
/// flags and the environment are converted into it.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentLayer {
    pub(crate) relay_host: Option<String>,
    pub(crate) relay_addr: Option<SocketAddr>,
    pub(crate) ca_cert: Option<PathBuf>,
    pub(crate) token_file: Option<PathBuf>,
    pub(crate) upstream: Option<String>,
    pub(crate) subdomains: Option<Vec<String>>,
    pub(crate) allow_remote_targets: Option<bool>,
}

impl AgentLayer {
    pub(crate) fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        super::parse::parse(path, text, true)
    }

    pub(crate) fn from_env(env: &Env) -> Result<Self, ConfigError> {
        Ok(Self {
            relay_host: env.get("VORP_RELAY_HOST")?,
            relay_addr: env.get("VORP_RELAY_ADDR")?,
            ca_cert: env.get("VORP_CA_CERT")?,
            token_file: env.get("VORP_TOKEN_FILE")?,
            upstream: env.get("VORP_UPSTREAM")?,
            subdomains: env.list("VORP_SUBDOMAINS"),
            allow_remote_targets: env.get("VORP_ALLOW_REMOTE_TARGETS")?,
        })
    }
}

/// Effective agent settings, each with the layer that set it.
#[derive(Debug, PartialEq)]
pub(crate) struct AgentSettings {
    pub(crate) relay_host: Setting<String>,
    pub(crate) relay_addr: Option<Setting<SocketAddr>>,
    pub(crate) ca_cert: Option<Setting<PathBuf>>,
    /// Unset only when there is no config directory to default to.
    pub(crate) token_file: Option<Setting<PathBuf>>,
    pub(crate) upstream: Option<Setting<String>>,
    pub(crate) subdomains: Setting<Vec<String>>,
    pub(crate) allow_remote_targets: Setting<bool>,
}

impl AgentSettings {
    /// `default_token_file` is `authtoken` next to the config file in use.
    pub(crate) fn merge(
        flags: AgentLayer,
        env: AgentLayer,
        file: AgentLayer,
        default_token_file: Option<PathBuf>,
    ) -> Self {
        let token_file = pick(flags.token_file, env.token_file, file.token_file)
            .or(default_token_file.map(|path| Setting::new(path, super::Source::Default)));
        Self {
            relay_host: pick_or(
                flags.relay_host,
                env.relay_host,
                file.relay_host,
                DEFAULT_RELAY_HOST.to_owned(),
            ),
            relay_addr: pick(flags.relay_addr, env.relay_addr, file.relay_addr),
            ca_cert: pick(flags.ca_cert, env.ca_cert, file.ca_cert),
            token_file,
            upstream: pick(flags.upstream, env.upstream, file.upstream),
            subdomains: pick_or(
                flags.subdomains,
                env.subdomains,
                file.subdomains,
                Vec::new(),
            ),
            allow_remote_targets: pick_or(
                flags.allow_remote_targets,
                env.allow_remote_targets,
                file.allow_remote_targets,
                false,
            ),
        }
    }

    pub(crate) fn into_agent_config(self, token: String) -> Result<AgentConfig, ConfigError> {
        let upstream = self.upstream.ok_or(ConfigError::Missing {
            key: "upstream",
            flag: "--upstream",
            env: "VORP_UPSTREAM",
        })?;
        let requested_subdomains = if self.subdomains.value.is_empty() {
            vec![None]
        } else {
            self.subdomains.value.into_iter().map(Some).collect()
        };
        Ok(AgentConfig {
            relay_host: self.relay_host.value,
            relay_addr: self.relay_addr.map(|s| s.value),
            ca_cert: self.ca_cert.map(|s| s.value),
            token,
            upstream: upstream.value,
            requested_subdomains,
            allow_remote_targets: self.allow_remote_targets.value,
        })
    }

    pub(crate) fn rows(&self) -> Vec<Row> {
        let subdomains = Setting::new(self.subdomains.value.join(","), self.subdomains.source);
        let token_file = self
            .token_file
            .as_ref()
            .map(|s| Setting::new(s.value.display(), s.source));
        let ca_cert = self
            .ca_cert
            .as_ref()
            .map(|s| Setting::new(s.value.display(), s.source));
        vec![
            row("relay_host", &self.relay_host),
            optional_row("relay_addr", &self.relay_addr),
            optional_row("ca_cert", &ca_cert),
            optional_row("token_file", &token_file),
            optional_row("upstream", &self.upstream),
            row("subdomains", &subdomains),
            row("allow_remote_targets", &self.allow_remote_targets),
        ]
    }
}

#[cfg(test)]
mod tests {
    /// YAML text, an expected substring of the message, and a check of the variant.
    type ParseCase = (&'static str, &'static str, fn(&ConfigError) -> bool);

    use super::*;
    use crate::config::Source;

    fn layer_with(relay_host: &str, subdomains: &[&str]) -> AgentLayer {
        AgentLayer {
            relay_host: Some(relay_host.to_owned()),
            subdomains: Some(subdomains.iter().map(|s| (*s).to_owned()).collect()),
            ..AgentLayer::default()
        }
    }

    #[test]
    fn precedence_for_scalars_and_lists() {
        // (flag, env, file) presence, then the expected source.
        let cases = [
            (true, true, true, Source::Flag),
            (true, false, true, Source::Flag),
            (false, true, true, Source::Env),
            (false, true, false, Source::Env),
            (false, false, true, Source::File),
            (false, false, false, Source::Default),
        ];
        for (flag, env, file, expected) in cases {
            let layer = |set: bool, name: &str| {
                if set {
                    layer_with(name, &[name])
                } else {
                    AgentLayer::default()
                }
            };
            let settings = AgentSettings::merge(
                layer(flag, "flag"),
                layer(env, "env"),
                layer(file, "file"),
                None,
            );
            let expected_value = match expected {
                Source::Flag => "flag",
                Source::Env => "env",
                Source::File => "file",
                Source::Default => DEFAULT_RELAY_HOST,
            };
            assert_eq!(
                settings.relay_host,
                Setting::new(expected_value.to_owned(), expected)
            );
            let expected_list = match expected {
                Source::Default => Vec::new(),
                _ => vec![expected_value.to_owned()],
            };
            assert_eq!(settings.subdomains, Setting::new(expected_list, expected));
        }
    }

    #[test]
    fn token_file_defaults_next_to_the_config() {
        let settings = AgentSettings::merge(
            AgentLayer::default(),
            AgentLayer::default(),
            AgentLayer::default(),
            Some("/home/u/.config/vorp/authtoken".into()),
        );
        assert_eq!(
            settings.token_file,
            Some(Setting::new(
                "/home/u/.config/vorp/authtoken".into(),
                Source::Default
            ))
        );
        let file = AgentLayer {
            token_file: Some("/run/secrets/vorp".into()),
            ..AgentLayer::default()
        };
        let settings =
            AgentSettings::merge(AgentLayer::default(), AgentLayer::default(), file, None);
        assert_eq!(
            settings.token_file,
            Some(Setting::new("/run/secrets/vorp".into(), Source::File))
        );
    }

    #[test]
    fn env_layer_reads_every_key() {
        let env = Env::from_pairs(&[
            ("VORP_RELAY_HOST", "tunnels.example.com"),
            ("VORP_RELAY_ADDR", "203.0.113.10:443"),
            ("VORP_CA_CERT", "/ca.pem"),
            ("VORP_TOKEN_FILE", "/token"),
            ("VORP_UPSTREAM", "http://127.0.0.1:3000"),
            ("VORP_SUBDOMAINS", "web,api"),
            ("VORP_ALLOW_REMOTE_TARGETS", "true"),
        ]);
        let layer = AgentLayer::from_env(&env).expect("valid environment");
        assert_eq!(
            layer,
            AgentLayer {
                relay_host: Some("tunnels.example.com".into()),
                relay_addr: Some("203.0.113.10:443".parse().expect("valid address")),
                ca_cert: Some("/ca.pem".into()),
                token_file: Some("/token".into()),
                upstream: Some("http://127.0.0.1:3000".into()),
                subdomains: Some(vec!["web".into(), "api".into()]),
                allow_remote_targets: Some(true),
            }
        );
    }

    #[test]
    fn file_with_every_key_parses() {
        let text = "relay_host: tunnels.example.com\nrelay_addr: 203.0.113.10:443\n\
                    ca_cert: /ca.pem\ntoken_file: /token\nupstream: http://127.0.0.1:3000\n\
                    subdomains: [myapp]\nallow_remote_targets: false\n";
        let layer = AgentLayer::parse(Path::new("config.yaml"), text).expect("valid config");
        assert_eq!(layer.subdomains, Some(vec!["myapp".to_owned()]));
        assert_eq!(layer.allow_remote_targets, Some(false));
    }

    #[test]
    fn parse_failures() {
        let path = Path::new("/home/u/.config/vorp/config.yaml");
        let cases: &[ParseCase] = &[
            (
                "relay_host: a\nrelay_hots: b\n",
                ":2: unknown key `relay_hots`",
                |e| matches!(e, ConfigError::UnknownKey { line: 2, key, .. } if key == "relay_hots"),
            ),
            (
                "relay_host: a\ntoken: vorp_1_abc\n",
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
            ("dev_token: x\n", ":1: `dev_token` is a secret", |e| {
                matches!(
                    e,
                    ConfigError::SecretKey {
                        line: 1,
                        key: "dev_token",
                        ..
                    }
                )
            }),
            (
                "tunnels: {web: {upstream: x}}\n",
                ":1: `tunnels`: multi-tunnel",
                |e| matches!(e, ConfigError::UnsupportedTunnels { line: 1, .. }),
            ),
            ("allow_remote_targets: maybe\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            ("subdomains: myapp\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            ("relay_addr: [\n", "config.yaml", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
        ];
        for (text, message, is_expected) in cases {
            let error = AgentLayer::parse(path, text).expect_err(text);
            assert!(is_expected(&error), "{text:?}: {error:?}");
            let shown = error.to_string();
            assert!(shown.starts_with(&path.display().to_string()), "{shown}");
            assert!(shown.contains(message), "{text:?}: {shown}");
            assert!(!shown.contains("vorp_1_abc"), "secret echoed: {shown}");
        }
    }

    #[test]
    fn empty_file_is_an_empty_layer() {
        for text in ["", "# nothing yet\n"] {
            let layer = AgentLayer::parse(Path::new("c.yaml"), text).expect("empty is valid");
            assert_eq!(layer, AgentLayer::default());
        }
    }

    #[test]
    fn missing_upstream_names_every_layer() {
        let settings = AgentSettings::merge(
            AgentLayer::default(),
            AgentLayer::default(),
            AgentLayer::default(),
            None,
        );
        let error = settings
            .into_agent_config("t".into())
            .err()
            .expect("upstream is required");
        assert_eq!(
            error.to_string(),
            "upstream is not set: pass --upstream, set VORP_UPSTREAM, or add `upstream` to the config file"
        );
    }
}
