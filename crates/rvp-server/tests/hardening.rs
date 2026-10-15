//! P7b integration: server hardening — datagram size cap, ingress rate limit,
//! per-IP/per-user connection caps, and QUIC Retry transparency.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use quinn::Endpoint;
use rvp_proto::control::{error_code, ControlMessage, PROTOCOL_VERSION};
use rvp_proto::{MediaDatagram, MediaHeader};
use rvp_server::test_utils::insecure_client_config;
use rvp_server::{
    server_config_self_signed, start_server, start_server_with_state, Limits, RvpServer,
    ServerState,
};

#[derive(serde::Serialize)]
struct TestClaims {
    sub: String,
    room_id: String,
    role: String,
    iss: String,
    aud: String,
    exp: u64,
    jti: Option<String>,
}

struct TestAuthority {
    key_pair: rcgen::KeyPair,
    verifier: Arc<rvp_auth::Verifier>,
}

fn authority() -> TestAuthority {
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let verifier =
        rvp_auth::Verifier::from_public_pem(key_pair.public_key_pem().as_bytes()).unwrap();
    TestAuthority {
        key_pair,
        verifier: Arc::new(verifier),
    }
}

fn grant(authority: &TestAuthority, user: &str, room: &str) -> String {
    let claims = TestClaims {
        sub: user.to_owned(),
        room_id: room.to_owned(),
        role: "host".to_owned(),
        iss: "nest".to_owned(),
        aud: "rvp".to_owned(),
        exp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120,
        jti: Some("test-jti".to_owned()),
    };
    let key = EncodingKey::from_ed_pem(authority.key_pair.serialize_pem().as_bytes()).unwrap();
    encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap()
}

/// A connected client with its control stream kept open for events.
struct Client {
    conn: quinn::Connection,
    #[allow(dead_code)]
    control: quinn::RecvStream,
}

/// Connect without panicking — used where the server is expected to refuse.
async fn try_connect(server: SocketAddr) -> Result<quinn::Connection, quinn::ConnectionError> {
    let client_cfg = insecure_client_config();
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_cfg);
    endpoint
        .connect(server, "localhost")
        .expect("connect")
        .await
}

async fn connect(server: SocketAddr) -> quinn::Connection {
    try_connect(server).await.expect("handshake")
}

async fn read_frame(recv: &mut quinn::RecvStream) -> ControlMessage {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("frame len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("frame body");
    ControlMessage::decode_body(&body).expect("decode frame")
}

/// P3 handshake: Hello + JoinRoom → returns Welcome (control stream stays open).
async fn join(conn: quinn::Connection, jwt: &str) -> (Client, ControlMessage) {
    let (mut send, recv) = conn.open_bi().await.expect("open control");
    send.write_all(
        &ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            grant_jwt: jwt.to_owned(),
        }
        .encode(),
    )
    .await
    .expect("hello");
    send.write_all(&ControlMessage::JoinRoom.encode())
        .await
        .expect("join");
    let mut control = recv;
    let welcome = read_frame(&mut control).await;
    (Client { conn, control }, welcome)
}

async fn start_test_server(authority: &TestAuthority) -> (RvpServer, SocketAddr) {
    let server = start_server(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        authority.verifier.clone(),
    )
    .await
    .expect("start server");
    let addr = server.local_addr();
    (server, addr)
}

async fn start_limited_server(
    authority: &TestAuthority,
    limits: Limits,
) -> (RvpServer, SocketAddr) {
    let state = Arc::new(ServerState::with_limits(authority.verifier.clone(), limits));
    let server = start_server_with_state(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        state,
    )
    .await
    .expect("start limited server");
    let addr = server.local_addr();
    (server, addr)
}

fn datagram(seq: u32, payload: &str) -> Bytes {
    MediaDatagram::build(
        MediaHeader {
            sequence: seq,
            ..Default::default()
        },
        Bytes::from(payload.to_owned()),
    )
}

async fn recv_datagram(conn: &quinn::Connection) -> Bytes {
    tokio::time::timeout(Duration::from_secs(5), conn.read_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv")
}

/// No datagram should arrive within this window.
async fn assert_no_datagram(conn: &quinn::Connection) {
    let res = tokio::time::timeout(Duration::from_millis(300), conn.read_datagram()).await;
    assert!(res.is_err(), "unexpected datagram received");
}

/// Connect + join until it succeeds or `deadline` passes (slot release probes).
async fn try_rejoin(server: SocketAddr, jwt: String, deadline: Duration) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        let attempt = async {
            let Ok(conn) = try_connect(server).await else {
                return false;
            };
            let (client, welcome) = join(conn, &jwt).await;
            let ok = matches!(welcome, ControlMessage::Welcome { .. });
            if !ok {
                client.conn.close(0u32.into(), b"not welcome");
            }
            ok
        };
        if tokio::time::timeout(Duration::from_millis(500), attempt)
            .await
            .is_ok_and(|ok| ok)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn oversized_datagram_dropped() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let (alice, welcome_a) = join(
        connect(addr).await,
        &grant(&authority, "alice", "room-size"),
    )
    .await;
    match welcome_a {
        ControlMessage::Welcome { max_datagram, .. } => {
            assert_eq!(max_datagram as usize, rvp_server::DEFAULT_MAX_DATAGRAM);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
    let (bob, _) = join(connect(addr).await, &grant(&authority, "bob", "room-size")).await;

    // 1300 > DEFAULT_MAX_DATAGRAM (1200): dropped by the ingress limiter
    // (the test client MTU is raised to 1450 so the send itself succeeds).
    alice
        .conn
        .send_datagram(Bytes::from(vec![0xEE; 1300]))
        .expect("client MTU must allow a 1300-byte datagram");
    assert_no_datagram(&bob.conn).await;

    // The cap is size-based, not a session kill: valid traffic still fans out.
    alice.conn.send_datagram(datagram(1, "within-cap")).unwrap();
    let parsed = MediaDatagram::parse(recv_datagram(&bob.conn).await).unwrap();
    assert_eq!(&parsed.payload[..], b"within-cap");

    server.shutdown().await;
}

#[tokio::test]
async fn ingress_flood_is_rate_limited() {
    let authority = authority();
    let limits = Limits {
        ingress_pps: 5.0,
        ..Limits::default()
    };
    let (server, addr) = start_limited_server(&authority, limits).await;

    let (alice, _) = join(
        connect(addr).await,
        &grant(&authority, "alice", "room-flood"),
    )
    .await;
    let (bob, _) = join(connect(addr).await, &grant(&authority, "bob", "room-flood")).await;

    for seq in 0..100u32 {
        let _ = alice.conn.send_datagram(datagram(seq, "flood"));
    }

    // Count what survived until the receiver goes quiet for 400ms.
    let mut received = 0usize;
    while let Ok(Ok(_)) =
        tokio::time::timeout(Duration::from_millis(400), bob.conn.read_datagram()).await
    {
        received += 1;
        if received >= 100 {
            break;
        }
    }
    assert!(received >= 1, "burst must admit the first datagrams");
    assert!(
        received <= 20,
        "pps cap must drop most of the flood, got {received}/100"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn per_ip_connection_cap_and_release() {
    let authority = authority();
    let limits = Limits {
        max_conn_per_ip: 2,
        ..Limits::default()
    };
    let (server, addr) = start_limited_server(&authority, limits).await;

    let (_alice, _) = join(connect(addr).await, &grant(&authority, "alice", "room-ip")).await;
    let (_bob, _) = join(connect(addr).await, &grant(&authority, "bob", "room-ip")).await;

    // Third handshake from the same source IP is refused at admission
    // (quinn answers CONNECTION_REFUSED, so this fails within ~1 RTT).
    match tokio::time::timeout(Duration::from_secs(6), try_connect(addr)).await {
        Ok(Ok(_)) => panic!("third connection from one IP must be refused"),
        Ok(Err(_)) => {}
        Err(_) => panic!("refusal did not arrive within 6s"),
    }

    // Closing a session releases its slot for a new connection.
    _alice.conn.close(0u32.into(), b"done");
    assert!(
        try_rejoin(
            addr,
            grant(&authority, "carol", "room-ip"),
            Duration::from_secs(5)
        )
        .await,
        "per-IP slot must be released after close"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn per_user_cap_sends_connection_limit() {
    let authority = authority();
    let limits = Limits {
        max_conn_per_user: 1,
        ..Limits::default()
    };
    let (server, addr) = start_limited_server(&authority, limits).await;

    let (alice, welcome_a) = join(
        connect(addr).await,
        &grant(&authority, "alice", "room-user"),
    )
    .await;
    assert!(matches!(welcome_a, ControlMessage::Welcome { .. }));

    // A second session for the same subject is rejected with 503.
    let conn2 = connect(addr).await;
    let (mut send, mut recv) = conn2.open_bi().await.unwrap();
    send.write_all(
        &ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            grant_jwt: grant(&authority, "alice", "room-user"),
        }
        .encode(),
    )
    .await
    .unwrap();
    send.write_all(&ControlMessage::JoinRoom.encode())
        .await
        .unwrap();
    match read_frame(&mut recv).await {
        ControlMessage::Error { code, .. } => assert_eq!(code, error_code::CONNECTION_LIMIT),
        other => panic!("expected Error(CONNECTION_LIMIT), got {other:?}"),
    }

    // The first session is unaffected: fanout still works.
    let (bob, _) = join(connect(addr).await, &grant(&authority, "bob", "room-user")).await;
    alice
        .conn
        .send_datagram(datagram(1, "still-alive"))
        .unwrap();
    let parsed = MediaDatagram::parse(recv_datagram(&bob.conn).await).unwrap();
    assert_eq!(&parsed.payload[..], b"still-alive");

    // Closing the session releases the per-user slot.
    alice.conn.close(0u32.into(), b"bye");
    assert!(
        try_rejoin(
            addr,
            grant(&authority, "alice", "room-user"),
            Duration::from_secs(5)
        )
        .await,
        "per-user slot must be released after close"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn retry_handshake_carries_media() {
    // Default limits have enable_retry: true — every fresh connection below
    // rides a Retry round trip transparently and must not trip the per-IP cap.
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let (alice, welcome_a) = join(
        connect(addr).await,
        &grant(&authority, "alice", "room-retry"),
    )
    .await;
    assert!(matches!(welcome_a, ControlMessage::Welcome { .. }));

    let (bob, welcome_b) = join(connect(addr).await, &grant(&authority, "bob", "room-retry")).await;
    assert!(matches!(welcome_b, ControlMessage::Welcome { .. }));

    alice.conn.send_datagram(datagram(9, "post-retry")).unwrap();
    let parsed = MediaDatagram::parse(recv_datagram(&bob.conn).await).unwrap();
    assert_eq!(&parsed.payload[..], b"post-retry");

    server.shutdown().await;
}
