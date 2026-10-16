//! RVP media server (P3 rooms + SFU fanout; P6a transport-agnostic core).
//!
//! Per connection (plan §3.4), on either endpoint (raw QUIC or WebTransport):
//! 1. TLS 1.3 (ALPN `rvp/1` or HTTP/3 WebTransport)
//! 2. Client opens the control bi-stream and sends `Hello { version, grant_jwt }`
//! 3. Server verifies the EdDSA grant **offline** (`rvp-auth`)
//! 4. `JoinRoom` → capacity check → `Welcome { participant_id, roster, ... }`
//! 5. Only after `Welcome` are DATAGRAMs read — and then they are fanned out
//!    to every *other* connection in the room (opaque bytes; no crypto here)

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rvp_auth::{GrantClaims, Verifier};
use rvp_metrics::instrument;
use rvp_proto::control::{
    error_code, ControlError, ControlMessage, MAX_FRAME_LEN, PROTOCOL_VERSION,
};
use tokio::time::timeout;
use tracing::{debug, info, warn};

pub mod limits;
pub mod revocation;
pub mod rooms;
pub mod transport;
pub mod wt;

pub use limits::{ConnGuard, ConnLimiter, IngressLimiter, Limits};
pub use revocation::Revocation;
pub use rooms::{RoomError, RoomRegistry, MAX_ROOM_PARTICIPANTS};

use revocation::wait_for_kick;
use rooms::Room;
use transport::{
    ControlReader, ControlWriter, DatagramSource, ParticipantTx, QuinnControlReader,
    QuinnControlWriter, QuinnDatagramSource, QuinnTx,
};

pub const DEFAULT_MAX_DATAGRAM: usize = 1200;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

static PARTICIPANT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls config: {0}")]
    Tls(String),
    #[error("quinn connection: {0}")]
    Connection(String),
}

/// Running server handle.
pub struct RvpServer {
    local_addr: SocketAddr,
    task: tokio::task::JoinHandle<Result<(), ServerError>>,
}

impl RvpServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting and abort connection tasks.
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Install the `ring` crypto provider exactly once per process.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Build a `quinn::ServerConfig` from PEM cert/key (prod path: env/files).
pub fn server_config_from_pem(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<ServerConfig, ServerError> {
    use rustls::pki_types::pem::PemObject;
    install_crypto_provider();
    let chain: Vec<CertificateDer<'static>> = vec![CertificateDer::from_pem_slice(cert_pem)
        .map_err(|e| ServerError::Tls(format!("cert: {e}")))?];
    let key = PrivateKeyDer::from_pem_slice(key_pem)
        .map_err(|e| ServerError::Tls(format!("key: {e}")))?;
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|e| ServerError::Tls(format!("single_cert: {e}")))?;
    tls.alpn_protocols = vec![b"rvp/1".to_vec()];
    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|e| ServerError::Tls(format!("quic server config: {e}")))?;
    let mut cfg = ServerConfig::with_crypto(Arc::new(quic_tls));
    cfg.transport_config(Arc::new(transport_config()));
    Ok(cfg)
}

/// Self-signed server config for local demo/tests (P2 replaces with CA certs).
pub fn server_config_self_signed() -> Result<ServerConfig, ServerError> {
    install_crypto_provider();
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .map_err(|e| ServerError::Tls(format!("rcgen: {e}")))?;
    server_config_from_pem(
        cert.cert.pem().as_bytes(),
        cert.key_pair.serialize_pem().as_bytes(),
    )
}

fn transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    // REQUIRED: datagrams are disabled unless a receive buffer is set (plan §1).
    t.datagram_receive_buffer_size(Some(1024 * 1024));
    t.datagram_send_buffer_size(1024 * 1024);
    t.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(30)).expect("30s idle"),
    ));
    t.keep_alive_interval(Some(Duration::from_secs(10)));
    t
}

/// Shared per-server state (rooms + grant verifier). One instance serves
/// both the quinn and WebTransport endpoints (plan §7).
pub struct ServerState {
    pub registry: RoomRegistry,
    pub(crate) verifier: Arc<Verifier>,
    /// P7b admission-control tunables (shared by both endpoints).
    pub limits: Limits,
    /// P7b per-IP/per-user connection counters (shared by both endpoints).
    pub conns: Arc<ConnLimiter>,
    /// P7c grant revocation: jti denylist + live-session kick (plan decision 5).
    pub revocation: Arc<Revocation>,
    /// P8a media-node identity (plan §2/§5). Empty = unpinned (single-node
    /// default, current tests). When set, a grant whose `media_node` claim
    /// differs from this id is refused at the handshake so the client can
    /// reconnect to the pinned node.
    pub node_id: String,
}

impl ServerState {
    pub fn new(verifier: Arc<Verifier>) -> Self {
        Self::with_limits(verifier, Limits::default())
    }

    /// Build with explicit hardening limits (tests, deployments).
    pub fn with_limits(verifier: Arc<Verifier>, limits: Limits) -> Self {
        let conns = ConnLimiter::new(&limits);
        Self {
            registry: RoomRegistry::new(),
            verifier,
            limits,
            conns,
            revocation: Arc::new(Revocation::new()),
            node_id: String::new(),
        }
    }

    /// Set the P8a media-node identity (plan §2/§5). A grant whose
    /// `media_node` claim differs from this id is refused at the handshake.
    pub fn with_node_id(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = node_id.into();
        self
    }

    /// Same as [`with_node_id`] but on an existing state (handy for tests
    /// that build `ServerState::new(...)` then set the node id).
    pub fn set_node_id(&mut self, node_id: impl Into<String>) {
        self.node_id = node_id.into();
    }
}

/// Start the media server on `addr` (use port 0 for ephemeral).
pub async fn start_server(
    addr: SocketAddr,
    server_config: ServerConfig,
    verifier: Arc<Verifier>,
) -> Result<RvpServer, ServerError> {
    start_server_with_state(addr, server_config, Arc::new(ServerState::new(verifier))).await
}

/// Start with explicit shared state (multi-endpoint deployments, tests).
pub async fn start_server_with_state(
    addr: SocketAddr,
    server_config: ServerConfig,
    state: Arc<ServerState>,
) -> Result<RvpServer, ServerError> {
    let endpoint = Endpoint::server(server_config, addr)?;
    let local_addr = endpoint.local_addr()?;
    info!(%local_addr, "rvp server listening (quic, alpn=rvp/1)");
    let task = tokio::spawn(accept_loop(endpoint, state));
    Ok(RvpServer { local_addr, task })
}

async fn accept_loop(endpoint: Endpoint, state: Arc<ServerState>) -> Result<(), ServerError> {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();

        // P7b: validate the source address statelessly BEFORE any state is
        // allocated — a spoofed-source flood can only earn Retry packets.
        let incoming = if state.limits.enable_retry && !incoming.remote_address_validated() {
            match incoming.retry() {
                Ok(()) => {
                    instrument::retry_sent();
                    continue;
                }
                Err(e) => {
                    // Unreachable: !remote_address_validated() ⇒ may_retry().
                    // Recover the incoming so the connection can still proceed.
                    warn!(error = %e, "retry refused; accepting peer as-is");
                    e.into_incoming()
                }
            }
        } else {
            incoming
        };

        // P7b: per-source-IP cap (connection task holds the slot to completion).
        let ip = incoming.remote_address().ip();
        let Some(slot) = Arc::clone(&state.conns).try_acquire_ip(ip) else {
            instrument::connection_rejected("ip_cap");
            incoming.refuse();
            continue;
        };

        tokio::spawn(async move {
            let _slot = slot;
            instrument::connection_opened("quic");
            match incoming.await {
                Ok(conn) => {
                    let remote = conn.remote_address();
                    info!(%remote, "connection established");
                    if let Err(e) = handle_connection(conn, state).await {
                        warn!(%remote, error = %e, "connection closed with error");
                    }
                }
                Err(e) => {
                    instrument::handshake_failed("transport");
                    warn!(error = %e, "handshake failed");
                }
            }
            instrument::connection_closed("quic");
        });
    }
    Ok(())
}

async fn handle_connection(
    conn: quinn::Connection,
    state: Arc<ServerState>,
) -> Result<(), ServerError> {
    let (send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| ServerError::Connection(format!("control stream: {e}")))?;

    let mut writer = QuinnControlWriter(send);
    let mut reader = QuinnControlReader(recv);
    let claims = match drive_handshake(&state, &mut reader, &mut writer).await {
        Some(claims) => claims,
        None => return Ok(()),
    };
    let QuinnControlWriter(send) = writer;
    let tx = Arc::new(QuinnTx::new(conn.clone(), send));
    let mut dgrams = QuinnDatagramSource(conn);
    serve_joined(state, claims, tx, Box::new(reader), &mut dgrams).await
}

/// Control handshake (plan §3.4 steps 2–4): Hello → verify grant → JoinRoom.
/// Returns `None` after writing an Error frame (unsupported version, bad
/// grant, malformed frames). Transport-agnostic — used by both endpoints.
pub(crate) async fn drive_handshake(
    state: &ServerState,
    reader: &mut dyn ControlReader,
    writer: &mut dyn ControlWriter,
) -> Option<GrantClaims> {
    let started = Instant::now();
    // --- Hello: verify grant (plan §3.4 step 3) ---
    let claims = match read_control(reader).await {
        Ok(ControlMessage::Hello {
            protocol_version,
            grant_jwt,
        }) => {
            if protocol_version != PROTOCOL_VERSION {
                instrument::handshake_failed("version");
                send_error(
                    writer,
                    error_code::VERSION_UNSUPPORTED,
                    &format!("supported version {PROTOCOL_VERSION}"),
                )
                .await;
                return None;
            }
            match state.verifier.verify(&grant_jwt) {
                Ok(claims) => {
                    // P7c: a revoked jti must not reach a room (decision 5 —
                    // in-memory denylist fed by the auth-service poll).
                    if claims
                        .jti
                        .as_deref()
                        .is_some_and(|jti| state.revocation.is_denied(jti))
                    {
                        instrument::handshake_failed("revoked");
                        warn!(user = %claims.user_id, "revoked grant rejected");
                        send_error(writer, error_code::FORBIDDEN, "grant revoked").await;
                        return None;
                    }
                    // P8a: a grant pinned to another media node is refused
                    // here; the client reconnects to the pinned address.
                    if !state.node_id.is_empty()
                        && claims
                            .media_node
                            .as_deref()
                            .is_some_and(|pin| pin != state.node_id)
                    {
                        instrument::handshake_failed("wrong_node");
                        warn!(
                            user = %claims.user_id,
                            want = %claims.media_node.as_deref().unwrap_or_default(),
                            have = %state.node_id,
                            "grant pinned to another media node"
                        );
                        send_error(
                            writer,
                            error_code::WRONG_NODE,
                            "room is hosted on another media node",
                        )
                        .await;
                        return None;
                    }
                    claims
                }
                Err(e) => {
                    instrument::handshake_failed("unauthorized");
                    warn!(error = %e, "grant rejected");
                    send_error(writer, error_code::UNAUTHORIZED, &e.to_string()).await;
                    return None;
                }
            }
        }
        Ok(other) => {
            instrument::handshake_failed("bad_request");
            send_error(
                writer,
                error_code::BAD_REQUEST,
                &format!("expected Hello, got {other:?}"),
            )
            .await;
            return None;
        }
        Err(e) => {
            instrument::handshake_failed("bad_request");
            send_error(writer, error_code::BAD_REQUEST, &e.to_string()).await;
            return None;
        }
    };

    // --- JoinRoom (plan §3.4 step 4) ---
    match read_control(reader).await {
        Ok(ControlMessage::JoinRoom) => {
            instrument::handshake_ok(started.elapsed().as_secs_f64());
            Some(claims)
        }
        Ok(other) => {
            instrument::handshake_failed("bad_request");
            send_error(
                writer,
                error_code::BAD_REQUEST,
                &format!("expected JoinRoom, got {other:?}"),
            )
            .await;
            None
        }
        Err(e) => {
            instrument::handshake_failed("bad_request");
            send_error(writer, error_code::BAD_REQUEST, &e.to_string()).await;
            None
        }
    }
}

/// Join the room, send Welcome, broadcast ParticipantJoined, fan out media
/// until the datagram source ends, then tear down. Transport-agnostic.
///
/// P7c: after Welcome the client's control stream keeps being read in the
/// background — `KeyUpdate { kid }` announces are relayed to the rest of the
/// room — and a revoked grant jti closes the session while it runs.
pub(crate) async fn serve_joined(
    state: Arc<ServerState>,
    claims: GrantClaims,
    tx: Arc<dyn ParticipantTx>,
    reader: Box<dyn ControlReader>,
    datagrams: &mut dyn DatagramSource,
) -> Result<(), ServerError> {
    // P7b: per-user session cap (grant verified; slot held until we return).
    let _user_slot = match Arc::clone(&state.conns).try_acquire_user(&claims.user_id) {
        Some(slot) => slot,
        None => {
            instrument::connection_rejected("user_cap");
            tx.send_control(
                ControlMessage::Error {
                    code: error_code::CONNECTION_LIMIT,
                    message: format!(
                        "per-user connection limit {}",
                        state.limits.max_conn_per_user
                    ),
                }
                .encode(),
            )
            .await;
            // Deliver the frame before this task drops the connection.
            tx.finish_control().await;
            return Ok(());
        }
    };

    let participant_id = PARTICIPANT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let (room, roster) = match state.registry.join(
        &claims.room_id,
        participant_id,
        &claims.user_id,
        Arc::clone(&tx),
    ) {
        Ok(joined) => joined,
        Err(RoomError::Full) => {
            instrument::room_full();
            tx.send_control(
                ControlMessage::Error {
                    code: error_code::ROOM_FULL,
                    message: format!("room capacity {}", MAX_ROOM_PARTICIPANTS),
                }
                .encode(),
            )
            .await;
            // Deliver the frame before this task drops the connection.
            tx.finish_control().await;
            return Ok(());
        }
    };

    // Welcome keeps the control stream OPEN for server-push events.
    let welcome = ControlMessage::Welcome {
        participant_id,
        max_datagram: state.limits.max_datagram as u16,
        roster,
    };
    tx.send_control(welcome.encode()).await;
    info!(
        %participant_id,
        user = %claims.user_id,
        room = %claims.room_id,
        role = %claims.role,
        "joined room (datagrams now accepted)"
    );

    broadcast_event(
        &room,
        participant_id,
        ControlMessage::ParticipantJoined {
            participant_id,
            user_id: claims.user_id.clone(),
        },
    )
    .await;

    // P7c: keep reading this client's control stream for the rest of the
    // session (relay KeyUpdate, ignore anything else). The handshake's 5s
    // frame timeout must NOT apply here — idle control streams are normal.
    let control_task = tokio::spawn(control_loop(reader, Arc::clone(&room), participant_id));

    // P7c: watch for revocation of this session's grant jti. Harness/test
    // grants have no jti and are never revoked.
    let mut kick_rx = claims
        .jti
        .as_ref()
        .map(|jti| state.revocation.register(jti.clone(), participant_id));

    let mut limiter = IngressLimiter::new(&state.limits);
    let result = tokio::select! {
        res = fanout_loop(datagrams, &room, participant_id, &mut limiter) => res,
        _ = async {
            match kick_rx.as_mut() {
                Some(rx) => wait_for_kick(rx).await,
                // No jti ⇒ no revocation possible; only fanout can end us.
                None => std::future::pending::<()>().await,
            }
        } => {
            instrument::session_revoked();
            warn!(%participant_id, user = %claims.user_id, "grant revoked; closing session");
            tx.send_control(
                ControlMessage::Error {
                    code: error_code::FORBIDDEN,
                    message: "grant revoked".to_owned(),
                }
                .encode(),
            )
            .await;
            // Deliver the Error frame before this task drops the connection.
            tx.finish_control().await;
            Ok(())
        }
    };
    control_task.abort();

    if let Some(jti) = claims.jti.as_ref() {
        state.revocation.unregister(jti, participant_id);
    }
    if let Some(room) = state.registry.leave(&claims.room_id, participant_id) {
        broadcast_event(
            &room,
            participant_id,
            ControlMessage::ParticipantLeft { participant_id },
        )
        .await;
        debug!(%participant_id, "participant left");
    }
    result
}

/// Hot path: admit (size/rate caps) → read datagram → fanout `Bytes` clones to
/// every other member.
async fn fanout_loop(
    datagrams: &mut dyn DatagramSource,
    room: &Arc<Room>,
    from: u64,
    limiter: &mut IngressLimiter,
) -> Result<(), ServerError> {
    while let Some(dgram) = datagrams.recv_datagram().await {
        if let Err(reason) = limiter.admit(Instant::now(), dgram.len()) {
            instrument::datagram_dropped(reason);
            continue;
        }
        let targets = room.fanout_targets(from);
        debug!(len = dgram.len(), peers = targets.len(), "fanout datagram");
        instrument::fanout_datagram(dgram.len(), targets.len());
        for peer in targets {
            // Congestion drops the datagram — correct for live audio (plan §4).
            peer.send_datagram(dgram.clone());
        }
    }
    Ok(())
}

/// Broadcast a control event to every member except `except`.
async fn broadcast_event(room: &Arc<Room>, except: u64, msg: ControlMessage) {
    let targets = room.event_targets(except);
    let frame = msg.encode();
    for (_pid, tx) in targets {
        tx.send_control(frame.clone()).await;
    }
}

/// P7c post-join control reader (runs as a task beside `fanout_loop`).
///
/// Relays `KeyUpdate { kid }` to every *other* member — the kid is unique
/// per room so it identifies the sender's key (plan §3) — and ignores any
/// other frame. A decode error or stream EOF ends the loop only; media
/// (datagrams) continues until the transport says otherwise. No frame
/// timeout: liveness after join is governed by the connection's 30s idle
/// timeout + keepalives.
async fn control_loop(mut reader: Box<dyn ControlReader>, room: Arc<Room>, participant_id: u64) {
    loop {
        match read_control_inner(reader.as_mut()).await {
            Ok(ControlMessage::KeyUpdate { kid }) => {
                instrument::key_update_relayed();
                debug!(%participant_id, kid, "relaying KeyUpdate");
                broadcast_event(&room, participant_id, ControlMessage::KeyUpdate { kid }).await;
            }
            Ok(other) => {
                // Clients send nothing else after JoinRoom today; tolerate
                // (and log) forward-compat frames instead of tearing down.
                debug!(%participant_id, frame = ?other, "ignoring client control frame");
            }
            Err(_) => {
                debug!(%participant_id, "control stream ended");
                break;
            }
        }
    }
}

/// Read one length-prefixed control frame (u32 BE length + body).
async fn read_control(recv: &mut dyn ControlReader) -> Result<ControlMessage, ControlError> {
    timeout(HANDSHAKE_TIMEOUT, read_control_inner(recv))
        .await
        .map_err(|_| ControlError::Truncated)?
}

async fn read_control_inner(recv: &mut dyn ControlReader) -> Result<ControlMessage, ControlError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|_| ControlError::Truncated)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        return Err(ControlError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|_| ControlError::Truncated)?;
    ControlMessage::decode_body(&body)
}

async fn send_error(send: &mut dyn ControlWriter, code: u16, message: &str) {
    instrument::control_error(code);
    let frame = ControlMessage::Error {
        code,
        message: message.to_owned(),
    }
    .encode();
    let _ = send.write_all(&frame).await;
    let _ = send.finish().await;
}

#[cfg(feature = "test-utils")]
pub mod test_utils {
    //! TEST ONLY — never enable in production builds.
    use quinn::ClientConfig;
    use std::sync::Arc;

    /// Client config that skips server-cert verification (loopback tests).
    pub fn insecure_client_config() -> ClientConfig {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();
        let mut tls = crypto;
        tls.alpn_protocols = vec![b"rvp/1".to_vec()];
        let mut t = quinn::TransportConfig::default();
        t.datagram_receive_buffer_size(Some(1024 * 1024));
        t.datagram_send_buffer_size(1024 * 1024);
        // Raise the send MTU and freeze discovery so tests can emit a
        // datagram above the server's DEFAULT_MAX_DATAGRAM cap (P7b).
        t.initial_mtu(1450);
        t.mtu_discovery_config(None);
        let mut cfg = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client config"),
        ));
        cfg.transport_config(Arc::new(t));
        cfg
    }

    #[derive(Debug)]
    struct SkipServerVerification;

    impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}
