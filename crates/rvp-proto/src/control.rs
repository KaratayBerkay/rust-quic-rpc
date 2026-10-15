//! RVP/1 control stream: length-prefixed binary frames (plan §3.1).
//!
//! One dedicated bidirectional QUIC stream per connection.
//! Frame = `u32 BE length` ‖ `u16 BE type` ‖ body, where `length` covers type+body.
//!
//! Messages (P3 subset):
//! - `Hello { protocol_version, grant_jwt }` — client → server, first frame
//! - `JoinRoom` — client → server (room comes from the verified grant)
//! - `Welcome { participant_id, max_datagram, roster }` — server → client; only
//!   after this are DATAGRAMs accepted from the connection (plan §3.4)
//! - `ParticipantJoined / ParticipantLeft` — server → client room events
//! - `Error { code, message }` — server → client

use bytes::{BufMut, Bytes, BytesMut};

pub const TYPE_HELLO: u16 = 1;
pub const TYPE_JOIN_ROOM: u16 = 2;
pub const TYPE_WELCOME: u16 = 3;
pub const TYPE_PARTICIPANT_JOINED: u16 = 4;
pub const TYPE_PARTICIPANT_LEFT: u16 = 5;
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
    pub const ROOM_FULL: u16 = 429;
    pub const INTERNAL: u16 = 500;
    pub const CONNECTION_LIMIT: u16 = 503;
    pub const VERSION_UNSUPPORTED: u16 = 505;
}

/// A room member advertised in `Welcome` / `ParticipantJoined`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    pub participant_id: u64,
    pub user_id: String,
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
        roster: Vec<RosterEntry>,
    },
    ParticipantJoined {
        participant_id: u64,
        user_id: String,
    },
    ParticipantLeft {
        participant_id: u64,
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
                roster,
            } => {
                body.put_u16(TYPE_WELCOME);
                body.put_u64(*participant_id);
                body.put_u16(*max_datagram);
                body.put_u16(roster.len() as u16);
                for entry in roster {
                    body.put_u64(entry.participant_id);
                    body.put_u16(entry.user_id.len() as u16);
                    body.extend_from_slice(entry.user_id.as_bytes());
                }
            }
            Self::ParticipantJoined {
                participant_id,
                user_id,
            } => {
                body.put_u16(TYPE_PARTICIPANT_JOINED);
                body.put_u64(*participant_id);
                body.put_u16(user_id.len() as u16);
                body.extend_from_slice(user_id.as_bytes());
            }
            Self::ParticipantLeft { participant_id } => {
                body.put_u16(TYPE_PARTICIPANT_LEFT);
                body.put_u64(*participant_id);
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
                if rest.len() < 12 {
                    return Err(ControlError::BadBody(TYPE_WELCOME));
                }
                let mut pid = [0u8; 8];
                pid.copy_from_slice(&rest[..8]);
                let participant_id = u64::from_be_bytes(pid);
                let max_datagram = u16::from_be_bytes([rest[8], rest[9]]);
                let count = u16::from_be_bytes([rest[10], rest[11]]) as usize;
                let mut cursor = &rest[12..];
                let mut roster = Vec::with_capacity(count);
                for _ in 0..count {
                    if cursor.len() < 10 {
                        return Err(ControlError::BadBody(TYPE_WELCOME));
                    }
                    let mut pid = [0u8; 8];
                    pid.copy_from_slice(&cursor[..8]);
                    let participant_id = u64::from_be_bytes(pid);
                    let uid_len = u16::from_be_bytes([cursor[8], cursor[9]]) as usize;
                    cursor = &cursor[10..];
                    if cursor.len() < uid_len {
                        return Err(ControlError::BadBody(TYPE_WELCOME));
                    }
                    let user_id = std::str::from_utf8(&cursor[..uid_len])
                        .map_err(|_| ControlError::BadBody(TYPE_WELCOME))?
                        .to_owned();
                    cursor = &cursor[uid_len..];
                    roster.push(RosterEntry {
                        participant_id,
                        user_id,
                    });
                }
                Ok(Self::Welcome {
                    participant_id,
                    max_datagram,
                    roster,
                })
            }
            TYPE_PARTICIPANT_JOINED => {
                if rest.len() < 10 {
                    return Err(ControlError::BadBody(TYPE_PARTICIPANT_JOINED));
                }
                let mut pid = [0u8; 8];
                pid.copy_from_slice(&rest[..8]);
                let participant_id = u64::from_be_bytes(pid);
                let uid_len = u16::from_be_bytes([rest[8], rest[9]]) as usize;
                if rest.len() < 10 + uid_len {
                    return Err(ControlError::BadBody(TYPE_PARTICIPANT_JOINED));
                }
                let user_id = std::str::from_utf8(&rest[10..10 + uid_len])
                    .map_err(|_| ControlError::BadBody(TYPE_PARTICIPANT_JOINED))?
                    .to_owned();
                Ok(Self::ParticipantJoined {
                    participant_id,
                    user_id,
                })
            }
            TYPE_PARTICIPANT_LEFT => {
                if rest.len() < 8 {
                    return Err(ControlError::BadBody(TYPE_PARTICIPANT_LEFT));
                }
                let mut pid = [0u8; 8];
                pid.copy_from_slice(&rest[..8]);
                Ok(Self::ParticipantLeft {
                    participant_id: u64::from_be_bytes(pid),
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
    fn welcome_roundtrip_with_roster() {
        let msg = ControlMessage::Welcome {
            participant_id: 0xDEAD_BEEF,
            max_datagram: 1200,
            roster: vec![
                RosterEntry {
                    participant_id: 1,
                    user_id: "alice".into(),
                },
                RosterEntry {
                    participant_id: 2,
                    user_id: "bob".into(),
                },
            ],
        };
        let frame = msg.encode();
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), msg);
    }

    #[test]
    fn participant_joined_left_roundtrip() {
        let joined = ControlMessage::ParticipantJoined {
            participant_id: 9,
            user_id: "carol".into(),
        };
        let frame = joined.encode();
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), joined);
        let left = ControlMessage::ParticipantLeft { participant_id: 9 };
        let frame = left.encode();
        assert_eq!(ControlMessage::decode_body(&frame[4..]).unwrap(), left);
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

    /// Exact wire bytes — these hex strings are also pinned in the TypeScript
    /// codec tests (`apps/web/src/lib/control.test.ts`), so a change here must
    /// be mirrored on the web client (plan §7).
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn hello_wire_vector() {
        let msg = ControlMessage::Hello {
            protocol_version: 1,
            grant_jwt: "j".into(),
        };
        assert_eq!(hex(&msg.encode()), "00000005000100016a");
    }

    #[test]
    fn join_room_wire_vector() {
        assert_eq!(hex(&ControlMessage::JoinRoom.encode()), "000000020002");
    }

    #[test]
    fn welcome_empty_roster_wire_vector() {
        let msg = ControlMessage::Welcome {
            participant_id: 1,
            max_datagram: 1200,
            roster: vec![],
        };
        assert_eq!(hex(&msg.encode()), "0000000e0003000000000000000104b00000");
    }

    #[test]
    fn welcome_roster_wire_vector() {
        let msg = ControlMessage::Welcome {
            participant_id: 1,
            max_datagram: 1200,
            roster: vec![
                RosterEntry {
                    participant_id: 1,
                    user_id: "alice".into(),
                },
                RosterEntry {
                    participant_id: 2,
                    user_id: "bob".into(),
                },
            ],
        };
        assert_eq!(
            hex(&msg.encode()),
            "0000002a0003000000000000000104b0000200000000000000010005616c69636500000000000000020003626f62"
        );
    }

    #[test]
    fn participant_joined_wire_vector() {
        let msg = ControlMessage::ParticipantJoined {
            participant_id: 2,
            user_id: "bob".into(),
        };
        assert_eq!(hex(&msg.encode()), "0000000f000400000000000000020003626f62");
    }

    #[test]
    fn participant_left_wire_vector() {
        let msg = ControlMessage::ParticipantLeft { participant_id: 1 };
        assert_eq!(hex(&msg.encode()), "0000000a00050000000000000001");
    }

    #[test]
    fn error_wire_vector() {
        let msg = ControlMessage::Error {
            code: 401,
            message: "x".into(),
        };
        assert_eq!(hex(&msg.encode()), "00000005000f019178");
    }
}
