//! RVP server observability (plan §9, P7a).
//!
//! Wraps the [`metrics`] facade + [`metrics-exporter-prometheus`] so the media
//! server exposes a scrapeable `/metrics` endpoint. The `metrics` crate uses a
//! process-global recorder, so [`install`] must be called exactly once per
//! process (the binary wires it; tests can skip it and rely on the no-op
//! fallback).
//!
//! Metric conventions (plan §9: CPU/participant, RSS/connection, pps, e2e
//! latency, loss/jitter, bandwidth, p50/p95/p99):
//! - Names are snake_case with an `rvp_` prefix.
//! - Counters are monotonic; gauges track instantaneous size; histograms
//!   record durations in seconds and sizes in bytes.
//! - Fanout samples arrival time on ingress (plan §3.2: arrival-time stamping
//!   lives on the metrics path only, never in the media header).

use std::net::SocketAddr;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("prometheus exporter: {0}")]
    Prometheus(#[from] metrics_exporter_prometheus::BuildError),
    #[error("bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
}

/// Handle to the running exporter. Cloning is cheap; used to render the
/// exposition text in tests and by the HTTP endpoint.
pub struct MetricsServer {
    handle: PrometheusHandle,
    local_addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl MetricsServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Render the current exposition format (Prometheus text).
    pub fn render(&self) -> String {
        self.handle.render()
    }

    /// Stop the HTTP listener.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Install the process-global Prometheus recorder and serve `/metrics` on
/// `addr` (use port 0 for ephemeral). Idempotent-ish: a second call replaces
/// the recorder, which is fine for tests but the binary should call it once.
pub async fn install(addr: SocketAddr) -> Result<MetricsServer, MetricsError> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full("rvp_fanout_datagram_bytes".to_owned()),
            &[64.0, 128.0, 256.0, 512.0, 1024.0, 1200.0],
        )
        .unwrap_or_else(|e| {
            // Bucket misconfiguration is a build-time bug; keep the default
            // buckets rather than failing startup.
            tracing::warn!(error = %e, "fanout byte buckets rejected; using defaults");
            PrometheusBuilder::new()
        })
        .install_recorder()?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| MetricsError::Bind { addr, source })?;
    let local_addr = listener
        .local_addr()
        .map_err(|source| MetricsError::Bind { addr, source })?;
    let task = tokio::spawn(serve(handle.clone(), listener));
    info!(%local_addr, "metrics endpoint up (GET /metrics)");
    Ok(MetricsServer {
        handle,
        local_addr,
        task,
    })
}

async fn serve(handle: PrometheusHandle, listener: tokio::net::TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let body = handle.render();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; \
                 charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
        });
    }
}

/// Instrumentation helpers — thin wrappers so call sites stay readable and
/// metric names stay consistent.
pub mod instrument {
    /// Fanout: one media datagram was received and forwarded to `peers` others.
    pub fn fanout_datagram(bytes: usize, peers: usize) {
        metrics::counter!("rvp_fanout_datagrams_total").increment(1);
        metrics::counter!("rvp_fanout_datagram_bytes_total").increment(bytes as u64);
        metrics::histogram!("rvp_fanout_datagram_bytes").record(bytes as f64);
        metrics::counter!("rvp_fanout_copies_total").increment(peers as u64);
    }

    /// A participant successfully joined a room.
    pub fn participant_joined() {
        metrics::counter!("rvp_participants_joined_total").increment(1);
        metrics::gauge!("rvp_participants").increment(1.0);
    }

    /// A participant left (or was removed).
    pub fn participant_left() {
        metrics::counter!("rvp_participants_left_total").increment(1);
        metrics::gauge!("rvp_participants").decrement(1.0);
    }

    /// A room was created (first join) and garbage-collected (last leave).
    pub fn room_created() {
        metrics::counter!("rvp_rooms_created_total").increment(1);
        metrics::gauge!("rvp_rooms").increment(1.0);
    }

    pub fn room_garbage_collected() {
        metrics::gauge!("rvp_rooms").decrement(1.0);
    }

    /// Handshake outcome (plan §3.4). `latency` is seconds from Hello to Welcome.
    pub fn handshake_ok(latency_secs: f64) {
        metrics::counter!("rvp_handshakes_total", "outcome" => "ok").increment(1);
        metrics::histogram!("rvp_handshake_seconds").record(latency_secs);
    }

    pub fn handshake_failed(reason: &'static str) {
        metrics::counter!("rvp_handshakes_total", "outcome" => reason).increment(1);
    }

    /// Control-plane error frame written to a client.
    pub fn control_error(code: u16) {
        metrics::counter!("rvp_control_errors_total", "code" => code.to_string()).increment(1);
    }

    /// Room capacity rejection.
    pub fn room_full() {
        metrics::counter!("rvp_room_full_total").increment(1);
    }

    /// Connection accepted (QUIC or WebTransport) and closed.
    pub fn connection_opened(transport: &'static str) {
        metrics::counter!("rvp_connections_total", "transport" => transport).increment(1);
        metrics::gauge!("rvp_connections", "transport" => transport).increment(1.0);
    }

    pub fn connection_closed(transport: &'static str) {
        metrics::gauge!("rvp_connections", "transport" => transport).decrement(1.0);
    }

    /// Ingress datagram dropped by a P7b hardening rule (`oversized`,
    /// `rate_pps`, `rate_bps`).
    pub fn datagram_dropped(reason: &'static str) {
        metrics::counter!("rvp_datagrams_dropped_total", "reason" => reason).increment(1);
    }

    /// Connection rejected before/at admission (`ip_cap`, `user_cap`).
    pub fn connection_rejected(reason: &'static str) {
        metrics::counter!("rvp_connections_rejected_total", "reason" => reason).increment(1);
    }

    /// QUIC Retry packet sent to validate the peer address (plan §10).
    pub fn retry_sent() {
        metrics::counter!("rvp_quic_retries_total").increment(1);
    }

    /// A `KeyUpdate { kid }` announce was relayed to the rest of the room (P7c).
    pub fn key_update_relayed() {
        metrics::counter!("rvp_key_updates_total").increment(1);
    }

    /// A joined session was closed because its grant jti was revoked (P7c).
    pub fn session_revoked() {
        metrics::counter!("rvp_sessions_revoked_total").increment(1);
    }
}
