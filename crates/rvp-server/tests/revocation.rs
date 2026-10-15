//! P7c integration: grant revocation — join denial + live kick (plan §6,
//! decision 5: in-memory jti denylist fed by the auth-service poll).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use quinn::Endpoint;
use rvp_proto::control::{error_code, ControlMessage, PROTOCOL_VERSION};
use rvp_server::test_utils::insecure_client_config;
use rvp_server::{server_config_self_signed, start_server_with_state, ServerState};

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

fn grant_with_jti(authority: &TestAuthority, user: &str, room: &str, jti: &str) -> String {
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
        jti: Some(jti.to_owned()),
    };
    let key = EncodingKey::from_ed_pem(authority.key_pair.serialize_pem().as_bytes()).unwrap();
    encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap()
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

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<ControlMessage, String> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("frame len: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| format!("frame body: {e}"))?;
    ControlMessage::decode_body(&body).map_err(|e| format!("decode: {e}"))
}

/// Hello + JoinRoom; returns the control streams (Welcome read separately so
/// rejection tests can observe the reply either way).
async fn handshake(conn: &quinn::Connection, jwt: &str) -> (quinn::SendStream, quinn::RecvStream) {
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
    (send, recv)
}

async fn recv_frame_within(recv: &mut quinn::RecvStream) -> ControlMessage {
    tokio::time::timeout(Duration::from_secs(5), read_frame(recv))
        .await
        .expect("frame timeout")
        .expect("frame")
}

fn expect_welcome(msg: ControlMessage) {
    if !matches!(msg, ControlMessage::Welcome { .. }) {
        panic!("expected Welcome, got {msg:?}");
    }
}

async fn start_test_server(state: &Arc<ServerState>) -> (rvp_server::RvpServer, SocketAddr) {
    let server = start_server_with_state(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        Arc::clone(state),
    )
    .await
    .expect("start server");
    let addr = server.local_addr();
    (server, addr)
}

#[tokio::test]
async fn revoked_jti_rejected_at_join() {
    let authority = authority();
    let state = Arc::new(ServerState::new(authority.verifier.clone()));
    let (server, addr) = start_test_server(&state).await;

    // A poll ingests the denylist before this client shows up.
    assert_eq!(
        state
            .revocation
            .apply(std::iter::once("stolen-jti".to_owned())),
        1
    );

    let conn = connect(addr).await;
    let (_send, mut recv) = handshake(
        &conn,
        &grant_with_jti(&authority, "mallory", "room-r", "stolen-jti"),
    )
    .await;

    match recv_frame_within(&mut recv).await {
        ControlMessage::Error { code, message } => {
            assert_eq!(code, error_code::FORBIDDEN);
            assert_eq!(message, "grant revoked");
        }
        other => panic!("expected Error(FORBIDDEN), got {other:?}"),
    }

    // Re-ingesting the same list is a no-op (idempotent poll).
    assert_eq!(
        state
            .revocation
            .apply(std::iter::once("stolen-jti".to_owned())),
        0
    );

    server.shutdown().await;
}

#[tokio::test]
async fn revocation_kicks_live_session_and_rest_of_room_keeps_working() {
    let authority = authority();
    let state = Arc::new(ServerState::new(authority.verifier.clone()));
    let (server, addr) = start_test_server(&state).await;

    let alice = connect(addr).await;
    let (_s, mut alice_recv) = handshake(
        &alice,
        &grant_with_jti(&authority, "alice", "room-r2", "jti-alice"),
    )
    .await;
    expect_welcome(recv_frame_within(&mut alice_recv).await);

    let bob = connect(addr).await;
    let (_s, mut bob_recv) = handshake(
        &bob,
        &grant_with_jti(&authority, "bob", "room-r2", "jti-bob"),
    )
    .await;
    expect_welcome(recv_frame_within(&mut bob_recv).await);

    // Clear alice's pending ParticipantJoined(bob) before the kick arrives.
    match recv_frame_within(&mut alice_recv).await {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "bob"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    // The auth service revokes alice's grant (rvp-cli poll → apply).
    assert_eq!(
        state
            .revocation
            .apply(std::iter::once("jti-alice".to_owned())),
        1
    );

    // Live kick: terminal 403 then the stream/connection goes away.
    match recv_frame_within(&mut alice_recv).await {
        ControlMessage::Error { code, message } => {
            assert_eq!(code, error_code::FORBIDDEN);
            assert_eq!(message, "grant revoked");
        }
        other => panic!("expected Error(FORBIDDEN), got {other:?}"),
    }
    let after_kick =
        tokio::time::timeout(Duration::from_secs(5), read_frame(&mut alice_recv)).await;
    match after_kick {
        Ok(Err(_)) => {} // clean EOF / reset — expected
        Ok(Ok(other)) => panic!("unexpected frame after kick: {other:?}"),
        Err(_) => panic!("control stream neither closed nor reset after kick"),
    }

    // The room notices the departure and keeps working for everyone else.
    match recv_frame_within(&mut bob_recv).await {
        ControlMessage::ParticipantLeft { .. } => {}
        other => panic!("expected ParticipantLeft, got {other:?}"),
    }
    let carol = connect(addr).await;
    let (_s, mut carol_recv) = handshake(
        &carol,
        &grant_with_jti(&authority, "carol", "room-r2", "jti-carol"),
    )
    .await;
    expect_welcome(recv_frame_within(&mut carol_recv).await);

    // Media still flows between the survivors (bob → carol).
    bob.send_datagram(rvp_proto::MediaDatagram::build(
        rvp_proto::MediaHeader {
            sequence: 1,
            ..Default::default()
        },
        bytes::Bytes::from_static(b"still-alive"),
    ))
    .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), carol.read_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv");
    assert_eq!(
        &rvp_proto::MediaDatagram::parse(got).unwrap().payload[..],
        b"still-alive"
    );

    // A replay of the revoked grant cannot rejoin (same denylist check).
    let replay = connect(addr).await;
    let (_send, mut replay_recv) = handshake(
        &replay,
        &grant_with_jti(&authority, "alice", "room-r2", "jti-alice"),
    )
    .await;
    match recv_frame_within(&mut replay_recv).await {
        ControlMessage::Error { code, message } => {
            assert_eq!(code, error_code::FORBIDDEN);
            assert_eq!(message, "grant revoked");
        }
        other => panic!("expected Error(FORBIDDEN), got {other:?}"),
    }

    server.shutdown().await;
}

#[tokio::test]
async fn unrevoked_joins_are_unaffected() {
    let authority = authority();
    let state = Arc::new(ServerState::new(authority.verifier.clone()));
    let (server, addr) = start_test_server(&state).await;

    // Some other session's revocation must not touch these two.
    assert_eq!(
        state
            .revocation
            .apply(std::iter::once("someone-else".to_owned())),
        1
    );

    let conn_dave = connect(addr).await;
    let (_s, mut dave_recv) = handshake(
        &conn_dave,
        &grant_with_jti(&authority, "dave", "room-r3", "jti-dave"),
    )
    .await;
    expect_welcome(recv_frame_within(&mut dave_recv).await);

    let conn_erin = connect(addr).await;
    let (_s, mut erin_recv) = handshake(
        &conn_erin,
        &grant_with_jti(&authority, "erin", "room-r3", "jti-erin"),
    )
    .await;
    expect_welcome(recv_frame_within(&mut erin_recv).await);
    match recv_frame_within(&mut dave_recv).await {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "erin"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    // Full join + fanout path still works for unrevoked jtis.
    conn_erin
        .send_datagram(rvp_proto::MediaDatagram::build(
            rvp_proto::MediaHeader {
                sequence: 9,
                ..Default::default()
            },
            bytes::Bytes::from_static(b"fine"),
        ))
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), conn_dave.read_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv");
    assert_eq!(
        &rvp_proto::MediaDatagram::parse(got).unwrap().payload[..],
        b"fine"
    );

    server.shutdown().await;
}
