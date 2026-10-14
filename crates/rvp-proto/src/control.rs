//! RVP/1 control stream: length-prefixed binary frames (plan §3.1).
//!
//! One dedicated bidirectional QUIC stream per connection.
//! Frame = `u32 BE length` ‖ `u16 BE type` ‖ body, where `length` covers type+body.
//!
//! Messages (P2 subset):
//! - `Hello { protocol_version, grant_jwt }` — client → server, first frame
//! - `JoinRoom` — client → server (room comes from the verified grant)
//! - `Welcome { participant_id, max_datagram }` — server → client; only after
//!   this are DATAGRAMs accepted from the connection (plan §3.4)
//! - `Error { code, message }` — server → client

use bytes::{BufMut, Bytes, BytesMut};

pub const TYPE_HELLO: u16 = 1;
pub const TYPE_JOIN_ROOM: u16 = 2;
pub const TYPE_WELCOME: u16 = 3;
pub const TYPE_ERROR: u16 = 15;

/// Current control/media protocol version (`Hello.protocol_version`).
pub const PROTOCOL_VERSION: u16 = 1;

/// Hard cap on control frames (JWT grants can be a few KB).
pub const MAX_FRAME_LEN: usize = 16 * 1024;

/// Error codes carried in `Error` frames.
pub mod error_code {
    pub const BAD_REQUEST: u16 = 400;
    pub const UNAUTHORIZED: u16 = 401;
    pub const FORBIDDEN: u16 = 403;
    pub const TIMEOUT: u16 = 408;
    pub const INTERNAL: u16 = 500;
    pub const VERSION_UNSUPPORTED: u16 = 505;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    Hello {
        protocol_version: u16,
        grant_jwt: String,
    },
    JoinRoom,
    Welcome {
        participant_id: u64,
        max_datagram: u16,
    },
    Error {
        code: u16,
        message: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("frame too large: {0} > {MAX_FRAME_LEN}")]
    FrameTooLarge(usize),
    #[error("unknown frame type {0}")]
    UnknownType(u16),
    #[error("malformed body for type {0}")]
    BadBody(u16),
    #[error("frame truncated")]
    Truncated,
}

impl ControlMessage {
    /// Encode a complete frame (length prefix included).
    pub fn encode(&self) -> Bytes {
        let mut body = BytesMut::with_capacity(32);
        match self {
            Self::Hello {
                protocol_version,
                grant_jwt,
            } => {
                body.put_u16(TYPE_HELLO);
                body.put_u16(*protocol_version);
                body.extend_from_slice(grant_jwt.as_bytes());
            }
            Self::JoinRoom => {
                body.put_u16(TYPE_JOIN_ROOM);
            }
            Self::Welcome {
                participant_id,
                max_datagram,
            } => {
                body.put_u16(TYPE_WELCOME);
                body.put_u64(*participant_id);
                body.put_u16(*max_datagram);
            }
            Self::Error { code, message } => {
                body.put_u16(TYPE_ERROR);
                body.put_u16(*code);
                body.extend_from_slice(message.as_bytes());
            }
        }
        let mut frame = BytesMut::with_capacity(4 + body.len());
        frame.put_u32(body.len() as u32);
        frame.extend_from_slice(&body);
        frame.freeze()
    }

    /// Decode one frame body (after the u32 length prefix has been consumed).
    pub fn decode_body(body: &[u8]) -> Result<Self, ControlError> {
        if body.len() < 2 {
            return Err(ControlError::Truncated);
        }
        let msg_type = u16::from_be_bytes([body[0], body[1]]);
        let rest = &body[2..];
        match msg_type {
            TYPE_HELLO => {
                if rest.len() < 2 {
                    return Err(ControlError::BadBody(TYPE_HELLO));
                }
                let protocol_version = u16::from_be_bytes([rest[0], rest[1]]);
                let grant_jwt = std::str::from_utf8(&rest[2..])
                    .map_err(|_| ControlError::BadBody(TYPE_HELLO))?
                    .to_owned();
                Ok(Self::Hello {
                    protocol_version,
                    grant_jwt,
                })
            }
            TYPE_JOIN_ROOM => Ok(Self::JoinRoom),
            TYPE_WELCOME => {
                if rest.len() < 10 {
                    return Err(ControlError::BadBody(TYPE_WELCOME));
                }
                let mut pid = [0u8; 8];
                pid.copy_from_slice(&rest[..8]);
                Ok(Self::Welcome {
                    participant_id: u64::from_be_bytes(pid),
                    max_datagram: u16::from_be_bytes([rest[8], rest[9]]),
                })
            }
            TYPE_ERROR => {
                if rest.len() < 2 {
                    return Err(ControlError::BadBody(TYPE_ERROR));
                }
                let code = u16::from_be_bytes([rest[0], rest[1]]);
                let message = std::str::from_utf8(&rest[2..])
                    .map_err(|_| ControlError::BadBody(TYPE_ERROR))?
                    .to_owned();
                Ok(Self::Error { code, message })
            }
            other => Err(ControlError::UnknownType(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let msg = ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            grant_jwt: "eyJhbGciOiJFZERTQSJ9.fake".into(),
        };
        let frame = msg.encode();
        let len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(len, frame.len() - 4);
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), msg);
    }

    #[test]
    fn join_room_roundtrip() {
        let msg = ControlMessage::JoinRoom;
        let frame = msg.encode();
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), msg);
    }

    #[test]
    fn welcome_roundtrip() {
        let msg = ControlMessage::Welcome {
            participant_id: 0xDEAD_BEEF,
            max_datagram: 1200,
        };
        let frame = msg.encode();
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), msg);
    }

    #[test]
    fn error_roundtrip() {
        let msg = ControlMessage::Error {
            code: error_code::UNAUTHORIZED,
            message: "bad grant".into(),
        };
        let frame = msg.encode();
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), msg);
    }

    #[test]
    fn unknown_type_rejected() {
        let body = [0xEEu8, 0x01];
        assert_eq!(
            ControlMessage::decode_body(&body),
            Err(ControlError::UnknownType(0xEE01))
        );
    }
}
