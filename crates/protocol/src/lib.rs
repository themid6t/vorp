mod codec;
mod message;

pub use codec::{BodySequence, CodecError, MAX_FRAME_PAYLOAD, read_message, write_message};
pub use message::{
    CloseReason, ErrorCode, Header, Message, PROTOCOL_VERSION, RequestHead, ResponseHead,
};

pub const AGENT_ALPN: &[u8] = b"vorp-agent/1";
