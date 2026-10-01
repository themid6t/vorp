use std::{convert::Infallible, net::SocketAddr};

use axum::body::Body;
use http::{Request, Response, StatusCode};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::{net::TcpListener, task::JoinSet};
use tower::ServiceExt;

use crate::{Relay, RelayError, limits::ConnectionLimiter};

const MAX_CONNECTIONS: usize = 1024;
const MAX_CONNECTIONS_PER_IP: usize = 64;
const HTTP_HEADER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
enum DispatchProtocol {
    Agent,
    Http1,
    Http2,
    Unsupported,
}

fn dispatch_protocol(alpn: Option<&[u8]>) -> DispatchProtocol {
    match alpn {
        Some(value) if value == vorp_protocol::AGENT_ALPN => DispatchProtocol::Agent,
        Some(b"http/1.1") | None => DispatchProtocol::Http1,
        Some(b"h2") => DispatchProtocol::Http2,
        _ => DispatchProtocol::Unsupported,
    }
}

impl Relay {
    pub(crate) async fn run(&self) -> Result<(), RelayError> {
        let mut tls = crate::tls::TlsManager::new(&self.state.config).await?;
        let mut certificate_check = tokio::time::interval(std::time::Duration::from_secs(30));
        let listener = TcpListener::bind(self.state.config.listen)
            .await
            .map_err(|error| {
                RelayError::Listener(format!("bind {}: {error}", self.state.config.listen))
            })?;
        tracing::info!(address = %self.state.config.listen, "relay listening");
        let mut connections = JoinSet::new();
        let limiter = ConnectionLimiter::new(MAX_CONNECTIONS, MAX_CONNECTIONS_PER_IP);
        loop {
            tokio::select! {
                _ = self.state.shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let (socket, peer) = accepted.map_err(|error| RelayError::Listener(format!("accept TCP connection: {error}")))?;
                    let Some(permit) = limiter.try_acquire(peer.ip()) else {
                        tracing::debug!(peer = %peer, "connection limit reached");
                        continue;
                    };
                    let acceptor = tls.acceptor();
                    let relay = self.clone();
                    connections.spawn(async move {
                        let _permit = permit;
                        let result = tokio::time::timeout(std::time::Duration::from_secs(30), acceptor.accept(socket)).await;
                        match result {
                            Ok(Ok(tls)) => relay.dispatch(tls, peer).await,
                            Ok(Err(error)) => tracing::warn!(peer = %peer, error = %error, "TLS handshake failed"),
                            Err(_) => tracing::warn!(peer = %peer, "TLS handshake timed out"),
                        }
                    });
                }
                Some(joined) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = joined {
                        tracing::error!(error = %error, "connection task failed");
                    }
                }
                _ = certificate_check.tick(), if tls.is_reloadable() => {
                    match tls.reload_if_changed().await {
                        Ok(true) => tracing::info!("TLS certificate reloaded"),
                        Ok(false) => {},
                        Err(error) => tracing::warn!(error = %error, "TLS certificate reload failed; retaining the last valid certificate"),
                    }
                }
            }
        }
        self.state.shutdown.cancel();
        let drain = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        if drain.is_err() {
            connections.abort_all();
        }
        Ok(())
    }

    async fn dispatch(
        &self,
        tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
        peer: SocketAddr,
    ) {
        let alpn = tls.get_ref().1.alpn_protocol();
        match dispatch_protocol(alpn) {
            DispatchProtocol::Agent => {
                if let Err(error) = self.handle_agent(tls).await {
                    tracing::warn!(peer = %peer, error = %error, "agent session ended");
                }
            }
            DispatchProtocol::Http1 => self.serve_http1(tls, peer).await,
            DispatchProtocol::Http2 => self.serve_http2(tls, peer).await,
            DispatchProtocol::Unsupported => tracing::warn!(peer = %peer, "unsupported TLS ALPN"),
        }
    }

    async fn serve_http1(
        &self,
        tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
        peer: SocketAddr,
    ) {
        let relay = self.clone();
        let service = service_fn(move |request| {
            let relay = relay.clone();
            async move { Ok::<_, Infallible>(relay.handle_http(request, peer).await) }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(HTTP_HEADER_TIMEOUT)
            .max_buf_size(64 * 1024);
        let connection = builder
            .serve_connection(TokioIo::new(tls), service)
            .with_upgrades();
        tokio::select! {
            _ = self.state.shutdown.cancelled() => {},
            result = connection => if let Err(error) = result {
                tracing::warn!(peer = %peer, error = %error, "HTTP/1 connection failed");
            },
        }
    }

    async fn serve_http2(
        &self,
        tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
        peer: SocketAddr,
    ) {
        let relay = self.clone();
        let service = service_fn(move |request| {
            let relay = relay.clone();
            async move { Ok::<_, Infallible>(relay.handle_http(request, peer).await) }
        });
        let mut builder = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
        builder
            .timer(TokioTimer::new())
            .max_concurrent_streams(128)
            .max_header_list_size(64 * 1024)
            .keep_alive_interval(std::time::Duration::from_secs(30))
            .keep_alive_timeout(std::time::Duration::from_secs(20));
        let connection = builder.serve_connection(TokioIo::new(tls), service);
        tokio::select! {
            _ = self.state.shutdown.cancelled() => {},
            result = connection => if let Err(error) = result {
                tracing::warn!(peer = %peer, error = %error, "HTTP/2 connection failed");
            },
        }
    }

    async fn handle_http(&self, request: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
        let requested_host = request
            .uri()
            .authority()
            .map(|a| a.as_str())
            .or_else(|| {
                request
                    .headers()
                    .get(http::header::HOST)
                    .and_then(|value| value.to_str().ok())
            })
            .unwrap_or("")
            .to_owned();
        let host = requested_host.split(':').next().unwrap_or("");
        if host.eq_ignore_ascii_case(&self.state.config.dashboard_host) {
            return self.handle_dashboard(request).await;
        }
        let suffix = format!(".{}", self.state.config.base_domain);
        let Some(subdomain) = host.strip_suffix(&suffix) else {
            return status(StatusCode::NOT_FOUND);
        };
        if subdomain.contains('.') || subdomain.is_empty() {
            return status(StatusCode::NOT_FOUND);
        }
        self.proxy(request, peer, subdomain, &requested_host).await
    }

    async fn handle_dashboard(&self, request: Request<Incoming>) -> Response<Body> {
        let Some(repository) = &self.state.repository else {
            return status(StatusCode::NOT_FOUND);
        };
        let router = self.state.web_router.get_or_init(|| {
            let hooks =
                std::sync::Arc::new(crate::WebHooks(std::sync::Arc::downgrade(&self.state)));
            vorp_web::router_with_runtime(
                repository.clone(),
                vorp_web::WebConfig {
                    signup_mode: self.state.config.signup_mode,
                    session_ttl_secs: 86_400,
                },
                Some(hooks.clone()),
                Some(hooks),
            )
        });
        match router.clone().oneshot(request.map(Body::new)).await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(error = %error, "dashboard request failed");
                status(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}

pub(crate) fn status(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpn_dispatch_table() {
        for (alpn, expected) in [
            (None, DispatchProtocol::Http1),
            (Some(&b"http/1.1"[..]), DispatchProtocol::Http1),
            (Some(&b"h2"[..]), DispatchProtocol::Http2),
            (Some(vorp_protocol::AGENT_ALPN), DispatchProtocol::Agent),
            (Some(&b"unknown"[..]), DispatchProtocol::Unsupported),
        ] {
            assert_eq!(dispatch_protocol(alpn), expected);
        }
    }
}
