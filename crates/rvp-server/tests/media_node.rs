//! P8a integration: media-node pinning (plan §2/§5).
//!
//! A grant whose `media_node` claim differs from the server's `--node-id`
//! is refused at the handshake with `WRONG_NODE` (521). Matching or absent
//! pins are accepted as before.
#![cfg(feature = "test-utils")]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    media_node: Option<String>,
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

fn grant(authority: &TestAuthority, user: &str, room: &str, media_node: Option<&str>) -> String {
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
        media_node: media_node.map(|s| s.to_owned()),
    };
    let key = EncodingKey::from_ed_pem(authority.key_pair.serialize_pem().as_bytes()).unwrap();
    encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap()
}

async fn read_frame(recv: &mut quinn::RecvStream) -> ControlMessage {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("frame len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("frame body");
    ControlMessage::decode_body(&body).expect("decode frame")
}

/// Hello + JoinRoom; returns the first server frame (Welcome or Error).
async fn handshake(conn: &quinn::Connection, jwt: &str) -> ControlMessage {
    let (mut send, mut recv) = conn.open_bi().await.expect("open control");
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
    read_frame(&mut recv).await
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

async fn start_pinned_server(
    authority: &TestAuthority,
    node_id: &str,
) -> (rvp_server::RvpServer, SocketAddr) {
    let state =
        Arc::new(ServerState::new(authority.verifier.clone()).with_node_id(node_id.to_owned()));
    let server = start_server_with_state(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        state,
    )
    .await
    .expect("start server");
    let addr = server.local_addr();
    (server, addr)
}

#[tokio::test]
async fn wrong_node_pin_rejected() {
    let authority = authority();
    let (_server, addr) = start_pinned_server(&authority, "node-a").await;

    let conn = connect(addr).await;
    let jwt = grant(&authority, "alice", "room-1", Some("node-b"));
    let frame = handshake(&conn, &jwt).await;
    match frame {
        ControlMessage::Error { code, message } => {
            assert_eq!(code, error_code::WRONG_NODE, "message: {message}");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn matching_pin_accepted() {
    let authority = authority();
    let (_server, addr) = start_pinned_server(&authority, "node-a").await;

    let conn = connect(addr).await;
    let jwt = grant(&authority, "alice", "room-1", Some("node-a"));
    let frame = handshake(&conn, &jwt).await;
    match frame {
        ControlMessage::Welcome { .. } => {}
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[tokio::test]
async fn unpinned_server_accepts_any_pin() {
    // node_id empty = single-node default; pins are not enforced.
    let authority = authority();
    let server = rvp_server::start_server(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        authority.verifier.clone(),
    )
    .await
    .expect("start server");
    let addr = server.local_addr();

    let conn = connect(addr).await;
    let jwt = grant(&authority, "alice", "room-1", Some("node-anywhere"));
    let frame = handshake(&conn, &jwt).await;
    match frame {
        ControlMessage::Welcome { .. } => {}
        other => panic!("expected Welcome, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn unpinned_grant_accepted_on_pinned_server() {
    // A grant with no media_node claim (e.g. issued before pinning was
    // configured) is accepted by any node — backward compatible.
    let authority = authority();
    let (_server, addr) = start_pinned_server(&authority, "node-a").await;

    let conn = connect(addr).await;
    let jwt = grant(&authority, "alice", "room-1", None);
    let frame = handshake(&conn, &jwt).await;
    match frame {
        ControlMessage::Welcome { .. } => {}
        other => panic!("expected Welcome, got {other:?}"),
    }
}
