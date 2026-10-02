mod authz;
mod config;
mod httpnorm;
mod limits;
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
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Relay {
    state: Arc<State>,
}

struct State {
    config: RelayConfig,
    repository: Option<vorp_store::Repository>,
    registry: registry::Registry,
    http_requests: Arc<Semaphore>,
    websockets: Arc<Semaphore>,
    request_rates: limits::RequestRateLimiter,
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

pub async fn serve_until(
    config: RelayConfig,
    shutdown: CancellationToken,
) -> Result<(), RelayError> {
    if !dev_token_mode_allowed(&config) {
        return Err(RelayError::Config(
            "development token requires self-signed TLS and a loopback listener".into(),
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
            http_requests: Arc::new(Semaphore::new(256)),
            websockets: Arc::new(Semaphore::new(128)),
            request_rates: limits::RequestRateLimiter::new(),
            shutdown,
            web_router: OnceLock::new(),
        }),
    };
    relay.run().await
}

fn dev_token_mode_allowed(config: &RelayConfig) -> bool {
    config.dev_token.is_none()
        || (matches!(config.tls, TlsConfig::SelfSigned { .. }) && config.listen.ip().is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_token_requires_both_loopback_and_self_signed_tls() {
        let mut config = RelayConfig {
            listen: "127.0.0.1:8443".parse().expect("loopback socket"),
            base_domain: "localhost".into(),
            dashboard_host: "localhost".into(),
            database_path: "unused.sqlite3".into(),
            tls: TlsConfig::SelfSigned {
                cert_output: "unused.crt".into(),
            },
            signup_mode: SignupMode::Closed,
            dev_token: Some("test-token".into()),
        };
        assert!(dev_token_mode_allowed(&config));
        config.listen = "0.0.0.0:8443".parse().expect("public socket");
        assert!(!dev_token_mode_allowed(&config));
        config.listen = "127.0.0.1:8443".parse().expect("loopback socket");
        config.tls = TlsConfig::Files {
            cert: "unused.crt".into(),
            key: "unused.key".into(),
        };
        assert!(!dev_token_mode_allowed(&config));
        config.dev_token = None;
        assert!(dev_token_mode_allowed(&config));
    }
}
