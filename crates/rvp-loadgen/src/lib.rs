//! RVP load generator (plan §9, P7d).
//!
//! Simulates `rooms` × `clients_per_room` participants against an `rvp-server`.
//! Each publisher emits one "Opus-sized" payload per tick and, with pairwise
//! crypto enabled (the product default, ADR-004), seals it once per co-member
//! (`PT_OPUS_ENC`, publisher uplink ×(N−1)); receivers open by `kid` and drop
//! foreign sealed copies — the exact fanout/waste model of the real Flutter
//! and web clients. `--plaintext` sends a single `PT_OPUS` datagram per tick
//! for the without-crypto A/B leg (plan §9: "report latency/CPU with and
//! without app-layer crypto").
//!
//! Every payload carries a `u64` big-endian nanosecond send timestamp (shared
//! process `Instant` origin — all simulated clients live in one process, so no
//! clock sync is needed). Receivers measure transport+crypto e2e latency
//! percentiles from it. This is wire-level latency, not audio-device playout
//! latency.
//!
//! Modes:
//! - default: starts an in-process `rvp-server` with load-test-friendly limits;
//! - `--server <addr> --grant-key <pem>`: drives an external server started
//!   with the matching `rvp-server --jwt-public-key` (generate the pair with
//!   `rvp-loadgen keygen`).
//!
//! Pairwise keys are distributed in-process (harness-style stand-in for the
//! NestJS keys API, which the Nest e2e tests already cover) — the loadgen
//! measures the media path, not the control plane.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use quinn::Endpoint;
use rvp_crypto::PairwiseKey;
use rvp_proto::control::{ControlMessage, PROTOCOL_VERSION};
use rvp_proto::{MediaDatagram, MediaHeader, PT_OPUS, PT_OPUS_ENC};
use serde::Serialize;
use tokio::time::MissedTickBehavior;

/// Latency samples are capped so hour-long runs stay memory-bounded.
const MAX_LATENCY_SAMPLES: usize = 500_000;

/// Grace period after the measured window for in-flight datagrams to land.
const DRAIN: Duration = Duration::from_millis(200);

/// Grant TTL used for minted loadgen grants.
const GRANT_TTL_SECS: u64 = 3600;

#[derive(Debug, thiserror::Error)]
pub enum LoadgenError {
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("connection: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("handshake: {0}")]
    Handshake(String),
    #[error("join rejected by server: code {code}, {message}")]
    JoinRejected { code: u16, message: String },
    #[error("server: {0}")]
    Server(String),
}

/// Scenario configuration. `Default` is a small two-room smoke scenario.
#[derive(Debug, Clone)]
pub struct Config {
    /// External `rvp-server` address; `None` starts an in-process server.
    pub server: Option<SocketAddr>,
    /// Ed25519 private key PEM used to sign grants (required with `server`).
    pub grant_key_pem: Option<String>,
    /// Number of rooms.
    pub rooms: usize,
    /// Participants per room (server caps a room at 8).
    pub clients_per_room: usize,
    /// Measured publish window (after warm-up).
    pub duration: Duration,
    /// Traffic warm-up excluded from the report.
    pub warmup: Duration,
    /// Publisher tick interval (20 ms = 50 fps, Opus real-time cadence).
    pub tick: Duration,
    /// Plaintext payload bytes per frame (8-byte timestamp prefix included).
    pub payload_bytes: usize,
    /// `false` sends one plaintext datagram per tick (`PT_OPUS`, `kid=0`).
    pub crypto: bool,
    /// Prometheus listen address for the in-process server (plan §9).
    pub metrics_listen: Option<SocketAddr>,
    /// Optional CA PEM to verify an external server; `None` skips verification
    /// (loopback/self-signed lab setups only).
    pub ca_pem: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: None,
            grant_key_pem: None,
            rooms: 2,
            clients_per_room: 3,
            duration: Duration::from_secs(5),
            warmup: Duration::from_secs(1),
            tick: Duration::from_millis(20),
            payload_bytes: 80,
            crypto: true,
            metrics_listen: None,
            ca_pem: None,
        }
    }
}

impl Config {
    fn validate(&self) -> Result<(), LoadgenError> {
        if self.rooms == 0 {
            return Err(LoadgenError::Config("--rooms must be >= 1".into()));
        }
        if self.clients_per_room == 0 {
            return Err(LoadgenError::Config(
                "--clients-per-room must be >= 1".into(),
            ));
        }
        let max = rvp_server::MAX_ROOM_PARTICIPANTS;
        if self.clients_per_room > max {
            return Err(LoadgenError::Config(format!(
                "--clients-per-room {max_cap} exceeds server room cap {max}",
                max_cap = self.clients_per_room,
                max = max
            )));
        }
        if self.tick.is_zero() {
            return Err(LoadgenError::Config("--tick-ms must be > 0".into()));
        }
        if self.payload_bytes < 8 {
            return Err(LoadgenError::Config(
                "--payload-bytes must be >= 8 (timestamp prefix)".into(),
            ));
        }
        let wire = self.payload_bytes + 16 + if self.crypto { 16 } else { 0 };
        if wire > rvp_server::DEFAULT_MAX_DATAGRAM {
            return Err(LoadgenError::Config(format!(
                "wire datagram {wire} B exceeds server cap {} B; lower --payload-bytes",
                rvp_server::DEFAULT_MAX_DATAGRAM
            )));
        }
        if self.server.is_some() && self.grant_key_pem.is_none() {
            return Err(LoadgenError::Config(
                "--server requires --grant-key (Ed25519 private key PEM)".into(),
            ));
        }
        Ok(())
    }
}

/// Latency summary in milliseconds (plan §9: p50/p95/p99).
#[derive(Debug, Clone, Serialize, Default)]
pub struct LatencyStats {
    pub count: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub mean_ms: f64,
}

/// Full scenario report (serialized to stdout and optional `--report` file).
#[derive(Debug, Serialize)]
pub struct Report {
    pub rooms: usize,
    pub clients_per_room: usize,
    pub clients: usize,
    pub crypto: bool,
    pub tick_ms: f64,
    pub payload_bytes: usize,
    pub duration_secs: f64,
    pub warmup_secs: f64,
    /// Connect start → Welcome received, per client.
    pub join: LatencyStats,
    /// Seal/send → open at a receiver, per datagram (measured window only).
    pub media: LatencyStats,
    /// Datagrams sent by all publishers in the measured window.
    pub datagrams_sent: u64,
    /// Datagrams successfully processed at a receiver in the measured window.
    pub datagrams_opened: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Sealed copies delivered to a receiver that lacks that `kid` — expected
    /// uplink×(N−1) fanout waste with crypto on; always 0 with plaintext.
    pub missing_key_dropped: u64,
    pub decrypt_failures: u64,
    pub malformed_dropped: u64,
    pub send_errors: u64,
    pub wall_secs: f64,
    /// Loadgen process CPU (utime+stime) over the measured window (Linux).
    pub cpu_secs: Option<f64>,
    /// True when the latency sample cap truncated collection.
    pub samples_truncated: bool,
}

#[derive(Serialize)]
struct GrantClaims {
    sub: String,
    room_id: String,
    role: String,
    iss: String,
    aud: String,
    exp: u64,
    jti: Option<String>,
}

/// One co-member to seal toward: the pair's room-unique `kid` + shared key.
struct PeerSend {
    kid: u8,
    key: PairwiseKey,
}

struct Client {
    conn: quinn::Connection,
    send_task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Stats {
    recording: AtomicBool,
    stop: AtomicBool,
    sent: AtomicU64,
    opened: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    missing_key: AtomicU64,
    decrypt_fail: AtomicU64,
    malformed: AtomicU64,
    send_errors: AtomicU64,
    join_ms: Mutex<Vec<f64>>,
    media_ms: Mutex<Vec<f64>>,
    truncated: AtomicBool,
}

impl Stats {
    fn push_sample(&self, sink: &Mutex<Vec<f64>>, ms: f64) {
        let mut guard = sink.lock().expect("stats lock poisoned");
        if guard.len() >= MAX_LATENCY_SAMPLES {
            self.truncated.store(true, Ordering::Relaxed);
            return;
        }
        guard.push(ms);
    }
}

/// Run a scenario end-to-end and return the report.
pub async fn run(cfg: &Config) -> Result<Report, LoadgenError> {
    cfg.validate()?;
    let origin = Instant::now();
    let stats = Arc::new(Stats::default());

    let (server_addr, maybe_server, maybe_metrics, maybe_authority) = match &cfg.server {
        None => {
            let (key_pair, verifier) = generate_authority()?;
            let metrics = match cfg.metrics_listen {
                Some(addr) => Some(
                    rvp_metrics::install(addr)
                        .await
                        .map_err(|e| LoadgenError::Server(e.to_string()))?,
                ),
                None => None,
            };
            let server = start_in_process_server(verifier).await?;
            let addr = server.local_addr();
            (addr, Some(server), metrics, Some(key_pair))
        }
        Some(addr) => (*addr, None, None, None),
    };

    let encoding_key = match (&cfg.grant_key_pem, &maybe_authority) {
        (Some(pem), _) => {
            let kp = key_pair_from_pem(pem)?;
            EncodingKey::from_ed_pem(kp.serialize_pem().as_bytes())
        }
        (None, Some(kp)) => EncodingKey::from_ed_pem(kp.serialize_pem().as_bytes()),
        _ => unreachable!("validated: external mode requires grant key"),
    }
    .map_err(|e| LoadgenError::Auth(format!("signing key not usable as Ed25519 PEM: {e}")))?;

    let endpoint = build_client_endpoint(cfg.ca_pem.as_deref())?;

    // Pairwise key table: one key per unordered pair, dense room-unique kids.
    let total = cfg.rooms * cfg.clients_per_room;
    let mut recv_keys: Vec<HashMap<u8, PairwiseKey>> = vec![HashMap::new(); total];
    let mut send_peers: Vec<Vec<PeerSend>> = (0..total).map(|_| Vec::new()).collect();
    for room in 0..cfg.rooms {
        let mut kid: u8 = 1;
        for a in 0..cfg.clients_per_room {
            for b in (a + 1)..cfg.clients_per_room {
                let key = PairwiseKey::new(&random_key()?);
                let ia = room * cfg.clients_per_room + a;
                let ib = room * cfg.clients_per_room + b;
                recv_keys[ia].insert(kid, key.clone());
                recv_keys[ib].insert(kid, key.clone());
                send_peers[ia].push(PeerSend {
                    kid,
                    key: key.clone(),
                });
                send_peers[ib].push(PeerSend { kid, key });
                kid = kid
                    .checked_add(1)
                    .ok_or_else(|| LoadgenError::Config("kid space exhausted".into()))?;
            }
        }
    }

    // Connect + handshake all clients concurrently; measure join latency.
    let mut clients: Vec<Client> = Vec::with_capacity(total);
    let mut joins = tokio::task::JoinSet::new();
    for room in 0..cfg.rooms {
        for i in 0..cfg.clients_per_room {
            let idx = room * cfg.clients_per_room + i;
            let endpoint = endpoint.clone();
            let jwt = grant(
                &encoding_key,
                &format!("r{room}c{i}"),
                &format!("room-{room}"),
            )?;
            let peers = send_peers[idx]
                .iter()
                .map(|p| (p.kid, p.key.clone()))
                .collect::<Vec<_>>();
            let recv = recv_keys[idx].clone();
            let tick = cfg.tick;
            let payload_bytes = cfg.payload_bytes;
            let crypto = cfg.crypto;
            let stats = Arc::clone(&stats);
            joins.spawn(async move {
                let start = Instant::now();
                let (conn, control) = connect_and_join(endpoint, server_addr, &jwt).await?;
                let join_ms = start.elapsed().as_secs_f64() * 1000.0;
                stats.push_sample(&stats.join_ms, join_ms);
                // Drain room events so the server's control sends never block.
                tokio::spawn(async move {
                    let mut control = control;
                    while read_frame(&mut control).await.is_ok() {}
                });
                tokio::spawn(receive_loop(conn.clone(), recv, origin, Arc::clone(&stats)));
                let send_task = tokio::spawn(publish_loop(
                    conn.clone(),
                    peers,
                    tick,
                    payload_bytes,
                    crypto,
                    origin,
                    Arc::clone(&stats),
                ));
                Ok::<Client, LoadgenError>(Client { conn, send_task })
            });
        }
    }

    let mut join_errors = 0usize;
    while let Some(res) = joins.join_next().await {
        match res {
            Ok(Ok(client)) => clients.push(client),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "client failed to join");
                join_errors += 1;
            }
            Err(e) => {
                tracing::error!(error = %e, "join task panicked");
                join_errors += 1;
            }
        }
    }
    if join_errors > 0 {
        stop_all(&stats, &clients).await;
        return Err(LoadgenError::Handshake(format!(
            "{join_errors}/{total} clients failed to join"
        )));
    }

    // Warm-up (traffic flows, stats off) → measured window → drain → stop.
    if !cfg.warmup.is_zero() {
        tokio::time::sleep(cfg.warmup).await;
    }
    stats.recording.store(true, Ordering::Relaxed);
    let cpu_before = process_cpu_secs();
    tokio::time::sleep(cfg.duration).await;
    let cpu_after = process_cpu_secs();
    stats.recording.store(false, Ordering::Relaxed);
    stats.stop.store(true, Ordering::Relaxed);
    tokio::time::sleep(DRAIN).await;
    for client in &clients {
        client.conn.close(0u32.into(), b"loadgen done");
    }
    for client in clients {
        let _ = client.send_task.await;
    }

    if let Some(server) = maybe_server {
        server.shutdown().await;
    }
    if let Some(metrics) = maybe_metrics {
        metrics.shutdown().await;
    }
    drop(endpoint);

    let join_samples = stats.join_ms.lock().expect("stats lock poisoned").clone();
    let media_samples = stats.media_ms.lock().expect("stats lock poisoned").clone();

    let cpu_secs = match (cpu_before, cpu_after) {
        (Some(a), Some(b)) => Some((b - a).max(0.0)),
        _ => None,
    };

    Ok(Report {
        rooms: cfg.rooms,
        clients_per_room: cfg.clients_per_room,
        clients: total,
        crypto: cfg.crypto,
        tick_ms: cfg.tick.as_secs_f64() * 1000.0,
        payload_bytes: cfg.payload_bytes,
        duration_secs: cfg.duration.as_secs_f64(),
        warmup_secs: cfg.warmup.as_secs_f64(),
        join: latency_stats(&join_samples),
        media: latency_stats(&media_samples),
        datagrams_sent: stats.sent.load(Ordering::Relaxed),
        datagrams_opened: stats.opened.load(Ordering::Relaxed),
        bytes_sent: stats.bytes_sent.load(Ordering::Relaxed),
        bytes_received: stats.bytes_received.load(Ordering::Relaxed),
        missing_key_dropped: stats.missing_key.load(Ordering::Relaxed),
        decrypt_failures: stats.decrypt_fail.load(Ordering::Relaxed),
        malformed_dropped: stats.malformed.load(Ordering::Relaxed),
        send_errors: stats.send_errors.load(Ordering::Relaxed),
        wall_secs: origin.elapsed().as_secs_f64(),
        cpu_secs,
        samples_truncated: stats.truncated.load(Ordering::Relaxed),
    })
}

async fn stop_all(stats: &Stats, clients: &[Client]) {
    stats.stop.store(true, Ordering::Relaxed);
    for client in clients {
        client.conn.close(0u32.into(), b"loadgen abort");
    }
}

/// Generate an in-process authority (Ed25519) for grant minting + verification.
pub fn generate_authority() -> Result<(rcgen::KeyPair, Arc<rvp_auth::Verifier>), LoadgenError> {
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
        .map_err(|e| LoadgenError::Tls(e.to_string()))?;
    let verifier = rvp_auth::Verifier::from_public_pem(key_pair.public_key_pem().as_bytes())
        .map_err(|e| LoadgenError::Auth(e.to_string()))?;
    Ok((key_pair, Arc::new(verifier)))
}

/// Write an Ed25519 keypair for external-server grant signing (`keygen`).
pub fn write_keypair(
    out_dir: &std::path::Path,
) -> Result<(std::path::PathBuf, std::path::PathBuf), LoadgenError> {
    std::fs::create_dir_all(out_dir)?;
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
        .map_err(|e| LoadgenError::Tls(e.to_string()))?;
    let key_path = out_dir.join("grant_key.pem");
    let pub_path = out_dir.join("grant_pub.pem");
    std::fs::write(&key_path, key_pair.serialize_pem())?;
    std::fs::write(&pub_path, key_pair.public_key_pem())?;
    Ok((key_path, pub_path))
}

fn key_pair_from_pem(pem: &str) -> Result<rcgen::KeyPair, LoadgenError> {
    rcgen::KeyPair::from_pem(pem).map_err(|e| LoadgenError::Auth(format!("--grant-key: {e}")))
}

async fn start_in_process_server(
    verifier: Arc<rvp_auth::Verifier>,
) -> Result<rvp_server::RvpServer, LoadgenError> {
    // Load-test limits: many loopback clients share one source IP and crypto
    // mode intentionally runs well above the small-room production defaults.
    let limits = rvp_server::Limits {
        ingress_pps: 100_000.0,
        ingress_bps: 100_000_000.0,
        burst_secs: 2.0,
        max_conn_per_ip: 10_000,
        max_conn_per_user: 10_000,
        ..rvp_server::Limits::default()
    };
    let state = Arc::new(rvp_server::ServerState::with_limits(verifier, limits));
    rvp_server::start_server_with_state(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        rvp_server::server_config_self_signed().map_err(|e| LoadgenError::Server(e.to_string()))?,
        state,
    )
    .await
    .map_err(|e| LoadgenError::Server(e.to_string()))
}

fn build_client_endpoint(ca_pem: Option<&str>) -> Result<Endpoint, LoadgenError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut tls = match ca_pem {
        Some(pem) => {
            use rustls::pki_types::pem::PemObject;
            let mut roots = rustls::RootCertStore::empty();
            let certs: Vec<_> = rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| LoadgenError::Tls(format!("--ca: {e}")))?;
            let (added, _invalid) = roots.add_parsable_certificates(certs);
            if added == 0 {
                return Err(LoadgenError::Tls("--ca: no valid certificates".into()));
            }
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
        None => {
            // Lab-only: loadgen is a benchmark tool, not a production client.
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
                .with_no_client_auth()
        }
    };
    tls.alpn_protocols = vec![b"rvp/1".to_vec()];
    let mut transport = quinn::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(4 * 1024 * 1024));
    transport.datagram_send_buffer_size(4 * 1024 * 1024);
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(30)).expect("30s idle"),
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    let mut cfg = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| LoadgenError::Tls(e.to_string()))?,
    ));
    cfg.transport_config(Arc::new(transport));
    let mut endpoint =
        Endpoint::client(SocketAddr::from(([127, 0, 0, 1], 0))).map_err(LoadgenError::Io)?;
    endpoint.set_default_client_config(cfg);
    Ok(endpoint)
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

fn grant(key: &EncodingKey, user: &str, room: &str) -> Result<String, LoadgenError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs();
    let claims = GrantClaims {
        sub: user.to_owned(),
        room_id: room.to_owned(),
        role: "host".to_owned(),
        iss: "nest".to_owned(),
        aud: "rvp".to_owned(),
        exp: now + GRANT_TTL_SECS,
        jti: Some(format!("loadgen-{user}")),
    };
    encode(&Header::new(Algorithm::EdDSA), &claims, key)
        .map_err(|e| LoadgenError::Auth(format!("mint grant: {e}")))
}

fn random_key() -> Result<[u8; rvp_crypto::KEY_LEN], LoadgenError> {
    let mut key = [0u8; rvp_crypto::KEY_LEN];
    getrandom::getrandom(&mut key).map_err(|e| LoadgenError::Io(std::io::Error::other(e)))?;
    Ok(key)
}

/// Connect, `Hello` + `JoinRoom`, wait for `Welcome`; returns the connection
/// plus the still-open control stream and the measured join latency (ms).
async fn connect_and_join(
    endpoint: Endpoint,
    server: SocketAddr,
    jwt: &str,
) -> Result<(quinn::Connection, quinn::RecvStream), LoadgenError> {
    let conn = endpoint
        .connect(server, "localhost")
        .map_err(LoadgenError::Connect)?
        .await
        .map_err(LoadgenError::Connection)?;
    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| LoadgenError::Handshake(e.to_string()))?;
    send.write_all(
        &ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            grant_jwt: jwt.to_owned(),
        }
        .encode(),
    )
    .await
    .map_err(|e| LoadgenError::Handshake(e.to_string()))?;
    send.write_all(&ControlMessage::JoinRoom.encode())
        .await
        .map_err(|e| LoadgenError::Handshake(e.to_string()))?;
    drop(send);
    let mut control = recv;
    match read_frame(&mut control).await? {
        ControlMessage::Welcome { .. } => Ok((conn, control)),
        ControlMessage::Error { code, message } => {
            Err(LoadgenError::JoinRejected { code, message })
        }
        other => Err(LoadgenError::Handshake(format!(
            "expected Welcome, got {other:?}"
        ))),
    }
}

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<ControlMessage, LoadgenError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| LoadgenError::Handshake(format!("control frame len: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| LoadgenError::Handshake(format!("control frame body: {e}")))?;
    ControlMessage::decode_body(&body).map_err(|e| LoadgenError::Handshake(e.to_string()))
}

/// Publish at `tick` cadence: one plaintext datagram, or one sealed copy per
/// co-member (uplink ×(N−1), ADR-004). Runs until `stats.stop`.
async fn publish_loop(
    conn: quinn::Connection,
    peers: Vec<(u8, PairwiseKey)>,
    tick: Duration,
    payload_bytes: usize,
    crypto: bool,
    origin: Instant,
    stats: Arc<Stats>,
) {
    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut seq: u32 = 0;
    loop {
        interval.tick().await;
        if stats.stop.load(Ordering::Relaxed) {
            return;
        }
        seq = seq.wrapping_add(1);
        let ts = origin.elapsed().as_nanos() as u64;
        let mut payload = vec![0u8; payload_bytes];
        payload[..8].copy_from_slice(&ts.to_be_bytes());
        let recording = stats.recording.load(Ordering::Relaxed);
        if !crypto {
            let dgram = MediaDatagram::build(
                MediaHeader {
                    pt: PT_OPUS,
                    kid: 0,
                    sequence: seq,
                    timestamp: seq.wrapping_mul(960),
                    ..Default::default()
                },
                bytes::Bytes::from(payload),
            );
            let len = dgram.len() as u64;
            match conn.send_datagram(dgram) {
                Ok(()) => {
                    if recording {
                        stats.sent.fetch_add(1, Ordering::Relaxed);
                        stats.bytes_sent.fetch_add(len, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    if recording {
                        stats.send_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            continue;
        }
        for (kid, key) in &peers {
            let sealed = match key.seal(seq, &payload) {
                Ok(ct) => ct,
                Err(_) => {
                    if recording {
                        stats.send_errors.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
            };
            let dgram = MediaDatagram::build(
                MediaHeader {
                    pt: PT_OPUS_ENC,
                    kid: *kid,
                    sequence: seq,
                    timestamp: seq.wrapping_mul(960),
                    ..Default::default()
                },
                bytes::Bytes::from(sealed),
            );
            let len = dgram.len() as u64;
            match conn.send_datagram(dgram) {
                Ok(()) => {
                    if recording {
                        stats.sent.fetch_add(1, Ordering::Relaxed);
                        stats.bytes_sent.fetch_add(len, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    if recording {
                        stats.send_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

/// Receive, open by `kid`, and record e2e latency for the measured window.
/// Drains continuously so warm-up traffic never pressure-drops the socket.
async fn receive_loop(
    conn: quinn::Connection,
    recv_keys: HashMap<u8, PairwiseKey>,
    origin: Instant,
    stats: Arc<Stats>,
) {
    while let Ok(dgram) = conn.read_datagram().await {
        let now = origin.elapsed().as_nanos() as u64;
        if !stats.recording.load(Ordering::Relaxed) {
            continue;
        }
        let len = dgram.len() as u64;
        let parsed = match MediaDatagram::parse(dgram) {
            Ok(p) => p,
            Err(_) => {
                stats.malformed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let plaintext: Vec<u8> = match parsed.header.pt {
            PT_OPUS => parsed.payload.to_vec(),
            PT_OPUS_ENC => {
                let Some(key) = recv_keys.get(&parsed.header.kid) else {
                    stats.missing_key.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                match key.open(parsed.header.sequence, &parsed.payload) {
                    Ok(pt) => pt,
                    Err(_) => {
                        stats.decrypt_fail.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                }
            }
            _ => {
                stats.malformed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        if plaintext.len() < 8 {
            stats.malformed.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&plaintext[..8]);
        let sent_at = u64::from_be_bytes(ts);
        let ms = now.saturating_sub(sent_at) as f64 / 1_000_000.0;
        stats.push_sample(&stats.media_ms, ms);
        stats.opened.fetch_add(1, Ordering::Relaxed);
        stats.bytes_received.fetch_add(len, Ordering::Relaxed);
    }
}

fn latency_stats(samples: &[f64]) -> LatencyStats {
    if samples.is_empty() {
        return LatencyStats::default();
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite latencies"));
    let n = sorted.len();
    let mean = sorted.iter().sum::<f64>() / n as f64;
    LatencyStats {
        count: n as u64,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        p99_ms: percentile(&sorted, 0.99),
        max_ms: sorted[n - 1],
        mean_ms: mean,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = (p * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Process CPU (utime+stime) in seconds from `/proc/self/stat` (Linux only).
fn process_cpu_secs() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // comm may contain spaces/parentheses; fields after the last ')' start
    // at field 3 (state), so utime (field 14) is index 11 there.
    let after = stat.rfind(')')?;
    let fields: Vec<&str> = stat[after + 1..].split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    // CLK_TCK is 100 on all mainstream Linux targets.
    Some((utime + stime) / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_math() {
        let samples = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&samples, 0.5), 3.0);
        assert_eq!(percentile(&samples, 1.0), 5.0);
        assert_eq!(percentile(&samples, 0.0), 1.0);
    }

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn oversized_datagram_rejected() {
        let cfg = Config {
            payload_bytes: 1200,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn room_cap_enforced() {
        let cfg = Config {
            clients_per_room: 9,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn external_mode_requires_grant_key() {
        let cfg = Config {
            server: Some("127.0.0.1:4433".parse().unwrap()),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn keygen_roundtrip() {
        let dir = std::env::temp_dir().join(format!("rvp-loadgen-test-{}", std::process::id()));
        let (key_path, pub_path) = write_keypair(&dir).unwrap();
        let pem = std::fs::read_to_string(&key_path).unwrap();
        key_pair_from_pem(&pem).unwrap();
        assert!(std::fs::read_to_string(&pub_path)
            .unwrap()
            .contains("PUBLIC KEY"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
