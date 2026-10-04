use std::{
    collections::HashSet,
    future::poll_fn,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::channel::oneshot;
use sha2::{Digest, Sha256};
use tokio::{net::TcpStream, sync::mpsc, task::JoinSet, time};
use tokio_rustls::{
    TlsConnector,
    rustls::{
        self, RootCertStore,
        pki_types::{CertificateDer, ServerName, pem::PemObject},
    },
};
use tokio_util::{
    compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt},
    sync::CancellationToken,
};
use vorp_protocol::{
    AGENT_ALPN, CloseReason, ErrorCode, Message, PROTOCOL_VERSION, read_message, write_message,
};
use yamux::{Config, Connection, Mode, Stream};

use crate::{AgentConfig, AgentError, upstream};

type OpenRequest = oneshot::Sender<Result<Stream, String>>;
const SESSION_BACKOFF: [Duration; 7] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// Upper bound of the random wait before redialing after a live session ends.
/// Without it every agent of a restarted relay redials at once, and on a small
/// host that herd re-exhausted memory and crashed the relay in a loop.
const RECONNECT_SPREAD: Duration = Duration::from_secs(5);
const TUNNEL_BACKOFF: [Duration; 5] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseAction {
    Retry,
    StopTunnel,
    StopAgent,
}

fn close_action(reason: CloseReason) -> CloseAction {
    match reason {
        CloseReason::Recoverable => CloseAction::Retry,
        CloseReason::Forced | CloseReason::Expired => CloseAction::StopTunnel,
        CloseReason::Revoked => CloseAction::StopAgent,
    }
}

/// Keep reconnecting after transient failures until cancellation or a permanent rejection.
pub async fn run_until(config: AgentConfig, shutdown: CancellationToken) -> Result<(), AgentError> {
    config.validate()?;
    let machine_id = machine_id()?;
    let mut failed_attempts = 0_usize;
    let suppressed = Arc::new(Mutex::new(HashSet::<usize>::new()));
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        if all_tunnels_suppressed(&suppressed, config.requested_subdomains.len()) {
            return Err(AgentError::TunnelsClosed);
        }
        let mut was_connected = false;
        match run_session(
            &config,
            &machine_id,
            shutdown.child_token(),
            &mut was_connected,
            Arc::clone(&suppressed),
        )
        .await
        {
            Ok(()) if shutdown.is_cancelled() => return Ok(()),
            Ok(()) if all_tunnels_suppressed(&suppressed, config.requested_subdomains.len()) => {
                return Err(AgentError::TunnelsClosed);
            }
            Ok(()) => {
                failed_attempts = 0;
                if !sleep_or_shutdown(random_up_to(RECONNECT_SPREAD), &shutdown).await {
                    return Ok(());
                }
            }
            Err(AgentError::Authentication) => return Err(AgentError::Authentication),
            Err(AgentError::UnsupportedVersion) => return Err(AgentError::UnsupportedVersion),
            Err(err) => {
                if all_tunnels_suppressed(&suppressed, config.requested_subdomains.len()) {
                    return Err(AgentError::TunnelsClosed);
                }
                tracing::warn!(error = %err, "agent session disconnected");
                let delay = if was_connected {
                    failed_attempts = 0;
                    random_up_to(RECONNECT_SPREAD)
                } else {
                    let base = SESSION_BACKOFF[failed_attempts.min(SESSION_BACKOFF.len() - 1)];
                    failed_attempts = failed_attempts.saturating_add(1);
                    base / 2 + random_up_to(base / 2)
                };
                if !sleep_or_shutdown(delay, &shutdown).await {
                    return Ok(());
                }
            }
        }
    }
}

/// Returns false when shutdown arrived first.
async fn sleep_or_shutdown(delay: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = shutdown.cancelled() => false,
        _ = time::sleep(delay) => true,
    }
}

/// Uniform in `[0, max]`. Spreading reconnects needs no CSPRNG; std's
/// per-instance random hasher keys are enough.
fn random_up_to(max: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let sample = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    Duration::from_nanos(sample % nanos.saturating_add(1))
}

fn all_tunnels_suppressed(suppressed: &Mutex<HashSet<usize>>, total: usize) -> bool {
    total != 0
        && suppressed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            >= total
}

fn machine_id() -> Result<String, AgentError> {
    let hostname = hostname::get()
        .map_err(|err| AgentError::Connection(format!("read machine hostname: {err}")))?;
    let mac = mac_address::get_mac_address()
        .map_err(|err| AgentError::Connection(format!("read machine MAC address: {err}")))?
        .ok_or_else(|| AgentError::Connection("no machine MAC address found".into()))?;
    // The process nonce keeps two agents on one host and account from
    // displacing each other's session; reconnects reuse it, so a process still
    // replaces its own stale session.
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut hasher = Sha256::new();
    hasher.update(hostname.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(mac.to_string().as_bytes());
    hasher.update(format!(":{}:{started}", std::process::id()).as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

async fn connect(
    config: &AgentConfig,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, AgentError> {
    let mut roots = RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        roots
            .add(cert)
            .map_err(|err| AgentError::Connection(format!("load native CA: {err}")))?;
    }
    if let Some(path) = &config.ca_cert {
        add_ca_cert(&mut roots, path)?;
    }
    let mut tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![AGENT_ALPN.to_vec()];
    let name = ServerName::try_from(config.relay_host.clone())
        .map_err(|err| AgentError::Connection(format!("invalid relay host: {err}")))?;
    let tcp = match config.relay_addr {
        Some(addr) => TcpStream::connect(addr).await,
        None => TcpStream::connect((config.relay_host.as_str(), 443)).await,
    }
    .map_err(|err| AgentError::Connection(format!("dial relay: {err}")))?;
    let tls = TlsConnector::from(Arc::new(tls_config))
        .connect(name, tcp)
        .await
        .map_err(|err| AgentError::Connection(format!("TLS handshake: {err}")))?;
    if tls.get_ref().1.alpn_protocol() != Some(AGENT_ALPN) {
        return Err(AgentError::Connection(
            "relay did not negotiate vorp-agent/1 ALPN".into(),
        ));
    }
    Ok(tls)
}

fn add_ca_cert(roots: &mut RootCertStore, path: &Path) -> Result<(), AgentError> {
    let bytes = std::fs::read(path).map_err(|err| {
        AgentError::Connection(format!("open CA certificate {}: {err}", path.display()))
    })?;
    let mut count = 0;
    for cert in CertificateDer::pem_slice_iter(&bytes) {
        let cert =
            cert.map_err(|err| AgentError::Connection(format!("parse CA certificate: {err}")))?;
        roots
            .add(cert)
            .map_err(|err| AgentError::Connection(format!("add CA certificate: {err}")))?;
        count += 1;
    }
    if count == 0 {
        return Err(AgentError::Connection(
            "CA certificate file contained no PEM certificates".into(),
        ));
    }
    Ok(())
}

async fn run_session(
    config: &AgentConfig,
    machine_id: &str,
    cancel: CancellationToken,
    was_connected: &mut bool,
    suppressed: Arc<Mutex<HashSet<usize>>>,
) -> Result<(), AgentError> {
    let tls = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = connect(config) => result?,
    };
    let connection = Connection::new(tls.compat(), Config::default(), Mode::Client);
    let (open_tx, open_rx) = mpsc::channel::<OpenRequest>(32);
    let mut actor = tokio::spawn(connection_actor(
        connection,
        open_rx,
        config.clone(),
        cancel.child_token(),
    ));
    let result = session_work(
        config,
        machine_id,
        &open_tx,
        cancel.child_token(),
        &mut actor,
        was_connected,
        suppressed,
    )
    .await;
    cancel.cancel();
    if !actor.is_finished() {
        actor.abort();
        let _ = actor.await; // Actor was cancelled after session work ended.
    }
    result
}

async fn session_work(
    config: &AgentConfig,
    machine_id: &str,
    open_tx: &mpsc::Sender<OpenRequest>,
    cancel: CancellationToken,
    actor: &mut tokio::task::JoinHandle<Result<(), AgentError>>,
    was_connected: &mut bool,
    suppressed: Arc<Mutex<HashSet<usize>>>,
) -> Result<(), AgentError> {
    let mut control = tokio::select! {
        result = open_stream(open_tx) => result?,
        result = &mut *actor => return result.map_err(|err| AgentError::Connection(format!("yamux actor: {err}")))?,
        _ = cancel.cancelled() => return Ok(()),
    };
    let registration = Message::RegisterAgent {
        protocol_version: PROTOCOL_VERSION,
        token: config.token.clone(),
        machine_id: machine_id.into(),
    };
    tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = write_message(&mut control, &registration) => result.map_err(|err| AgentError::Connection(format!("register agent: {err}")))?,
    }
    let acknowledgement = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        result = time::timeout(Duration::from_secs(30), read_message(&mut control)) => result
            .map_err(|_| AgentError::Connection("agent registration timed out".into()))?
            .map_err(|err| AgentError::Connection(format!("read agent acknowledgement: {err}")))?,
    };
    match acknowledgement {
        Message::AgentAck {
            server_version: PROTOCOL_VERSION,
            ..
        } => {}
        Message::AgentAck { .. }
        | Message::AgentErr {
            code: ErrorCode::UnsupportedVersion,
            ..
        } => return Err(AgentError::UnsupportedVersion),
        Message::AgentErr {
            code: ErrorCode::AuthFailed,
            ..
        } => return Err(AgentError::Authentication),
        _ => {
            return Err(AgentError::Connection(
                "unexpected agent registration response".into(),
            ));
        }
    }
    *was_connected = true;
    tracing::info!(machine_id = %machine_id, "agent connected");
    let mut tasks = JoinSet::new();
    let heartbeat_cancel = cancel.child_token();
    tasks.spawn(async move { heartbeat(control, heartbeat_cancel).await });
    for (index, requested) in config.requested_subdomains.iter().cloned().enumerate() {
        if suppressed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&index)
        {
            continue;
        }
        let tx = open_tx.clone();
        let hint = Some(config.upstream.clone());
        let task_cancel = cancel.child_token();
        let task_suppressed = Arc::clone(&suppressed);
        tasks.spawn(async move {
            tunnel_lifecycle(tx, requested, hint, task_cancel, index, task_suppressed).await
        });
    }
    let outcome = loop {
        tokio::select! {
            result = &mut *actor => break result.map_err(|err| AgentError::Connection(format!("yamux actor: {err}")))?,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                let result = result.map_err(|err| AgentError::Connection(format!("session task: {err}")))?;
                if result.is_err() { break result; }
                if all_tunnels_suppressed(&suppressed, config.requested_subdomains.len()) { break Ok(()); }
            }
            _ = cancel.cancelled() => break Ok(()),
        }
    };
    cancel.cancel();
    while tasks.join_next().await.is_some() {}
    outcome
}

async fn connection_actor<T>(
    mut connection: Connection<T>,
    mut open_rx: mpsc::Receiver<OpenRequest>,
    config: AgentConfig,
    cancel: CancellationToken,
) -> Result<(), AgentError>
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + 'static,
{
    enum Event {
        Inbound(Option<Result<Stream, yamux::ConnectionError>>),
        Outbound(Result<Stream, yamux::ConnectionError>),
    }
    let mut requests = JoinSet::new();
    let mut pending_open = None;
    let mut open_channel_open = true;
    let outcome = loop {
        tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            reply = open_rx.recv(), if pending_open.is_none() && open_channel_open => {
                if let Some(reply) = reply { pending_open = Some(reply); }
                else { open_channel_open = false; }
            },
            event = poll_fn(|cx| {
                // A saturated outbound-open limit must not suspend inbound
                // polling and window updates for existing request streams.
                if pending_open.is_some()
                    && let std::task::Poll::Ready(opened) = connection.poll_new_outbound(cx) {
                    return std::task::Poll::Ready(Event::Outbound(opened));
                }
                connection.poll_next_inbound(cx).map(Event::Inbound)
            }) => match event {
                Event::Outbound(opened) => {
                    if let Some(reply) = pending_open.take() {
                        let _ = reply.send(opened.map_err(|error| error.to_string()));
                    }
                }
                Event::Inbound(Some(Ok(stream))) => {
                    let task_config = config.clone();
                    let task_cancel = cancel.child_token();
                    requests.spawn(async move {
                        tokio::select! {
                            _ = task_cancel.cancelled() => {},
                            result = upstream::handle_stream(stream.compat(), &task_config, task_cancel.clone()) => {
                                if let Err(err) = result { tracing::warn!(error = %err, "request stream failed"); }
                            }
                        }
                    });
                }
                Event::Inbound(Some(Err(err))) => break Err(AgentError::Connection(format!("yamux inbound: {err}"))),
                Event::Inbound(None) => break Err(AgentError::Connection("relay closed yamux session".into())),
            },
            Some(result) = requests.join_next(), if !requests.is_empty() => {
                if let Err(err) = result { tracing::warn!(error = %err, "request task failed"); }
            }
        }
    };
    cancel.cancel();
    while requests.join_next().await.is_some() {}
    outcome
}

async fn open_stream(
    tx: &mpsc::Sender<OpenRequest>,
) -> Result<tokio_util::compat::Compat<Stream>, AgentError> {
    let (reply, answer) = oneshot::channel();
    tx.send(reply)
        .await
        .map_err(|_| AgentError::Connection("yamux actor stopped".into()))?;
    let stream = answer
        .await
        .map_err(|_| AgentError::Connection("yamux actor dropped stream request".into()))?
        .map_err(|err| AgentError::Connection(format!("open yamux stream: {err}")))?;
    Ok(stream.compat())
}

async fn heartbeat(
    mut stream: tokio_util::compat::Compat<Stream>,
    cancel: CancellationToken,
) -> Result<(), AgentError> {
    let mut interval = time::interval(Duration::from_secs(20));
    interval.tick().await;
    let mut missed = 0;
    loop {
        tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = interval.tick() => {} }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| AgentError::Connection(format!("system clock before epoch: {err}")))?
            .as_millis() as i64;
        let ping = Message::Ping { timestamp_ms };
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = write_message(&mut stream, &ping) => result.map_err(|err| AgentError::Connection(format!("send heartbeat: {err}")))?,
        }
        let reply = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = time::timeout(Duration::from_secs(5), read_message(&mut stream)) => result,
        };
        match reply {
            Ok(Ok(Message::Pong {
                timestamp_ms: echoed,
            })) if echoed == timestamp_ms => missed = 0,
            Ok(Ok(_)) | Err(_) => {
                missed += 1;
                if missed >= 3 {
                    return Err(AgentError::Connection("missed three heartbeats".into()));
                }
            }
            Ok(Err(err)) => return Err(AgentError::Connection(format!("read heartbeat: {err}"))),
        }
    }
}

async fn tunnel_lifecycle(
    tx: mpsc::Sender<OpenRequest>,
    requested: Option<String>,
    hint: Option<String>,
    cancel: CancellationToken,
    index: usize,
    suppressed: Arc<Mutex<HashSet<usize>>>,
) -> Result<(), AgentError> {
    let mut retry = 0_usize;
    loop {
        let mut stream = tokio::select! { _ = cancel.cancelled() => return Ok(()), result = open_stream(&tx) => result? };
        let registration = Message::RegisterTunnel {
            subdomain: requested.clone(),
            upstream_hint: hint.clone(),
        };
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = write_message(&mut stream, &registration) => result.map_err(|err| AgentError::Connection(format!("register tunnel: {err}")))?,
        }
        let acknowledgement = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = read_message(&mut stream) => result.map_err(|err| AgentError::Connection(format!("read tunnel acknowledgement: {err}")))?,
        };
        match acknowledgement {
            Message::TunnelAck { subdomain, url } => {
                tracing::info!(subdomain = %subdomain, url = %url, "tunnel registered");
                let frame = tokio::select! { _ = cancel.cancelled() => return Ok(()), frame = read_message(&mut stream) => frame };
                match frame {
                    Ok(Message::TunnelClose {
                        subdomain: closed,
                        reason,
                    }) if closed == subdomain => {
                        let ack = Message::TunnelCloseAck { subdomain: closed };
                        let acknowledged = tokio::select! {
                            _ = cancel.cancelled() => return Ok(()),
                            result = write_message(&mut stream, &ack) => result,
                        };
                        if let Err(error) = &acknowledged {
                            tracing::warn!(error = %error, "tunnel close acknowledgement failed");
                        }
                        match close_action(reason) {
                            CloseAction::StopAgent => return Err(AgentError::Authentication),
                            CloseAction::StopTunnel => {
                                tracing::warn!(subdomain = %subdomain, reason = ?reason, "relay closed tunnel permanently");
                                suppressed
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .insert(index);
                                return Ok(());
                            }
                            CloseAction::Retry => acknowledged.map_err(|error| {
                                AgentError::Connection(format!("acknowledge tunnel close: {error}"))
                            })?,
                        }
                    }
                    Ok(_) => return Err(AgentError::Connection("unexpected tunnel frame".into())),
                    Err(err) => {
                        return Err(AgentError::Connection(format!(
                            "tunnel stream closed: {err}"
                        )));
                    }
                }
            }
            Message::TunnelErr {
                code: ErrorCode::TunnelLimit,
                ..
            } => {
                tracing::error!(
                    subdomain = requested.as_deref().unwrap_or(""),
                    "relay rejected tunnel: your account's tunnel limit is reached; close another tunnel or ask an admin to raise it"
                );
                suppressed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(index);
                return Ok(());
            }
            Message::TunnelErr {
                code:
                    code @ (ErrorCode::SubdomainTaken
                    | ErrorCode::SubdomainInvalid
                    | ErrorCode::SubdomainNotAllowed),
                ..
            } => {
                // The relay's message is peer-controlled text; log the code only.
                tracing::error!(
                    subdomain = requested.as_deref().unwrap_or(""),
                    code = ?code,
                    "relay rejected tunnel; reserve the name in the dashboard or omit --subdomain"
                );
                suppressed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(index);
                return Ok(());
            }
            _ => {
                return Err(AgentError::Connection(
                    "unexpected tunnel acknowledgement".into(),
                ));
            }
        }
        let delay = TUNNEL_BACKOFF[retry.min(TUNNEL_BACKOFF.len() - 1)];
        retry = retry.saturating_add(1);
        tokio::select! { _ = cancel.cancelled() => return Ok(()), _ = time::sleep(delay) => {} }
    }
}

#[cfg(test)]
mod jitter_tests {
    use super::*;

    #[test]
    fn random_up_to_stays_in_range_and_varies() {
        let max = Duration::from_secs(5);
        let samples: HashSet<Duration> = (0..64).map(|_| random_up_to(max)).collect();
        assert!(samples.iter().all(|d| *d <= max));
        assert!(
            samples.len() > 32,
            "reconnect delays must be spread, not constant"
        );
        assert_eq!(random_up_to(Duration::ZERO), Duration::ZERO);
    }
}

#[cfg(test)]
mod close_tests {
    use super::{CloseAction, close_action};
    use vorp_protocol::CloseReason;

    #[test]
    fn close_reason_decides_reconnect() {
        let cases = [
            (CloseReason::Recoverable, CloseAction::Retry),
            (CloseReason::Forced, CloseAction::StopTunnel),
            (CloseReason::Expired, CloseAction::StopTunnel),
            (CloseReason::Revoked, CloseAction::StopAgent),
        ];
        for (reason, expected) in cases {
            assert_eq!(close_action(reason), expected);
        }
    }
}
