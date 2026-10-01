mod authz;
mod config;
mod httpnorm;
mod proxy;
mod registry;
mod server;
mod session;
mod subdomain;
mod tls;

pub use config::{AcmeConfig, RelayConfig, TlsConfig};
pub use vorp_web::SignupMode;

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("relay listener failed: {0}")]
    Listener(String),
    #[error("TLS setup failed: {0}")]
    Tls(String),
    #[error("relay configuration invalid: {0}")]
    Config(String),
    #[error("repository failed: {0}")]
    Repository(#[from] vorp_store::RepositoryError),
}

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock, Weak},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Relay {
    state: Arc<State>,
}

struct State {
    config: RelayConfig,
    repository: Option<vorp_store::Repository>,
    registry: registry::Registry,
    shutdown: CancellationToken,
    web_router: OnceLock<axum::Router>,
}

struct WebHooks(Weak<State>);

impl vorp_web::TokenDisconnect for WebHooks {
    fn disconnect(
        &self,
        token_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            let state = self
                .0
                .upgrade()
                .ok_or_else(|| "relay is shutting down".to_owned())?;
            state.registry.disconnect_token(token_id);
            Ok(())
        })
    }
}

impl vorp_web::DashboardRuntime for WebHooks {
    fn tunnels(
        &self,
        user_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<vorp_web::TunnelView>, String>> + Send + '_>> {
        Box::pin(async move {
            let state = self
                .0
                .upgrade()
                .ok_or_else(|| "relay is shutting down".to_owned())?;
            Ok(state.registry.tunnels_for_user(user_id))
        })
    }

    fn close_tunnel(
        &self,
        user_id: i64,
        subdomain: &str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, String>> + Send + '_>> {
        let subdomain = subdomain.to_owned();
        Box::pin(async move {
            let state = self
                .0
                .upgrade()
                .ok_or_else(|| "relay is shutting down".to_owned())?;
            Ok(state.registry.close_for_user(user_id, &subdomain))
        })
    }

    fn recent_traffic(
        &self,
        user_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<vorp_web::TrafficEvent>, String>> + Send + '_>>
    {
        Box::pin(async move {
            let state = self
                .0
                .upgrade()
                .ok_or_else(|| "relay is shutting down".to_owned())?;
            Ok(state.registry.recent_traffic(user_id))
        })
    }
}

impl Relay {
    pub fn disconnect_token(&self, token_id: i64) {
        self.state.registry.disconnect_token(token_id);
    }
}

pub async fn serve(config: RelayConfig) -> Result<(), RelayError> {
    let shutdown = CancellationToken::new();
    let on_signal = shutdown.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_signal.cancel();
        }
    });
    let result = serve_until(config, shutdown).await;
    signal.abort();
    result
}

pub async fn serve_until(
    config: RelayConfig,
    shutdown: CancellationToken,
) -> Result<(), RelayError> {
    if config.dev_token.is_some()
        && !matches!(config.tls, TlsConfig::SelfSigned { .. })
        && !config.listen.ip().is_loopback()
    {
        return Err(RelayError::Config(
            "development token requires self-signed TLS or a loopback listener".into(),
        ));
    }
    let repository = if config.dev_token.is_some() {
        None
    } else {
        Some(vorp_store::Repository::open(&config.database_path).await?)
    };
    let relay = Relay {
        state: Arc::new(State {
            config,
            repository,
            registry: registry::Registry::default(),
            shutdown,
            web_router: OnceLock::new(),
        }),
    };
    relay.run().await
}
