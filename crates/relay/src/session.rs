use std::{
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::future::poll_fn;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::{
    compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt},
    sync::CancellationToken,
};
use vorp_protocol::{
    CloseReason, ErrorCode, Message, PROTOCOL_VERSION, read_message, write_message,
};
use vorp_store::{BindPolicy, TokenRecord};
use yamux::{Connection, Mode, Stream};

use crate::{
    Relay,
    authz::{self, BindContext},
    registry::{Session, Tunnel, TunnelLimits},
    subdomain,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("yamux connection failed: {0}")]
    Yamux(#[from] yamux::ConnectionError),
    #[error("protocol failed: {0}")]
    Protocol(#[from] vorp_protocol::CodecError),
    #[error("repository failed: {0}")]
    Repository(#[from] vorp_store::RepositoryError),
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("agent disconnected during handshake")]
    HandshakeClosed,
    #[error("invalid agent handshake")]
    InvalidHandshake,
    #[error("connection ended")]
    Closed,
    #[error("agent token was revoked during registration")]
    Revoked,
    #[error("random generation failed: {0}")]
    Random(#[from] getrandom::Error),
}

impl Relay {
    pub(crate) async fn handle_agent<T>(&self, io: T) -> Result<(), SessionError>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let connection = Connection::new(io.compat(), yamux::Config::default(), Mode::Server);
        let (open, open_rx) =
            mpsc::channel::<oneshot::Sender<Result<Stream, yamux::ConnectionError>>>(128);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(32);
        let mut driver = DriverTask::spawn(connection, open_rx, inbound_tx);
        let first = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::select! {
                inbound = inbound_rx.recv() => inbound.ok_or(SessionError::HandshakeClosed),
                result = &mut driver.handle => {
                    result.map_err(|_| SessionError::Closed)??;
                    Err(SessionError::HandshakeClosed)
                }
            }
        })
        .await
        .map_err(|_| SessionError::HandshakeTimeout)??;
        let mut control = first.compat();
        let register = tokio::time::timeout(Duration::from_secs(30), read_message(&mut control))
            .await
            .map_err(|_| SessionError::HandshakeTimeout)??;
        let Message::RegisterAgent {
            protocol_version,
            token,
            machine_id,
        } = register
        else {
            write_message(
                &mut control,
                &Message::AgentErr {
                    code: ErrorCode::StreamError,
                    message: "expected RegisterAgent".into(),
                },
            )
            .await?;
            driver.close().await;
            return Err(SessionError::InvalidHandshake);
        };
        if protocol_version != PROTOCOL_VERSION {
            write_message(
                &mut control,
                &Message::AgentErr {
                    code: ErrorCode::UnsupportedVersion,
                    message: "unsupported protocol version".into(),
                },
            )
            .await?;
            driver.close().await;
            return Err(SessionError::InvalidHandshake);
        }
        let credential = match self.authenticate(&token).await? {
            Some(value) => value,
            None => {
                write_message(
                    &mut control,
                    &Message::AgentErr {
                        code: ErrorCode::AuthFailed,
                        message: "invalid or revoked token".into(),
                    },
                )
                .await?;
                driver.close().await;
                return Err(SessionError::InvalidHandshake);
            }
        };
        let session = Arc::new(Session {
            id: random_session_id()?,
            key: (credential.user_id, machine_id),
            token_id: credential.token_id,
            user_id: credential.user_id,
            cancel: self.state.shutdown.child_token(),
            open,
            teardown_started: AtomicBool::new(false),
            close_reason: Mutex::new(CloseReason::Forced),
        });
        let previous = match self.state.registry.insert_session(Arc::clone(&session)) {
            Ok(previous) => previous,
            Err(()) => {
                write_message(
                    &mut control,
                    &Message::AgentErr {
                        code: ErrorCode::AuthFailed,
                        message: "token was revoked".into(),
                    },
                )
                .await?;
                driver.close().await;
                return Err(SessionError::Revoked);
            }
        };
        if let Some(previous) = previous {
            self.state.registry.teardown(&previous);
        }
        if let Err(error) = write_message(
            &mut control,
            &Message::AgentAck {
                session_id: session.id.clone(),
                server_version: PROTOCOL_VERSION,
            },
        )
        .await
        {
            self.state.registry.teardown(&session);
            return Err(error.into());
        }
        tracing::info!(machine_id = %session.key.1, user_id = session.user_id, "agent connected");
        let mut tunnels = JoinSet::new();
        let mut last_ping = tokio::time::Instant::now();
        let mut watchdog = tokio::time::interval(Duration::from_secs(20));
        let result = loop {
            tokio::select! {
                _ = session.cancel.cancelled() => break Ok(()),
                _ = watchdog.tick() => {
                    if last_ping.elapsed() > Duration::from_secs(75) {
                        break Err(SessionError::Closed);
                    }
                }
                frame = read_message(&mut control) => match frame {
                    Ok(Message::Ping { timestamp_ms }) => {
                        let age = now_ms().saturating_sub(timestamp_ms);
                        if age <= 40_000 {
                            last_ping = tokio::time::Instant::now();
                            if let Err(error) = write_message(&mut control, &Message::Pong { timestamp_ms }).await {
                                break Err(error.into());
                            }
                        }
                    }
                    Ok(_) => break Err(SessionError::InvalidHandshake),
                    Err(error) => break Err(error.into()),
                },
                inbound = inbound_rx.recv() => match inbound {
                    Some(stream) => {
                        let relay = self.clone();
                        let session = Arc::clone(&session);
                        tunnels.spawn(async move {
                            relay.handle_tunnel(session, stream).await;
                        });
                    }
                    None => break Ok(()),
                },
                result = &mut driver.handle => break result.map_err(|_| SessionError::Closed)?,
            }
        };
        self.state.registry.teardown(&session);
        let drain = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(joined) = tunnels.join_next().await {
                if let Err(error) = joined {
                    tracing::warn!(error = %error, "tunnel task failed during shutdown");
                }
            }
        })
        .await;
        if drain.is_err() {
            tunnels.abort_all();
        }
        drop(control);
        driver.close().await;
        tracing::info!(machine_id = %session.key.1, user_id = session.user_id, "agent disconnected");
        result
    }

    async fn authenticate(&self, token: &str) -> Result<Option<Credential>, SessionError> {
        if let Some(expected) = &self.state.config.dev_token {
            use sha2::{Digest, Sha256};
            use subtle::ConstantTimeEq;
            let actual = Sha256::digest(token.as_bytes());
            let expected = Sha256::digest(expected.as_bytes());
            return Ok(bool::from(actual.ct_eq(&expected)).then_some(Credential {
                user_id: 0,
                token_id: None,
                record: None,
            }));
        }
        let Some(repository) = &self.state.repository else {
            return Ok(None);
        };
        let record = repository.authenticate_token(token).await?;
        Ok(record.map(|record| Credential {
            user_id: record.user_id,
            token_id: Some(record.id),
            record: Some(record),
        }))
    }

    /// Why a tunnel ends with its session. A relay shutdown (a deploy or
    /// restart) is recoverable: reporting the session's default `Forced` made
    /// every agent treat a routine restart as permanent and exit.
    fn session_close_reason(&self, session: &Session) -> CloseReason {
        if self.state.shutdown.is_cancelled() {
            return CloseReason::Recoverable;
        }
        *session
            .close_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn handle_tunnel(&self, session: Arc<Session>, stream: Stream) {
        let mut stream = stream.compat();
        let result = self.register_tunnel(&session, &mut stream).await;
        let tunnel = match result {
            Ok(tunnel) => tunnel,
            Err((code, message)) => {
                if let Err(error) =
                    write_message(&mut stream, &Message::TunnelErr { code, message }).await
                {
                    tracing::warn!(error = %error, "failed to send tunnel rejection");
                }
                return;
            }
        };
        let url = format!(
            "https://{}.{}",
            tunnel.subdomain, self.state.config.base_domain
        );
        if let Err(error) = write_message(
            &mut stream,
            &Message::TunnelAck {
                subdomain: tunnel.subdomain.clone(),
                url,
            },
        )
        .await
        {
            tracing::warn!(error = %error, "failed to acknowledge tunnel");
            self.state.registry.remove_tunnel_if_same(&tunnel);
            return;
        }
        tracing::info!(subdomain = %tunnel.subdomain, user_id = session.user_id, "tunnel registered");
        tokio::select! {
            _ = session.cancel.cancelled() => {
                let reason = self.session_close_reason(&session);
                let _ = tokio::time::timeout(Duration::from_secs(2), write_message(&mut stream, &Message::TunnelClose {
                    subdomain: tunnel.subdomain.clone(), reason,
                })).await;
            }
            _ = tunnel.cancel.cancelled() => {
                // The dashboard has already detached this tunnel from routing.
                let reason = if session.cancel.is_cancelled() {
                    self.session_close_reason(&session)
                } else {
                    CloseReason::Forced
                };
                let _ = tokio::time::timeout(Duration::from_secs(2), write_message(&mut stream, &Message::TunnelClose {
                    subdomain: tunnel.subdomain.clone(), reason,
                })).await;
            }
            received = read_message(&mut stream) => {
                if let Err(error) = received {
                    tracing::warn!(subdomain = %tunnel.subdomain, error = %error, "tunnel stream ended");
                }
            }
        }
        self.state.registry.remove_tunnel_if_same(&tunnel);
    }

    async fn register_tunnel<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
        &self,
        session: &Arc<Session>,
        stream: &mut S,
    ) -> Result<Arc<Tunnel>, (ErrorCode, String)> {
        let request = read_message(stream)
            .await
            .map_err(|error| (ErrorCode::StreamError, error.to_string()))?;
        let Message::RegisterTunnel {
            subdomain,
            upstream_hint,
        } = request
        else {
            return Err((ErrorCode::StreamError, "expected RegisterTunnel".into()));
        };
        let requested = subdomain.as_deref().unwrap_or("");
        let policy = if session.token_id.is_none() {
            BindPolicy::Any
        } else {
            let repository = self
                .state
                .repository
                .as_ref()
                .ok_or((ErrorCode::StreamError, "repository unavailable".into()))?;
            repository
                .token_by_id(session.token_id.unwrap_or_default())
                .await
                .map_err(|error| (ErrorCode::StreamError, error.to_string()))?
                .ok_or((ErrorCode::AuthFailed, "token no longer exists".into()))?
                .bind_policy
        };
        let record =
            if let (Some(repository), Some(id)) = (&self.state.repository, session.token_id) {
                repository
                    .token_by_id(id)
                    .await
                    .map_err(|error| (ErrorCode::StreamError, error.to_string()))?
            } else {
                None
            };
        let owner = if requested.is_empty() {
            None
        } else if let Some(repository) = &self.state.repository {
            repository
                .reservation_owner(requested)
                .await
                .map_err(|error| (ErrorCode::StreamError, error.to_string()))?
        } else {
            None
        };
        let assigned = if !requested.is_empty() && owner.is_none() && policy == BindPolicy::Any {
            if let Some(repository) = &self.state.repository {
                Some(
                    repository
                        .get_or_create_assigned_subdomain(session.user_id)
                        .await
                        .map_err(|error| (ErrorCode::StreamError, error.to_string()))?,
                )
            } else {
                None
            }
        } else {
            None
        };
        let allowlist = record
            .as_ref()
            .map_or(&[][..], |record| record.allowlist.as_slice());
        if session.token_id.is_some() {
            authz::decide_bind(&BindContext {
                requested,
                user_id: session.user_id,
                policy,
                allowlist,
                assigned_name: assigned.as_deref(),
                reservation_owner: owner,
            })
            .map_err(|code| (code, "subdomain bind rejected".into()))?;
        } else if !requested.is_empty() && !vorp_protocol::valid_subdomain(requested) {
            return Err((ErrorCode::SubdomainInvalid, "invalid subdomain".into()));
        }
        if requested.is_empty() {
            return self
                .state
                .registry
                .allocate_tunnel(
                    Arc::clone(session),
                    upstream_hint,
                    TunnelLimits::from(&self.state.config.limits),
                    subdomain::generate_slug,
                )
                .map_err(|error| (ErrorCode::StreamError, error.to_string()));
        }
        let tunnel = Arc::new(Tunnel::new(
            Arc::clone(session),
            requested.into(),
            upstream_hint,
            TunnelLimits::from(&self.state.config.limits),
        ));
        if self.state.registry.insert_named_tunnel(Arc::clone(&tunnel)) {
            Ok(tunnel)
        } else {
            Err((ErrorCode::SubdomainTaken, "subdomain already in use".into()))
        }
    }
}

struct DriverTask {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<Result<(), SessionError>>,
}

impl DriverTask {
    fn spawn<T>(
        connection: Connection<T>,
        open_rx: mpsc::Receiver<oneshot::Sender<Result<Stream, yamux::ConnectionError>>>,
        inbound_tx: mpsc::Sender<Stream>,
    ) -> Self
    where
        T: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(connection_driver(
            connection,
            open_rx,
            inbound_tx,
            cancel.clone(),
        ));
        Self { cancel, handle }
    }

    async fn close(&mut self) {
        self.cancel.cancel();
        if !self.handle.is_finished() {
            match tokio::time::timeout(Duration::from_secs(2), &mut self.handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    tracing::warn!(error = %error, "yamux driver ended during close")
                }
                Ok(Err(error)) => tracing::warn!(error = %error, "yamux driver task failed"),
                Err(_) => tracing::warn!("yamux driver close timed out"),
            }
        }
    }
}

impl Drop for DriverTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.handle.abort();
    }
}

async fn connection_driver<T>(
    mut connection: Connection<T>,
    mut open_rx: mpsc::Receiver<oneshot::Sender<Result<Stream, yamux::ConnectionError>>>,
    inbound_tx: mpsc::Sender<Stream>,
    cancel: CancellationToken,
) -> Result<(), SessionError>
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + 'static,
{
    enum Event {
        Inbound(Option<Result<Stream, yamux::ConnectionError>>),
        Outbound(Result<Stream, yamux::ConnectionError>),
    }
    let mut pending_open = None;
    let mut open_channel_open = true;
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            request = open_rx.recv(), if pending_open.is_none() && open_channel_open => {
                if let Some(request) = request { pending_open = Some(request); }
                else { open_channel_open = false; }
            }
            event = poll_fn(|cx| {
                if pending_open.is_some()
                    && let std::task::Poll::Ready(opened) = connection.poll_new_outbound(cx) {
                    return std::task::Poll::Ready(Event::Outbound(opened));
                }
                connection.poll_next_inbound(cx).map(Event::Inbound)
            }) => match event {
                Event::Outbound(opened) => {
                    if let Some(reply) = pending_open.take() {
                        // HTTP callers may disconnect before yamux opens the stream.
                        let _ = reply.send(opened);
                    }
                }
                Event::Inbound(Some(Ok(stream))) => match inbound_tx.try_send(stream) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_stream)) => {
                        tracing::debug!("agent opened more streams than the relay can queue");
                    }
                    Err(mpsc::error::TrySendError::Closed(_stream)) => break Ok(()),
                },
                Event::Inbound(Some(Err(error))) => break Err(error.into()),
                Event::Inbound(None) => break Err(SessionError::Closed),
            },
        }
    };
    if result.is_ok() {
        match tokio::time::timeout(
            Duration::from_secs(2),
            poll_fn(|cx| connection.poll_close(cx)),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(error = %error, "failed to close agent connection"),
            Err(_) => tracing::warn!("timed out closing agent connection"),
        }
    }
    result
}

struct Credential {
    user_id: i64,
    token_id: Option<i64>,
    #[allow(dead_code)]
    // The resolved row is retained for future per-token policy without a second lookup.
    record: Option<TokenRecord>,
}

fn random_session_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(i64::MAX as u128) as i64
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RelayConfig, State, TlsConfig, registry::Registry};
    use std::sync::OnceLock;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn pending_outbound_open_does_not_block_inbound_driver() {
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let (server_open, server_requests) = mpsc::channel(300);
        let (server_inbound, mut incoming) = mpsc::channel(8);
        let server = DriverTask::spawn(
            Connection::new(server_io.compat(), yamux::Config::default(), Mode::Server),
            server_requests,
            server_inbound,
        );
        let (client_open, client_requests) = mpsc::channel(8);
        let (client_inbound, _client_incoming) = mpsc::channel(8);
        let client = DriverTask::spawn(
            Connection::new(client_io.compat(), yamux::Config::default(), Mode::Client),
            client_requests,
            client_inbound,
        );

        // Yamux stops granting new outbound streams at 256 unacknowledged
        // opens. Hold those streams so the 257th open stays pending.
        let mut held = Vec::new();
        for _ in 0..256 {
            let (reply, answer) = oneshot::channel();
            server_open.send(reply).await.expect("queue outbound open");
            held.push(answer.await.expect("driver reply").expect("open stream"));
        }
        let (reply, mut blocked) = oneshot::channel();
        server_open.send(reply).await.expect("queue blocked open");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut blocked)
                .await
                .is_err()
        );

        let (reply, answer) = oneshot::channel();
        client_open.send(reply).await.expect("queue client open");
        let mut from_client = answer
            .await
            .expect("client driver reply")
            .expect("client stream")
            .compat();
        from_client
            .write_all(b"x")
            .await
            .expect("initiate inbound stream");
        let _received = tokio::time::timeout(Duration::from_secs(2), incoming.recv())
            .await
            .expect("inbound stream was not driven")
            .expect("driver ended");
        drop(held);
        drop(server);
        drop(client);
    }

    #[tokio::test]
    async fn driver_moves_control_and_tunnel_frames_while_streams_are_read() {
        let shutdown = CancellationToken::new();
        let relay = Relay {
            state: Arc::new(State {
                config: RelayConfig {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    base_domain: "localhost".into(),
                    dashboard_host: "localhost".into(),
                    database_path: "unused.sqlite".into(),
                    tls: TlsConfig::SelfSigned {
                        cert_output: "unused-cert.pem".into(),
                    },
                    signup_mode: vorp_web::SignupMode::Closed,
                    dev_token: Some("test-secret".into()),
                    limits: crate::EdgeLimits::default(),
                },
                repository: None,
                registry: Registry::default(),
                http_requests: Arc::new(tokio::sync::Semaphore::new(1)),
                websockets: Arc::new(tokio::sync::Semaphore::new(1)),
                request_rates: crate::limits::RequestRateLimiter::new(200),
                shutdown: shutdown.clone(),
                web_router: OnceLock::new(),
            }),
        };
        let (client_io, server_io) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move { relay.handle_agent(server_io).await });
        let (open_tx, open_rx) = mpsc::channel(8);
        let (inbound_tx, _inbound_rx) = mpsc::channel(8);
        let client_connection =
            Connection::new(client_io.compat(), yamux::Config::default(), Mode::Client);
        let client_driver = DriverTask::spawn(client_connection, open_rx, inbound_tx);
        let (reply, received) = oneshot::channel();
        open_tx.send(reply).await.unwrap();
        let mut control = received.await.unwrap().unwrap().compat();
        write_message(
            &mut control,
            &Message::RegisterAgent {
                protocol_version: PROTOCOL_VERSION,
                token: "test-secret".into(),
                machine_id: "test-machine".into(),
            },
        )
        .await
        .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(2), read_message(&mut control))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ack, Message::AgentAck { .. }));
        write_message(
            &mut control,
            &Message::Ping {
                timestamp_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        let pong = tokio::time::timeout(Duration::from_secs(2), read_message(&mut control))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(pong, Message::Pong { .. }));
        let (reply, received) = oneshot::channel();
        open_tx.send(reply).await.unwrap();
        let mut tunnel = received.await.unwrap().unwrap().compat();
        write_message(
            &mut tunnel,
            &Message::RegisterTunnel {
                subdomain: Some("app".into()),
                upstream_hint: None,
            },
        )
        .await
        .unwrap();
        let ack = tokio::time::timeout(Duration::from_secs(2), read_message(&mut tunnel))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ack, Message::TunnelAck { .. }));
        shutdown.cancel();
        let close = tokio::time::timeout(Duration::from_secs(2), read_message(&mut tunnel))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                close,
                Message::TunnelClose {
                    reason: CloseReason::Recoverable,
                    ..
                }
            ),
            "a relay shutdown must not tell agents to give up: {close:?}"
        );
        tokio::time::timeout(Duration::from_secs(7), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(client_driver);
    }
}
