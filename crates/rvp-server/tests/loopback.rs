//! P2 integration: JWT handshake + DATAGRAM loopback.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use quinn::Endpoint;
use rvp_proto::control::{ControlMessage, PROTOCOL_VERSION};
use rvp_proto::{MediaDatagram, MediaHeader, HEADER_LEN};
use rvp_server::test_utils::insecure_client_config;
use rvp_server::{server_config_self_signed, start_server, DEFAULT_MAX_DATAGRAM};

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

/// Client side of the P2 handshake: Hello + JoinRoom, await Welcome.
async fn handshake(conn: &quinn::Connection, jwt: &str) -> ControlMessage {
    let (mut send, mut recv) = conn.open_bi().await.expect("open control stream");
    send.write_all(
        &ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            grant_jwt: jwt.to_owned(),
        }
        .encode(),
    )
    .await
    .expect("send hello");
    send.write_all(&ControlMessage::JoinRoom.encode())
        .await
        .expect("send join");
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("welcome len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("welcome body");
    ControlMessage::decode_body(&body).expect("decode welcome")
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

#[tokio::test]
async fn handshake_then_datagram_echo() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;
    let (conn, _endpoint) = connect_client(addr).await;

    let jwt = grant(&authority, "user-1", "room-a");
    match handshake(&conn, &jwt).await {
        ControlMessage::Welcome {
            participant_id,
            max_datagram,
        } => {
            assert!(participant_id >= 1);
            assert_eq!(max_datagram as usize, DEFAULT_MAX_DATAGRAM);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    let max = conn.max_datagram_size().expect("datagram support");
    assert!(max >= HEADER_LEN);

    let payload = Bytes::from_static(b"hello-rvp-p2");
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
    assert_eq!(echoed, raw);
    assert_eq!(MediaDatagram::parse(echoed).unwrap().payload, payload);

    server.shutdown().await;
}

#[tokio::test]
async fn many_datagrams_after_handshake() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;
    let (conn, _endpoint) = connect_client(addr).await;

    let jwt = grant(&authority, "user-2", "room-b");
    assert!(matches!(
        handshake(&conn, &jwt).await,
        ControlMessage::Welcome { .. }
    ));

    const N: u32 = 50;
    for i in 0..N {
        let raw = MediaDatagram::build(
            MediaHeader {
                sequence: i,
                timestamp: i * 960,
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
    assert_eq!(seen, (0..N).collect::<Vec<_>>());

    server.shutdown().await;
}

#[tokio::test]
async fn bad_grant_gets_error_and_no_datagrams() {
    let authority = authority();
    let (server, addr) = start_test_server(&authority).await;
    let (conn, _endpoint) = connect_client(addr).await;

    // Sign with the right shape but wrong key.
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

    let (mut send, mut recv) = conn.open_bi().await.unwrap();
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

    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.expect("error len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.expect("error body");
    match ControlMessage::decode_body(&body).unwrap() {
        ControlMessage::Error { code, .. } => {
            assert_eq!(code, rvp_proto::control::error_code::UNAUTHORIZED);
        }
        other => panic!("expected Error, got {other:?}"),
    }

    server.shutdown().await;
}
