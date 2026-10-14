//! RVP media server (Phase 2: JWT-authenticated control handshake + echo).
//!
//! Per connection (plan §3.4):
//! 1. TLS 1.3 (quinn/rustls, ALPN `rvp/1`)
//! 2. Client opens the control bi-stream and sends `Hello { version, grant_jwt }`
//! 3. Server verifies the EdDSA grant **offline** (`rvp-auth`), rate-limit hook slot
//! 4. `JoinRoom` → capacity check (P3) → `Welcome { participant_id, max_datagram }`
//! 5. Only after `Welcome` are DATAGRAMs read from the connection

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rvp_auth::{AuthError, Verifier};
use rvp_proto::control::{
    error_code, ControlError, ControlMessage, MAX_FRAME_LEN, PROTOCOL_VERSION,
};
use tokio::time::timeout;
use tracing::{debug, info, warn};

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

/// Start the media server on `addr` (use port 0 for ephemeral).
pub async fn start_server(
    addr: SocketAddr,
    server_config: ServerConfig,
    verifier: Arc<Verifier>,
) -> Result<RvpServer, ServerError> {
    let endpoint = Endpoint::server(server_config, addr)?;
    let local_addr = endpoint.local_addr()?;
    info!(%local_addr, "rvp server listening (quic, alpn=rvp/1)");
    let task = tokio::spawn(accept_loop(endpoint, verifier));
    Ok(RvpServer { local_addr, task })
}

async fn accept_loop(endpoint: Endpoint, verifier: Arc<Verifier>) -> Result<(), ServerError> {
    while let Some(connecting) = endpoint.accept().await {
        let verifier = verifier.clone();
        tokio::spawn(async move {
            match connecting.await {
                Ok(conn) => {
                    let remote = conn.remote_address();
                    info!(%remote, "connection established");
                    if let Err(e) = handle_connection(conn, verifier).await {
                        warn!(%remote, error = %e, "connection closed with error");
                    }
                }
                Err(e) => warn!(error = %e, "handshake failed"),
            }
        });
    }
    Ok(())
}

async fn handle_connection(
    conn: quinn::Connection,
    verifier: Arc<Verifier>,
) -> Result<(), ServerError> {
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|e| ServerError::Connection(format!("control stream: {e}")))?;

    // --- Hello: verify grant (plan §3.4 step 3) ---
    let claims = match read_control(&mut recv).await {
        Ok(ControlMessage::Hello {
            protocol_version,
            grant_jwt,
        }) => {
            if protocol_version != PROTOCOL_VERSION {
                send_error(
                    &mut send,
                    error_code::VERSION_UNSUPPORTED,
                    &format!("supported version {PROTOCOL_VERSION}"),
                )
                .await;
                return Ok(());
            }
            match verifier.verify(&grant_jwt) {
                Ok(claims) => claims,
                Err(e) => {
                    warn!(error = %e, "grant rejected");
                    let code = match e {
                        AuthError::Expired => error_code::UNAUTHORIZED,
                        _ => error_code::UNAUTHORIZED,
                    };
                    send_error(&mut send, code, &e.to_string()).await;
                    return Ok(());
                }
            }
        }
        Ok(other) => {
            send_error(
                &mut send,
                error_code::BAD_REQUEST,
                &format!("expected Hello, got {other:?}"),
            )
            .await;
            return Ok(());
        }
        Err(e) => {
            send_error(&mut send, error_code::BAD_REQUEST, &e.to_string()).await;
            return Ok(());
        }
    };

    // --- JoinRoom (plan §3.4 step 4) ---
    match read_control(&mut recv).await {
        Ok(ControlMessage::JoinRoom) => {}
        Ok(other) => {
            send_error(
                &mut send,
                error_code::BAD_REQUEST,
                &format!("expected JoinRoom, got {other:?}"),
            )
            .await;
            return Ok(());
        }
        Err(e) => {
            send_error(&mut send, error_code::BAD_REQUEST, &e.to_string()).await;
            return Ok(());
        }
    }

    let participant_id = PARTICIPANT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let welcome = ControlMessage::Welcome {
        participant_id,
        max_datagram: DEFAULT_MAX_DATAGRAM as u16,
    };
    send.write_all(&welcome.encode())
        .await
        .map_err(|e| ServerError::Connection(format!("welcome: {e}")))?;
    send.finish()
        .map_err(|e| ServerError::Connection(format!("finish: {e}")))?;
    info!(
        %participant_id,
        user = %claims.user_id,
        room = %claims.room_id,
        role = %claims.role,
        "joined room (datagrams now accepted)"
    );

    // --- P2 media path: echo (P3 replaces with room fanout) ---
    echo_datagrams(conn).await
}

/// P2 hot path: echo every datagram back verbatim (`Bytes` refcount, no copy).
async fn echo_datagrams(conn: quinn::Connection) -> Result<(), ServerError> {
    while let Ok(dgram) = conn.read_datagram().await {
        debug!(len = dgram.len(), "echo datagram");
        if conn.send_datagram(dgram).is_err() {
            break;
        }
    }
    Ok(())
}

/// Read one length-prefixed control frame (u32 BE length + body).
async fn read_control(recv: &mut quinn::RecvStream) -> Result<ControlMessage, ControlError> {
    let msg = timeout(HANDSHAKE_TIMEOUT, read_control_inner(recv))
        .await
        .map_err(|_| ControlError::Truncated)?;
    msg
}

async fn read_control_inner(recv: &mut quinn::RecvStream) -> Result<ControlMessage, ControlError> {
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

async fn send_error(send: &mut quinn::SendStream, code: u16, message: &str) {
    let frame = ControlMessage::Error {
        code,
        message: message.to_owned(),
    }
    .encode();
    let _ = send.write_all(&frame).await;
    let _ = send.finish();
    // Give the frame a chance to be delivered before the connection is dropped.
    let _ = timeout(Duration::from_secs(1), send.stopped()).await;
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
