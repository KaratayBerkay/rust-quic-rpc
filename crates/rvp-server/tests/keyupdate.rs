//! P7c integration: `KeyUpdate { kid }` announce relay (control stream reads
//! after JoinRoom).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use quinn::Endpoint;
use rvp_proto::control::{ControlMessage, PROTOCOL_VERSION};
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
        jti: Some(format!("jti-{user}")),
    };
    let key = EncodingKey::from_ed_pem(authority.key_pair.serialize_pem().as_bytes()).unwrap();
    encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap()
}

/// A joined client with its control streams kept open (clients may push
/// frames after JoinRoom — KeyUpdate today).
struct Client {
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
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

/// P3 handshake: Hello + JoinRoom (streams stay open for later use).
async fn join(conn: quinn::Connection, jwt: &str) -> Client {
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
    Client { conn, send, recv }
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

fn expect_welcome(msg: ControlMessage) -> u64 {
    match msg {
        ControlMessage::Welcome { participant_id, .. } => participant_id,
        other => panic!("expected Welcome, got {other:?}"),
    }
}

async fn recv_frame_within(recv: &mut quinn::RecvStream) -> ControlMessage {
    tokio::time::timeout(Duration::from_secs(5), read_frame(recv))
        .await
        .expect("frame timeout")
}

#[tokio::test]
async fn key_update_relays_to_others_not_sender() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let mut alice = join(connect(addr).await, &grant(&authority, "alice", "room-1")).await;
    expect_welcome(recv_frame_within(&mut alice.recv).await);

    let mut bob = join(connect(addr).await, &grant(&authority, "bob", "room-1")).await;
    expect_welcome(recv_frame_within(&mut bob.recv).await);
    match recv_frame_within(&mut alice.recv).await {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "bob"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    let mut carol = join(connect(addr).await, &grant(&authority, "carol", "room-1")).await;
    expect_welcome(recv_frame_within(&mut carol.recv).await);
    match recv_frame_within(&mut alice.recv).await {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "carol"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }
    match recv_frame_within(&mut bob.recv).await {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "carol"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    // Alice announces a key rotation: kid=7 (unique per room, plan §3).
    alice
        .send
        .write_all(&ControlMessage::KeyUpdate { kid: 7 }.encode())
        .await
        .expect("key update");

    for (name, recv) in [("bob", &mut bob.recv), ("carol", &mut carol.recv)] {
        match recv_frame_within(recv).await {
            ControlMessage::KeyUpdate { kid } => assert_eq!(kid, 7, "{name} kid"),
            other => panic!("{name}: expected KeyUpdate, got {other:?}"),
        }
    }

    // No self-echo: the sender must not receive its own announce back.
    let quiet = tokio::time::timeout(Duration::from_millis(400), read_frame(&mut alice.recv)).await;
    assert!(quiet.is_err(), "sender received its own KeyUpdate");

    server.shutdown().await;
}

#[tokio::test]
async fn media_survives_control_stream_close() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;

    let mut alice = join(connect(addr).await, &grant(&authority, "alice", "room-2")).await;
    expect_welcome(recv_frame_within(&mut alice.recv).await);
    let mut bob = join(connect(addr).await, &grant(&authority, "bob", "room-2")).await;
    expect_welcome(recv_frame_within(&mut bob.recv).await);

    // End the client's control stream: the server's control loop sees EOF and
    // stops, but the session — and media fanout — must continue.
    drop(alice.send);

    alice
        .conn
        .send_datagram(MediaDatagram::build(
            MediaHeader {
                sequence: 1,
                ..Default::default()
            },
            bytes::Bytes::from_static(b"after-control-close"),
        ))
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), bob.conn.read_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv");
    assert_eq!(
        &MediaDatagram::parse(got).unwrap().payload[..],
        b"after-control-close"
    );

    server.shutdown().await;
}
