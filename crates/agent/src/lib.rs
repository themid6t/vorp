mod config;

pub use config::{AgentConfig, AgentConfigError};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent connection failed: {0}")]
    Connection(String),
}

pub async fn run(_config: AgentConfig) -> Result<(), AgentError> {
    todo!("agent workstream implements outbound TLS and yamux session")
}
