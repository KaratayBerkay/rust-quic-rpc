//! Transport-agnostic participant + control-stream traits (P6a).
//!
//! The room registry and SFU fanout serve both the raw-QUIC endpoint (quinn)
//! and the WebTransport/HTTP3 endpoint (`wtransport`) with one shared room
//! state (plan §7: "same room registry, same JWT checks, same fanout").
//! These traits are the seam; each transport ships a thin adapter.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::Mutex;

/// Boxed future used by the transport traits (object-safety).
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How long a single server-push control write may block before we give up
/// (a stuck peer must not stall broadcasts to the rest of the room).
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-participant send side: media datagrams + server-push control frames.
pub trait ParticipantTx: Send + Sync + 'static {
    /// Fire-and-forget media datagram; drop on congestion (live audio, §4).
    fn send_datagram(&self, bytes: Bytes);
    /// Best-effort control frame write; implementations serialize per peer.
    fn send_control(&self, frame: Bytes) -> BoxFut<'static, ()>;
    /// Flush and close the control stream after a terminal Error frame so the
    /// frame reaches the peer before the connection is torn down (P7b:
    /// dropping the connection otherwise races the write). Default no-op.
    fn finish_control(&self) -> BoxFut<'static, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// Frame reader used by the control handshake (plan §3.4).
pub trait ControlReader: Send {
    fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFut<'a, std::io::Result<()>>;
}

/// Frame writer used by the control handshake and error replies.
pub trait ControlWriter: Send {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFut<'a, std::io::Result<()>>;
    fn finish<'a>(&'a mut self) -> BoxFut<'a, std::io::Result<()>>;
}

/// Media datagram source for the SFU fanout loop.
pub trait DatagramSource: Send {
    /// Next datagram, or `None` when the connection is done.
    fn recv_datagram<'a>(&'a mut self) -> BoxFut<'a, Option<Bytes>>;
}

// --- quinn adapters -------------------------------------------------------

pub struct QuinnControlReader(pub quinn::RecvStream);

impl ControlReader for QuinnControlReader {
    fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0
                .read_exact(buf)
                .await
                .map(drop)
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
        })
    }
}

pub struct QuinnControlWriter(pub quinn::SendStream);

impl ControlWriter for QuinnControlWriter {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0
                .write_all(buf)
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        })
    }

    fn finish<'a>(&'a mut self) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0
                .finish()
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
            // Give the final frame a chance to be delivered before the
            // connection is dropped (matches the pre-trait send_error path).
            let _ = tokio::time::timeout(Duration::from_secs(1), self.0.stopped()).await;
            Ok(())
        })
    }
}

pub struct QuinnDatagramSource(pub quinn::Connection);

impl DatagramSource for QuinnDatagramSource {
    fn recv_datagram<'a>(&'a mut self) -> BoxFut<'a, Option<Bytes>> {
        Box::pin(async move { self.0.read_datagram().await.ok() })
    }
}

/// quinn participant: datagrams via the connection, control via the send
/// stream kept open for server-push events after Welcome.
pub struct QuinnTx {
    conn: quinn::Connection,
    control: Arc<Mutex<quinn::SendStream>>,
}

impl QuinnTx {
    pub fn new(conn: quinn::Connection, control: quinn::SendStream) -> Self {
        Self {
            conn,
            control: Arc::new(Mutex::new(control)),
        }
    }
}

impl ParticipantTx for QuinnTx {
    fn send_datagram(&self, bytes: Bytes) {
        // Quinn drops on overflow — correct for live audio (plan §4).
        let _ = self.conn.send_datagram(bytes);
    }

    fn send_control(&self, frame: Bytes) -> BoxFut<'static, ()> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let mut guard = control.lock().await;
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, guard.write_all(&frame)).await;
        })
    }

    fn finish_control(&self) -> BoxFut<'static, ()> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let mut guard = control.lock().await;
            let flush = async {
                if guard.finish().is_ok() {
                    // Resolves once the peer reads the stream to completion.
                    let _ = guard.stopped().await;
                }
            };
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, flush).await;
        })
    }
}

// --- wtransport adapters --------------------------------------------------

pub struct WtControlReader(pub wtransport::RecvStream);

impl ControlReader for WtControlReader {
    fn read_exact<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0
                .read_exact(buf)
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
        })
    }
}

pub struct WtControlWriter(pub wtransport::SendStream);

impl ControlWriter for WtControlWriter {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0
                .write_all(buf)
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        })
    }

    fn finish<'a>(&'a mut self) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.0
                .finish()
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
            // Best-effort delivery wait, same rationale as the quinn adapter.
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, self.0.stopped()).await;
            Ok(())
        })
    }
}

pub struct WtDatagramSource(pub wtransport::Connection);

impl DatagramSource for WtDatagramSource {
    fn recv_datagram<'a>(&'a mut self) -> BoxFut<'a, Option<Bytes>> {
        Box::pin(async move {
            self.0
                .receive_datagram()
                .await
                .ok()
                .map(|dgram| dgram.payload())
        })
    }
}

/// WebTransport participant: same media/control split as [`QuinnTx`].
pub struct WtTx {
    conn: wtransport::Connection,
    control: Arc<Mutex<wtransport::SendStream>>,
}

impl WtTx {
    pub fn new(conn: wtransport::Connection, control: wtransport::SendStream) -> Self {
        Self {
            conn,
            control: Arc::new(Mutex::new(control)),
        }
    }
}

impl ParticipantTx for WtTx {
    fn send_datagram(&self, bytes: Bytes) {
        // Drop on congestion; live audio is not retransmitted (plan §4).
        let _ = self.conn.send_datagram(bytes);
    }

    fn send_control(&self, frame: Bytes) -> BoxFut<'static, ()> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let mut guard = control.lock().await;
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, guard.write_all(&frame)).await;
        })
    }

    fn finish_control(&self) -> BoxFut<'static, ()> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let mut guard = control.lock().await;
            let flush = async {
                if guard.finish().await.is_ok() {
                    let _ = guard.stopped().await;
                }
            };
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, flush).await;
        })
    }
}
