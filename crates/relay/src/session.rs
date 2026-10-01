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
    registry::{Session, Tunnel},
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
    #[error("random generation failed: {0}")]
    Random(#[from] getrandom::Error),
}

impl Relay {
    pub(crate) async fn handle_agent<T>(&self, io: T) -> Result<(), SessionError>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let mut connection = Connection::new(io.compat(), yamux::Config::default(), Mode::Server);
        let first = tokio::time::timeout(
            Duration::from_secs(30),
            poll_fn(|cx| connection.poll_next_inbound(cx)),
        )
        .await
        .map_err(|_| SessionError::HandshakeTimeout)?
        .ok_or(SessionError::HandshakeClosed)??;
        let mut control = first.compat();
        let register = tokio::time::timeout(
            Duration::from_secs(30),
            read_handshake_message(&mut connection, &mut control),
        )
        .await
        .map_err(|_| SessionError::HandshakeTimeout)??;
        let Message::RegisterAgent {
            protocol_version,
            token,
            machine_id,
        } = register
        else {
            write_handshake_message(
                &mut connection,
                &mut control,
                &Message::AgentErr {
                    code: ErrorCode::StreamError,
                    message: "expected RegisterAgent".into(),
                },
            )
            .await?;
            drop(control);
            close_yamux_connection(&mut connection).await;
            return Err(SessionError::InvalidHandshake);
        };
        if protocol_version != PROTOCOL_VERSION {
            write_handshake_message(
                &mut connection,
                &mut control,
                &Message::AgentErr {
                    code: ErrorCode::UnsupportedVersion,
                    message: "unsupported protocol version".into(),
                },
            )
            .await?;
            drop(control);
            close_yamux_connection(&mut connection).await;
            return Err(SessionError::InvalidHandshake);
        }
        let credential = match self.authenticate(&token).await? {
            Some(value) => value,
            None => {
                write_handshake_message(
                    &mut connection,
                    &mut control,
                    &Message::AgentErr {
                        code: ErrorCode::AuthFailed,
                        message: "invalid or revoked token".into(),
                    },
                )
                .await?;
                drop(control);
                close_yamux_connection(&mut connection).await;
                return Err(SessionError::InvalidHandshake);
            }
        };
        let (open, mut open_rx) =
            mpsc::channel::<oneshot::Sender<Result<Stream, yamux::ConnectionError>>>(128);
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
        if let Some(previous) = self.state.registry.insert_session(Arc::clone(&session)) {
            self.state.registry.teardown(&previous);
        }
        if let Err(error) = write_handshake_message(
            &mut connection,
            &mut control,
            &Message::AgentAck {
                session_id: session.id.clone(),
                server_version: PROTOCOL_VERSION,
            },
        )
        .await
        {
            self.state.registry.teardown(&session);
            return Err(error);
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
                inbound = poll_fn(|cx| connection.poll_next_inbound(cx)) => match inbound {
                    Some(Ok(stream)) => {
                        let relay = self.clone();
                        let session = Arc::clone(&session);
                        tunnels.spawn(async move {
                            relay.handle_tunnel(session, stream).await;
                        });
                    }
                    Some(Err(error)) => break Err(error.into()),
                    None => break Ok(()),
                },
                request = open_rx.recv() => if let Some(reply) = request {
                    let opened = poll_fn(|cx| connection.poll_new_outbound(cx)).await;
                    // A dropped HTTP caller no longer needs its stream.
                    let _ = reply.send(opened);
                },
            }
        };
        self.state.registry.teardown(&session);
        let drain = tokio::time::timeout(Duration::from_secs(5), async {
            let mut connection_open = true;
            while !tunnels.is_empty() {
                tokio::select! {
                    joined = tunnels.join_next() => {
                        if let Some(Err(error)) = joined {
                            tracing::warn!(error = %error, "tunnel task failed during shutdown");
                        }
                    }
                    inbound = poll_fn(|cx| connection.poll_next_inbound(cx)), if connection_open => {
                        connection_open = inbound.is_some();
                    }
                }
            }
        }).await;
        if drain.is_err() {
            tunnels.abort_all();
        }
        drop(control);
        close_yamux_connection(&mut connection).await;
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
                let reason = *session.close_reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = tokio::time::timeout(Duration::from_secs(2), write_message(&mut stream, &Message::TunnelClose {
                    subdomain: tunnel.subdomain.clone(), reason,
                })).await;
            }
            _ = tunnel.cancel.cancelled() => {
                // The dashboard has already detached this tunnel from routing.
                let reason = if session.cancel.is_cancelled() {
                    *session.close_reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
        } else if !requested.is_empty() && !subdomain::valid(requested) {
            return Err((ErrorCode::SubdomainInvalid, "invalid subdomain".into()));
        }
        if requested.is_empty() {
            return self
                .state
                .registry
                .allocate_tunnel(Arc::clone(session), upstream_hint, subdomain::generate_slug)
                .map_err(|error| (ErrorCode::StreamError, error.to_string()));
        }
        let tunnel = Arc::new(Tunnel {
            session: Arc::clone(session),
            subdomain: requested.into(),
            concurrency_limit: 128,
            active: std::sync::atomic::AtomicUsize::new(0),
            upstream_hint,
            cancel: CancellationToken::new(),
        });
        if self.state.registry.insert_named_tunnel(Arc::clone(&tunnel)) {
            Ok(tunnel)
        } else {
            Err((ErrorCode::SubdomainTaken, "subdomain already in use".into()))
        }
    }
}

async fn read_handshake_message<T>(
    connection: &mut Connection<T>,
    control: &mut tokio_util::compat::Compat<Stream>,
) -> Result<Message, SessionError>
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin,
{
    tokio::select! {
        result = read_message(control) => result.map_err(Into::into),
        _ = poll_fn(|cx| connection.poll_next_inbound(cx)) => Err(SessionError::InvalidHandshake),
    }
}

async fn write_handshake_message<T>(
    connection: &mut Connection<T>,
    control: &mut tokio_util::compat::Compat<Stream>,
    message: &Message,
) -> Result<(), SessionError>
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin,
{
    tokio::select! {
        result = write_message(control, message) => result.map_err(Into::into),
        _ = poll_fn(|cx| connection.poll_next_inbound(cx)) => Err(SessionError::InvalidHandshake),
    }
}

async fn close_yamux_connection<T>(connection: &mut Connection<T>)
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin,
{
    match tokio::time::timeout(
        Duration::from_secs(2),
        poll_fn(|cx| connection.poll_close(cx)),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "failed to close agent connection")
        }
        Err(_) => tracing::warn!("timed out closing agent connection"),
    }
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
