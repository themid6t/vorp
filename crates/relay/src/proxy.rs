use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use tokio::{
    io::AsyncWrite,
    sync::{mpsc, oneshot},
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::{compat::FuturesAsyncReadCompatExt, sync::CancellationToken};
use vorp_protocol::{
    MAX_FRAME_PAYLOAD, Message, RequestHead, ResponseHead, read_message, write_message,
};

use crate::{Relay, State, httpnorm, registry::TunnelPermit, server::status};

type RelayStream = tokio_util::compat::Compat<yamux::Stream>;
type BodySender = mpsc::Sender<Result<Bytes, io::Error>>;

struct TrafficMeta {
    state: Arc<State>,
    user_id: i64,
    subdomain: String,
    method: String,
    timestamp_ms: i64,
}

impl TrafficMeta {
    fn record(self, status: u16, bytes_in: u64, bytes_out: u64) {
        self.state.registry.record_traffic(
            self.user_id,
            vorp_web::TrafficEvent {
                subdomain: self.subdomain,
                timestamp_ms: self.timestamp_ms,
                method: self.method,
                status,
                bytes_in,
                bytes_out,
            },
        );
    }
}

impl Relay {
    pub(crate) async fn proxy(
        &self,
        mut request: Request<Incoming>,
        peer: SocketAddr,
        subdomain: &str,
        host: &str,
    ) -> Response<Body> {
        let Some(tunnel) = self.state.registry.tunnel(subdomain) else {
            return status(StatusCode::BAD_GATEWAY);
        };
        let Some(permit) = tunnel.try_acquire() else {
            return status(StatusCode::SERVICE_UNAVAILABLE);
        };
        let content_length = match httpnorm::validate_framing(request.headers()) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "rejected ambiguous request framing");
                return status(StatusCode::BAD_REQUEST);
            }
        };
        let websocket =
            match httpnorm::normalize_request_headers(request.headers_mut(), host, peer.ip()) {
                Ok(value) => value,
                Err(_) => return status(StatusCode::BAD_REQUEST),
            };
        let headers = match wire_headers(request.headers()) {
            Ok(value) => value,
            Err(_) => return status(StatusCode::BAD_REQUEST),
        };
        let head = RequestHead {
            subdomain: subdomain.into(),
            method: request.method().to_string(),
            target: request
                .uri()
                .path_and_query()
                .map_or("/", |target| target.as_str())
                .into(),
            headers,
            content_length,
        };
        let traffic = TrafficMeta {
            state: Arc::clone(&self.state),
            user_id: tunnel.session.user_id,
            subdomain: subdomain.into(),
            method: head.method.clone(),
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis().min(i64::MAX as u128) as i64),
        };
        match serde_json::to_vec(&head) {
            Ok(bytes) if bytes.len() > MAX_FRAME_PAYLOAD => {
                return status(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
            }
            Err(_) => return status(StatusCode::BAD_REQUEST),
            _ => {}
        }
        let (reply, receive) = oneshot::channel();
        if tunnel.session.open.send(reply).await.is_err() {
            return status(StatusCode::BAD_GATEWAY);
        }
        let stream = match tokio::time::timeout(Duration::from_secs(30), receive).await {
            Ok(Ok(Ok(stream))) => stream.compat(),
            _ => return status(StatusCode::BAD_GATEWAY),
        };
        if websocket {
            return websocket_exchange(
                request,
                stream,
                head,
                permit,
                tunnel.session.cancel.clone(),
                traffic,
            )
            .await;
        }
        streaming_exchange(
            request.into_body(),
            stream,
            head,
            permit,
            tunnel.session.cancel.clone(),
            traffic,
        )
        .await
    }
}

async fn streaming_exchange(
    body: Incoming,
    mut stream: RelayStream,
    head: RequestHead,
    permit: TunnelPermit,
    cancel: CancellationToken,
    traffic: TrafficMeta,
) -> Response<Body> {
    if write_message(&mut stream, &Message::RequestHead(head))
        .await
        .is_err()
    {
        return status(StatusCode::BAD_GATEWAY);
    }
    let (mut reader, mut writer) = tokio::io::split(stream);
    let upload_cancel = cancel.clone();
    let bytes_in = Arc::new(AtomicU64::new(0));
    let upload_bytes = Arc::clone(&bytes_in);
    let upload = tokio::spawn(async move {
        if let Err(error) = upload_body(body, &mut writer, &upload_cancel, &upload_bytes).await {
            tracing::warn!(error = %error, "request body forwarding failed");
        }
    });
    let first = tokio::select! {
        _ = cancel.cancelled() => { upload.abort(); return status(StatusCode::BAD_GATEWAY) },
        result = tokio::time::timeout(Duration::from_secs(30), read_message(&mut reader)) => result,
    };
    let head = match first {
        Ok(Ok(Message::ResponseHead(head))) => head,
        Err(_) => {
            upload.abort();
            return status(StatusCode::GATEWAY_TIMEOUT);
        }
        _ => {
            upload.abort();
            return status(StatusCode::BAD_GATEWAY);
        }
    };
    if head.status == 101 {
        upload.abort();
        return status(StatusCode::BAD_GATEWAY);
    }
    let (mut response, sender) = match build_response(&head, false) {
        Ok(value) => value,
        Err(_) => {
            upload.abort();
            return status(StatusCode::BAD_GATEWAY);
        }
    };
    tokio::spawn(async move {
        let bytes_out = pump_response(reader, sender, cancel).await;
        upload.abort();
        traffic.record(head.status, bytes_in.load(Ordering::Acquire), bytes_out);
        drop(permit);
    });
    if let Some(length) = head.content_length
        && let Ok(value) = HeaderValue::from_str(&length.to_string())
    {
        response
            .headers_mut()
            .insert(http::header::CONTENT_LENGTH, value);
    }
    response
}

async fn websocket_exchange(
    mut request: Request<Incoming>,
    mut stream: RelayStream,
    head: RequestHead,
    permit: TunnelPermit,
    cancel: CancellationToken,
    traffic: TrafficMeta,
) -> Response<Body> {
    if write_message(&mut stream, &Message::RequestHead(head))
        .await
        .is_err()
        || write_message(&mut stream, &Message::BodyEnd).await.is_err()
    {
        return status(StatusCode::BAD_GATEWAY);
    }
    let response_head =
        match tokio::time::timeout(Duration::from_secs(30), read_message(&mut stream)).await {
            Ok(Ok(Message::ResponseHead(head))) if head.status == 101 => head,
            _ => return status(StatusCode::BAD_GATEWAY),
        };
    let upgraded = hyper::upgrade::on(&mut request);
    let mut response = match build_response(&response_head, true) {
        Ok((response, _)) => response,
        Err(_) => return status(StatusCode::BAD_GATEWAY),
    };
    // A 101 carries no framed body; hyper owns the upgraded HTTP/1 connection.
    *response.body_mut() = Body::empty();
    tokio::spawn(async move {
        let Ok(upgraded) = upgraded.await else { return };
        let mut client = TokioIo::new(upgraded);
        tokio::select! {
            _ = cancel.cancelled() => {},
            result = tokio::io::copy_bidirectional(&mut client, &mut stream) => {
                match result {
                    Ok((bytes_in, bytes_out)) => traffic.record(101, bytes_in, bytes_out),
                    Err(error) => tracing::warn!(error = %error, "WebSocket tunnel ended"),
                }
            }
        }
        drop(permit);
    });
    response
}

use hyper_util::rt::TokioIo;

fn wire_headers(headers: &HeaderMap) -> Result<Vec<(String, String)>, ()> {
    headers
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
                .map_err(|_| ())
        })
        .collect()
}

fn response_headers(head: &ResponseHead, websocket: bool) -> Result<HeaderMap, ()> {
    let mut headers = HeaderMap::new();
    for (name, value) in &head.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| ())?;
        let value = HeaderValue::from_str(value).map_err(|_| ())?;
        headers.append(name, value);
    }
    httpnorm::normalize_response_headers(&mut headers, websocket);
    headers.remove(http::header::CONTENT_LENGTH);
    Ok(headers)
}

fn build_response(
    head: &ResponseHead,
    websocket: bool,
) -> Result<(Response<Body>, BodySender), ()> {
    let status = StatusCode::from_u16(head.status).map_err(|_| ())?;
    let headers = response_headers(head, websocket)?;
    let (sender, receiver) = mpsc::channel(1);
    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(ReceiverStream::new(receiver)))
        .map_err(|_| ())?;
    *response.headers_mut() = headers;
    Ok((response, sender))
}

async fn upload_body<W: AsyncWrite + Unpin>(
    mut body: Incoming,
    writer: &mut W,
    cancel: &CancellationToken,
    bytes_in: &AtomicU64,
) -> Result<(), io::Error> {
    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            result = body.frame() => result,
        };
        match frame {
            Some(Ok(frame)) => {
                if let Ok(bytes) = frame.into_data() {
                    for chunk in bytes.chunks(MAX_FRAME_PAYLOAD) {
                        write_message(writer, &Message::BodyChunk(chunk.to_vec()))
                            .await
                            .map_err(io::Error::other)?;
                        bytes_in.fetch_add(chunk.len() as u64, Ordering::Release);
                    }
                }
            }
            Some(Err(error)) => {
                write_message(
                    writer,
                    &Message::BodyAbort {
                        message: "client upload aborted".into(),
                    },
                )
                .await
                .map_err(io::Error::other)?;
                return Err(io::Error::other(error));
            }
            None => {
                write_message(writer, &Message::BodyEnd)
                    .await
                    .map_err(io::Error::other)?;
                return Ok(());
            }
        }
    }
}

async fn pump_response(
    mut reader: tokio::io::ReadHalf<RelayStream>,
    sender: BodySender,
    cancel: CancellationToken,
) -> u64 {
    let mut bytes_out = 0;
    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            value = tokio::time::timeout(Duration::from_secs(60), read_message(&mut reader)) => value,
        };
        match frame {
            Ok(Ok(Message::BodyChunk(bytes))) => {
                bytes_out += bytes.len() as u64;
                if sender.send(Ok(Bytes::from(bytes))).await.is_err() {
                    break;
                }
            }
            Ok(Ok(Message::BodyEnd)) => break,
            Ok(Ok(Message::BodyAbort { .. })) | Ok(Err(_)) | Err(_) | Ok(Ok(_)) => {
                let _ = sender
                    .send(Err(io::Error::other("upstream response truncated")))
                    .await;
                break;
            }
        }
    }
    bytes_out
}
