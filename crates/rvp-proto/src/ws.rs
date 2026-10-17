//! P9a WebSocket transport framing (plan §7 fallback, open decision 8).
//!
//! WebSocket is message-oriented while RVP has two logical channels (control
//! byte-stream + fire-and-forget media datagrams) sharing one connection.
//! Every WS binary message therefore starts with a 1-byte tag:
//!
//! - [`TAG_CONTROL`] (0) — a chunk of the RVP control byte-stream
//!   (`u32 BE length` ‖ body frames, exactly the bytes that ride the QUIC
//!   bi-stream; chunks are arbitrary, the reader concatenates them).
//! - [`TAG_DATAGRAM`] (1) — one complete media datagram
//!   (16-byte header + payload, [`HEADER_LEN`] + payload).
//!
//! The TS client (`apps/web/src/lib/ws.ts`) mirrors these tags with the same
//! anchored vectors, so the two sides cannot drift silently.

use bytes::{BufMut, Bytes, BytesMut};

use crate::{ProtoError, HEADER_LEN};

/// Tag byte prefixing a control byte-stream chunk in a WS binary message.
pub const TAG_CONTROL: u8 = 0;
/// Tag byte prefixing one complete media datagram in a WS binary message.
pub const TAG_DATAGRAM: u8 = 1;

/// One decoded WS binary message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    /// Chunk of the control byte-stream (frames are decoded separately).
    Control(Bytes),
    /// One complete media datagram (header + payload).
    Datagram(Bytes),
}

/// Frame a control byte-stream chunk for sending.
pub fn frame_control(chunk: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(1 + chunk.len());
    out.put_u8(TAG_CONTROL);
    out.put_slice(chunk);
    out.freeze()
}

/// Frame one media datagram for sending.
pub fn frame_datagram(datagram: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(1 + datagram.len());
    out.put_u8(TAG_DATAGRAM);
    out.put_slice(datagram);
    out.freeze()
}

/// Decode a received WS binary message.
///
/// Datagram messages are additionally checked against [`HEADER_LEN`] so a
/// truncated frame fails here instead of deep in the SFU.
pub fn parse_ws_message(msg: &[u8]) -> Result<WsMessage, ProtoError> {
    let (&tag, rest) = msg
        .split_first()
        .ok_or(ProtoError::ShortBuffer { need: 1, got: 0 })?;
    match tag {
        TAG_CONTROL => Ok(WsMessage::Control(Bytes::copy_from_slice(rest))),
        TAG_DATAGRAM => {
            if rest.len() < HEADER_LEN {
                return Err(ProtoError::ShortBuffer {
                    need: HEADER_LEN,
                    got: rest.len(),
                });
            }
            Ok(WsMessage::Datagram(Bytes::copy_from_slice(rest)))
        }
        other => Err(ProtoError::UnknownTag(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_round_trip() {
        let chunk = [0x00, 0x00, 0x00, 0x02, 0x00, 0x02]; // len=2, JoinRoom
        let framed = frame_control(&chunk);
        assert_eq!(framed[0], TAG_CONTROL);
        match parse_ws_message(&framed).unwrap() {
            WsMessage::Control(bytes) => assert_eq!(&bytes[..], &chunk),
            other => panic!("expected control, got {other:?}"),
        }
    }

    #[test]
    fn datagram_round_trip() {
        let dgram = [0x11u8; HEADER_LEN + 4];
        let framed = frame_datagram(&dgram);
        assert_eq!(framed[0], TAG_DATAGRAM);
        match parse_ws_message(&framed).unwrap() {
            WsMessage::Datagram(bytes) => assert_eq!(&bytes[..], &dgram),
            other => panic!("expected datagram, got {other:?}"),
        }
    }

    #[test]
    fn short_datagram_rejected() {
        let framed = frame_datagram(&[0u8; HEADER_LEN - 1]);
        assert!(matches!(
            parse_ws_message(&framed),
            Err(ProtoError::ShortBuffer { .. })
        ));
    }

    #[test]
    fn empty_and_unknown_tag_rejected() {
        assert!(parse_ws_message(&[]).is_err());
        assert!(parse_ws_message(&[0xFF, 0x01]).is_err());
    }

    /// Anchored vector (mirrored in `apps/web/src/lib/ws.test.ts`).
    #[test]
    fn anchored_control_vector() {
        // Hello frame for protocol version 1: len=7, type=1, version=1, jwt=""?
        // Minimal anchored bytes: length prefix 00000002 ‖ type 0002 (JoinRoom).
        let framed = frame_control(&[0, 0, 0, 2, 0, 2]);
        assert_eq!(&framed[..], &[0, 0, 0, 0, 2, 0, 2]);
    }
}
