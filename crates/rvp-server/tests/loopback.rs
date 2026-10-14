//! P1 integration: QUIC DATAGRAM loopback against the echo server.

use std::net::{Ipv4Addr, SocketAddr};

use bytes::Bytes;
use quinn::Endpoint;
use rvp_proto::{MediaDatagram, MediaHeader, HEADER_LEN};
use rvp_server::test_utils::insecure_client_config;

async fn connect_client(server: SocketAddr) -> (quinn::Connection, Endpoint) {
    let client_cfg = insecure_client_config();
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_cfg);
    let conn = endpoint
        .connect(server, "localhost")
        .expect("connect")
        .await
        .expect("handshake");
    (conn, endpoint)
}

#[tokio::test]
async fn datagram_echo_roundtrip() {
    let server = rvp_server::start_echo_server(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("start server");
    let (conn, _endpoint) = connect_client(server.local_addr()).await;

    let max = conn.max_datagram_size().expect("datagram support");
    assert!(max >= HEADER_LEN, "peer must allow at least a header");

    let payload = Bytes::from_static(b"hello-rvp-p1");
    let raw = MediaDatagram::build(
        MediaHeader {
            sequence: 42,
            ..Default::default()
        },
        payload.clone(),
    );
    conn.send_datagram(raw.clone()).expect("send datagram");

    let echoed = tokio::time::timeout(std::time::Duration::from_secs(5), conn.read_datagram())
        .await
        .expect("echo timeout")
        .expect("echo recv");
    assert_eq!(echoed, raw, "echo must be byte-identical");

    let parsed = MediaDatagram::parse(echoed).unwrap();
    assert_eq!(parsed.header.sequence, 42);
    assert_eq!(parsed.payload, payload);

    server.shutdown().await;
}

#[tokio::test]
async fn many_datagrams_preserved() {
    let server = rvp_server::start_echo_server(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("start server");
    let (conn, _endpoint) = connect_client(server.local_addr()).await;

    const N: u32 = 50;
    for i in 0..N {
        let raw = MediaDatagram::build(
            MediaHeader {
                sequence: i,
                timestamp: i * 960, // 20 ms @ 48 kHz
                ..Default::default()
            },
            Bytes::from(format!("frame-{i}")),
        );
        conn.send_datagram(raw).expect("send");
    }

    let mut seen = Vec::with_capacity(N as usize);
    for _ in 0..N {
        let d = tokio::time::timeout(std::time::Duration::from_secs(5), conn.read_datagram())
            .await
            .expect("recv timeout")
            .expect("recv");
        seen.push(MediaDatagram::parse(d).unwrap().header.sequence);
    }
    seen.sort_unstable();
    assert_eq!(seen, (0..N).collect::<Vec<_>>(), "all sequences echoed");

    server.shutdown().await;
}
