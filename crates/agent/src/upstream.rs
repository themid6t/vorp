use std::io;

use bytes::Bytes;
use futures::stream;
use http::{HeaderName, HeaderValue, Method, Request, Uri};
use http_body_util::{BodyExt, StreamBody};
use hyper::{body::Frame, client::conn::http1};
use hyper_util::rt::TokioIo;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, lookup_host},
};
use tokio_util::sync::CancellationToken;
use vorp_protocol::{
    BodySequence, Header, Message, RequestHead, ResponseHead, read_message, write_message,
};

use crate::AgentConfig;
use url::Host;

#[derive(Debug, thiserror::Error)]
pub(crate) enum UpstreamError {
    #[error("request stream protocol: {0}")]
    Protocol(#[from] vorp_protocol::CodecError),
    #[error("upstream I/O: {0}")]
    Io(#[from] io::Error),
    #[error("invalid HTTP request: {0}")]
    Http(String),
    #[error("upstream connection: {0}")]
    Connection(String),
}

pub(crate) async fn handle_stream<S>(
    mut stream: S,
    config: &AgentConfig,
    cancel: CancellationToken,
) -> Result<(), UpstreamError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let head = match read_message(&mut stream).await? {
        Message::RequestHead(head) => head,
        _ => {
            return Err(UpstreamError::Http(
                "request stream did not begin with RequestHead".into(),
            ));
        }
    };
    handle_request(stream, head, config, cancel).await
}

async fn handle_request<S>(
    mut stream: S,
    head: RequestHead,
    config: &AgentConfig,
    cancel: CancellationToken,
) -> Result<(), UpstreamError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let url = config
        .upstream_url()
        .map_err(|err| UpstreamError::Http(err.to_string()))?;
    let host = match url
        .host()
        .ok_or_else(|| UpstreamError::Http("upstream host missing".into()))?
    {
        Host::Domain(value) => value.to_string(),
        Host::Ipv4(value) => value.to_string(),
        Host::Ipv6(value) => value.to_string(),
    };
    let port = url
        .port_or_known_default()
        .ok_or_else(|| UpstreamError::Http("upstream port missing".into()))?;
    let upstream = match connect_upstream(&host, port, config.allow_remote_targets).await {
        Ok(socket) => socket,
        Err(err) => {
            tracing::warn!(error = %err, "local upstream unavailable");
            write_message(
                &mut stream,
                &Message::ResponseHead(ResponseHead {
                    status: 502,
                    headers: vec![],
                    content_length: Some(0),
                }),
            )
            .await?;
            write_message(&mut stream, &Message::BodyEnd).await?;
            return Ok(());
        }
    };
    if is_websocket(&head) {
        return websocket(stream, upstream, head, cancel).await;
    }
    let (reader, mut writer) = tokio::io::split(stream);
    let body_stream = stream::try_unfold(
        (reader, BodySequence::default()),
        |(mut reader, mut sequence)| async move {
            loop {
                let frame = read_message(&mut reader).await.map_err(codec_io)?;
                sequence.accept(&frame).map_err(codec_io)?;
                match frame {
                    Message::BodyChunk(bytes) if bytes.is_empty() => continue,
                    Message::BodyChunk(bytes) => {
                        return Ok(Some((Frame::data(Bytes::from(bytes)), (reader, sequence))));
                    }
                    Message::BodyEnd => return Ok(None),
                    Message::BodyAbort { message } => {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, message));
                    }
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected request body frame",
                        ));
                    }
                }
            }
        },
    );
    let body = StreamBody::new(body_stream);
    let mut request = Request::builder()
        .method(
            Method::from_bytes(head.method.as_bytes())
                .map_err(|err| UpstreamError::Http(format!("request method: {err}")))?,
        )
        .uri(
            Uri::try_from(head.target.as_str())
                .map_err(|err| UpstreamError::Http(format!("request target: {err}")))?,
        )
        .body(body)
        .map_err(|err| UpstreamError::Http(format!("build upstream request: {err}")))?;
    for (name, value) in &head.headers {
        let name = HeaderName::try_from(name.as_str())
            .map_err(|err| UpstreamError::Http(format!("request header name: {err}")))?;
        let value = HeaderValue::try_from(value.as_str())
            .map_err(|err| UpstreamError::Http(format!("request header value: {err}")))?;
        request.headers_mut().append(name, value);
    }
    if let Some(length) = head.content_length {
        request.headers_mut().insert(
            http::header::CONTENT_LENGTH,
            HeaderValue::try_from(length.to_string())
                .map_err(|err| UpstreamError::Http(format!("content length: {err}")))?,
        );
    }
    let (mut sender, connection) = http1::handshake(TokioIo::new(upstream))
        .await
        .map_err(|err| UpstreamError::Connection(format!("upstream HTTP handshake: {err}")))?;
    let driver_cancel = cancel.child_token();
    let driver = tokio::spawn(async move {
        tokio::select! {
            _ = driver_cancel.cancelled() => {},
            result = connection => if let Err(err) = result { tracing::warn!(error = %err, "upstream HTTP connection ended"); },
        }
    });
    let response = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        response = sender.send_request(request) => response.map_err(|err| UpstreamError::Connection(format!("upstream request: {err}")))?,
    };
    let status = response.status().as_u16();
    let headers = response_headers(response.headers())?;
    let content_length = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    write_message(
        &mut writer,
        &Message::ResponseHead(ResponseHead {
            status,
            headers,
            content_length,
        }),
    )
    .await?;
    let mut body = response.into_body();
    while let Some(next) =
        tokio::select! { _ = cancel.cancelled() => return Ok(()), next = body.frame() => next }
    {
        match next {
            Ok(frame) => {
                if let Ok(data) = frame.into_data() {
                    for chunk in data.chunks(vorp_protocol::MAX_FRAME_PAYLOAD) {
                        write_message(&mut writer, &Message::BodyChunk(chunk.to_vec())).await?;
                    }
                }
            }
            Err(err) => {
                write_message(
                    &mut writer,
                    &Message::BodyAbort {
                        message: format!("upstream body failed: {err}"),
                    },
                )
                .await?;
                driver.abort();
                let _ = driver.await; // Connection driver is no longer useful after body failure.
                return Ok(());
            }
        }
    }
    write_message(&mut writer, &Message::BodyEnd).await?;
    driver.abort();
    let _ = driver.await; // Keepalive is not reused across request streams.
    Ok(())
}

async fn connect_upstream(host: &str, port: u16, allow_remote: bool) -> io::Result<TcpStream> {
    let addresses = lookup_host((host, port)).await?;
    let mut last_error = None;
    for address in addresses {
        if !allow_remote && !address.ip().is_loopback() {
            continue;
        }
        match TcpStream::connect(address).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "upstream did not resolve to a loopback address",
        )
    }))
}

fn is_websocket(head: &RequestHead) -> bool {
    let upgrade = head.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("upgrade") && value.eq_ignore_ascii_case("websocket")
    });
    let connection = head.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("connection")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    });
    upgrade && connection
}

async fn websocket<S>(
    mut stream: S,
    mut upstream: TcpStream,
    head: RequestHead,
    cancel: CancellationToken,
) -> Result<(), UpstreamError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let method = Method::from_bytes(head.method.as_bytes())
        .map_err(|err| UpstreamError::Http(format!("websocket method: {err}")))?;
    let target = Uri::try_from(head.target.as_str())
        .map_err(|err| UpstreamError::Http(format!("websocket target: {err}")))?;
    let first_line = format!("{} {} HTTP/1.1\r\n", method, target);
    upstream.write_all(first_line.as_bytes()).await?;
    for (name, value) in &head.headers {
        let name = HeaderName::try_from(name.as_str())
            .map_err(|err| UpstreamError::Http(format!("websocket header name: {err}")))?;
        let value = HeaderValue::try_from(value.as_str())
            .map_err(|err| UpstreamError::Http(format!("websocket header value: {err}")))?;
        upstream.write_all(name.as_str().as_bytes()).await?;
        upstream.write_all(b": ").await?;
        upstream.write_all(value.as_bytes()).await?;
        upstream.write_all(b"\r\n").await?;
    }
    upstream.write_all(b"\r\n").await?;
    match read_message(&mut stream).await? {
        Message::BodyEnd => {}
        _ => {
            return Err(UpstreamError::Http(
                "websocket handshake must have an empty body".into(),
            ));
        }
    }
    let mut raw_head = Vec::with_capacity(1024);
    loop {
        if raw_head.len() == vorp_protocol::MAX_FRAME_PAYLOAD {
            return Err(UpstreamError::Http(
                "upstream websocket response headers exceed 64 KiB".into(),
            ));
        }
        let mut byte = [0_u8; 1];
        upstream.read_exact(&mut byte).await?;
        raw_head.push(byte[0]);
        if raw_head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let mut slots = [httparse::EMPTY_HEADER; 128];
    let mut response = httparse::Response::new(&mut slots);
    response
        .parse(&raw_head)
        .map_err(|err| UpstreamError::Http(format!("parse websocket response: {err}")))?;
    let status = response
        .code
        .ok_or_else(|| UpstreamError::Http("websocket response has no status".into()))?;
    if status != 101 {
        return tokio::select! {
            _ = cancel.cancelled() => Ok(()),
            result = forward_websocket_rejection(&mut stream, &mut upstream, &head, status, response.headers) => result,
        };
    }
    let headers = response
        .headers
        .iter()
        .map(|header| {
            let value = std::str::from_utf8(header.value)
                .map_err(|_| UpstreamError::Http("non-UTF-8 websocket response header".into()))?;
            Ok((header.name.to_string(), value.to_string()))
        })
        .collect::<Result<Vec<_>, UpstreamError>>()?;
    write_message(
        &mut stream,
        &Message::ResponseHead(ResponseHead {
            status,
            headers,
            content_length: None,
        }),
    )
    .await?;
    tokio::select! {
        _ = cancel.cancelled() => Ok(()),
        result = tokio::io::copy_bidirectional(&mut stream, &mut upstream) => result.map(|_| ()).map_err(UpstreamError::Io),
    }
}

#[derive(Debug, Clone, Copy)]
enum ResponseBodyFraming {
    Empty,
    Length(u64),
    Chunked,
    UntilEof,
}

fn websocket_rejection_headers(
    headers: &[httparse::Header<'_>],
    status: u16,
    request_method: &str,
) -> Result<(Vec<Header>, ResponseBodyFraming), UpstreamError> {
    let mut map = http::HeaderMap::new();
    let mut length = None;
    let mut transfer = None;
    for header in headers {
        let name = HeaderName::try_from(header.name)
            .map_err(|err| UpstreamError::Http(format!("upstream response header name: {err}")))?;
        let value = HeaderValue::from_bytes(header.value)
            .map_err(|err| UpstreamError::Http(format!("upstream response header value: {err}")))?;
        if name == http::header::CONTENT_LENGTH {
            let raw = value
                .to_str()
                .map_err(|_| UpstreamError::Http("non-UTF-8 Content-Length".into()))?;
            let parsed = raw
                .parse::<u64>()
                .map_err(|_| UpstreamError::Http("invalid Content-Length".into()))?;
            if length.is_some_and(|existing| existing != parsed) {
                return Err(UpstreamError::Http(
                    "conflicting Content-Length values".into(),
                ));
            }
            length = Some(parsed);
        }
        if name == http::header::TRANSFER_ENCODING {
            if transfer.is_some()
                || value
                    .to_str()
                    .map(|s| !s.eq_ignore_ascii_case("chunked"))
                    .unwrap_or(true)
            {
                return Err(UpstreamError::Http("unsupported Transfer-Encoding".into()));
            }
            transfer = Some(());
        }
        map.append(name, value);
    }
    if length.is_some() && transfer.is_some() {
        return Err(UpstreamError::Http(
            "ambiguous upstream response framing".into(),
        ));
    }
    let framing = if request_method.eq_ignore_ascii_case("HEAD")
        || status == 204
        || status == 304
        || (100..200).contains(&status)
    {
        ResponseBodyFraming::Empty
    } else if transfer.is_some() {
        ResponseBodyFraming::Chunked
    } else if let Some(length) = length {
        ResponseBodyFraming::Length(length)
    } else {
        ResponseBodyFraming::UntilEof
    };
    Ok((response_headers(&map)?, framing))
}

async fn forward_websocket_rejection<S>(
    stream: &mut S,
    upstream: &mut TcpStream,
    request: &RequestHead,
    status: u16,
    raw_headers: &[httparse::Header<'_>],
) -> Result<(), UpstreamError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (headers, framing) = websocket_rejection_headers(raw_headers, status, &request.method)?;
    let content_length = match framing {
        ResponseBodyFraming::Length(length) => Some(length),
        ResponseBodyFraming::Empty => Some(0),
        ResponseBodyFraming::Chunked | ResponseBodyFraming::UntilEof => None,
    };
    write_message(
        stream,
        &Message::ResponseHead(ResponseHead {
            status,
            headers,
            content_length,
        }),
    )
    .await?;
    if let Err(err) = forward_response_body(stream, upstream, framing).await {
        write_message(
            stream,
            &Message::BodyAbort {
                message: format!("upstream response body failed: {err}"),
            },
        )
        .await?;
        return Ok(());
    }
    write_message(stream, &Message::BodyEnd).await?;
    Ok(())
}

async fn forward_response_body<S>(
    stream: &mut S,
    upstream: &mut TcpStream,
    framing: ResponseBodyFraming,
) -> Result<(), UpstreamError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buffer = [0u8; 16 * 1024];
    match framing {
        ResponseBodyFraming::Empty => {}
        ResponseBodyFraming::Length(mut left) => {
            while left > 0 {
                let take = usize::try_from(left.min(buffer.len() as u64))
                    .map_err(|_| UpstreamError::Http("body chunk length overflow".into()))?;
                upstream.read_exact(&mut buffer[..take]).await?;
                write_message(stream, &Message::BodyChunk(buffer[..take].to_vec())).await?;
                left -= take as u64;
            }
        }
        ResponseBodyFraming::Chunked => loop {
            let line = read_crlf_line(upstream).await?;
            let hex = line
                .split(|b| *b == b';')
                .next()
                .ok_or_else(|| UpstreamError::Http("empty chunk length".into()))?;
            let hex = std::str::from_utf8(hex)
                .map_err(|_| UpstreamError::Http("invalid chunk length".into()))?;
            let mut left = u64::from_str_radix(hex.trim(), 16)
                .map_err(|_| UpstreamError::Http("invalid chunk length".into()))?;
            if left == 0 {
                while !read_crlf_line(upstream).await?.is_empty() {}
                break;
            }
            while left > 0 {
                let take = usize::try_from(left.min(buffer.len() as u64))
                    .map_err(|_| UpstreamError::Http("chunk length overflow".into()))?;
                upstream.read_exact(&mut buffer[..take]).await?;
                write_message(stream, &Message::BodyChunk(buffer[..take].to_vec())).await?;
                left -= take as u64;
            }
            let mut end = [0u8; 2];
            upstream.read_exact(&mut end).await?;
            if end != *b"\r\n" {
                return Err(UpstreamError::Http("malformed chunk terminator".into()));
            }
        },
        ResponseBodyFraming::UntilEof => loop {
            let read = upstream.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            write_message(stream, &Message::BodyChunk(buffer[..read].to_vec())).await?;
        },
    }
    Ok(())
}

async fn read_crlf_line(stream: &mut TcpStream) -> Result<Vec<u8>, UpstreamError> {
    let mut line = Vec::new();
    loop {
        if line.len() >= 8192 {
            return Err(UpstreamError::Http("chunk line too long".into()));
        }
        let byte = stream.read_u8().await?;
        line.push(byte);
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
            return Ok(line);
        }
    }
}

fn codec_io(err: vorp_protocol::CodecError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

fn response_headers(headers: &http::HeaderMap) -> Result<Vec<Header>, UpstreamError> {
    let mut forbidden = vec![
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
    ];
    for value in headers.get_all(http::header::CONNECTION) {
        let value = value
            .to_str()
            .map_err(|_| UpstreamError::Http("non-UTF-8 Connection header".into()))?;
        forbidden.extend(value.split(',').map(str::trim));
    }
    headers
        .iter()
        .filter(|(name, _)| {
            !forbidden
                .iter()
                .any(|item| name.as_str().eq_ignore_ascii_case(item))
        })
        .map(|(name, value)| {
            let value = value
                .to_str()
                .map_err(|_| UpstreamError::Http("non-UTF-8 upstream response header".into()))?;
            Ok((name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{net::TcpListener, time::timeout};

    fn test_config(port: u16) -> AgentConfig {
        AgentConfig {
            relay_host: "localhost".into(),
            relay_addr: "127.0.0.1:443".parse().expect("address"),
            token: "not-used".into(),
            upstream: format!("http://127.0.0.1:{port}"),
            requested_subdomains: vec![None],
            allow_remote_targets: false,
            ca_cert: None,
        }
    }
    fn websocket_head() -> RequestHead {
        RequestHead {
            subdomain: "test".into(),
            method: "GET".into(),
            target: "/socket".into(),
            headers: vec![
                ("host".into(), "test.localhost".into()),
                ("connection".into(), "Upgrade".into()),
                ("upgrade".into(), "websocket".into()),
                (
                    "sec-websocket-key".into(),
                    "dGhlIHNhbXBsZSBub25jZQ==".into(),
                ),
                ("sec-websocket-version".into(), "13".into()),
            ],
            content_length: None,
        }
    }
    async fn consume_request_head(socket: &mut TcpStream) {
        let mut tail = [0u8; 4];
        loop {
            let byte = socket.read_u8().await.expect("request byte");
            tail.rotate_left(1);
            tail[3] = byte;
            if tail == *b"\r\n\r\n" {
                break;
            }
        }
    }

    #[tokio::test]
    async fn websocket_rejection_preserves_status_headers_and_streams_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let port = listener.local_addr().expect("addr").port();
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            consume_request_head(&mut socket).await;
            socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\nContent-Type: text/plain\r\nConnection: close, x-secret\r\nX-Secret: hidden\r\n\r\nforbidden").await.expect("response");
        });
        let (mut relay, agent) = tokio::io::duplex(8192);
        let config = test_config(port);
        let cancel = CancellationToken::new();
        let agent_task = tokio::spawn(async move { handle_stream(agent, &config, cancel).await });
        write_message(&mut relay, &Message::RequestHead(websocket_head()))
            .await
            .expect("head");
        write_message(&mut relay, &Message::BodyEnd)
            .await
            .expect("end");
        let response = timeout(Duration::from_secs(3), read_message(&mut relay))
            .await
            .expect("timeout")
            .expect("response head");
        assert_eq!(
            response,
            Message::ResponseHead(ResponseHead {
                status: 403,
                headers: vec![("content-type".into(), "text/plain".into())],
                content_length: Some(9)
            })
        );
        assert_eq!(
            read_message(&mut relay).await.expect("body"),
            Message::BodyChunk(b"forbidden".to_vec())
        );
        assert_eq!(
            read_message(&mut relay).await.expect("body end"),
            Message::BodyEnd
        );
        assert!(agent_task.await.expect("agent task").is_ok());
        upstream_task.await.expect("upstream task");
    }

    #[tokio::test]
    async fn websocket_101_still_switches_to_raw_copy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
        let port = listener.local_addr().expect("addr").port();
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            consume_request_head(&mut socket).await;
            socket.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n").await.expect("response");
            let mut raw = [0u8; 4];
            socket.read_exact(&mut raw).await.expect("raw");
            assert_eq!(&raw, b"ping");
            socket.write_all(b"pong").await.expect("echo");
        });
        let (mut relay, agent) = tokio::io::duplex(8192);
        let config = test_config(port);
        let cancel = CancellationToken::new();
        let agent_cancel = cancel.clone();
        let agent_task =
            tokio::spawn(async move { handle_stream(agent, &config, agent_cancel).await });
        write_message(&mut relay, &Message::RequestHead(websocket_head()))
            .await
            .expect("head");
        write_message(&mut relay, &Message::BodyEnd)
            .await
            .expect("end");
        let response = timeout(Duration::from_secs(3), read_message(&mut relay))
            .await
            .expect("timeout")
            .expect("head");
        assert!(matches!(
            response,
            Message::ResponseHead(ResponseHead { status: 101, .. })
        ));
        relay.write_all(b"ping").await.expect("send raw");
        let mut raw = [0u8; 4];
        relay.read_exact(&mut raw).await.expect("read raw");
        assert_eq!(&raw, b"pong");
        cancel.cancel();
        assert!(agent_task.await.expect("agent task").is_ok());
        upstream_task.await.expect("upstream task");
    }

    #[tokio::test]
    async fn websocket_rejection_streams_chunked_and_close_delimited_bodies() {
        for (response,expected) in [
            (b"HTTP/1.1 403 Forbidden\r\nTransfer-Encoding: chunked\r\nContent-Type: text/plain\r\n\r\n4\r\nfail\r\n3\r\nure\r\n0\r\nX-Trailer: value\r\n\r\n".as_slice(),vec![b"fail".to_vec(),b"ure".to_vec()]),
            (b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nforbidden".as_slice(),vec![b"forbidden".to_vec()]),
        ] {
            let listener=TcpListener::bind("127.0.0.1:0").await.expect("listen");let port=listener.local_addr().expect("addr").port();
            let upstream_task=tokio::spawn(async move {let(mut socket,_)=listener.accept().await.expect("accept");consume_request_head(&mut socket).await;socket.write_all(response).await.expect("response");});
            let(mut relay,agent)=tokio::io::duplex(8192);let config=test_config(port);let cancel=CancellationToken::new();
            let agent_task=tokio::spawn(async move{handle_stream(agent,&config,cancel).await});
            write_message(&mut relay,&Message::RequestHead(websocket_head())).await.expect("head");write_message(&mut relay,&Message::BodyEnd).await.expect("end");
            assert!(matches!(read_message(&mut relay).await.expect("response"),Message::ResponseHead(ResponseHead{status:403,content_length:None,..})));
            for bytes in expected {assert_eq!(read_message(&mut relay).await.expect("chunk"),Message::BodyChunk(bytes));}
            assert_eq!(read_message(&mut relay).await.expect("end"),Message::BodyEnd);
            assert!(agent_task.await.expect("agent task").is_ok());upstream_task.await.expect("upstream task");
        }
    }

    #[test]
    fn websocket_rejection_rejects_ambiguous_framing() {
        let headers = [
            httparse::Header {
                name: "Content-Length",
                value: b"4",
            },
            httparse::Header {
                name: "Transfer-Encoding",
                value: b"chunked",
            },
        ];
        assert!(matches!(
            websocket_rejection_headers(&headers, 403, "GET"),
            Err(UpstreamError::Http(_))
        ));
        let headers = [
            httparse::Header {
                name: "Content-Length",
                value: b"4",
            },
            httparse::Header {
                name: "Content-Length",
                value: b"5",
            },
        ];
        assert!(matches!(
            websocket_rejection_headers(&headers, 403, "GET"),
            Err(UpstreamError::Http(_))
        ));
    }

    #[tokio::test]
    async fn response_streams_before_upload_ends() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            loop {
                let mut byte = [0_u8; 1];
                socket.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        let config = AgentConfig {
            relay_host: "localhost".into(),
            relay_addr: "127.0.0.1:443".parse().unwrap(),
            token: "not-used".into(),
            upstream: format!("http://127.0.0.1:{}", address.port()),
            requested_subdomains: vec![None],
            allow_remote_targets: false,
            ca_cert: None,
        };
        let (mut relay, agent) = tokio::io::duplex(8192);
        let cancel = CancellationToken::new();
        let agent_task = tokio::spawn(async move { handle_stream(agent, &config, cancel).await });
        write_message(
            &mut relay,
            &Message::RequestHead(RequestHead {
                subdomain: "test".into(),
                method: "POST".into(),
                target: "/upload".into(),
                headers: vec![("host".into(), "test.localhost".into())],
                content_length: None,
            }),
        )
        .await
        .unwrap();
        let response = timeout(Duration::from_secs(3), read_message(&mut relay))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            response,
            Message::ResponseHead(ResponseHead {
                status: 200,
                content_length: Some(2),
                ..
            })
        ));
        assert_eq!(
            read_message(&mut relay).await.unwrap(),
            Message::BodyChunk(b"ok".to_vec())
        );
        assert_eq!(read_message(&mut relay).await.unwrap(), Message::BodyEnd);
        assert!(agent_task.await.unwrap().is_ok());
        upstream_task.await.unwrap();
    }

    #[test]
    fn response_hop_headers_are_removed() {
        let mut headers = http::HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("x-internal"));
        headers.insert("x-internal", HeaderValue::from_static("secret"));
        headers.insert("content-type", HeaderValue::from_static("text/plain"));
        assert_eq!(
            response_headers(&headers).unwrap(),
            vec![("content-type".into(), "text/plain".into())]
        );
    }
}
