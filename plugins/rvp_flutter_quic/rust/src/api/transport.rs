//! RVP QUIC transport (P4a): thin datagram + control-stream surface over quinn.
//!
//! Threading model: one lazily-created multi-thread tokio `Runtime` owns every
//! socket; all quinn I/O runs there via `runtime().spawn(..).await`. Dart calls
//! may originate from any FRB thread. Incoming packets are pushed to Dart as
//! FRB streams; outgoing frames are written through an async-mutexed handle.
//!
//! Framing contract (mirrors `rvp-proto`):
//! - control: complete frames including the `u32 BE` length prefix,
//! - datagrams: raw RVP media DATAGRAM bytes (header included by the caller).
//!
//! Errors are a plain struct + unit-variant kind enum on purpose: FRB renders
//! that as a simple Dart exception class, avoiding freezed/build_runner.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::frb_generated::StreamSink;
use quinn::{ClientConfig, Connection, Endpoint, TransportConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::runtime::Runtime;

/// ALPN protocol id spoken by `rvp-server`.
const ALPN: &[u8] = b"rvp/1";

/// Hard cap on inbound control frames (matches `rvp-proto::control`).
const MAX_CONTROL_FRAME_LEN: usize = 16 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("rvp_flutter_quic: tokio runtime"))
}

/// Failure category, surfaced to Dart as a plain enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransportErrorKind {
    #[error("connect")]
    Connect,
    #[error("tls")]
    Tls,
    #[error("control")]
    Control,
    #[error("closed")]
    Closed,
    #[error("datagram")]
    Datagram,
}

/// Transport-level failure surfaced to Dart as an exception.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

impl TransportError {
    fn connect(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Connect,
            message: message.into(),
        }
    }

    fn tls(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Tls,
            message: message.into(),
        }
    }

    fn control(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Control,
            message: message.into(),
        }
    }

    fn closed(message: impl Into<String>) -> Self {
        Self {
            kind: TransportErrorKind::Closed,
            message: message.into(),
        }
    }

    fn datagram(len: usize) -> Self {
        Self {
            kind: TransportErrorKind::Datagram,
            message: format!("datagram of {len} bytes exceeds the path MTU"),
        }
    }
}

/// TLS verification for the handshake.
enum Tls {
    /// DEV ONLY — accept any server certificate (self-signed test servers).
    Insecure,
    /// Verify against this PEM bundle (root CA or pinned leaf).
    Verified(String),
}

struct RvpInner {
    _endpoint: Endpoint,
    conn: Connection,
    control_send: tokio::sync::Mutex<Option<quinn::SendStream>>,
    control_recv: tokio::sync::Mutex<Option<quinn::RecvStream>>,
}

/// An open RVP connection: control bi-stream + media DATAGRAMs.
#[derive(Clone)]
#[flutter_rust_bridge::frb(opaque)]
pub struct RvpConnection {
    inner: Arc<RvpInner>,
}

/// Connect while **skipping certificate verification** — local testing only
/// (against `rvp-server --self-signed`); production must use [`connect_verified`].
pub async fn connect_insecure(
    addr: String,
    server_name: String,
) -> Result<RvpConnection, TransportError> {
    connect_with(addr, server_name, Tls::Insecure).await
}

/// Connect and verify the server certificate against `ca_pem`.
pub async fn connect_verified(
    addr: String,
    server_name: String,
    ca_pem: String,
) -> Result<RvpConnection, TransportError> {
    connect_with(addr, server_name, Tls::Verified(ca_pem)).await
}

async fn connect_with(
    addr: String,
    server_name: String,
    tls: Tls,
) -> Result<RvpConnection, TransportError> {
    let client_config = build_client_config(&tls)?;
    let task = async move {
        let addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| TransportError::connect(format!("bad address {addr:?}: {e}")))?;
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().expect("unspecified endpoint"))
            .map_err(|e| TransportError::connect(e.to_string()))?;
        endpoint.set_default_client_config(client_config);
        let connecting = endpoint
            .connect(addr, &server_name)
            .map_err(|e| TransportError::connect(e.to_string()))?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| TransportError::connect(format!("timeout after {CONNECT_TIMEOUT:?}")))?
            .map_err(|e| TransportError::connect(e.to_string()))?;
        Ok::<_, TransportError>((endpoint, conn))
    };
    let (endpoint, conn) = runtime()
        .spawn(task)
        .await
        .map_err(|e| TransportError::connect(format!("runtime: {e}")))??;
    Ok(RvpConnection {
        inner: Arc::new(RvpInner {
            _endpoint: endpoint,
            conn,
            control_send: tokio::sync::Mutex::new(None),
            control_recv: tokio::sync::Mutex::new(None),
        }),
    })
}

impl RvpConnection {
    /// Remote socket address (`ip:port`).
    pub fn remote_addr(&self) -> String {
        self.inner.conn.remote_address().to_string()
    }

    /// Largest DATAGRAM the current path accepts, in bytes.
    pub fn max_datagram_size(&self) -> Option<u16> {
        self.inner.conn.max_datagram_size().map(|s| s as u16)
    }

    /// Open the RVP control bi-stream (at most one per connection).
    ///
    /// After this, [`Self::write_control_frame`] and the control readers work.
    /// Opening again replaces the previous stream.
    pub async fn open_control(&self) -> Result<(), TransportError> {
        let conn = self.inner.conn.clone();
        let (send, recv) = runtime()
            .spawn(async move { conn.open_bi().await })
            .await
            .map_err(|e| TransportError::control(format!("runtime: {e}")))?
            .map_err(|e| TransportError::control(e.to_string()))?;
        *self.inner.control_send.lock().await = Some(send);
        *self.inner.control_recv.lock().await = Some(recv);
        Ok(())
    }

    /// Write one complete RVP control frame (`u32 BE` length prefix included).
    pub async fn write_control_frame(&self, frame: Vec<u8>) -> Result<(), TransportError> {
        let inner = self.inner.clone();
        runtime()
            .spawn(async move {
                let mut guard = inner.control_send.lock().await;
                match guard.as_mut() {
                    Some(send) => send
                        .write_all(&frame)
                        .await
                        .map_err(|e| TransportError::control(e.to_string())),
                    None => Err(TransportError::control("control stream not open")),
                }
            })
            .await
            .map_err(|e| TransportError::control(format!("runtime: {e}")))?
    }

    /// Read the next complete control frame (prefix included).
    ///
    /// Single consumer: either pull with this, or push with
    /// [`Self::start_control_reader`], not both.
    pub async fn read_control_frame(&self) -> Result<Vec<u8>, TransportError> {
        let mut guard = self.inner.control_recv.lock().await;
        match guard.as_mut() {
            Some(recv) => read_frame(recv).await,
            None => Err(TransportError::control("control stream not open")),
        }
    }

    /// Push every incoming control frame (prefix included) into `sink`.
    /// The Dart stream completes when the connection closes.
    pub fn start_control_reader(&self, sink: StreamSink<Vec<u8>>) {
        let this = self.clone();
        runtime().spawn(async move {
            while let Ok(frame) = this.read_control_frame().await {
                if sink.add(frame).is_err() {
                    break;
                }
            }
            // Dropping `sink` closes the Dart stream.
        });
    }

    /// Send one RVP DATAGRAM (raw bytes; media header included by the caller).
    pub fn send_datagram(&self, data: Vec<u8>) -> Result<(), TransportError> {
        let len = data.len();
        self.inner
            .conn
            .send_datagram(data.into())
            .map_err(|e| match e {
                quinn::SendDatagramError::TooLarge => TransportError::datagram(len),
                quinn::SendDatagramError::ConnectionLost(inner) => {
                    TransportError::closed(inner.to_string())
                }
                other => TransportError::closed(other.to_string()),
            })
    }

    /// Read the next DATAGRAM payload.
    ///
    /// Single consumer: either pull with this, or push with
    /// [`Self::start_datagram_reader`], not both.
    pub async fn read_datagram(&self) -> Result<Vec<u8>, TransportError> {
        match self.inner.conn.read_datagram().await {
            Ok(data) => Ok(data.to_vec()),
            Err(e) => Err(TransportError::closed(e.to_string())),
        }
    }

    /// Push every incoming DATAGRAM payload into `sink`.
    /// The Dart stream completes when the connection closes.
    pub fn start_datagram_reader(&self, sink: StreamSink<Vec<u8>>) {
        let inner = self.inner.clone();
        runtime().spawn(async move {
            while let Ok(data) = inner.conn.read_datagram().await {
                if sink.add(data.to_vec()).is_err() {
                    break;
                }
            }
        });
    }

    /// Close the connection (QUIC application close, best-effort).
    pub fn close(&self, code: u32, reason: String) {
        self.inner.conn.close(code.into(), reason.as_bytes());
    }
}

/// Read one length-prefixed frame into a `Vec` (prefix included).
async fn read_frame(recv: &mut quinn::RecvStream) -> Result<Vec<u8>, TransportError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| TransportError::control(e.to_string()))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if !(2..=MAX_CONTROL_FRAME_LEN).contains(&len) {
        return Err(TransportError::control(format!(
            "invalid frame length {len}"
        )));
    }
    let mut frame = vec![0u8; 4 + len];
    frame[..4].copy_from_slice(&len_buf);
    recv.read_exact(&mut frame[4..])
        .await
        .map_err(|e| TransportError::control(e.to_string()))?;
    Ok(frame)
}

fn build_client_config(tls: &Tls) -> Result<ClientConfig, TransportError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut crypto = match tls {
        Tls::Insecure => rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth(),
        Tls::Verified(pem) => {
            let mut roots = rustls::RootCertStore::empty();
            let input = pem.as_bytes();
            for cert in CertificateDer::pem_slice_iter(input) {
                let cert = cert.map_err(|e| TransportError::tls(e.to_string()))?;
                roots
                    .add(cert)
                    .map_err(|e| TransportError::tls(e.to_string()))?;
            }
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
    };
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let mut transport = TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(1024 * 1024));
    transport.datagram_send_buffer_size(1024 * 1024);
    let mut cfg = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| TransportError::tls(e.to_string()))?,
    ));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

/// Dev-mode certificate verifier (mirrors `rvp-server`'s test client).
#[derive(Debug)]
struct SkipServerVerification;

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
