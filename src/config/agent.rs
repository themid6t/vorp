use std::{net::SocketAddr, path::PathBuf};

use clap::Args;
use serde::Deserialize;
use vorp_agent::AgentConfig;

use super::{ConfigError, Merge, required};

/// The agent's settings: the flags (with their `VORP_*` variables) and the
/// config file share this one definition. The token is deliberately absent.
#[derive(Args, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentLayer {
    /// Relay name; its certificate is checked against it.
    #[arg(long, env = "VORP_RELAY_HOST", default_value = "localhost")]
    pub(crate) relay_host: Option<String>,

    /// Dial this address instead of resolving the relay host on port 443.
    #[arg(long, env = "VORP_RELAY_ADDR")]
    pub(crate) relay_addr: Option<SocketAddr>,

    /// Extra CA certificate to trust, for test relays.
    #[arg(long, env = "VORP_CA_CERT")]
    pub(crate) ca_cert: Option<PathBuf>,

    /// [default: authtoken next to the config file]
    #[arg(long, env = "VORP_TOKEN_FILE")]
    pub(crate) token_file: Option<PathBuf>,

    #[arg(long, env = "VORP_UPSTREAM")]
    pub(crate) upstream: Option<String>,

    /// A name to claim; repeat for several. Omit for one random name.
    #[arg(long = "subdomain", env = "VORP_SUBDOMAINS", value_delimiter = ',')]
    pub(crate) subdomains: Option<Vec<String>>,

    /// Allow an upstream that is not loopback.
    #[arg(
        long,
        env = "VORP_ALLOW_REMOTE_TARGETS",
        num_args = 0..=1,
        require_equals = true,
        default_value = "false",
        default_missing_value = "true"
    )]
    pub(crate) allow_remote_targets: Option<bool>,
}

impl AgentLayer {
    pub(crate) fn merge(self, file: Self, merge: &mut Merge) -> Self {
        Self {
            relay_host: merge.pick("relay_host", self.relay_host, file.relay_host),
            relay_addr: merge.pick("relay_addr", self.relay_addr, file.relay_addr),
            ca_cert: merge.pick("ca_cert", self.ca_cert, file.ca_cert),
            token_file: merge.pick("token_file", self.token_file, file.token_file),
            upstream: merge.pick("upstream", self.upstream, file.upstream),
            subdomains: merge.pick("subdomains", self.subdomains, file.subdomains),
            allow_remote_targets: merge.pick(
                "allow_remote_targets",
                self.allow_remote_targets,
                file.allow_remote_targets,
            ),
        }
    }

    pub(crate) fn into_agent_config(self, token: String) -> Result<AgentConfig, ConfigError> {
        let requested_subdomains = match self.subdomains {
            Some(names) if !names.is_empty() => names.into_iter().map(Some).collect(),
            _ => vec![None],
        };
        Ok(AgentConfig {
            relay_host: required(self.relay_host, "relay_host")?,
            relay_addr: self.relay_addr,
            ca_cert: self.ca_cert,
            token,
            upstream: required(self.upstream, "upstream")?,
            requested_subdomains,
            allow_remote_targets: self.allow_remote_targets.unwrap_or_default(),
        })
    }
}
