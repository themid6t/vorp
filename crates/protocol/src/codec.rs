use tokio::io::{AsyncRead, AsyncWrite};

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
pub async fn read_message<R: AsyncRead + Unpin>(_reader: &mut R) -> Result<Message, CodecError> {
    todo!("protocol workstream implements frame decoding")
}

/// Write exactly one frame and await the write to preserve backpressure.
pub async fn write_message<W: AsyncWrite + Unpin>(
    _writer: &mut W,
    _message: &Message,
) -> Result<(), CodecError> {
    todo!("protocol workstream implements frame encoding")
}
