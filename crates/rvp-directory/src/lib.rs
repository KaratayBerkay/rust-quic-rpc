//! Shared multi-instance directory protocol (plan §2/§5, P8).
//!
//! One place defines the Redis keys, channels and JSON shapes used by both
//! sides of the control plane:
//! - the **media plane** (`rvp-server`, `rvp-cli`) — registers its node,
//!   subscribes for revocation kicks;
//! - the **control plane** (`services/nest`) — reads the node list, publishes
//!   revocation events.
//!
//! Keeping the wire format here (mirrored in `services/nest/src/redis/keys.ts`)
//! means Rust and TS cannot drift without a compile/test failure on the side
//! that changed.
//!
//! **Redis is optional.** With no `REDIS_URL` every component falls back to
//! the P8a behaviour (static/polled node list, 30 s HTTP revocation poll), so
//! a single-node deployment needs no Redis at all.

/// Prefix for per-node directory keys (`rvp:node:<id>`). Each key holds the
/// node's JSON entry with its own TTL, so one node's heartbeat never extends
/// another's liveness (a single shared hash has no per-field TTL).
pub const NODE_KEY_PREFIX: &str = "rvp:node:";

/// Glob matching every live node key (for `SCAN`/`KEYS` in the reader).
pub const NODE_KEY_GLOB: &str = "rvp:node:*";

/// Channel carrying revocation events (`{"jtis":[...]}`, one publish per kick).
pub const REVOCATION_CHANNEL: &str = "rvp:revocations";

/// Per-node registration TTL in seconds: a node that stops heartbeating
/// disappears from the directory after this long.
pub const NODE_TTL_SECS: u64 = 30;

/// Build the directory key for one node id.
pub fn node_key(id: &str) -> String {
    format!("{NODE_KEY_PREFIX}{id}")
}

/// One live media node (P8a pin target, P8b directory entry).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeEntry {
    /// Media-node identity (`rvp-cli --node-id`).
    pub id: String,
    /// `host:port` clients dial (QUIC/UDP + optional WebTransport).
    pub address: String,
}

/// Revocation event published on [`REVOCATION_CHANNEL`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevocationEvent {
    /// Newly revoked grant jtis (the media servers apply them immediately).
    pub jtis: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_key_prefixes_id() {
        assert_eq!(node_key("node-a"), "rvp:node:node-a");
    }

    #[test]
    fn node_entry_round_trips() {
        let node = NodeEntry {
            id: "node-a".into(),
            address: "127.0.0.1:4433".into(),
        };
        let json = serde_json::to_string(&node).unwrap();
        let back: NodeEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, node);
    }

    #[test]
    fn revocation_event_round_trips() {
        let event = RevocationEvent {
            jtis: vec!["jti-1".into(), "jti-2".into()],
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: RevocationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
        assert!(json.contains("jti-1"));
    }
}
