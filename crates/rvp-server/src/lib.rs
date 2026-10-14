//! RVP media server (Phase 1: QUIC DATAGRAM echo).
//!
//! Hot path (plan §4): per-connection tokio task reads DATAGRAMs and acts on
//! them without copying payload buffers (`bytes::Bytes` refcounts).
//! P1 echoes every datagram back; P3 replaces echo with room fanout.

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tracing::{debug, info, warn};

pub const DEFAULT_MAX_DATAGRAM: usize = 1200;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls config: {0}")]
    Tls(String),
}

/// Listen address + running echo server handle.
pub struct EchoServer {
    local_addr: SocketAddr,
    task: tokio::task::JoinHandle<Result<(), ServerError>>,
}

impl EchoServer {
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
        quinn::IdleTimeout::try_from(std::time::Duration::from_secs(30)).expect("30s idle"),
    ));
    t.keep_alive_interval(Some(std::time::Duration::from_secs(10)));
    t
}

/// Start an echo server on `addr` (use port 0 for ephemeral).
pub async fn start_echo_server(addr: SocketAddr) -> Result<EchoServer, ServerError> {
    start_echo_server_with_config(addr, server_config_self_signed()?).await
}

/// Start an echo server with an explicit `ServerConfig` (prod entrypoint).
pub async fn start_echo_server_with_config(
    addr: SocketAddr,
    server_config: ServerConfig,
) -> Result<EchoServer, ServerError> {
    let endpoint = Endpoint::server(server_config, addr)?;
    let local_addr = endpoint.local_addr()?;
    info!(%local_addr, "rvp echo server listening (quic, alpn=rvp/1)");
    let task = tokio::spawn(accept_loop(endpoint));
    Ok(EchoServer { local_addr, task })
}

async fn accept_loop(endpoint: Endpoint) -> Result<(), ServerError> {
    while let Some(connecting) = endpoint.accept().await {
        tokio::spawn(async move {
            match connecting.await {
                Ok(conn) => {
                    let remote = conn.remote_address();
                    info!(%remote, "connection established");
                    if let Err(e) = echo_connection(conn).await {
                        warn!(%remote, error = %e, "connection closed with error");
                    }
                }
                Err(e) => warn!(error = %e, "handshake failed"),
            }
        });
    }
    Ok(())
}

/// P1 hot path: read datagram -> echo back verbatim (`Bytes` refcount, no copy).
async fn echo_connection(conn: quinn::Connection) -> Result<(), ServerError> {
    while let Ok(dgram) = conn.read_datagram().await {
        debug!(len = dgram.len(), "echo datagram");
        if conn.send_datagram(dgram).is_err() {
            break;
        }
    }
    Ok(())
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
