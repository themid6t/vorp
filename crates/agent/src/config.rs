use std::net::SocketAddr;

#[derive(Clone)]
pub struct AgentConfig {
    pub relay_host: String,
    pub relay_addr: SocketAddr,
    pub token: String,
    pub upstream: String,
    pub requested_subdomains: Vec<Option<String>>,
    pub allow_remote_targets: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentConfigError {
    #[error("upstream URL is invalid: {0}")]
    InvalidUpstream(String),
    #[error("non-loopback upstream requires explicit opt-in")]
    RemoteUpstreamDenied,
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), AgentConfigError> {
        todo!("agent workstream validates upstream targets")
    }
}
