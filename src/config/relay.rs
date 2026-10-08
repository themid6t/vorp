use std::{
    fmt,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use clap::ValueEnum;
use serde::Deserialize;
use vorp_relay::{EdgeLimits, RelayConfig, SignupMode, TlsConfig};

use super::{
    ConfigError, Env, Setting,
    setting::{Row, optional_row, pick, pick_or, row},
};

const DEFAULT_DATABASE: &str = "vorp.sqlite3";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Signup {
    Open,
    Closed,
}

impl FromStr for Signup {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        <Self as ValueEnum>::from_str(value, true)
    }
}

impl fmt::Display for Signup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Closed => "closed",
        })
    }
}

impl From<Signup> for SignupMode {
    fn from(value: Signup) -> Self {
        match value {
            Signup::Open => Self::Open,
            Signup::Closed => Self::Closed,
        }
    }
}

/// One layer of relay settings, mirroring `vorp serve`'s flags. The dev-only
/// flags are deliberately absent, so a config file can never enable them.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayLayer {
    pub(crate) base_domain: Option<String>,
    pub(crate) dashboard_host: Option<String>,
    pub(crate) listen: Option<SocketAddr>,
    pub(crate) database_path: Option<PathBuf>,
    pub(crate) signup: Option<Signup>,
    #[serde(default)]
    pub(crate) tls: TlsLayer,
    #[serde(default)]
    pub(crate) limits: LimitsLayer,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsLayer {
    pub(crate) cert: Option<PathBuf>,
    pub(crate) key: Option<PathBuf>,
}

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LimitsLayer {
    pub(crate) max_connections: Option<usize>,
    pub(crate) max_connections_per_ip: Option<usize>,
    pub(crate) rate_limit_rps: Option<u64>,
    pub(crate) max_requests: Option<usize>,
    pub(crate) max_websockets: Option<usize>,
    pub(crate) tunnel_requests: Option<usize>,
    pub(crate) tunnel_websockets: Option<usize>,
    pub(crate) response_timeout_secs: Option<u64>,
}

impl RelayLayer {
    pub(crate) fn parse(path: &Path, text: &str) -> Result<Self, ConfigError> {
        super::parse::parse(path, text, false)
    }

    /// A nested key's variable joins its path with `_`: `limits.max_connections`
    /// is `VORP_LIMITS_MAX_CONNECTIONS`.
    pub(crate) fn from_env(env: &Env) -> Result<Self, ConfigError> {
        Ok(Self {
            base_domain: env.get("VORP_BASE_DOMAIN")?,
            dashboard_host: env.get("VORP_DASHBOARD_HOST")?,
            listen: env.get("VORP_LISTEN")?,
            database_path: env.get("VORP_DATABASE_PATH")?,
            signup: env.get("VORP_SIGNUP")?,
            tls: TlsLayer {
                cert: env.get("VORP_TLS_CERT")?,
                key: env.get("VORP_TLS_KEY")?,
            },
            limits: LimitsLayer {
                max_connections: env.get("VORP_LIMITS_MAX_CONNECTIONS")?,
                max_connections_per_ip: env.get("VORP_LIMITS_MAX_CONNECTIONS_PER_IP")?,
                rate_limit_rps: env.get("VORP_LIMITS_RATE_LIMIT_RPS")?,
                max_requests: env.get("VORP_LIMITS_MAX_REQUESTS")?,
                max_websockets: env.get("VORP_LIMITS_MAX_WEBSOCKETS")?,
                tunnel_requests: env.get("VORP_LIMITS_TUNNEL_REQUESTS")?,
                tunnel_websockets: env.get("VORP_LIMITS_TUNNEL_WEBSOCKETS")?,
                response_timeout_secs: env.get("VORP_LIMITS_RESPONSE_TIMEOUT_SECS")?,
            },
        })
    }
}

/// The dev-only `vorp serve` flags, which never come from the file or the
/// environment (except `VORP_DEV_TOKEN`, as before).
#[derive(Default)]
pub(crate) struct DevOptions {
    pub(crate) self_signed: bool,
    pub(crate) cert_out: Option<PathBuf>,
    pub(crate) token: Option<String>,
}

/// Effective relay settings, each with the layer that set it.
#[derive(Debug, PartialEq)]
pub(crate) struct RelaySettings {
    pub(crate) base_domain: Option<Setting<String>>,
    /// Defaults to `base_domain` when unset.
    pub(crate) dashboard_host: Option<Setting<String>>,
    pub(crate) listen: Setting<SocketAddr>,
    pub(crate) database_path: Setting<PathBuf>,
    pub(crate) signup: Setting<Signup>,
    pub(crate) tls_cert: Option<Setting<PathBuf>>,
    pub(crate) tls_key: Option<Setting<PathBuf>>,
    pub(crate) limits: LimitSettings,
}

#[derive(Debug, PartialEq)]
pub(crate) struct LimitSettings {
    pub(crate) max_connections: Setting<usize>,
    pub(crate) max_connections_per_ip: Setting<usize>,
    pub(crate) rate_limit_rps: Setting<u64>,
    pub(crate) max_requests: Setting<usize>,
    pub(crate) max_websockets: Setting<usize>,
    pub(crate) tunnel_requests: Setting<usize>,
    pub(crate) tunnel_websockets: Setting<usize>,
    pub(crate) response_timeout_secs: Setting<u64>,
}

impl LimitSettings {
    /// Defaults come from [`EdgeLimits::default`], the single source of truth.
    fn merge(flags: LimitsLayer, env: LimitsLayer, file: LimitsLayer) -> Self {
        let default = EdgeLimits::default();
        Self {
            max_connections: pick_or(
                flags.max_connections,
                env.max_connections,
                file.max_connections,
                default.max_connections,
            ),
            max_connections_per_ip: pick_or(
                flags.max_connections_per_ip,
                env.max_connections_per_ip,
                file.max_connections_per_ip,
                default.max_connections_per_ip,
            ),
            rate_limit_rps: pick_or(
                flags.rate_limit_rps,
                env.rate_limit_rps,
                file.rate_limit_rps,
                default.requests_per_second_per_ip,
            ),
            max_requests: pick_or(
                flags.max_requests,
                env.max_requests,
                file.max_requests,
                default.max_requests,
            ),
            max_websockets: pick_or(
                flags.max_websockets,
                env.max_websockets,
                file.max_websockets,
                default.max_websockets,
            ),
            tunnel_requests: pick_or(
                flags.tunnel_requests,
                env.tunnel_requests,
                file.tunnel_requests,
                default.tunnel_requests,
            ),
            tunnel_websockets: pick_or(
                flags.tunnel_websockets,
                env.tunnel_websockets,
                file.tunnel_websockets,
                default.tunnel_websockets,
            ),
            response_timeout_secs: pick_or(
                flags.response_timeout_secs,
                env.response_timeout_secs,
                file.response_timeout_secs,
                default.response_timeout.as_secs(),
            ),
        }
    }

    fn edge_limits(&self) -> EdgeLimits {
        EdgeLimits {
            max_connections: self.max_connections.value,
            max_connections_per_ip: self.max_connections_per_ip.value,
            requests_per_second_per_ip: self.rate_limit_rps.value,
            max_requests: self.max_requests.value,
            max_websockets: self.max_websockets.value,
            tunnel_requests: self.tunnel_requests.value,
            tunnel_websockets: self.tunnel_websockets.value,
            response_timeout: Duration::from_secs(self.response_timeout_secs.value),
        }
    }

    fn rows(&self) -> Vec<Row> {
        vec![
            row("limits.max_connections", &self.max_connections),
            row(
                "limits.max_connections_per_ip",
                &self.max_connections_per_ip,
            ),
            row("limits.rate_limit_rps", &self.rate_limit_rps),
            row("limits.max_requests", &self.max_requests),
            row("limits.max_websockets", &self.max_websockets),
            row("limits.tunnel_requests", &self.tunnel_requests),
            row("limits.tunnel_websockets", &self.tunnel_websockets),
            row("limits.response_timeout_secs", &self.response_timeout_secs),
        ]
    }
}

impl RelaySettings {
    pub(crate) fn merge(flags: RelayLayer, env: RelayLayer, file: RelayLayer) -> Self {
        Self {
            base_domain: pick(flags.base_domain, env.base_domain, file.base_domain),
            dashboard_host: pick(
                flags.dashboard_host,
                env.dashboard_host,
                file.dashboard_host,
            ),
            listen: pick_or(
                flags.listen,
                env.listen,
                file.listen,
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, 443)),
            ),
            database_path: pick_or(
                flags.database_path,
                env.database_path,
                file.database_path,
                PathBuf::from(DEFAULT_DATABASE),
            ),
            signup: pick_or(flags.signup, env.signup, file.signup, Signup::Closed),
            tls_cert: pick(flags.tls.cert, env.tls.cert, file.tls.cert),
            tls_key: pick(flags.tls.key, env.tls.key, file.tls.key),
            limits: LimitSettings::merge(flags.limits, env.limits, file.limits),
        }
    }

    pub(crate) fn into_relay_config(self, dev: DevOptions) -> Result<RelayConfig, ConfigError> {
        let tls = self.tls(&dev)?;
        let base_domain = self
            .base_domain
            .ok_or(ConfigError::Missing {
                key: "base_domain",
                flag: "--base-domain",
                env: "VORP_BASE_DOMAIN",
            })?
            .value;
        Ok(RelayConfig {
            listen: self.listen.value,
            dashboard_host: self
                .dashboard_host
                .map_or_else(|| base_domain.clone(), |s| s.value),
            base_domain,
            database_path: self.database_path.value,
            tls,
            signup_mode: self.signup.value.into(),
            dev_token: dev.token,
            limits: self.limits.edge_limits(),
        })
    }

    fn tls(&self, dev: &DevOptions) -> Result<TlsConfig, ConfigError> {
        let invalid = |message: &str| ConfigError::InvalidValue {
            key: "tls".to_owned(),
            message: message.to_owned(),
        };
        match (&self.tls_cert, &self.tls_key, dev.self_signed) {
            (Some(cert), Some(key), false) => Ok(TlsConfig::Files {
                cert: cert.value.clone(),
                key: key.value.clone(),
            }),
            (None, None, true) => Ok(TlsConfig::SelfSigned {
                cert_output: dev
                    .cert_out
                    .clone()
                    .ok_or_else(|| invalid("--dev-self-signed requires --dev-cert-out"))?,
            }),
            (_, _, true) => Err(invalid(
                "--dev-self-signed cannot be combined with tls.cert or tls.key; check `vorp config show --relay`",
            )),
            _ => Err(invalid(
                "set both tls.cert and tls.key (--tls-cert/--tls-key, VORP_TLS_CERT/VORP_TLS_KEY, or the config file), or use --dev-self-signed --dev-cert-out",
            )),
        }
    }

    pub(crate) fn rows(&self) -> Vec<Row> {
        let path = |s: &Option<Setting<PathBuf>>| {
            s.as_ref()
                .map(|s| Setting::new(s.value.display().to_string(), s.source))
        };
        let database_path = Setting::new(
            self.database_path.value.display().to_string(),
            self.database_path.source,
        );
        let dashboard_host = self.dashboard_host.clone().or_else(|| {
            self.base_domain
                .as_ref()
                .map(|s| Setting::new(s.value.clone(), super::Source::Default))
        });
        let mut rows = vec![
            optional_row("base_domain", &self.base_domain),
            optional_row("dashboard_host", &dashboard_host),
            row("listen", &self.listen),
            row("database_path", &database_path),
            row("signup", &self.signup),
            optional_row("tls.cert", &path(&self.tls_cert)),
            optional_row("tls.key", &path(&self.tls_key)),
        ];
        rows.extend(self.limits.rows());
        rows
    }
}

#[cfg(test)]
mod tests {
    /// YAML text, an expected substring of the message, and a check of the variant.
    type ParseCase = (&'static str, &'static str, fn(&ConfigError) -> bool);

    use super::*;
    use crate::config::Source;

    fn limits(max_connections: Option<usize>) -> RelayLayer {
        RelayLayer {
            limits: LimitsLayer {
                max_connections,
                ..LimitsLayer::default()
            },
            ..RelayLayer::default()
        }
    }

    #[test]
    fn precedence_for_a_nested_key() {
        let default = EdgeLimits::default().max_connections;
        let cases = [
            (Some(1), Some(2), Some(3), Setting::new(1, Source::Flag)),
            (None, Some(2), Some(3), Setting::new(2, Source::Env)),
            (None, None, Some(3), Setting::new(3, Source::File)),
            (None, None, None, Setting::new(default, Source::Default)),
        ];
        for (flag, env, file, expected) in cases {
            let settings = RelaySettings::merge(limits(flag), limits(env), limits(file));
            assert_eq!(settings.limits.max_connections, expected);
        }
    }

    #[test]
    fn defaults_match_edge_limits_and_previous_flags() {
        let settings = RelaySettings::merge(
            RelayLayer::default(),
            RelayLayer::default(),
            RelayLayer::default(),
        );
        assert_eq!(
            settings.listen.value,
            "0.0.0.0:443".parse().expect("valid address")
        );
        assert_eq!(settings.database_path.value, PathBuf::from("vorp.sqlite3"));
        assert_eq!(settings.signup.value, Signup::Closed);
        let limits = settings.limits.edge_limits();
        let default = EdgeLimits::default();
        assert_eq!(limits.max_connections, default.max_connections);
        assert_eq!(
            limits.max_connections_per_ip,
            default.max_connections_per_ip
        );
        assert_eq!(
            limits.requests_per_second_per_ip,
            default.requests_per_second_per_ip
        );
        assert_eq!(limits.max_requests, default.max_requests);
        assert_eq!(limits.max_websockets, default.max_websockets);
        assert_eq!(limits.tunnel_requests, default.tunnel_requests);
        assert_eq!(limits.tunnel_websockets, default.tunnel_websockets);
        assert_eq!(limits.response_timeout, default.response_timeout);
    }

    #[test]
    fn full_file_parses() {
        let text = "\
base_domain: example.com
dashboard_host: dashboard.example.com
listen: 0.0.0.0:443
database_path: /var/lib/vorp/vorp.sqlite3
signup: open
tls:
  cert: /etc/vorp/fullchain.pem
  key: /etc/vorp/privkey.pem
limits:
  max_connections: 2048
  max_connections_per_ip: 64
  rate_limit_rps: 200
  max_requests: 256
  max_websockets: 128
  tunnel_requests: 128
  tunnel_websockets: 128
  response_timeout_secs: 30
";
        let layer =
            RelayLayer::parse(Path::new("/etc/vorp/vorp.yaml"), text).expect("valid config");
        assert_eq!(layer.signup, Some(Signup::Open));
        assert_eq!(layer.tls.key, Some(PathBuf::from("/etc/vorp/privkey.pem")));
        assert_eq!(layer.limits.max_connections, Some(2048));
        let config = RelaySettings::merge(RelayLayer::default(), RelayLayer::default(), layer)
            .into_relay_config(DevOptions::default())
            .expect("complete config");
        assert_eq!(config.dashboard_host, "dashboard.example.com");
        assert_eq!(config.signup_mode, SignupMode::Open);
        assert_eq!(config.limits.max_connections, 2048);
        assert!(config.dev_token.is_none());
    }

    #[test]
    fn deploy_example_parses() {
        let text = include_str!("../../deploy/vorp.yaml");
        let layer =
            RelayLayer::parse(Path::new("deploy/vorp.yaml"), text).expect("example is valid");
        assert_eq!(layer.base_domain.as_deref(), Some("example.com"));

        // The commented-out `limits` block documents the defaults; uncomment
        // it and check it still matches EdgeLimits::default().
        let mut in_limits = false;
        let uncommented: Vec<&str> = text
            .lines()
            .map(|line| {
                in_limits |= line == "# limits:";
                match line.strip_prefix("# ") {
                    Some(rest) if in_limits => rest,
                    _ => line,
                }
            })
            .collect();
        let layer = RelayLayer::parse(Path::new("deploy/vorp.yaml"), &uncommented.join("\n"))
            .expect("uncommented example is valid");
        let documented = RelaySettings::merge(RelayLayer::default(), RelayLayer::default(), layer);
        let defaults = RelaySettings::merge(
            RelayLayer::default(),
            RelayLayer::default(),
            RelayLayer::default(),
        );
        let values = |limits: &LimitSettings| -> Vec<String> {
            limits
                .rows()
                .into_iter()
                .map(|(_, value)| format!("{:?}", value.map(|v| v.0)))
                .collect()
        };
        assert_eq!(values(&documented.limits), values(&defaults.limits));
    }

    #[test]
    fn parse_failures() {
        let path = Path::new("/etc/vorp/vorp.yaml");
        let cases: &[ParseCase] = &[
            (
                "base_domain: a\nlimits:\n  max_conections: 1\n",
                ":3: unknown key `max_conections`",
                |e| matches!(e, ConfigError::UnknownKey { line: 3, .. }),
            ),
            (
                "dev_self_signed: true\n",
                ":1: unknown key `dev_self_signed`",
                |e| matches!(e, ConfigError::UnknownKey { line: 1, .. }),
            ),
            ("tunnels: {}\n", ":1: unknown key `tunnels`", |e| {
                matches!(e, ConfigError::UnknownKey { line: 1, .. })
            }),
            (
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
            ("token: hunter2\n", ":1: `token` is a secret", |e| {
                matches!(
                    e,
                    ConfigError::SecretKey {
                        line: 1,
                        key: "token",
                        ..
                    }
                )
            }),
            ("listen: yes\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            ("limits:\n  max_connections: -1\n", "line 2", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            ("signup: maybe\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
            ("- base_domain\n", "line 1", |e| {
                matches!(e, ConfigError::Parse { .. })
            }),
        ];
        for (text, message, is_expected) in cases {
            let error = RelayLayer::parse(path, text).expect_err(text);
            assert!(is_expected(&error), "{text:?}: {error:?}");
            let shown = error.to_string();
            assert!(shown.starts_with("/etc/vorp/vorp.yaml"), "{shown}");
            assert!(shown.contains(message), "{text:?}: {shown}");
            assert!(!shown.contains("hunter2"), "secret echoed: {shown}");
        }
    }

    #[test]
    fn tls_resolution() {
        let with_tls = |cert: bool, key: bool| RelayLayer {
            base_domain: Some("example.com".into()),
            tls: TlsLayer {
                cert: cert.then(|| "/c.pem".into()),
                key: key.then(|| "/k.pem".into()),
            },
            ..RelayLayer::default()
        };
        let dev = |self_signed: bool| DevOptions {
            self_signed,
            cert_out: self_signed.then(|| "/tmp/dev.pem".into()),
            token: None,
        };
        let cases = [
            (true, true, false, Some("files")),
            (false, false, true, Some("self-signed")),
            (true, false, false, None),
            (false, true, false, None),
            (false, false, false, None),
            (true, true, true, None),
        ];
        for (cert, key, self_signed, expected) in cases {
            let result = RelaySettings::merge(
                RelayLayer::default(),
                RelayLayer::default(),
                with_tls(cert, key),
            )
            .into_relay_config(dev(self_signed));
            let got = match result.as_ref().map(|c| &c.tls) {
                Ok(TlsConfig::Files { .. }) => Some("files"),
                Ok(TlsConfig::SelfSigned { .. }) => Some("self-signed"),
                Ok(TlsConfig::Acme(_)) => Some("acme"),
                Err(ConfigError::InvalidValue { key, .. }) if key == "tls" => None,
                Err(other) => panic!("unexpected error {other}"),
            };
            assert_eq!(got, expected, "cert={cert} key={key} dev={self_signed}");
        }
    }

    #[test]
    fn missing_base_domain() {
        let error = RelaySettings::merge(
            RelayLayer::default(),
            RelayLayer::default(),
            RelayLayer::default(),
        )
        .into_relay_config(DevOptions {
            self_signed: true,
            cert_out: Some("/tmp/c.pem".into()),
            token: None,
        })
        .err()
        .expect("base_domain is required");
        assert!(matches!(
            error,
            ConfigError::Missing {
                key: "base_domain",
                ..
            }
        ));
    }

    #[test]
    fn env_layer_reads_nested_keys() {
        let env = Env::from_pairs(&[
            ("VORP_SIGNUP", "open"),
            ("VORP_TLS_CERT", "/c.pem"),
            ("VORP_LIMITS_MAX_CONNECTIONS", "10"),
            ("VORP_LIMITS_RESPONSE_TIMEOUT_SECS", "5"),
        ]);
        let layer = RelayLayer::from_env(&env).expect("valid environment");
        assert_eq!(layer.signup, Some(Signup::Open));
        assert_eq!(layer.tls.cert, Some(PathBuf::from("/c.pem")));
        assert_eq!(layer.limits.max_connections, Some(10));
        assert_eq!(layer.limits.response_timeout_secs, Some(5));

        let bad = Env::from_pairs(&[("VORP_LIMITS_MAX_CONNECTIONS", "-1")]);
        assert!(matches!(
            RelayLayer::from_env(&bad),
            Err(ConfigError::InvalidValue { key, .. }) if key == "VORP_LIMITS_MAX_CONNECTIONS"
        ));
    }
}
