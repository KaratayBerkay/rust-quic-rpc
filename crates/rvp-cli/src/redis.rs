//! P8b multi-instance Redis integration (plan §2/§5).
//!
//! Two jobs, both driven by `--redis-url` on the media server:
//! 1. **Directory registration** — the node writes `rvp:node:<id>` with a 30 s
//!    TTL and refreshes it on a heartbeat, so NestJS sees live nodes without
//!    a static list. (Per-node keys, not one hash: a hash has no per-field
//!    TTL, and a single shared key's TTL would let one node hide another.)
//! 2. **Revocation subscription** — the node listens on `rvp:revocations` and
//!    applies published jtis immediately (kicks land in milliseconds instead
//!    of waiting for the 30 s `--nest-revoked` HTTP poll, which still runs as
//!    the reliable fallback).

use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use redis::aio::PubSub;
use redis::AsyncCommands;
use rvp_directory::{node_key, NodeEntry, RevocationEvent, NODE_TTL_SECS, REVOCATION_CHANNEL};
use rvp_server::ServerState;
use tracing::{info, warn};

/// How often the node refreshes its directory TTL (well under the 30 s TTL so
/// a single missed beat does not drop the node).
const REGISTER_REFRESH: Duration = Duration::from_secs(10);

/// Register this media node in the directory and keep its TTL alive.
/// Returns early (leaving the server single-node capable) on a bad URL or a
/// failed first connect.
pub async fn spawn_directory_registration(url: String, node_id: String, address: String) {
    tokio::spawn(async move {
        let client = match redis::Client::open(url.as_str()) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "redis directory registration disabled (bad url)");
                return;
            }
        };
        let manager = match client.get_connection_manager().await {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "redis directory registration disabled (connect failed)");
                return;
            }
        };
        let entry = NodeEntry {
            id: node_id.clone(),
            address,
        };
        let json = match serde_json::to_string(&entry) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "cannot serialize node entry");
                return;
            }
        };
        let key = node_key(&node_id);
        let mut ticker = tokio::time::interval(REGISTER_REFRESH);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let mut conn = manager.clone();
            let res: redis::RedisResult<()> = conn.set_ex(&key, &json, NODE_TTL_SECS).await;
            match res {
                Ok(()) => info!(node_id = %node_id, "directory registration refreshed"),
                Err(e) => warn!(error = %e, "directory registration failed"),
            }
        }
    });
}

/// Subscribe to the revocation channel and apply published jtis immediately.
/// Falls back to the existing `--nest-revoked` HTTP poll (which the caller
/// still spawns) when Redis is unavailable.
pub async fn spawn_revocation_subscription(url: String, state: Arc<ServerState>) {
    tokio::spawn(async move {
        loop {
            match subscribe_once(&url, &state).await {
                Ok(()) => info!("revocation subscription ended; reconnecting"),
                Err(e) => warn!(error = %e, "revocation subscription failed; reconnecting"),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// One subscription session: connect, subscribe, apply until the stream ends
/// or errors. Returns `Ok` when the stream ended cleanly (caller reconnects).
async fn subscribe_once(url: &str, state: &ServerState) -> Result<(), String> {
    let client = redis::Client::open(url).map_err(|e| format!("parse redis url: {e}"))?;
    let mut pubsub: PubSub = client
        .get_async_pubsub()
        .await
        .map_err(|e| format!("pubsub connect: {e}"))?;
    pubsub
        .subscribe(REVOCATION_CHANNEL)
        .await
        .map_err(|e| format!("subscribe: {e}"))?;
    info!(channel = %REVOCATION_CHANNEL, "subscribed to revocation channel");
    let mut stream = pin!(pubsub.on_message());
    while let Some(msg) = stream.next().await {
        let payload: String = msg.get_payload().unwrap_or_default();
        match serde_json::from_str::<RevocationEvent>(&payload) {
            Ok(event) => {
                let newly = state.revocation.apply(event.jtis);
                if newly > 0 {
                    warn!(newly, "revoked jtis applied via redis");
                }
            }
            Err(e) => warn!(error = %e, "bad revocation payload"),
        }
    }
    Ok(())
}
