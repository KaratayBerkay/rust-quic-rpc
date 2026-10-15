//! Grant revocation: in-memory jti denylist + live-session kick (P7c).
//!
//! The auth service records every issued grant jti (`grant_issues` in Nest)
//! and marks it revoked when a host kicks a member. The server polls
//! `GET /auth/revoked` (rvp-cli `--nest-revoked`, same shape as the JWKS
//! refresh) and feeds the list here via [`Revocation::apply`].
//!
//! Two enforcement points, both keyed on the *verified* grant jti:
//! - join: [`Revocation::is_denied`] rejects new handshakes with 403;
//! - live: sessions register at join ([`Revocation::register`]) and get a
//!   watch channel that [`Revocation::apply`] flips to kick them promptly.
//!
//! Deliberately in-memory (plan decision 5): single-process, no Redis for
//! v1 — poll results are the source of truth.

use dashmap::{DashMap, DashSet};
use tokio::sync::watch;

/// Shared per-server revocation state (in `ServerState`, one instance per
/// process serving both QUIC and WebTransport endpoints).
#[derive(Debug, Default)]
pub struct Revocation {
    /// jtis the auth service reported revoked.
    denied: DashSet<String>,
    /// jti → live sessions holding it (`participant_id`, kick watcher).
    sessions: DashMap<String, Vec<(u64, watch::Sender<bool>)>>,
}

impl Revocation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is this grant jti revoked? (`None`/harness jtis are never denied.)
    pub fn is_denied(&self, jti: &str) -> bool {
        self.denied.contains(jti)
    }

    /// Track a live session for `jti` and return the kick watcher.
    ///
    /// The denylist is re-checked *after* the session is inserted so a
    /// concurrent [`Revocation::apply`] cannot slip between "not yet denied"
    /// and "registered": either `apply` sees this session and flips it, or
    /// this re-check sees the jti and flips it before we hand back the
    /// receiver. Both paths yield an already-true receiver.
    pub fn register(&self, jti: String, participant_id: u64) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        self.sessions
            .entry(jti.clone())
            .or_default()
            .push((participant_id, tx));
        if self.denied.contains(&jti) {
            // Kick this late registrant (apply() already drained the list).
            if let Some(entry) = self.sessions.get(&jti) {
                for (_, tx) in entry.value().iter() {
                    let _ = tx.send(true);
                }
            }
        }
        rx
    }

    /// Stop tracking a session (called when the session ends for any reason).
    pub fn unregister(&self, jti: &str, participant_id: u64) {
        if let Some(mut entry) = self.sessions.get_mut(jti) {
            entry.retain(|(pid, _)| *pid != participant_id);
            if entry.is_empty() {
                drop(entry);
                self.sessions.remove(jti);
            }
        }
    }

    /// Ingest a poll result: mark every `jti` revoked and kick the live
    /// sessions holding any *newly* denied jti. Returns how many jtis were
    /// newly denied (0 when nothing changed — idempotent re-polls).
    pub fn apply<I: IntoIterator<Item = String>>(&self, jtis: I) -> usize {
        let mut newly = 0usize;
        for jti in jtis {
            // DashSet::insert returns true only when it was absent — repeated
            // polls of the same list are no-ops (no double kick, no churn).
            if !self.denied.insert(jti.clone()) {
                continue;
            }
            newly += 1;
            if let Some((_, sessions)) = self.sessions.remove(&jti) {
                for (_, tx) in sessions {
                    let _ = tx.send(true);
                }
            }
        }
        newly
    }
}

/// Await a kick signal. `true` (revoked) or a dropped sender both end the
/// session — a sender only disappears when the session is unregistered or
/// the server is tearing down, and closing in both cases is fail-closed.
pub(crate) async fn wait_for_kick(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn denied_before_register_kicks_immediately() {
        let rev = Revocation::new();
        assert_eq!(rev.apply(["jti-1".to_string()]), 1);
        let mut rx = rev.register("jti-1".into(), 1);
        wait_for_kick(&mut rx).await;
        assert!(*rx.borrow());
        assert!(rev.is_denied("jti-1"));
    }

    #[tokio::test]
    async fn register_before_apply_kicks_live_session() {
        let rev = Revocation::new();
        let mut rx = rev.register("jti-2".into(), 7);
        assert!(!*rx.borrow());
        assert_eq!(rev.apply(["jti-2".to_string()]), 1);
        wait_for_kick(&mut rx).await;
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn apply_is_idempotent_and_returns_new_count() {
        let rev = Revocation::new();
        assert_eq!(rev.apply(["a".to_string(), "b".to_string()]), 2);
        assert_eq!(rev.apply(["a".to_string(), "b".to_string()]), 0);
        assert_eq!(rev.apply(["c".to_string()]), 1);
        assert!(rev.is_denied("c"));
    }

    #[tokio::test]
    async fn apply_only_kicks_sessions_for_newly_denied_jti() {
        let rev = Revocation::new();
        let keeper = rev.register("kept".into(), 1);
        let mut denied = rev.register("denied".into(), 2);
        assert_eq!(rev.apply(["denied".to_string()]), 1);
        wait_for_kick(&mut denied).await;
        assert!(!*keeper.borrow());
        // Unrelated sessions are untouched by later polls.
        assert_eq!(rev.apply(["other".to_string()]), 1);
        assert!(!*keeper.borrow());
    }

    #[tokio::test]
    async fn unregister_stops_tracking_only_that_session() {
        let rev = Revocation::new();
        let _a = rev.register("jti".into(), 1);
        let mut b = rev.register("jti".into(), 2);
        rev.unregister("jti", 1);
        // Session 2 must still be kicked by a later apply.
        assert_eq!(rev.apply(["jti".to_string()]), 1);
        wait_for_kick(&mut b).await;
        assert!(*b.borrow());
        rev.unregister("jti", 2);
        assert!(rev.sessions.get("jti").is_none());
    }

    #[tokio::test]
    async fn closed_sender_ends_wait_fail_closed() {
        let rev = Revocation::new();
        let mut rx = rev.register("jti".into(), 1);
        rev.unregister("jti", 1); // drops the sender without a kick
        wait_for_kick(&mut rx).await; // must not hang
    }
}
