//! P3 integration: rooms + SFU fanout between multiple clients.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use quinn::Endpoint;
use rvp_proto::control::{ControlMessage, RosterEntry, PROTOCOL_VERSION};
use rvp_proto::{MediaDatagram, MediaHeader};
use rvp_server::test_utils::insecure_client_config;
use rvp_server::{server_config_self_signed, start_server};

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
    control: quinn::RecvStream,
}

async fn connect(server: SocketAddr) -> quinn::Connection {
    let client_cfg = insecure_client_config();
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(client_cfg);
    endpoint
        .connect(server, "localhost")
        .expect("connect")
        .await
        .expect("handshake")
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

async fn start_test_server(authority: &TestAuthority) -> (rvp_server::RvpServer, SocketAddr) {
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

#[tokio::test]
async fn two_clients_same_room_fanout() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let (alice, welcome_a) = join(connect(addr).await, &grant(&authority, "alice", "room-1")).await;
    let alice_pid = match welcome_a {
        ControlMessage::Welcome {
            participant_id,
            roster,
            ..
        } => {
            assert!(roster.is_empty(), "first member sees empty roster");
            participant_id
        }
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(alice_pid >= 1);

    let (bob, welcome_b) = join(connect(addr).await, &grant(&authority, "bob", "room-1")).await;
    let bob_pid = match welcome_b {
        ControlMessage::Welcome { roster, .. } => {
            assert_eq!(roster.len(), 1, "bob sees alice in roster");
            assert_eq!(roster[0].user_id, "alice");
            roster[0].participant_id
        }
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert_eq!(bob_pid, alice_pid);

    // A → B fanout
    alice.conn.send_datagram(datagram(1, "from-alice")).unwrap();
    let got = recv_datagram(&bob.conn).await;
    let parsed = MediaDatagram::parse(got).unwrap();
    assert_eq!(parsed.header.sequence, 1);
    assert_eq!(&parsed.payload[..], b"from-alice");

    // B → A fanout
    bob.conn.send_datagram(datagram(2, "from-bob")).unwrap();
    let got = recv_datagram(&alice.conn).await;
    assert_eq!(&MediaDatagram::parse(got).unwrap().payload[..], b"from-bob");

    // No self-echo: the sender must not receive its own datagram back.
    alice.conn.send_datagram(datagram(3, "selfie")).unwrap();
    assert_no_datagram(&alice.conn).await;

    server.shutdown().await;
}

#[tokio::test]
async fn rooms_are_isolated() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let (alice, _) = join(connect(addr).await, &grant(&authority, "alice", "room-a")).await;
    let (carol, _) = join(connect(addr).await, &grant(&authority, "carol", "room-b")).await;

    alice
        .conn
        .send_datagram(datagram(1, "room-a-only"))
        .unwrap();
    assert_no_datagram(&carol.conn).await;
    assert_no_datagram(&alice.conn).await; // no self-echo either

    server.shutdown().await;
}

#[tokio::test]
async fn participant_joined_and_left_events() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let (mut alice, _) = join(connect(addr).await, &grant(&authority, "alice", "room-2")).await;

    let (bob, _) = join(connect(addr).await, &grant(&authority, "bob", "room-2")).await;

    // Alice learns Bob joined.
    let event = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut alice.control))
        .await
        .expect("join event timeout");
    match event {
        ControlMessage::ParticipantJoined {
            participant_id,
            user_id,
        } => {
            assert_eq!(user_id, "bob");
            assert!(participant_id >= 1);
        }
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    // Bob leaves → alice gets ParticipantLeft.
    bob.conn.close(0u32.into(), b"bye");
    let event = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut alice.control))
        .await
        .expect("left event timeout");
    match event {
        ControlMessage::ParticipantLeft { participant_id } => {
            assert!(participant_id >= 1);
        }
        other => panic!("expected ParticipantLeft, got {other:?}"),
    }

    // Room still works after a departure: new joiner's roster excludes bob.
    let (_, welcome) = join(connect(addr).await, &grant(&authority, "dave", "room-2")).await;
    match welcome {
        ControlMessage::Welcome { roster, .. } => {
            assert_eq!(roster.len(), 1);
            assert_eq!(roster[0].user_id, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    server.shutdown().await;
}

#[tokio::test]
async fn bad_grant_gets_error() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;
    let client = connect(addr).await;

    let other = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let claims = TestClaims {
        sub: "intruder".to_owned(),
        room_id: "room-a".to_owned(),
        role: "host".to_owned(),
        iss: "nest".to_owned(),
        aud: "rvp".to_owned(),
        exp: u64::MAX,
        jti: None,
    };
    let key = EncodingKey::from_ed_pem(other.serialize_pem().as_bytes()).unwrap();
    let jwt = encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap();

    let (mut send, mut recv) = client.open_bi().await.unwrap();
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

    match read_frame(&mut recv).await {
        ControlMessage::Error { code, .. } => {
            assert_eq!(code, rvp_proto::control::error_code::UNAUTHORIZED);
        }
        other => panic!("expected Error, got {other:?}"),
    }

    server.shutdown().await;
}

#[allow(dead_code)]
fn _roster_type_anchor(_: &RosterEntry) {}
