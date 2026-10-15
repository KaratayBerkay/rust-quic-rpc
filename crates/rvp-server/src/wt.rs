//! WebTransport/HTTP3 endpoint (plan §7, P6a).
//!
//! Browser clients connect with `new WebTransport("https://host:port/", {
//!   serverCertificateHashes: [...]
//! })` — self-signed identity, leaf-cert SHA-256 logged/exposed for the
//! browser pin. Same `ServerState`, control handshake, and SFU fanout as
//! the raw-QUIC endpoint.

use std::net::SocketAddr;
use std::sync::Arc;

use tracing::{info, warn};
use wtransport::endpoint::endpoint_side;
use wtransport::tls::{Sha256Digest, Sha256DigestFmt};
use wtransport::{Endpoint, Identity, ServerConfig};

use rvp_metrics::instrument;

use crate::transport::{WtControlReader, WtControlWriter, WtDatagramSource, WtTx};
use crate::{drive_handshake, serve_joined, ServerError, ServerState};

/// Running WebTransport server handle.
pub struct WtServer {
    local_addr: SocketAddr,
    /// SHA-256 of the leaf certificate DER — feed to browser
    /// `WebTransportOptions.serverCertificateHashes`.
    cert_hash: Sha256Digest,
    task: tokio::task::JoinHandle<Result<(), ServerError>>,
}

impl WtServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn cert_hash(&self) -> Sha256Digest {
        self.cert_hash.clone()
    }

    /// Colon-separated hex (`ab:cd:...`) — the string form browsers accept
    /// after stripping colons, and the form `rvp-cli` logs.
    pub fn cert_hash_hex(&self) -> String {
        self.cert_hash.fmt(Sha256DigestFmt::DottedHex)
    }

    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Start the WebTransport endpoint on `addr` (port 0 = ephemeral) with a
/// fresh self-signed ECDSA P-256 identity (required by the W3C hash API;
/// 14-day validity, regenerated per process like the quinn self-signed path).
pub async fn start_wt_server(
    addr: SocketAddr,
    state: Arc<ServerState>,
) -> Result<WtServer, ServerError> {
    let identity = Identity::self_signed(["localhost", "127.0.0.1"])
        .map_err(|e| ServerError::Tls(format!("wt self-signed identity: {e}")))?;
    let cert_hash = identity
        .certificate_chain()
        .as_slice()
        .first()
        .ok_or_else(|| ServerError::Tls("wt identity: empty certificate chain".into()))?
        .hash();

    let config = ServerConfig::builder()
        .with_bind_address(addr)
        .with_identity(identity)
        .build();
    let endpoint = Endpoint::server(config)?;
    let local_addr = endpoint.local_addr()?;
    info!(
        %local_addr,
        cert_hash = %cert_hash.fmt(Sha256DigestFmt::DottedHex),
        "rvp server listening (webtransport, http/3)"
    );
    let task = tokio::spawn(accept_loop(endpoint, state));
    Ok(WtServer {
        local_addr,
        cert_hash,
        task,
    })
}

async fn accept_loop(
    endpoint: Endpoint<endpoint_side::Server>,
    state: Arc<ServerState>,
) -> Result<(), ServerError> {
    loop {
        let incoming = endpoint.accept().await;
        let state = state.clone();

        // P7b: stateless address validation before any state is allocated
        // (mirrors the quinn accept loop; `retry` panics only if validated).
        if state.limits.enable_retry && !incoming.remote_address_validated() {
            incoming.retry();
            instrument::retry_sent();
            continue;
        }

        // P7b: per-source-IP cap (task holds the slot to completion).
        let ip = incoming.remote_address().ip();
        let Some(slot) = Arc::clone(&state.conns).try_acquire_ip(ip) else {
            instrument::connection_rejected("ip_cap");
            incoming.refuse();
            continue;
        };

        tokio::spawn(async move {
            let _slot = slot;
            instrument::connection_opened("webtransport");
            match incoming.await {
                Ok(session_request) => {
                    let remote = session_request.remote_address();
                    match session_request.accept().await {
                        Ok(conn) => {
                            info!(%remote, "wt session established");
                            if let Err(e) = handle_connection(conn, state).await {
                                warn!(%remote, error = %e, "wt session closed with error");
                            }
                        }
                        Err(e) => {
                            instrument::handshake_failed("transport");
                            warn!(%remote, error = %e, "wt session refused");
                        }
                    }
                }
                Err(e) => {
                    instrument::handshake_failed("transport");
                    warn!(error = %e, "wt handshake failed");
                }
            }
            instrument::connection_closed("webtransport");
        });
    }
}

async fn handle_connection(
    conn: wtransport::Connection,
    state: Arc<ServerState>,
) -> Result<(), ServerError> {
    let (send, recv) = conn
        .accept_bi()
        .await
        .map_err(|e| ServerError::Connection(format!("wt control stream: {e}")))?;

    let mut writer = WtControlWriter(send);
    let mut reader = WtControlReader(recv);
    let claims = match drive_handshake(&state, &mut reader, &mut writer).await {
        Some(claims) => claims,
        None => return Ok(()),
    };
    let WtControlWriter(send) = writer;
    let tx = Arc::new(WtTx::new(conn.clone(), send));
    let mut dgrams = WtDatagramSource(conn);
    serve_joined(state, claims, tx, &mut dgrams).await
}
