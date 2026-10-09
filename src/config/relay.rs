use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Args, ValueEnum};
use serde::Deserialize;
use vorp_relay::{EdgeLimits, RelayConfig, SignupMode, TlsConfig};

use super::{ConfigError, Merge, Show, required, resolve};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Signup {
    Open,
    Closed,
}

impl Show for Signup {
    fn show(&self) -> String {
        format!("{self:?}").to_lowercase()
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

/// `vorp serve`'s settings: the flags (with their `VORP_*` variables) and the
/// config file share this one definition. The dev flags are in [`DevArgs`],
/// so a config file can never turn them on.
#[derive(Args, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RelayLayer {
    #[arg(long, env = "VORP_LISTEN", default_value = "0.0.0.0:443")]
    pub(crate) listen: Option<SocketAddr>,

    /// Tunnels are served at `*.<base-domain>`, the dashboard and agent
    /// endpoint at `vorp.<base-domain>`. Required.
    #[arg(long, env = "VORP_BASE_DOMAIN")]
    pub(crate) base_domain: Option<String>,

    #[arg(long, env = "VORP_DATABASE_PATH", default_value = "vorp.sqlite3")]
    pub(crate) database_path: Option<PathBuf>,

    #[arg(long, env = "VORP_SIGNUP", value_enum, default_value = "closed")]
    pub(crate) signup: Option<Signup>,

    #[command(flatten)]
    #[serde(default)]
    pub(crate) tls: TlsLayer,

    #[command(flatten)]
    #[serde(default)]
    pub(crate) limits: LimitsLayer,
}

#[derive(Args, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsLayer {
    /// PEM certificate chain, re-read every 30 s.
    #[arg(long = "tls-cert", env = "VORP_TLS_CERT")]
    pub(crate) cert: Option<PathBuf>,

    #[arg(long = "tls-key", env = "VORP_TLS_KEY")]
    pub(crate) key: Option<PathBuf>,
}

/// Defaults come from [`EdgeLimits::default`], the single source of truth.
#[derive(Args, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LimitsLayer {
    /// Concurrent TLS connections, including handshakes.
    #[arg(long, env = "VORP_LIMITS_MAX_CONNECTIONS", default_value = EdgeLimits::default().max_connections.to_string())]
    pub(crate) max_connections: Option<usize>,

    /// Concurrent TLS connections from one IP (agents and browsers combined).
    #[arg(long, env = "VORP_LIMITS_MAX_CONNECTIONS_PER_IP", default_value = EdgeLimits::default().max_connections_per_ip.to_string())]
    pub(crate) max_connections_per_ip: Option<usize>,

    /// HTTP requests per second per IP; the burst is twice this.
    #[arg(long, env = "VORP_LIMITS_RATE_LIMIT_RPS", default_value = EdgeLimits::default().requests_per_second_per_ip.to_string())]
    pub(crate) rate_limit_rps: Option<u64>,

    /// Concurrent proxied HTTP requests across all tunnels.
    #[arg(long, env = "VORP_LIMITS_MAX_REQUESTS", default_value = EdgeLimits::default().max_requests.to_string())]
    pub(crate) max_requests: Option<usize>,

    /// Concurrent WebSockets across all tunnels.
    #[arg(long, env = "VORP_LIMITS_MAX_WEBSOCKETS", default_value = EdgeLimits::default().max_websockets.to_string())]
    pub(crate) max_websockets: Option<usize>,

    /// Concurrent proxied HTTP requests per tunnel.
    #[arg(long, env = "VORP_LIMITS_TUNNEL_REQUESTS", default_value = EdgeLimits::default().tunnel_requests.to_string())]
    pub(crate) tunnel_requests: Option<usize>,

    /// Concurrent WebSockets per tunnel.
    #[arg(long, env = "VORP_LIMITS_TUNNEL_WEBSOCKETS", default_value = EdgeLimits::default().tunnel_websockets.to_string())]
    pub(crate) tunnel_websockets: Option<usize>,

    /// Seconds to wait for an upstream's response head (resets on upload progress).
    #[arg(long, env = "VORP_LIMITS_RESPONSE_TIMEOUT_SECS", default_value = EdgeLimits::default().response_timeout.as_secs().to_string())]
    pub(crate) response_timeout_secs: Option<u64>,
}

/// Development-only flags. They have no config-file key, so a production
/// config can never turn on the loopback dev bypass by accident.
#[derive(Args, Debug, Default)]
pub(crate) struct DevArgs {
    /// Generate a temporary certificate for local development.
    #[arg(long)]
    pub(crate) dev_self_signed: bool,

    #[arg(long, requires = "dev_self_signed")]
    pub(crate) dev_cert_out: Option<PathBuf>,

    #[arg(long, env = "VORP_DEV_TOKEN", hide_env_values = true)]
    pub(crate) dev_token: Option<String>,
}

impl RelayLayer {
    /// This layer, read from a config file in `dir`, with its relative paths
    /// made relative to `dir`.
    pub(crate) fn relative_to(mut self, dir: &Path) -> Self {
        resolve(dir, &mut self.database_path);
        resolve(dir, &mut self.tls.cert);
        resolve(dir, &mut self.tls.key);
        self
    }

    pub(crate) fn merge(self, file: Self, merge: &mut Merge) -> Self {
        let (limits, file_limits) = (self.limits, file.limits);
        Self {
            listen: merge.pick("listen", self.listen, file.listen),
            base_domain: merge.pick("base_domain", self.base_domain, file.base_domain),
            database_path: merge.pick("database_path", self.database_path, file.database_path),
            signup: merge.pick("signup", self.signup, file.signup),
            tls: TlsLayer {
                cert: merge.pick("tls.cert", self.tls.cert, file.tls.cert),
                key: merge.pick("tls.key", self.tls.key, file.tls.key),
            },
            limits: LimitsLayer {
                max_connections: merge.pick(
                    "limits.max_connections",
                    limits.max_connections,
                    file_limits.max_connections,
                ),
                max_connections_per_ip: merge.pick(
                    "limits.max_connections_per_ip",
                    limits.max_connections_per_ip,
                    file_limits.max_connections_per_ip,
                ),
                rate_limit_rps: merge.pick(
                    "limits.rate_limit_rps",
                    limits.rate_limit_rps,
                    file_limits.rate_limit_rps,
                ),
                max_requests: merge.pick(
                    "limits.max_requests",
                    limits.max_requests,
                    file_limits.max_requests,
                ),
                max_websockets: merge.pick(
                    "limits.max_websockets",
                    limits.max_websockets,
                    file_limits.max_websockets,
                ),
                tunnel_requests: merge.pick(
                    "limits.tunnel_requests",
                    limits.tunnel_requests,
                    file_limits.tunnel_requests,
                ),
                tunnel_websockets: merge.pick(
                    "limits.tunnel_websockets",
                    limits.tunnel_websockets,
                    file_limits.tunnel_websockets,
                ),
                response_timeout_secs: merge.pick(
                    "limits.response_timeout_secs",
                    limits.response_timeout_secs,
                    file_limits.response_timeout_secs,
                ),
            },
        }
    }

    pub(crate) fn into_relay_config(self, dev: DevArgs) -> Result<RelayConfig, ConfigError> {
        let tls = match (self.tls.cert, self.tls.key, dev.dev_self_signed) {
            (Some(cert), Some(key), false) => TlsConfig::Files { cert, key },
            (None, None, true) => TlsConfig::SelfSigned {
                cert_output: dev.dev_cert_out.ok_or(ConfigError::Tls(
                    "--dev-self-signed requires --dev-cert-out",
                ))?,
            },
            (_, _, true) => {
                return Err(ConfigError::Tls(
                    "--dev-self-signed cannot be combined with tls.cert or tls.key; check `vorp config show --relay`",
                ));
            }
            _ => {
                return Err(ConfigError::Tls(
                    "set both tls.cert and tls.key (--tls-cert/--tls-key, VORP_TLS_CERT/VORP_TLS_KEY, or the config file), or use --dev-self-signed --dev-cert-out",
                ));
            }
        };
        let base_domain = required(self.base_domain, "base_domain")?;
        let (limits, d) = (self.limits, EdgeLimits::default());
        Ok(RelayConfig {
            listen: required(self.listen, "listen")?,
            dashboard_host: format!("vorp.{base_domain}"),
            base_domain,
            database_path: required(self.database_path, "database_path")?,
            tls,
            signup_mode: required(self.signup, "signup")?.into(),
            dev_token: dev.dev_token,
            limits: EdgeLimits {
                max_connections: limits.max_connections.unwrap_or(d.max_connections),
                max_connections_per_ip: limits
                    .max_connections_per_ip
                    .unwrap_or(d.max_connections_per_ip),
                requests_per_second_per_ip: limits
                    .rate_limit_rps
                    .unwrap_or(d.requests_per_second_per_ip),
                max_requests: limits.max_requests.unwrap_or(d.max_requests),
                max_websockets: limits.max_websockets.unwrap_or(d.max_websockets),
                tunnel_requests: limits.tunnel_requests.unwrap_or(d.tunnel_requests),
                tunnel_websockets: limits.tunnel_websockets.unwrap_or(d.tunnel_websockets),
                response_timeout: limits
                    .response_timeout_secs
                    .map_or(d.response_timeout, Duration::from_secs),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::parse;

    fn dev(self_signed: bool) -> DevArgs {
        DevArgs {
            dev_self_signed: self_signed,
            dev_cert_out: self_signed.then(|| "/tmp/dev.pem".into()),
            dev_token: None,
        }
    }

    #[test]
    fn tls_resolution() {
        // (tls.cert set, tls.key set, --dev-self-signed) -> self-signed?, or an error
        let cases = [
            (true, true, false, Some(false)),
            (false, false, true, Some(true)),
            (true, false, false, None),
            (false, true, false, None),
            (false, false, false, None),
            (true, true, true, None),
            (true, false, true, None),
        ];
        for (cert, key, self_signed, expected) in cases {
            let mut layer = RelayLayer {
                base_domain: Some("example.com".into()),
                listen: Some("127.0.0.1:8443".parse().expect("valid address")),
                database_path: Some("vorp.sqlite3".into()),
                signup: Some(Signup::Closed),
                ..RelayLayer::default()
            };
            layer.tls.cert = cert.then(|| "/c.pem".into());
            layer.tls.key = key.then(|| "/k.pem".into());
            let got = match layer.into_relay_config(dev(self_signed)) {
                Ok(config) => Some(matches!(config.tls, TlsConfig::SelfSigned { .. })),
                Err(ConfigError::Tls(_)) => None,
                Err(other) => panic!("unexpected error {other}"),
            };
            assert_eq!(got, expected, "cert={cert} key={key} dev={self_signed}");
        }
    }

    #[test]
    fn missing_base_domain_names_every_layer() {
        let error = RelayLayer::default()
            .into_relay_config(dev(true))
            .err()
            .expect("base_domain is required");
        assert_eq!(
            error.to_string(),
            "base_domain is not set: pass --base-domain, set VORP_BASE_DOMAIN, or add `base_domain` to the config file"
        );
    }

    /// The example parses, and its commented-out `limits` block, uncommented,
    /// still matches `EdgeLimits::default()`.
    #[test]
    fn deploy_example_matches_the_defaults() {
        let text = include_str!("../../deploy/vorp.yaml");
        let path = Path::new("deploy/vorp.yaml");
        let layer: RelayLayer = parse(path, text, false).expect("example is valid");
        assert_eq!(layer.base_domain.as_deref(), Some("example.com"));

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
        let layer: RelayLayer =
            parse(path, &uncommented.join("\n"), false).expect("uncommented example is valid");
        let d = EdgeLimits::default();
        assert_eq!(
            layer.limits,
            LimitsLayer {
                max_connections: Some(d.max_connections),
                max_connections_per_ip: Some(d.max_connections_per_ip),
                rate_limit_rps: Some(d.requests_per_second_per_ip),
                max_requests: Some(d.max_requests),
                max_websockets: Some(d.max_websockets),
                tunnel_requests: Some(d.tunnel_requests),
                tunnel_websockets: Some(d.tunnel_websockets),
                response_timeout_secs: Some(d.response_timeout.as_secs()),
            }
        );
    }
}
