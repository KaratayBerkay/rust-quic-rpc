//! P9a integration: WebSocket endpoint sharing rooms/fanout with quinn+WT.
//!
//! Mirrors `wt_loopback.rs` with a tagged-framing WS client: two WS clients
//! in one room fan out (no self-echo), quinn ↔ WS cross fanout with the
//! server-push join event, and a forged grant is answered with an Error
//! frame before the socket closes.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rvp_proto::control::{ControlMessage, PROTOCOL_VERSION};
use rvp_proto::ws::{frame_control, frame_datagram, parse_ws_message, WsMessage};
use rvp_proto::{MediaDatagram, MediaHeader};
use rvp_server::test_utils::insecure_client_config;
use rvp_server::ws::{start_ws_server, WsServer};
use rvp_server::{server_config_self_signed, start_server_with_state, ServerState};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

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
) -> (rvp_server::RvpServer, WsServer, SocketAddr, SocketAddr) {
    let state = Arc::new(ServerState::new(authority.verifier.clone()));
    let quic = start_server_with_state(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        server_config_self_signed().unwrap(),
        state.clone(),
    )
    .await
    .expect("start quic");
    let ws = start_ws_server(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), state)
        .await
        .expect("start ws");
    let quic_addr = quic.local_addr();
    let ws_addr = ws.local_addr();
    (quic, ws, quic_addr, ws_addr)
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

/// WS test client: tagged binary messages + a byte buffer for the control
/// stream (server may split or coalesce frames across messages).
struct WsClient {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    ctrl_buf: Vec<u8>,
}

impl WsClient {
    async fn connect(addr: SocketAddr) -> Self {
        let url = format!("ws://{addr}/");
        let (ws, _) = connect_async(url).await.expect("ws connect");
        Self {
            ws,
            ctrl_buf: Vec::new(),
        }
    }

    async fn send_control(&mut self, frame: &[u8]) {
        self.ws
            .send(Message::Binary(frame_control(frame)))
            .await
            .expect("send control");
    }

    async fn send_datagram(&mut self, bytes: &[u8]) {
        self.ws
            .send(Message::Binary(frame_datagram(bytes)))
            .await
            .expect("send datagram");
    }

    /// Next complete control frame (4-byte BE length ‖ body), reassembled
    /// from arbitrary tagged control chunks.
    async fn read_frame(&mut self) -> ControlMessage {
        loop {
            if self.ctrl_buf.len() >= 4 {
                let len = u32::from_be_bytes(self.ctrl_buf[..4].try_into().unwrap()) as usize;
                if self.ctrl_buf.len() >= 4 + len {
                    let body: Vec<u8> = self.ctrl_buf[4..4 + len].to_vec();
                    self.ctrl_buf.drain(..4 + len);
                    return ControlMessage::decode_body(&body).expect("decode frame");
                }
            }
            let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                .await
                .expect("control frame timeout")
                .expect("ws closed before frame")
                .expect("ws read error");
            match msg {
                Message::Binary(bytes) => match parse_ws_message(&bytes).expect("tag parse") {
                    WsMessage::Control(chunk) => self.ctrl_buf.extend_from_slice(&chunk),
                    WsMessage::Datagram(_) => continue, // not a control frame
                },
                Message::Close(_) => panic!("ws closed before frame"),
                _ => continue,
            }
        }
    }

    /// Next media datagram (ignores control traffic such as join events).
    async fn recv_datagram(&mut self) -> Bytes {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                .await
                .expect("datagram timeout")
                .expect("ws closed before datagram")
                .expect("ws read error");
            match msg {
                Message::Binary(bytes) => match parse_ws_message(&bytes).expect("tag parse") {
                    WsMessage::Datagram(dgram) => return dgram,
                    WsMessage::Control(chunk) => self.ctrl_buf.extend_from_slice(&chunk),
                },
                Message::Close(_) => panic!("ws closed before datagram"),
                _ => continue,
            }
        }
    }

    /// No datagram must arrive within 300 ms (control events are fine).
    async fn assert_no_datagram(&mut self) {
        let res = tokio::time::timeout(Duration::from_millis(300), self.ws.next()).await;
        match res {
            Err(_) => {} // idle — correct
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                if let WsMessage::Datagram(dgram) = parse_ws_message(&bytes).expect("tag parse") {
                    panic!("unexpected datagram: {dgram:?}");
                }
                // Control event — keep going without failing.
            }
            Ok(Some(Ok(_))) => {}
            other => panic!("ws read failed: {other:?}"),
        }
    }

    async fn join(&mut self, jwt: &str) -> ControlMessage {
        self.send_control(
            &ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                grant_jwt: jwt.to_owned(),
            }
            .encode(),
        )
        .await;
        self.send_control(&ControlMessage::JoinRoom.encode()).await;
        self.read_frame().await
    }
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
    let mut len_buf = [0u8; 4];
    control.read_exact(&mut len_buf).await.expect("frame len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    control.read_exact(&mut body).await.expect("frame body");
    let welcome = ControlMessage::decode_body(&body).expect("decode frame");
    (conn, control, welcome)
}

async fn recv_datagram_quinn(conn: &quinn::Connection) -> Bytes {
    tokio::time::timeout(Duration::from_secs(5), conn.read_datagram())
        .await
        .expect("datagram timeout")
        .expect("datagram recv")
}

#[tokio::test]
async fn two_ws_clients_same_room_fanout() {
    let authority = authority();
    let (_quic, ws, _quic_addr, ws_addr) = start_shared(&authority).await;

    let mut alice = WsClient::connect(ws_addr).await;
    match alice.join(&grant(&authority, "alice", "ws-room")).await {
        ControlMessage::Welcome { roster, .. } => {
            assert!(roster.is_empty(), "first member sees empty roster");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    let mut bob = WsClient::connect(ws_addr).await;
    match bob.join(&grant(&authority, "bob", "ws-room")).await {
        ControlMessage::Welcome { roster, .. } => {
            assert_eq!(roster.len(), 1, "bob sees alice in roster");
            assert_eq!(roster[0].user_id, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // A → B fanout
    alice.send_datagram(&datagram(1, "from-alice")).await;
    let got = bob.recv_datagram().await;
    let parsed = MediaDatagram::parse(got).unwrap();
    assert_eq!(parsed.header.sequence, 1);
    assert_eq!(&parsed.payload[..], b"from-alice");

    // B → A fanout
    bob.send_datagram(&datagram(2, "from-bob")).await;
    let got = alice.recv_datagram().await;
    assert_eq!(&MediaDatagram::parse(got).unwrap().payload[..], b"from-bob");

    // No self-echo
    alice.send_datagram(&datagram(3, "selfie")).await;
    alice.assert_no_datagram().await;

    ws.shutdown().await;
    _quic.shutdown().await;
}

#[tokio::test]
async fn cross_endpoint_fanout_quinn_and_ws() {
    let authority = authority();
    let (quic, ws, quic_addr, ws_addr) = start_shared(&authority).await;

    // Alice on raw QUIC, bob on WebSocket — same room, shared registry.
    let (alice_conn, mut alice_ctrl, welcome_a) =
        join_quinn(quic_addr, &grant(&authority, "alice", "cross-room")).await;
    match welcome_a {
        ControlMessage::Welcome { roster, .. } => assert!(roster.is_empty()),
        other => panic!("expected Welcome, got {other:?}"),
    }

    let mut bob = WsClient::connect(ws_addr).await;
    match bob.join(&grant(&authority, "bob", "cross-room")).await {
        ControlMessage::Welcome { roster, .. } => {
            assert_eq!(roster.len(), 1);
            assert_eq!(roster[0].user_id, "alice");
        }
        other => panic!("expected Welcome, got {other:?}"),
    }

    // Alice (quinn) learns bob (ws) joined via server-push event.
    let mut len_buf = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(5), alice_ctrl.read_exact(&mut len_buf))
        .await
        .expect("join event timeout")
        .expect("join event len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    alice_ctrl.read_exact(&mut body).await.expect("event body");
    match ControlMessage::decode_body(&body).expect("decode event") {
        ControlMessage::ParticipantJoined { user_id, .. } => assert_eq!(user_id, "bob"),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }

    // quinn → WS fanout
    alice_conn
        .send_datagram(datagram(1, "quinn-to-ws"))
        .unwrap();
    let got = bob.recv_datagram().await;
    assert_eq!(
        &MediaDatagram::parse(got).unwrap().payload[..],
        b"quinn-to-ws"
    );

    // WS → quinn fanout
    bob.send_datagram(&datagram(2, "ws-to-quinn")).await;
    let got = recv_datagram_quinn(&alice_conn).await;
    assert_eq!(
        &MediaDatagram::parse(got).unwrap().payload[..],
        b"ws-to-quinn"
    );

    // No self-echo for either sender, fanout still works for the peer.
    alice_conn.send_datagram(datagram(3, "selfie")).unwrap();
    let res = tokio::time::timeout(Duration::from_millis(300), alice_conn.read_datagram()).await;
    assert!(res.is_err(), "unexpected self-echo to quinn sender");
    let got = bob.recv_datagram().await;
    assert_eq!(&MediaDatagram::parse(got).unwrap().payload[..], b"selfie");

    ws.shutdown().await;
    quic.shutdown().await;
}

#[tokio::test]
async fn ws_bad_grant_gets_error() {
    let authority = authority();
    let (_quic, ws, _quic_addr, ws_addr) = start_shared(&authority).await;

    let other = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    let claims = TestClaims {
        sub: "intruder".to_owned(),
        room_id: "ws-room".to_owned(),
        role: "host".to_owned(),
        iss: "nest".to_owned(),
        aud: "rvp".to_owned(),
        exp: u64::MAX,
        jti: None,
    };
    let key = EncodingKey::from_ed_pem(other.serialize_pem().as_bytes()).unwrap();
    let jwt = encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap();

    let mut client = WsClient::connect(ws_addr).await;
    client
        .send_control(
            &ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                grant_jwt: jwt,
            }
            .encode(),
        )
        .await;
    client
        .send_control(&ControlMessage::JoinRoom.encode())
        .await;

    match client.read_frame().await {
        ControlMessage::Error { code, .. } => {
            assert_eq!(code, rvp_proto::control::error_code::UNAUTHORIZED);
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // The server closes after the terminal frame — that's the contract.
    let closed = tokio::time::timeout(Duration::from_secs(5), client.ws.next()).await;
    assert!(
        matches!(closed, Ok(Some(Ok(Message::Close(_)))) | Ok(None)),
        "expected close after error, got {closed:?}"
    );

    ws.shutdown().await;
}

#[tokio::test]
async fn ws_control_chunking_is_byte_stream_transparent() {
    // A single logical control frame split across two tagged WS messages
    // must reassemble (chunking is invisible at the frame layer).
    let authority = authority();
    let (_quic, ws, _quic_addr, ws_addr) = start_shared(&authority).await;

    let mut client = WsClient::connect(ws_addr).await;
    let hello = ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        grant_jwt: grant(&authority, "chunky", "chunk-room"),
    }
    .encode();

    // Split the encoded frame (4-byte length + body) at an arbitrary point.
    assert!(hello.len() > 8, "frame too short to split");
    let (a, b) = hello.split_at(6);
    client
        .ws
        .send(Message::Binary(frame_control(a)))
        .await
        .unwrap();
    client
        .ws
        .send(Message::Binary(frame_control(b)))
        .await
        .unwrap();
    client
        .send_control(&ControlMessage::JoinRoom.encode())
        .await;

    match client.read_frame().await {
        ControlMessage::Welcome { .. } => {}
        other => panic!("expected Welcome, got {other:?}"),
    }

    ws.shutdown().await;
}
