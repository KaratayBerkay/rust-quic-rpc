//! P7b hardening (plan §4 bounded ingress, §10): admission control.
//!
//! Three independent defenses, checked in this order:
//! 1. **QUIC Retry** (stateless, in each accept loop) — proves the peer can
//!    receive packets at its source address before any per-address state
//!    exists, so spoofed-source floods cannot allocate anything.
//! 2. **Connection caps** — at most `max_conn_per_ip` handshakes per source IP
//!    and `max_conn_per_user` sessions per grant subject, released by RAII.
//! 3. **Ingress limiter** — per-participant token buckets enforce the datagram
//!    size cap plus pps/bps ceilings on the fanout hot path.

use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::DEFAULT_MAX_DATAGRAM;

/// Admission-control tunables. Defaults are sized for a small live room:
/// every existing flow stays far below them while a flood cannot amplify.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest accepted media datagram (also advertised in `Welcome`).
    pub max_datagram: usize,
    /// Sustained ingress ceiling in packets per second.
    pub ingress_pps: f64,
    /// Sustained ingress ceiling in bytes per second.
    pub ingress_bps: f64,
    /// Burst window (seconds) for both ingress buckets.
    pub burst_secs: f64,
    /// Concurrent connections from one source IP (either endpoint).
    pub max_conn_per_ip: usize,
    /// Concurrent sessions per user (grant subject).
    pub max_conn_per_user: usize,
    /// Send a QUIC Retry packet before admitting unvalidated peers.
    pub enable_retry: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_datagram: DEFAULT_MAX_DATAGRAM,
            ingress_pps: 200.0,
            ingress_bps: 150_000.0,
            burst_secs: 1.0,
            max_conn_per_ip: 32,
            max_conn_per_user: 8,
            enable_retry: true,
        }
    }
}

/// Fixed-capacity token bucket. `try_acquire` takes an explicit `now` so the
/// refill math is deterministic in tests.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// `min_capacity` is the largest single cost the bucket must ever admit;
    /// it keeps a small `rate × burst` configuration from wedging permanently
    /// below one maximum-size item.
    pub fn new(rate_per_sec: f64, burst_secs: f64, min_capacity: f64) -> Self {
        let capacity = (rate_per_sec * burst_secs).max(min_capacity).max(1.0);
        Self {
            capacity,
            refill_per_sec: rate_per_sec.max(0.0),
            tokens: capacity,
            last: Instant::now(),
        }
    }

    /// Consume `cost` tokens as of `now`. `false` when the bucket is dry.
    pub fn try_acquire(&mut self, now: Instant, cost: f64) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

/// Per-participant ingress gate for the fanout hot path (one per session).
pub struct IngressLimiter {
    pps: TokenBucket,
    bps: TokenBucket,
    max_datagram: usize,
}

impl IngressLimiter {
    pub fn new(limits: &Limits) -> Self {
        Self {
            pps: TokenBucket::new(limits.ingress_pps, limits.burst_secs, 1.0),
            bps: TokenBucket::new(
                limits.ingress_bps,
                limits.burst_secs,
                limits.max_datagram as f64,
            ),
            max_datagram: limits.max_datagram,
        }
    }

    /// Admit one datagram of `len` bytes as of `now`. The size cap is checked
    /// first so an oversized frame never consumes rate budget. On `Err` the
    /// reason doubles as the `rvp_datagrams_dropped_total` label.
    pub fn admit(&mut self, now: Instant, len: usize) -> Result<(), &'static str> {
        if len > self.max_datagram {
            return Err("oversized");
        }
        if !self.pps.try_acquire(now, 1.0) {
            return Err("rate_pps");
        }
        if !self.bps.try_acquire(now, len as f64) {
            return Err("rate_bps");
        }
        Ok(())
    }
}

/// Which admission counter a [`ConnGuard`] is holding.
enum Slot {
    Ip(IpAddr),
    User(String),
}

/// Process-wide connection counters, shared by the QUIC and WebTransport
/// endpoints. Counts are exact under concurrent acquires (DashMap shard locks
/// cover the check-and-increment).
pub struct ConnLimiter {
    per_ip: DashMap<IpAddr, usize>,
    per_user: DashMap<String, usize>,
    max_conn_per_ip: usize,
    max_conn_per_user: usize,
}

impl ConnLimiter {
    pub fn new(limits: &Limits) -> Arc<Self> {
        Arc::new(Self {
            per_ip: DashMap::new(),
            per_user: DashMap::new(),
            max_conn_per_ip: limits.max_conn_per_ip,
            max_conn_per_user: limits.max_conn_per_user,
        })
    }

    /// Reserve one slot against the source-IP cap.
    pub fn try_acquire_ip(self: Arc<Self>, ip: IpAddr) -> Option<ConnGuard> {
        if !Self::bump(&self.per_ip, ip, self.max_conn_per_ip) {
            return None;
        }
        Some(ConnGuard {
            limiter: self,
            slot: Slot::Ip(ip),
        })
    }

    /// Reserve one slot against the per-user cap (after grant verification).
    pub fn try_acquire_user(self: Arc<Self>, user: &str) -> Option<ConnGuard> {
        if !Self::bump(&self.per_user, user.to_owned(), self.max_conn_per_user) {
            return None;
        }
        Some(ConnGuard {
            limiter: self,
            slot: Slot::User(user.to_owned()),
        })
    }

    fn bump<K: Eq + Hash>(map: &DashMap<K, usize>, key: K, max: usize) -> bool {
        if max == 0 {
            return false;
        }
        let mut count = map.entry(key).or_insert(0);
        if *count >= max {
            return false;
        }
        *count += 1;
        true
    }
}

/// RAII lease on one connection slot; the count drops (and empty entries are
/// removed) when the owning connection task ends.
pub struct ConnGuard {
    limiter: Arc<ConnLimiter>,
    slot: Slot,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        match &self.slot {
            Slot::Ip(ip) => release(&self.limiter.per_ip, *ip),
            Slot::User(user) => release(&self.limiter.per_user, user.clone()),
        }
    }
}

fn release<K: Eq + Hash>(map: &DashMap<K, usize>, key: K) {
    if let Entry::Occupied(mut entry) = map.entry(key) {
        let empty = {
            let count = entry.get_mut();
            *count = count.saturating_sub(1);
            *count == 0
        };
        if empty {
            entry.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bucket_exhausts_then_refills() {
        let mut bucket = TokenBucket::new(10.0, 1.0, 1.0); // capacity 10
        let now = Instant::now();
        for _ in 0..10 {
            assert!(bucket.try_acquire(now, 1.0), "burst must admit capacity");
        }
        assert!(!bucket.try_acquire(now, 1.0), "bucket must be dry");

        let later = now + Duration::from_millis(101);
        assert!(bucket.try_acquire(later, 1.0), "refill over ~100ms");
        assert!(!bucket.try_acquire(later, 1.0), "only ~1 token refilled");
    }

    #[test]
    fn bucket_never_wedged_below_a_single_max_cost() {
        // rate × burst (10) is smaller than the largest item (1200).
        let mut bucket = TokenBucket::new(10.0, 1.0, 1200.0);
        assert!(bucket.try_acquire(Instant::now(), 1200.0));
    }

    #[test]
    fn oversized_never_consumes_rate_budget() {
        let limits = Limits {
            ingress_pps: 1.0,
            ..Limits::default()
        };
        let mut limiter = IngressLimiter::new(&limits);
        let now = Instant::now();
        assert_eq!(
            limiter.admit(now, limits.max_datagram + 1),
            Err("oversized")
        );
        // The pps bucket still holds its single token for a valid datagram.
        assert!(limiter.admit(now, 100).is_ok());
        assert_eq!(limiter.admit(now, 100), Err("rate_pps"));
    }

    #[test]
    fn bps_bucket_enforces_byte_ceiling() {
        let limits = Limits {
            ingress_pps: 1_000.0,
            ingress_bps: 1_500.0,
            burst_secs: 1.0,
            max_datagram: 1200,
            ..Limits::default()
        };
        let mut limiter = IngressLimiter::new(&limits);
        let now = Instant::now();
        assert!(limiter.admit(now, 1_200).is_ok());
        assert_eq!(limiter.admit(now, 1_200), Err("rate_bps"));
    }

    #[test]
    fn ip_slot_blocks_then_releases() {
        let limits = Limits {
            max_conn_per_ip: 1,
            ..Limits::default()
        };
        let conns = ConnLimiter::new(&limits);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let other: IpAddr = "10.0.0.9".parse().unwrap();

        let first = Arc::clone(&conns).try_acquire_ip(ip);
        assert!(first.is_some());
        assert!(Arc::clone(&conns).try_acquire_ip(ip).is_none());
        assert!(
            Arc::clone(&conns).try_acquire_ip(other).is_some(),
            "other addresses are independent"
        );

        drop(first);
        assert!(Arc::clone(&conns).try_acquire_ip(ip).is_some());
    }

    #[test]
    fn user_slot_blocks_then_releases() {
        let limits = Limits {
            max_conn_per_user: 1,
            ..Limits::default()
        };
        let conns = ConnLimiter::new(&limits);

        let alice = Arc::clone(&conns).try_acquire_user("alice");
        assert!(alice.is_some());
        assert!(Arc::clone(&conns).try_acquire_user("alice").is_none());
        assert!(Arc::clone(&conns).try_acquire_user("bob").is_some());

        drop(alice);
        assert!(Arc::clone(&conns).try_acquire_user("alice").is_some());
    }

    #[test]
    fn default_limits_admit_a_typical_datagram() {
        let mut limiter = IngressLimiter::new(&Limits::default());
        let now = Instant::now();
        assert!(limiter.admit(now, DEFAULT_MAX_DATAGRAM).is_ok());
    }
}
