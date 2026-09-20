//! RFC 6455 framing, only what one browser client needs: the handshake
//! accept key, a frame codec, and a reader that reassembles continuations.
//!
//! Server-to-client frames are never masked; client-to-server frames must
//! be (section 5.1), and a client that skips the mask is closed with 1002.

use tokio::io::{AsyncRead, AsyncReadExt};

/// The RFC 6455 section 1.3 handshake GUID.
pub const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Close codes used by this server.
pub mod close {
    pub const NORMAL: u16 = 1000;
    pub const GOING_AWAY: u16 = 1001;
    pub const PROTOCOL: u16 = 1002;
    pub const INVALID_DATA: u16 = 1007;
    pub const POLICY: u16 = 1008;
    pub const TOO_BIG: u16 = 1009;
    pub const INTERNAL: u16 = 1011;
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(client_key: &str) -> String {
    use base64::Engine as _;
    let digest = sha1_smol::Sha1::from(format!("{}{}", client_key.trim(), GUID)).digest();
    base64::engine::general_purpose::STANDARD.encode(digest.bytes())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    fn from_bits(bits: u8) -> Option<Opcode> {
        match bits {
            0x0 => Some(Opcode::Continuation),
            0x1 => Some(Opcode::Text),
            0x2 => Some(Opcode::Binary),
            0x8 => Some(Opcode::Close),
            0x9 => Some(Opcode::Ping),
            0xA => Some(Opcode::Pong),
            _ => None,
        }
    }

    fn bits(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xA,
        }
    }

    fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum Decoded {
    Frame {
        frame: Frame,
        consumed: usize,
    },
    Incomplete,
    /// Protocol violation; the connection closes with this code.
    Error(u16),
}

/// Decodes one frame, unmasking in place into the returned payload.
pub fn decode_frame(buf: &[u8], max_payload: usize, require_mask: bool) -> Decoded {
    if buf.len() < 2 {
        return Decoded::Incomplete;
    }
    let b0 = buf[0];
    let b1 = buf[1];
    if b0 & 0x70 != 0 {
        return Decoded::Error(close::PROTOCOL); // RSV bits must be zero
    }
    let Some(opcode) = Opcode::from_bits(b0 & 0x0F) else {
        return Decoded::Error(close::PROTOCOL);
    };
    let fin = b0 & 0x80 != 0;
    let masked = b1 & 0x80 != 0;
    if require_mask && !masked {
        return Decoded::Error(close::PROTOCOL);
    }
    let short_len = (b1 & 0x7F) as usize;
    let mut offset = 2;
    let len = match short_len {
        126 => {
            if buf.len() < offset + 2 {
                return Decoded::Incomplete;
            }
            let n = u16::from_be_bytes([buf[offset], buf[offset + 1]]) as usize;
            offset += 2;
            n
        }
        127 => {
            if buf.len() < offset + 8 {
                return Decoded::Incomplete;
            }
            let n = u64::from_be_bytes(buf[offset..offset + 8].try_into().unwrap());
            if n & (1 << 63) != 0 {
                return Decoded::Error(close::PROTOCOL); // top bit must be 0
            }
            offset += 8;
            match usize::try_from(n) {
                Ok(n) => n,
                Err(_) => return Decoded::Error(close::TOO_BIG),
            }
        }
        n => n,
    };
    // A control frame carries its whole meaning in one short frame.
    if opcode.is_control() && (len > 125 || !fin) {
        return Decoded::Error(close::PROTOCOL);
    }
    if len > max_payload {
        return Decoded::Error(close::TOO_BIG);
    }
    let mask: Option<[u8; 4]> = if masked {
        if buf.len() < offset + 4 {
            return Decoded::Incomplete;
        }
        let m = [
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ];
        offset += 4;
        Some(m)
    } else {
        None
    };
    if buf.len() < offset + len {
        return Decoded::Incomplete;
    }
    let mut payload = buf[offset..offset + len].to_vec();
    if let Some(m) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= m[i % 4];
        }
    }
    Decoded::Frame {
        frame: Frame {
            fin,
            opcode,
            payload,
        },
        consumed: offset + len,
    }
}

/// Encodes one frame. The server passes `mask = None`; tests masquerading
/// as a client pass `Some`.
pub fn encode_frame(opcode: Opcode, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode.bits()); // always FIN: nothing here fragments
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(mask_bit | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(m) => {
            out.extend_from_slice(&m);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

pub fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut p = code.to_be_bytes().to_vec();
    // A close reason must fit the 125-byte control frame with its code.
    let reason = reason.as_bytes();
    p.extend_from_slice(&reason[..reason.len().min(123)]);
    p
}

pub fn parse_close(payload: &[u8]) -> (u16, String) {
    if payload.len() < 2 {
        return (close::NORMAL, String::new());
    }
    (
        u16::from_be_bytes([payload[0], payload[1]]),
        String::from_utf8_lossy(&payload[2..]).into_owned(),
    )
}

/// One complete application message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(u16, String),
}

#[derive(Debug)]
pub enum WsError {
    /// The peer closed, or we must close, with this code.
    Protocol(u16),
    Io(std::io::Error),
    Eof,
}

/// Reads frames off a socket and hands back whole messages.
pub struct FrameReader {
    buf: Vec<u8>,
    partial: Option<(Opcode, Vec<u8>)>,
    max_message: usize,
}

impl FrameReader {
    pub fn new(max_message: usize) -> FrameReader {
        FrameReader {
            buf: Vec::with_capacity(8 * 1024),
            partial: None,
            max_message,
        }
    }

    /// Next complete message, reassembling continuation frames. Control
    /// frames are returned as they arrive, even mid-fragment.
    pub async fn next<R: AsyncRead + Unpin>(&mut self, r: &mut R) -> Result<Message, WsError> {
        loop {
            match decode_frame(&self.buf, self.max_message, true) {
                Decoded::Error(code) => return Err(WsError::Protocol(code)),
                Decoded::Frame { frame, consumed } => {
                    self.buf.drain(..consumed);
                    if let Some(msg) = self.absorb(frame)? {
                        return Ok(msg);
                    }
                }
                Decoded::Incomplete => {
                    let mut chunk = [0u8; 8 * 1024];
                    match r.read(&mut chunk).await {
                        Ok(0) => return Err(WsError::Eof),
                        Ok(n) => {
                            if self.buf.len() + n > self.max_message.saturating_mul(2) {
                                return Err(WsError::Protocol(close::TOO_BIG));
                            }
                            self.buf.extend_from_slice(&chunk[..n]);
                        }
                        Err(e) => return Err(WsError::Io(e)),
                    }
                }
            }
        }
    }

    fn absorb(&mut self, frame: Frame) -> Result<Option<Message>, WsError> {
        match frame.opcode {
            Opcode::Ping => Ok(Some(Message::Ping(frame.payload))),
            Opcode::Pong => Ok(Some(Message::Pong(frame.payload))),
            Opcode::Close => {
                let (code, reason) = parse_close(&frame.payload);
                Ok(Some(Message::Close(code, reason)))
            }
            Opcode::Text | Opcode::Binary => {
                if self.partial.is_some() {
                    return Err(WsError::Protocol(close::PROTOCOL));
                }
                if frame.fin {
                    return finish(frame.opcode, frame.payload).map(Some);
                }
                self.partial = Some((frame.opcode, frame.payload));
                Ok(None)
            }
            Opcode::Continuation => {
                let Some((opcode, mut acc)) = self.partial.take() else {
                    return Err(WsError::Protocol(close::PROTOCOL));
                };
                if acc.len() + frame.payload.len() > self.max_message {
                    return Err(WsError::Protocol(close::TOO_BIG));
                }
                acc.extend_from_slice(&frame.payload);
                if frame.fin {
                    return finish(opcode, acc).map(Some);
                }
                self.partial = Some((opcode, acc));
                Ok(None)
            }
        }
    }
}

fn finish(opcode: Opcode, payload: Vec<u8>) -> Result<Message, WsError> {
    match opcode {
        Opcode::Binary => Ok(Message::Binary(payload)),
        _ => match String::from_utf8(payload) {
            Ok(text) => Ok(Message::Text(text)),
            // A text frame that is not UTF-8 is 1007, not 1002.
            Err(_) => Err(WsError::Protocol(close::INVALID_DATA)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];

    #[test]
    fn accept_key_matches_the_rfc_vector() {
        // RFC 6455 section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    fn roundtrip(payload: &[u8]) {
        let encoded = encode_frame(Opcode::Binary, payload, Some(MASK));
        match decode_frame(&encoded, 1 << 20, true) {
            Decoded::Frame { frame, consumed } => {
                assert_eq!(consumed, encoded.len());
                assert_eq!(frame.payload, payload);
                assert!(frame.fin);
                assert_eq!(frame.opcode, Opcode::Binary);
            }
            other => panic!("len {}: {other:?}", payload.len()),
        }
    }

    #[test]
    fn frames_round_trip_across_every_length_class() {
        for len in [0usize, 1, 125, 126, 127, 65535, 65536] {
            roundtrip(&vec![0xA5; len]);
        }
    }

    #[test]
    fn an_unmasked_client_frame_is_a_protocol_error() {
        let encoded = encode_frame(Opcode::Text, b"hi", None);
        assert!(matches!(
            decode_frame(&encoded, 1 << 20, true),
            Decoded::Error(close::PROTOCOL)
        ));
        // The same bytes are fine when masking is not required.
        assert!(matches!(
            decode_frame(&encoded, 1 << 20, false),
            Decoded::Frame { .. }
        ));
    }

    #[test]
    fn a_payload_over_the_cap_is_too_big() {
        let encoded = encode_frame(Opcode::Binary, &vec![0u8; 300], Some(MASK));
        assert!(matches!(
            decode_frame(&encoded, 200, true),
            Decoded::Error(close::TOO_BIG)
        ));
    }

    #[test]
    fn rsv_bits_and_unknown_opcodes_are_rejected() {
        let mut encoded = encode_frame(Opcode::Text, b"x", Some(MASK));
        encoded[0] |= 0x40;
        assert!(matches!(
            decode_frame(&encoded, 1 << 20, true),
            Decoded::Error(close::PROTOCOL)
        ));
        let mut encoded = encode_frame(Opcode::Text, b"x", Some(MASK));
        encoded[0] = 0x80 | 0x3;
        assert!(matches!(
            decode_frame(&encoded, 1 << 20, true),
            Decoded::Error(close::PROTOCOL)
        ));
    }

    #[test]
    fn a_partial_buffer_is_incomplete() {
        let encoded = encode_frame(Opcode::Binary, &[1, 2, 3, 4], Some(MASK));
        for cut in 0..encoded.len() {
            assert!(matches!(
                decode_frame(&encoded[..cut], 1 << 20, true),
                Decoded::Incomplete
            ));
        }
    }

    #[tokio::test]
    async fn fragmented_text_is_reassembled() {
        let mut wire = Vec::new();
        // "he" + "ll" + "o" as text + continuation + continuation.
        let mut first = encode_frame(Opcode::Text, b"he", Some(MASK));
        first[0] &= 0x7F; // clear FIN
        wire.extend_from_slice(&first);
        let mut mid = encode_frame(Opcode::Continuation, b"ll", Some(MASK));
        mid[0] &= 0x7F;
        wire.extend_from_slice(&mid);
        wire.extend_from_slice(&encode_frame(Opcode::Continuation, b"o", Some(MASK)));

        let mut reader = FrameReader::new(1 << 20);
        let mut cursor = std::io::Cursor::new(wire);
        assert_eq!(
            reader.next(&mut cursor).await.unwrap(),
            Message::Text("hello".into())
        );
    }

    #[tokio::test]
    async fn invalid_utf8_in_a_text_frame_closes_1007() {
        let wire = encode_frame(Opcode::Text, &[0xff, 0xfe], Some(MASK));
        let mut reader = FrameReader::new(1 << 20);
        let mut cursor = std::io::Cursor::new(wire);
        assert!(matches!(
            reader.next(&mut cursor).await,
            Err(WsError::Protocol(close::INVALID_DATA))
        ));
    }

    #[tokio::test]
    async fn control_frames_pass_through_whole() {
        let mut wire = encode_frame(Opcode::Ping, b"beat", Some(MASK));
        wire.extend_from_slice(&encode_frame(
            Opcode::Close,
            &close_payload(close::NORMAL, "bye"),
            Some(MASK),
        ));
        let mut reader = FrameReader::new(1 << 20);
        let mut cursor = std::io::Cursor::new(wire);
        assert_eq!(
            reader.next(&mut cursor).await.unwrap(),
            Message::Ping(b"beat".to_vec())
        );
        assert_eq!(
            reader.next(&mut cursor).await.unwrap(),
            Message::Close(close::NORMAL, "bye".into())
        );
    }

    #[test]
    fn an_oversized_control_frame_is_rejected() {
        let encoded = encode_frame(Opcode::Ping, &[0u8; 200], Some(MASK));
        assert!(matches!(
            decode_frame(&encoded, 1 << 20, true),
            Decoded::Error(close::PROTOCOL)
        ));
    }

    #[test]
    fn close_payloads_round_trip_and_stay_within_a_control_frame() {
        let (code, reason) = parse_close(&close_payload(close::GOING_AWAY, "shutting down"));
        assert_eq!(
            (code, reason.as_str()),
            (close::GOING_AWAY, "shutting down")
        );
        assert_eq!(parse_close(&[]).0, close::NORMAL);
        assert!(close_payload(close::POLICY, &"x".repeat(500)).len() <= 125);
    }
}
