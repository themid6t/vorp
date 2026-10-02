use std::{net::SocketAddr, path::PathBuf};
use url::{Host, Url};

#[derive(Clone)]
pub struct AgentConfig {
    pub relay_host: String,
    /// Overrides DNS resolution of `relay_host` (port 443) when set.
    pub relay_addr: Option<SocketAddr>,
    pub token: String,
    pub upstream: String,
    pub requested_subdomains: Vec<Option<String>>,
    pub allow_remote_targets: bool,
    pub ca_cert: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentConfigError {
    #[error("upstream URL is invalid: {0}")]
    InvalidUpstream(String),
    #[error("non-loopback upstream requires explicit opt-in")]
    RemoteUpstreamDenied,
    #[error("only http upstreams are supported")]
    UnsupportedUpstreamScheme,
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), AgentConfigError> {
        self.upstream_url().map(|_| ())
    }

    pub(crate) fn upstream_url(&self) -> Result<Url, AgentConfigError> {
        let url = Url::parse(&self.upstream)
            .map_err(|err| AgentConfigError::InvalidUpstream(err.to_string()))?;
        if url.scheme() != "http" {
            return Err(AgentConfigError::UnsupportedUpstreamScheme);
        }
        if url.host_str().is_none()
            || url.port_or_known_default().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AgentConfigError::InvalidUpstream(
                "expected an origin with no userinfo, path, query or fragment".into(),
            ));
        }
        let loopback = match url.host() {
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if !loopback && !self.allow_remote_targets {
            return Err(AgentConfigError::RemoteUpstreamDenied);
        }
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(upstream: &str) -> AgentConfig {
        AgentConfig {
            relay_host: "localhost".into(),
            relay_addr: None,
            token: "x".into(),
            upstream: upstream.into(),
            requested_subdomains: vec![None],
            allow_remote_targets: false,
            ca_cert: None,
        }
    }

    #[test]
    fn upstream_validation_table() {
        let cases = [
            ("http://localhost:3000", true),
            ("http://127.0.0.1:3000", true),
            ("http://[::1]:3000", true),
            ("http://example.com:3000", false),
            ("http://localhost:3000/path", false),
            ("http://localhost:3000/?q=1", false),
            ("http://user@localhost:3000", false),
            ("https://localhost:3000", false),
        ];
        for (upstream, valid) in cases {
            assert_eq!(config(upstream).validate().is_ok(), valid, "{upstream}");
        }
    }
}
