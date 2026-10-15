//! RVP/1 wire format (Phase 1 subset: media datagram header).
//!
//! Media datagram = fixed 16-byte header + opaque payload (Opus or ciphertext).
//! The header is plaintext so the SFU can route without keys (ADR-004).
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! | Ver(4)  |Ty(4)|    Flags      |     PT        |     KID       |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                          Sequence (u32 BE)                    |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                    Timestamp (u32 BE, 48 kHz)                 |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! | FrameDur(ms)  |            Reserved (3 bytes)                 |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                     Opus / ciphertext payload ...             |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```

use bytes::{Bytes, BytesMut};

pub mod control;

/// Fixed media-header length in bytes.
pub const HEADER_LEN: usize = 16;

/// Current protocol version (`Ver` nibble).
pub const VERSION: u8 = 1;

/// Payload type: plaintext Opus (tests/interop only).
pub const PT_OPUS: u8 = 0;
/// Payload type: pairwise AES-GCM encrypted Opus (ADR-004).
pub const PT_OPUS_ENC: u8 = 1;

/// Conservative payload cap (plan §3.2: `min(peer max_datagram, 1200)`).
pub const MAX_PAYLOAD: usize = 1200 - HEADER_LEN;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("buffer too short: need {need} bytes, got {got}")]
    ShortBuffer { need: usize, got: usize },
    #[error("unsupported version {0}")]
    BadVersion(u8),
}

/// Fixed 16-byte RVP/1 media header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub version: u8,
    /// Datagram type nibble (reserved; 0 for media).
    pub kind: u8,
    pub flags: u8,
    /// `PT_OPUS` or `PT_OPUS_ENC`.
    pub pt: u8,
    /// Pairwise key id; 0 = plaintext payload.
    pub kid: u8,
    /// u32 wrapping sequence (per publisher).
    pub sequence: u32,
    /// Publisher media clock, 48 kHz ticks.
    pub timestamp: u32,
    pub frame_duration_ms: u8,
    pub reserved: [u8; 3],
}

impl Default for MediaHeader {
    fn default() -> Self {
        Self {
            version: VERSION,
            kind: 0,
            flags: 0,
            pt: PT_OPUS,
            kid: 0,
            sequence: 0,
            timestamp: 0,
            frame_duration_ms: 20,
            reserved: [0; 3],
        }
    }
}

impl MediaHeader {
    /// Encode into a fixed 16-byte array (big-endian integers).
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = ((self.version & 0x0f) << 4) | (self.kind & 0x0f);
        buf[1] = self.flags;
        buf[2] = self.pt;
        buf[3] = self.kid;
        buf[4..8].copy_from_slice(&self.sequence.to_be_bytes());
        buf[8..12].copy_from_slice(&self.timestamp.to_be_bytes());
        buf[12] = self.frame_duration_ms;
        buf[13..16].copy_from_slice(&self.reserved);
        buf
    }

    /// Decode from the first [`HEADER_LEN`] bytes of `buf`.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtoError::ShortBuffer {
                need: HEADER_LEN,
                got: buf.len(),
            });
        }
        let version = buf[0] >> 4;
        if version != VERSION {
            return Err(ProtoError::BadVersion(version));
        }
        let mut seq = [0u8; 4];
        seq.copy_from_slice(&buf[4..8]);
        let mut ts = [0u8; 4];
        ts.copy_from_slice(&buf[8..12]);
        Ok(Self {
            version,
            kind: buf[0] & 0x0f,
            flags: buf[1],
            pt: buf[2],
            kid: buf[3],
            sequence: u32::from_be_bytes(seq),
            timestamp: u32::from_be_bytes(ts),
            frame_duration_ms: buf[12],
            reserved: [buf[13], buf[14], buf[15]],
        })
    }
}

/// A received/sending media datagram: header + payload, zero-copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDatagram {
    pub header: MediaHeader,
    pub payload: Bytes,
}

impl MediaDatagram {
    /// Build an owned datagram buffer (single copy of header + payload).
    pub fn build(header: MediaHeader, payload: Bytes) -> Bytes {
        let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
        buf.extend_from_slice(&header.encode());
        buf.extend_from_slice(&payload);
        buf.freeze()
    }

    /// Parse a datagram buffer without copying the payload.
    pub fn parse(buf: Bytes) -> Result<Self, ProtoError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtoError::ShortBuffer {
                need: HEADER_LEN,
                got: buf.len(),
            });
        }
        let header = MediaHeader::decode(&buf[..HEADER_LEN])?;
        let payload = buf.slice(HEADER_LEN..);
        Ok(Self { header, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = MediaHeader {
            version: VERSION,
            kind: 0,
            flags: 0b0000_0001,
            pt: PT_OPUS_ENC,
            kid: 7,
            sequence: 0xDEAD_BEEF,
            timestamp: 0x0102_0304,
            frame_duration_ms: 20,
            reserved: [9, 8, 7],
        };
        let bytes = h.encode();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(MediaHeader::decode(&bytes).unwrap(), h);
    }

    #[test]
    fn decode_rejects_short_buffer() {
        let err = MediaHeader::decode(&[0u8; 8]).unwrap_err();
        assert_eq!(
            err,
            ProtoError::ShortBuffer {
                need: HEADER_LEN,
                got: 8
            }
        );
    }

    #[test]
    fn decode_rejects_bad_version() {
        let mut bytes = MediaHeader::default().encode();
        bytes[0] = 0x90; // version 9
        assert_eq!(
            MediaHeader::decode(&bytes).unwrap_err(),
            ProtoError::BadVersion(9)
        );
    }

    #[test]
    fn datagram_parse_zero_copy_payload() {
        let payload = Bytes::from_static(b"opus-bytes-here");
        let raw = MediaDatagram::build(MediaHeader::default(), payload.clone());
        assert_eq!(raw.len(), HEADER_LEN + payload.len());
        let parsed = MediaDatagram::parse(raw).unwrap();
        assert_eq!(parsed.header, MediaHeader::default());
        assert_eq!(parsed.payload, payload);
    }

    #[test]
    fn sequence_wraps_u32() {
        let h = MediaHeader {
            sequence: u32::MAX,
            ..Default::default()
        };
        assert_eq!(MediaHeader::decode(&h.encode()).unwrap().sequence, u32::MAX);
    }

    /// Exact wire bytes — pinned here and in the TypeScript codec tests
    /// (`apps/web/src/lib/media.test.ts`), so both sides cannot drift.
    #[test]
    fn header_wire_vector() {
        let h = MediaHeader {
            version: VERSION,
            kind: 0,
            flags: 0b0000_0001,
            pt: PT_OPUS_ENC,
            kid: 7,
            sequence: 0xDEAD_BEEF,
            timestamp: 0x0102_0304,
            frame_duration_ms: 20,
            reserved: [9, 8, 7],
        };
        let hex: String = h.encode().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "10010107deadbeef0102030414090807");
    }

    #[test]
    fn default_header_wire_vector() {
        let hex: String = MediaHeader::default()
            .encode()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, "10000000000000000000000014000000");
    }
}
