use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Message;

pub const MAX_FRAME_PAYLOAD: usize = 65_536;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("declared frame length {declared} exceeds {MAX_FRAME_PAYLOAD}")]
    Oversized { declared: u32 },
    #[error("unknown frame type: {0:#04x}")]
    UnknownType(u8),
    #[error("invalid frame payload: {0}")]
    InvalidPayload(String),
    #[error("unexpected frame sequence")]
    InvalidSequence,
}

/// Read exactly one frame. The implementation must reject a declared length
/// above MAX_FRAME_PAYLOAD before allocating its payload buffer.
pub async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Message, CodecError> {
    let mut header = [0_u8; 5];
    reader.read_exact(&mut header).await?;
    let declared = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    if declared as usize > MAX_FRAME_PAYLOAD {
        return Err(CodecError::Oversized { declared });
    }
    let mut payload = vec![0_u8; declared as usize];
    reader.read_exact(&mut payload).await?;
    decode(header[0], &payload)
}

/// Write exactly one frame and await the write to preserve backpressure.
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), CodecError> {
    let (kind, payload) = encode(message)?;
    let declared =
        u32::try_from(payload.len()).map_err(|_| CodecError::Oversized { declared: u32::MAX })?;
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(CodecError::Oversized { declared });
    }
    writer.write_all(&[kind]).await?;
    writer.write_all(&declared.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    serde_json::to_vec(value).map_err(|err| CodecError::InvalidPayload(err.to_string()))
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    serde_json::from_slice(bytes).map_err(|err| CodecError::InvalidPayload(err.to_string()))
}

fn encode(message: &Message) -> Result<(u8, Vec<u8>), CodecError> {
    let value = match message {
        Message::RegisterAgent {
            protocol_version,
            token,
            machine_id,
        } => {
            return Ok((
                0x01,
                json(
                    &serde_json::json!({"protocol_version": protocol_version, "token": token, "machine_id": machine_id}),
                )?,
            ));
        }
        Message::AgentAck {
            session_id,
            server_version,
        } => {
            return Ok((
                0x02,
                json(
                    &serde_json::json!({"session_id": session_id, "server_version": server_version}),
                )?,
            ));
        }
        Message::AgentErr { code, message } => {
            (0x03, serde_json::json!({"code": code, "message": message}))
        }
        Message::Ping { timestamp_ms } => (0x04, serde_json::json!({"timestamp_ms": timestamp_ms})),
        Message::Pong { timestamp_ms } => (0x05, serde_json::json!({"timestamp_ms": timestamp_ms})),
        Message::RegisterTunnel {
            subdomain,
            upstream_hint,
        } => (
            0x10,
            serde_json::json!({"subdomain": subdomain, "upstream_hint": upstream_hint}),
        ),
        Message::TunnelAck { subdomain, url } => (
            0x11,
            serde_json::json!({"subdomain": subdomain, "url": url}),
        ),
        Message::TunnelErr { code, message } => {
            (0x12, serde_json::json!({"code": code, "message": message}))
        }
        Message::TunnelClose { subdomain, reason } => (
            0x13,
            serde_json::json!({"subdomain": subdomain, "reason": reason}),
        ),
        Message::TunnelCloseAck { subdomain } => {
            (0x14, serde_json::json!({"subdomain": subdomain}))
        }
        Message::RequestHead(head) => return Ok((0x20, json(head)?)),
        Message::ResponseHead(head) => return Ok((0x21, json(head)?)),
        Message::BodyChunk(bytes) => return Ok((0x30, bytes.clone())),
        Message::BodyEnd => (0x31, serde_json::json!({})),
        Message::BodyAbort { message } => (0x32, serde_json::json!({"message": message})),
        Message::Error { code, message } => {
            (0x3f, serde_json::json!({"code": code, "message": message}))
        }
    };
    Ok((value.0, json(&value.1)?))
}

fn decode(kind: u8, payload: &[u8]) -> Result<Message, CodecError> {
    Ok(match kind {
        0x01 => {
            let v: AgentRegistration = parse(payload)?;
            Message::RegisterAgent {
                protocol_version: v.protocol_version,
                token: v.token,
                machine_id: v.machine_id,
            }
        }
        0x02 => {
            let v: AgentAcknowledgement = parse(payload)?;
            Message::AgentAck {
                session_id: v.session_id,
                server_version: v.server_version,
            }
        }
        0x03 => {
            let v: ErrorPayload = parse(payload)?;
            Message::AgentErr {
                code: v.code,
                message: v.message,
            }
        }
        0x04 => {
            let v: Timestamp = parse(payload)?;
            Message::Ping {
                timestamp_ms: v.timestamp_ms,
            }
        }
        0x05 => {
            let v: Timestamp = parse(payload)?;
            Message::Pong {
                timestamp_ms: v.timestamp_ms,
            }
        }
        0x10 => {
            let v: TunnelRegistration = parse(payload)?;
            Message::RegisterTunnel {
                subdomain: v.subdomain,
                upstream_hint: v.upstream_hint,
            }
        }
        0x11 => {
            let v: TunnelAcknowledgement = parse(payload)?;
            Message::TunnelAck {
                subdomain: v.subdomain,
                url: v.url,
            }
        }
        0x12 => {
            let v: ErrorPayload = parse(payload)?;
            Message::TunnelErr {
                code: v.code,
                message: v.message,
            }
        }
        0x13 => {
            let v: TunnelClosure = parse(payload)?;
            Message::TunnelClose {
                subdomain: v.subdomain,
                reason: v.reason,
            }
        }
        0x14 => {
            let v: TunnelClosureAck = parse(payload)?;
            Message::TunnelCloseAck {
                subdomain: v.subdomain,
            }
        }
        0x20 => Message::RequestHead(parse(payload)?),
        0x21 => Message::ResponseHead(parse(payload)?),
        0x30 => Message::BodyChunk(payload.to_vec()),
        0x31 => {
            let _: Empty = parse(payload)?;
            Message::BodyEnd
        }
        0x32 => {
            let v: AbortPayload = parse(payload)?;
            Message::BodyAbort { message: v.message }
        }
        0x3f => {
            let v: ErrorPayload = parse(payload)?;
            Message::Error {
                code: v.code,
                message: v.message,
            }
        }
        _ => return Err(CodecError::UnknownType(kind)),
    })
}

#[derive(serde::Deserialize)]
struct AgentRegistration {
    protocol_version: u32,
    token: String,
    machine_id: String,
}
#[derive(serde::Deserialize)]
struct AgentAcknowledgement {
    session_id: String,
    server_version: u32,
}
#[derive(serde::Deserialize)]
struct Timestamp {
    timestamp_ms: i64,
}
#[derive(serde::Deserialize)]
struct TunnelRegistration {
    subdomain: Option<String>,
    upstream_hint: Option<String>,
}
#[derive(serde::Deserialize)]
struct TunnelAcknowledgement {
    subdomain: String,
    url: String,
}
#[derive(serde::Deserialize)]
struct TunnelClosure {
    subdomain: String,
    reason: crate::CloseReason,
}
#[derive(serde::Deserialize)]
struct TunnelClosureAck {
    subdomain: String,
}
#[derive(serde::Deserialize)]
struct AbortPayload {
    message: String,
}
#[derive(serde::Deserialize)]
struct ErrorPayload {
    code: crate::ErrorCode,
    message: String,
}
#[derive(serde::Deserialize)]
struct Empty {}

/// Validates the chunk/end portion of one request or response body.
#[derive(Default, Debug)]
pub struct BodySequence {
    ended: bool,
}

impl BodySequence {
    pub fn accept(&mut self, frame: &Message) -> Result<(), CodecError> {
        if self.ended {
            return Err(CodecError::InvalidSequence);
        }
        match frame {
            Message::BodyChunk(_) => Ok(()),
            Message::BodyEnd | Message::BodyAbort { .. } => {
                self.ended = true;
                Ok(())
            }
            _ => Err(CodecError::InvalidSequence),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CloseReason, ErrorCode, RequestHead, ResponseHead};

    #[tokio::test]
    async fn all_frames_round_trip() {
        let messages = vec![
            Message::RegisterAgent {
                protocol_version: 1,
                token: "secret".into(),
                machine_id: "machine".into(),
            },
            Message::AgentAck {
                session_id: "id".into(),
                server_version: 1,
            },
            Message::AgentErr {
                code: ErrorCode::AuthFailed,
                message: "bad".into(),
            },
            Message::Ping { timestamp_ms: 42 },
            Message::Pong { timestamp_ms: 42 },
            Message::RegisterTunnel {
                subdomain: None,
                upstream_hint: Some("local".into()),
            },
            Message::TunnelAck {
                subdomain: "abc".into(),
                url: "https://abc".into(),
            },
            Message::TunnelErr {
                code: ErrorCode::SubdomainTaken,
                message: "taken".into(),
            },
            Message::TunnelClose {
                subdomain: "abc".into(),
                reason: CloseReason::Recoverable,
            },
            Message::TunnelCloseAck {
                subdomain: "abc".into(),
            },
            Message::RequestHead(RequestHead {
                subdomain: "abc".into(),
                method: "POST".into(),
                target: "/".into(),
                headers: vec![("set-cookie".into(), "a=b".into())],
                content_length: None,
            }),
            Message::ResponseHead(ResponseHead {
                status: 200,
                headers: vec![],
                content_length: Some(3),
            }),
            Message::BodyChunk(vec![0, 1, 255]),
            Message::BodyEnd,
            Message::BodyAbort {
                message: "lost".into(),
            },
            Message::Error {
                code: ErrorCode::StreamError,
                message: "oops".into(),
            },
        ];
        for expected in messages {
            let mut wire = Vec::new();
            write_message(&mut wire, &expected).await.unwrap();
            assert_eq!(read_message(&mut wire.as_slice()).await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn malformed_frame_table() {
        type MalformedCase = (&'static str, Vec<u8>, fn(CodecError) -> bool);
        let cases: Vec<MalformedCase> = vec![
            ("truncated header", vec![0x30, 0], |e| {
                matches!(e, CodecError::Io(_))
            }),
            ("truncated body", vec![0x30, 0, 0, 0, 2, 1], |e| {
                matches!(e, CodecError::Io(_))
            }),
            ("oversized", vec![0x30, 0, 1, 0, 1], |e| {
                matches!(e, CodecError::Oversized { .. })
            }),
            ("unknown type", vec![0xff, 0, 0, 0, 0], |e| {
                matches!(e, CodecError::UnknownType(0xff))
            }),
            ("invalid json", vec![0x31, 0, 0, 0, 1, b'x'], |e| {
                matches!(e, CodecError::InvalidPayload(_))
            }),
        ];
        for (name, wire, predicate) in cases {
            let err = read_message(&mut wire.as_slice()).await.unwrap_err();
            assert!(predicate(err), "{name}");
        }
    }

    #[test]
    fn body_sequence_table() {
        let cases = [
            ("empty", vec![Message::BodyEnd], true),
            (
                "chunks",
                vec![
                    Message::BodyChunk(vec![1]),
                    Message::BodyChunk(vec![2]),
                    Message::BodyEnd,
                ],
                true,
            ),
            (
                "abort",
                vec![
                    Message::BodyChunk(vec![1]),
                    Message::BodyAbort {
                        message: "gone".into(),
                    },
                ],
                true,
            ),
            (
                "double end",
                vec![Message::BodyEnd, Message::BodyEnd],
                false,
            ),
            (
                "chunk after end",
                vec![Message::BodyEnd, Message::BodyChunk(vec![1])],
                false,
            ),
            (
                "head in body",
                vec![Message::Ping { timestamp_ms: 1 }],
                false,
            ),
        ];
        for (name, frames, valid) in cases {
            let mut sequence = BodySequence::default();
            let result = frames.iter().try_for_each(|frame| sequence.accept(frame));
            assert_eq!(result.is_ok(), valid, "{name}");
        }
    }
}
