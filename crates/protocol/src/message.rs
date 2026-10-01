use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Ordered HTTP header pair; duplicates are significant.
pub type Header = (String, String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestHead {
    pub subdomain: String,
    pub method: String,
    pub target: String,
    pub headers: Vec<Header>,
    pub content_length: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseHead {
    pub status: u16,
    pub headers: Vec<Header>,
    pub content_length: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AuthFailed,
    UnsupportedVersion,
    SubdomainTaken,
    SubdomainInvalid,
    SubdomainNotAllowed,
    StreamError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CloseReason {
    Forced,
    Revoked,
    Expired,
    Recoverable,
}

/// Each variant has the discriminant assigned by docs/protocol.md §3.
/// BodyChunk is one bounded chunk, never a complete proxied body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    RegisterAgent {
        protocol_version: u32,
        token: String,
        machine_id: String,
    },
    AgentAck {
        session_id: String,
        server_version: u32,
    },
    AgentErr {
        code: ErrorCode,
        message: String,
    },
    Ping {
        timestamp_ms: i64,
    },
    Pong {
        timestamp_ms: i64,
    },
    RegisterTunnel {
        subdomain: Option<String>,
        upstream_hint: Option<String>,
    },
    TunnelAck {
        subdomain: String,
        url: String,
    },
    TunnelErr {
        code: ErrorCode,
        message: String,
    },
    TunnelClose {
        subdomain: String,
        reason: CloseReason,
    },
    TunnelCloseAck {
        subdomain: String,
    },
    RequestHead(RequestHead),
    ResponseHead(ResponseHead),
    BodyChunk(Vec<u8>),
    BodyEnd,
    BodyAbort {
        message: String,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}
