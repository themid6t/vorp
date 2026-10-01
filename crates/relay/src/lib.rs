mod config;

pub use config::{AcmeConfig, RelayConfig, TlsConfig};

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("relay listener failed: {0}")]
    Listener(String),
}

pub async fn serve(_config: RelayConfig) -> Result<(), RelayError> {
    todo!("relay workstream implements ALPN listener and session lifecycle")
}
