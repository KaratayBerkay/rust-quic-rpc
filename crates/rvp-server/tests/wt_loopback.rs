//! P6a integration: WebTransport endpoint sharing rooms/fanout with quinn.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rvp_proto::control::{ControlMessage, PROTOCOL_VERSION};
use rvp_proto::{MediaDatagram, MediaHeader};
use rvp_server::test_utils::insecure_client_config;
use rvp_server::wt::{start_wt_server, WtServer};
use rvp_server::{server_config_self_signed, start_server_with_state, ServerState};
use wtransport::tls::{Sha256Digest, Sha256DigestFmt};
use wtransport::{ClientConfig, Endpoint};

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

async fn start_shared(
    authority: &TestAuthority,
) -> (rvp_server::RvpServer, WtServer, SocketAddr, SocketAddr) {
    let state = Arc::new(ServerState::new(authority.verifier.clone()));
    let quic = start_server_with_state(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        state.clone(),
    )
    .await
    .expect("start quic");
    let wt = start_wt_server(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), state)
        .await
        .expect("start wt");
    let quic_addr = quic.local_addr();
    let wt_addr = wt.local_addr();
    (quic, wt, quic_addr, wt_addr)
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

async fn read_frame_wt(recv: &mut wtransport::RecvStream) -> ControlMessage {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("frame len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("frame body");
    ControlMessage::decode_body(&body).expect("decode frame")
}

async fn read_frame_quinn(recv: &mut quinn::RecvStream) -> ControlMessage {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("frame len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("frame body");
    ControlMessage::decode_body(&body).expect("decode frame")
}

struct WtClient {
    conn: wtransport::Connection,
}

async fn connect_wt(server: SocketAddr, cert_hash: Sha256Digest) -> wtransport::Connection {
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([cert_hash])
        .build();
    let url = format!("https://{server}");
    Endpoint::client(config)
        .expect("wt client endpoint")
        .connect(url)
        .await
        .expect("wt connect")
}

async fn join_wt(conn: wtransport::Connection, jwt: &str) -> (WtClient, ControlMessage) {
    let (mut send, recv) = conn
        .open_bi()
        .await
        .expect("open control")
        .await
        .expect("control ready");
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
    let welcome = read_frame_wt(&mut control).await;
    (WtClient { conn }, welcome)
}

async fn join_quinn(
    addr: SocketAddr,
    jwt: &str,
) -> (quinn::Connection, quinn::RecvStream, ControlMessage) {
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(insecure_client_config());
    let conn = endpoint
        .connect(addr, "localhost")
        .expect("connect")
        .await
        .expect("handshake");
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
    let welcome = read_frame_quinn(&mut control).await;
    (conn, control, welcome)
}

async fn recv_datagram_wt(conn: &wtransport::Connection) -> Bytes {
    tokio::time::timeout(Duration::from_secs(5), conn.receive_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv")
        .payload()
}

async fn recv_datagram_quinn(conn: &quinn::Connection) -> Bytes {
    tokio::time::timeout(Duration::from_secs(5), conn.read_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv")
}

async fn assert_no_datagram_wt(conn: &wtransport::Connection) {
    let res = tokio::time::timeout(Duration::from_millis(300), conn.receive_datagram()).await;
    assert!(res.is_err(), "unexpected datagram received");
}

async fn assert_no_datagram_quinn(conn: &quinn::Connection) {
    let res = tokio::time::timeout(Duration::from_millis(300), conn.read_datagram()).await;
    assert!(res.is_err(), "unexpected datagram received");
}

#[tokio::test]
async fn two_wt_clients_same_room_fanout() {
    let authority = authority();
    let (_quic, wt, _quic_addr, wt_addr) = start_shared(&authority).await;

    let (alice, welcome_a) = join_wt(
        connect_wt(wt_addr, wt.cert_hash()).await,
        &grant(&authority, "alice", "wt-room"),
    )
    .await;
    match welcome_a {
        ControlMessage::Welcome { roster, .. } => {
            assert!(roster.is_empty(), "first member sees empty roster");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    let (bob, welcome_b) = join_wt(
        connect_wt(wt_addr, wt.cert_hash()).await,
        &grant(&authority, "bob", "wt-room"),
    )
    .await;
    match welcome_b {
        ControlMessage::Welcome { roster, .. } => {
            assert_eq!(roster.len(), 1, "bob sees alice in roster");
            assert_eq!(roster[0].user_id, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // A → B fanout
    alice.conn.send_datagram(datagram(1, "from-alice")).unwrap();
    let got = recv_datagram_wt(&bob.conn).await;
    let parsed = MediaDatagram::parse(got).unwrap();
    assert_eq!(parsed.header.sequence, 1);
    assert_eq!(&parsed.payload[..], b"from-alice");

    // B → A fanout
    bob.conn.send_datagram(datagram(2, "from-bob")).unwrap();
    let got = recv_datagram_wt(&alice.conn).await;
    assert_eq!(&MediaDatagram::parse(got).unwrap().payload[..], b"from-bob");

    // No self-echo
    alice.conn.send_datagram(datagram(3, "selfie")).unwrap();
    assert_no_datagram_wt(&alice.conn).await;

    wt.shutdown().await;
    _quic.shutdown().await;
}

#[tokio::test]
async fn cross_endpoint_fanout_quinn_and_wt() {
    let authority = authority();
    let (quic, wt, quic_addr, wt_addr) = start_shared(&authority).await;

    // Alice on raw QUIC, bob on WebTransport — same room, shared registry.
    let (alice_conn, mut alice_ctrl, welcome_a) =
        join_quinn(quic_addr, &grant(&authority, "alice", "cross-room")).await;
    match welcome_a {
        ControlMessage::Welcome { roster, .. } => assert!(roster.is_empty()),
        other => panic!("expected Welcome, got {other:?}"),
    }

    let (bob, welcome_b) = join_wt(
        connect_wt(wt_addr, wt.cert_hash()).await,
        &grant(&authority, "bob", "cross-room"),
    )
    .await;
    match welcome_b {
        ControlMessage::Welcome { roster, .. } => {
            assert_eq!(roster.len(), 1);
            assert_eq!(roster[0].user_id, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // Alice (quinn) learns bob (wt) joined via server-push event.
    let event = tokio::time::timeout(Duration::from_secs(5), read_frame_quinn(&mut alice_ctrl))
        .await
        .expect("join event timeout");
    match event {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "bob"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    // quinn → WT fanout
    alice_conn
        .send_datagram(datagram(1, "quinn-to-wt"))
        .unwrap();
    let got = recv_datagram_wt(&bob.conn).await;
    assert_eq!(
        &MediaDatagram::parse(got).unwrap().payload[..],
        b"quinn-to-wt"
    );

    // WT → quinn fanout
    bob.conn.send_datagram(datagram(2, "wt-to-quinn")).unwrap();
    let got = recv_datagram_quinn(&alice_conn).await;
    assert_eq!(
        &MediaDatagram::parse(got).unwrap().payload[..],
        b"wt-to-quinn"
    );

    // No self-echo: only the sender must not receive its own datagram.
    alice_conn.send_datagram(datagram(3, "selfie")).unwrap();
    assert_no_datagram_quinn(&alice_conn).await;
    // Bob (the other endpoint) correctly receives the fanout.
    let got = recv_datagram_wt(&bob.conn).await;
    assert_eq!(&MediaDatagram::parse(got).unwrap().payload[..], b"selfie");

    wt.shutdown().await;
    quic.shutdown().await;
}

#[tokio::test]
async fn wt_bad_grant_gets_error() {
    let authority = authority();
    let (_quic, wt, _quic_addr, wt_addr) = start_shared(&authority).await;

    let other = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let claims = TestClaims {
        sub: "intruder".to_owned(),
        room_id: "wt-room".to_owned(),
        role: "host".to_owned(),
        iss: "nest".to_owned(),
        aud: "rvp".to_owned(),
        exp: u64::MAX,
        jti: None,
    };
    let key = EncodingKey::from_ed_pem(other.serialize_pem().as_bytes()).unwrap();
    let jwt = encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap();

    let conn = connect_wt(wt_addr, wt.cert_hash()).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap().await.unwrap();
    send.write_all(
        &ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            grant_jwt: jwt,
        }
        .encode(),
    )
    .await
    .unwrap();
    send.write_all(&ControlMessage::JoinRoom.encode())
        .await
        .unwrap();

    match read_frame_wt(&mut recv).await {
        ControlMessage::Error { code, .. } => {
            assert_eq!(code, rvp_proto::control::error_code::UNAUTHORIZED);
        }
        other => panic!("expected Error, got {other:?}"),
    }

    wt.shutdown().await;
}

#[tokio::test]
async fn wt_cert_hash_is_stable_format() {
    let authority = authority();
    let (_quic, wt, _q, _w) = start_shared(&authority).await;
    let hex = wt.cert_hash_hex();
    // Colon-separated hex, 32 bytes → 32 colon-separated pairs.
    let parts: Vec<&str> = hex.split(':').collect();
    assert_eq!(parts.len(), 32, "expected 32 hex pairs, got {hex}");
    for p in parts {
        assert_eq!(p.len(), 2, "hex byte must be 2 chars: {p}");
        assert!(u8::from_str_radix(p, 16).is_ok(), "invalid hex byte: {p}");
    }
    // Round-trip through the same format the browser path uses.
    let parsed = Sha256Digest::from_str_fmt(&hex, Sha256DigestFmt::DottedHex).unwrap();
    assert_eq!(parsed, wt.cert_hash());
    wt.shutdown().await;
}
