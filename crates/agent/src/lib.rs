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

/// Run until Ctrl-C, then allow the session and its child tasks to stop.
pub async fn run(config: AgentConfig) -> Result<(), AgentError> {
    let shutdown = tokio_util::sync::CancellationToken::new();
    let signal = shutdown.clone();
    let signal_task = tokio::spawn(async move {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(err) = result { tracing::warn!(error = %err, "Ctrl-C handler failed"); }
                signal.cancel();
            }
            _ = signal.cancelled() => {}
        }
    });
    let result = run_until(config, shutdown.clone()).await;
    shutdown.cancel();
    let _ = signal_task.await; // The signal task only records shutdown and has already been cancelled.
    result
}
