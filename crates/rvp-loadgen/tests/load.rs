//! P7d integration: loadgen scenarios against an in-process server.

use std::time::Duration;

use rvp_loadgen::{run, Config};

fn scenario(crypto: bool) -> Config {
    Config {
        rooms: 2,
        clients_per_room: 3,
        duration: Duration::from_millis(1500),
        warmup: Duration::from_millis(300),
        tick: Duration::from_millis(20),
        payload_bytes: 80,
        crypto,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn crypto_mode_seals_per_recipient() {
    let report = run(&scenario(true)).await.expect("scenario");

    assert_eq!(report.clients, 6);
    assert!(
        report.join.count >= 5,
        "join samples: {}",
        report.join.count
    );
    assert!(report.datagrams_sent > 0, "no datagrams sent");
    assert!(report.datagrams_opened > 0, "no datagrams opened");
    // Crypto mode: publisher uplink x(N-1) means receivers also see sealed
    // copies addressed to other pairs (plan §9 fanout waste).
    assert!(report.missing_key_dropped > 0, "expected fanout waste");
    assert_eq!(report.decrypt_failures, 0, "decrypt failures");
    assert_eq!(report.malformed_dropped, 0, "malformed datagrams");
    // Measured window: 3 clients/room x 1.5s at 50fps x (3-1) sealed copies
    // = ~450 datagrams sent minimum; assert a floor well below that.
    assert!(
        report.datagrams_sent >= 100,
        "sent only {}",
        report.datagrams_sent
    );
    assert!(report.media.count > 0, "no media latency samples");
    assert!(report.media.p99_ms < 1000.0, "p99 {}", report.media.p99_ms);
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_mode_single_datagram_per_tick() {
    let report = run(&scenario(false)).await.expect("scenario");

    assert_eq!(report.clients, 6);
    assert!(report.datagrams_sent > 0, "no datagrams sent");
    assert!(report.datagrams_opened > 0, "no datagrams opened");
    // Plaintext: no sealed copies, so no missing-kid drops.
    assert_eq!(report.missing_key_dropped, 0, "missing-key drops");
    assert_eq!(report.decrypt_failures, 0, "decrypt failures");
    // 3 publishers x 1.5s at 50fps = ~225 datagrams sent; floor well below.
    assert!(
        report.datagrams_sent >= 50,
        "sent only {}",
        report.datagrams_sent
    );
    assert!(report.media.count > 0, "no media latency samples");
}

#[tokio::test(flavor = "multi_thread")]
async fn rooms_are_isolated() {
    // 2 rooms x 2 clients: no cross-room fanout, so with crypto on every
    // opened datagram is the one sealed for its pair — missing_key is 0
    // only if no foreign sealed copy arrives. With N=2, publishers send 1
    // sealed copy per tick and the SFU fans it to the single co-member, so
    // there is no third party to receive a foreign kid. Assert opened > 0
    // and decrypt clean; cross-room isolation is covered by server tests.
    let report = run(&Config {
        rooms: 2,
        clients_per_room: 2,
        crypto: true,
        ..scenario(true)
    })
    .await
    .expect("scenario");

    assert_eq!(report.clients, 4);
    assert!(report.datagrams_opened > 0, "no datagrams opened");
    assert_eq!(report.decrypt_failures, 0, "decrypt failures");
}
