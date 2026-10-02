mod config;
mod session;
mod upstream;

pub use config::{AgentConfig, AgentConfigError};
pub use session::run_until;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("invalid agent configuration: {0}")]
    Config(#[from] AgentConfigError),
    #[error("agent connection failed: {0}")]
    Connection(String),
    #[error("agent authentication failed")]
    Authentication,
    #[error("relay protocol version is unsupported")]
    UnsupportedVersion,
}
