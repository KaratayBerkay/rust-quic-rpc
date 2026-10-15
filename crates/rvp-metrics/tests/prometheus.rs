//! Integration: exercise the P7a instrumentation helpers and assert the
//! Prometheus exposition (both rendered text and the HTTP `/metrics` scrape)
//! exposes every metric family the media server emits.
//!
//! The `metrics` crate uses a process-global recorder, so this crate's tests
//! share one install. They are grouped into a single test to avoid a
//! `FailedToSetGlobalRecorder` clash.

#[tokio::test]
async fn p7a_metrics_render_and_serve() {
    let server = rvp_metrics::install("127.0.0.1:0".parse().unwrap())
        .await
        .expect("install metrics");
    rvp_metrics::instrument::fanout_datagram(1200, 3);
    rvp_metrics::instrument::participant_joined();
    rvp_metrics::instrument::participant_left();
    rvp_metrics::instrument::room_created();
    rvp_metrics::instrument::room_garbage_collected();
    rvp_metrics::instrument::handshake_ok(0.012);
    rvp_metrics::instrument::handshake_failed("unauthorized");
    rvp_metrics::instrument::control_error(401);
    rvp_metrics::instrument::room_full();
    rvp_metrics::instrument::connection_opened("quic");
    rvp_metrics::instrument::connection_closed("quic");
    rvp_metrics::instrument::datagram_dropped("oversized");
    rvp_metrics::instrument::datagram_dropped("rate_pps");
    rvp_metrics::instrument::datagram_dropped("rate_bps");
    rvp_metrics::instrument::connection_rejected("ip_cap");
    rvp_metrics::instrument::connection_rejected("user_cap");
    rvp_metrics::instrument::retry_sent();
    rvp_metrics::instrument::key_update_relayed();
    rvp_metrics::instrument::session_revoked();

    let text = server.render();
    for needle in [
        "rvp_fanout_datagrams_total",
        "rvp_fanout_datagram_bytes_total",
        "rvp_fanout_datagram_bytes",
        "rvp_fanout_copies_total",
        "rvp_participants_joined_total",
        "rvp_participants_left_total",
        "rvp_participants",
        "rvp_rooms_created_total",
        "rvp_rooms",
        "rvp_handshakes_total",
        "rvp_handshake_seconds",
        "rvp_control_errors_total",
        "rvp_room_full_total",
        "rvp_connections_total",
        "rvp_connections",
        "rvp_datagrams_dropped_total",
        "rvp_connections_rejected_total",
        "rvp_quic_retries_total",
        "rvp_key_updates_total",
        "rvp_sessions_revoked_total",
    ] {
        assert!(
            text.contains(needle),
            "missing metric family {needle}:\n{text}"
        );
    }

    let addr = server.local_addr();
    let url = format!("http://{addr}/metrics");
    let response = reqwest::get(&url).await.expect("GET /metrics");
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("body");
    assert!(
        body.contains("rvp_fanout_datagrams_total"),
        "exposition missing fanout counter:\n{body}"
    );
    server.shutdown().await;
}
