//! P9a WebSocket relay endpoint (plan §7 fallback, open decision 8).
//!
//! Plain-WebSocket twin of the WebTransport endpoint for networks that block
//! UDP (CGNAT/mobile): same `ServerState`, same JWT handshake, same rooms and
//! SFU fanout — only the framing differs. Every WS binary message carries a
//! 1-byte tag ([`rvp_proto::ws`]): control byte-stream chunks vs one complete
//! media datagram. Reliable ordered delivery replaces QUIC congestion control
//! (degraded by design; live audio still drops when the outbound queue fills).
//!
//! One TCP connection carries both channels. A demux task routes inbound
//! messages to a control channel (unbounded — frames are ≤16 KiB) and a
//! datagram channel (bounded — drop on congestion, mirroring UDP semantics).
//! Outbound rides one channel too: datagrams `try_send` (drop when full),
//! control `send().await` under a timeout, and a `Finish` sentinel makes the
//! writer flush, close the sink and exit — so terminal `Error` frames always
//! reach the peer before the socket goes away.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{accept_async_with_config, WebSocketStream};
use tracing::{info, warn};

use rvp_metrics::instrument;
use rvp_proto::ws::{frame_control, frame_datagram, parse_ws_message, WsMessage};

use crate::transport::{BoxFut, ControlReader, ControlWriter, DatagramSource, ParticipantTx};
use crate::{drive_handshake, serve_joined, ServerError, ServerState};

/// Inbound/outbound datagram queue depth; beyond this live audio drops.
const DATAGRAM_QUEUE: usize = 256;
/// Headroom above the datagram queue so control frames always have room to
/// queue behind a full media queue (control must never be blocked by media).
const CONTROL_HEADROOM: usize = 32;
/// Control writes may block this long before we give up (mirrors the
/// `CONTROL_WRITE_TIMEOUT` of the WT/QUIC adapters).
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// Hard cap on one WS message: control frames ≤16 KiB, datagrams ≤1200 B.
const MAX_MESSAGE: usize = 32 * 1024;
/// Grace period for draining terminal frames / closing tasks after a session
/// ends (the peer may be gone — never block teardown on it).
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);

type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;
type WsStream = SplitStream<WebSocketStream<TcpStream>>;

/// Outbound message from a participant to the writer task.
enum Outbound {
    Control(Bytes),
    Datagram(Bytes),
    /// Flush and close (terminal `Error` delivered, then close).
    Finish,
}

/// Running WebSocket server handle.
pub struct WsServer {
    local_addr: SocketAddr,
    task: tokio::task::JoinHandle<Result<(), ServerError>>,
}

impl WsServer {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Start the WebSocket endpoint on `addr` (port 0 = ephemeral). Cleartext
/// `ws://` — put TLS at the edge (reverse proxy) for production; a browser
/// loading an https page refuses `ws://` (mixed content), and unlike QUIC
/// there is no certificate-hash pinning API for WebSockets, so `wss://`
/// always means "real cert, usually terminated at the proxy".
pub async fn start_ws_server(
    addr: SocketAddr,
    state: Arc<ServerState>,
) -> Result<WsServer, ServerError> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| ServerError::Connection(format!("ws bind {addr}: {e}")))?;
    let local_addr = listener.local_addr()?;
    info!(%local_addr, "rvp server listening (websocket, ws://)");
    let task = tokio::spawn(accept_loop(listener, state));
    Ok(WsServer { local_addr, task })
}

async fn accept_loop(listener: TcpListener, state: Arc<ServerState>) -> Result<(), ServerError> {
    loop {
        let (stream, remote) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                warn!(error = %e, "ws accept failed");
                continue;
            }
        };
        let state = state.clone();

        // P7b per-source-IP cap (no QUIC Retry on plain TCP — the HTTP
        // upgrade itself is the cheap stateless probe).
        let Some(slot) = Arc::clone(&state.conns).try_acquire_ip(remote.ip()) else {
            instrument::connection_rejected("ip_cap");
            drop(stream);
            continue;
        };

        tokio::spawn(async move {
            let _slot = slot;
            instrument::connection_opened("websocket");
            if let Err(e) = handle_connection(stream, state).await {
                warn!(%remote, error = %e, "ws session closed with error");
            }
            instrument::connection_closed("websocket");
        });
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<ServerState>) -> Result<(), ServerError> {
    // `WebSocketConfig` is `#[non_exhaustive]` — start from defaults and
    // cap message size (control ≤16 KiB, datagrams ≤1200 B).
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE);
    let ws = accept_async_with_config(stream, Some(config))
        .await
        .map_err(|e| {
            instrument::handshake_failed("transport");
            ServerError::Connection(format!("ws upgrade: {e}"))
        })?;

    let (sink, stream) = ws.split();

    // Shared outbound channel: handshake writer, participant tx and writer
    // task all hold a handle; the writer exits when every handle is gone or
    // a `Finish` arrives.
    let (out_tx, out_rx) = mpsc::channel::<Outbound>(DATAGRAM_QUEUE + CONTROL_HEADROOM);
    let writer_handle = tokio::spawn(writer_task(sink, out_rx));

    // Demux: tagged binary messages → control (reliable) / datagram queues.
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel::<Bytes>();
    let (dgram_tx, dgram_rx) = mpsc::channel::<Bytes>(DATAGRAM_QUEUE);
    let demux = tokio::spawn(demux_task(stream, ctrl_tx, dgram_tx));

    let mut reader = WsControlReader {
        rx: ctrl_rx,
        buf: BytesMut::new(),
    };
    let mut writer = WsControlWriter { tx: out_tx.clone() };

    let claims = match drive_handshake(&state, &mut reader, &mut writer).await {
        Some(claims) => claims,
        None => {
            // Error frame is queued: drop our handles so `Finish` + drain
            // close the socket after it lands.
            drop(writer);
            drop(out_tx);
            let _ = tokio::time::timeout(TEARDOWN_GRACE, writer_handle).await;
            demux.abort();
            return Ok(());
        }
    };
    drop(writer); // handshake phase over; WsTx owns the outbound channel now

    let tx = Arc::new(WsTx { tx: out_tx });
    let mut dgrams = WsDatagramSource { rx: dgram_rx };
    let result = serve_joined(state, claims, tx, Box::new(reader), &mut dgrams).await;

    // Session over: all outbound handles are gone (WsTx dropped with the
    // room), so the writer closes the sink. Give terminal frames a moment,
    // then stop waiting — the peer may never close its side.
    let _ = tokio::time::timeout(TEARDOWN_GRACE, writer_handle).await;
    let _ = tokio::time::timeout(TEARDOWN_GRACE, demux).await;
    result
}

/// Route inbound WS binary messages into the control/datagram queues. Ends
/// (and drops both senders) on close, socket error, unknown tag or framing
/// violation — the reader then sees EOF and the session tears down.
async fn demux_task(
    mut stream: WsStream,
    ctrl_tx: mpsc::UnboundedSender<Bytes>,
    dgram_tx: mpsc::Sender<Bytes>,
) {
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Binary(bytes)) => match parse_ws_message(&bytes) {
                Ok(WsMessage::Control(chunk)) => {
                    if ctrl_tx.send(chunk).is_err() {
                        break;
                    }
                }
                Ok(WsMessage::Datagram(dgram)) => {
                    // Full queue = congestion; drop like a UDP socket.
                    let _ = dgram_tx.try_send(dgram);
                }
                Err(e) => {
                    warn!(error = %e, "ws message rejected");
                    break;
                }
            },
            Ok(Message::Close(_)) | Err(_) => break,
            // Ping/Pong are handled by tungstenite's protocol layer; Text
            // is not part of RVP — ignore it.
            Ok(_) => {}
        }
    }
}

async fn writer_task(mut sink: WsSink, mut rx: mpsc::Receiver<Outbound>) {
    while let Some(msg) = rx.recv().await {
        let wire = match msg {
            Outbound::Control(bytes) => frame_control(&bytes),
            Outbound::Datagram(bytes) => frame_datagram(&bytes),
            Outbound::Finish => {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        };
        if sink.send(Message::Binary(wire)).await.is_err() {
            break;
        }
    }
    let _ = sink.close().await;
}

/// Control byte-stream reader: concatenates tagged control chunks until
/// `read_exact` is satisfied (mirrors a QUIC bi-stream's byte semantics;
/// `WsControlReader` is moved into `serve_joined` for the post-join
/// control loop).
pub struct WsControlReader {
    rx: mpsc::UnboundedReceiver<Bytes>,
    buf: BytesMut,
}

impl ControlReader for WsControlReader {
    fn read_exact<'a>(&'a mut self, out: &'a mut [u8]) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            let need = out.len();
            let mut filled = 0;
            while filled < need {
                if self.buf.is_empty() {
                    match self.rx.recv().await {
                        Some(chunk) => self.buf.extend_from_slice(&chunk),
                        None => {
                            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
                        }
                    }
                }
                let take = self.buf.len().min(need - filled);
                out[filled..filled + take].copy_from_slice(&self.buf[..take]);
                let _ = self.buf.split_to(take);
                filled += take;
            }
            Ok(())
        })
    }
}

/// Control writer: each `write_all` becomes one tagged control message — the
/// reader concatenates chunks, so byte-stream semantics hold across calls.
/// A stalled peer must not wedge the handshake, hence the timeout.
pub struct WsControlWriter {
    tx: mpsc::Sender<Outbound>,
}

impl ControlWriter for WsControlWriter {
    fn write_all<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            let frame = Bytes::copy_from_slice(bytes);
            tokio::time::timeout(
                CONTROL_WRITE_TIMEOUT,
                self.tx.send(Outbound::Control(frame)),
            )
            .await
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        })
    }

    fn finish<'a>(&'a mut self) -> BoxFut<'a, std::io::Result<()>> {
        Box::pin(async move {
            tokio::time::timeout(CONTROL_WRITE_TIMEOUT, self.tx.send(Outbound::Finish))
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))?
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        })
    }
}

/// Inbound media datagrams (queued by the demux task).
pub struct WsDatagramSource {
    rx: mpsc::Receiver<Bytes>,
}

impl DatagramSource for WsDatagramSource {
    fn recv_datagram<'a>(&'a mut self) -> BoxFut<'a, Option<Bytes>> {
        Box::pin(async move { self.rx.recv().await })
    }
}

/// WebSocket participant: datagrams drop when the outbound queue is full
/// (live audio, plan §4), control waits up to [`CONTROL_WRITE_TIMEOUT`].
pub struct WsTx {
    tx: mpsc::Sender<Outbound>,
}

impl ParticipantTx for WsTx {
    fn send_datagram(&self, bytes: Bytes) {
        // Full queue = congestion; drop rather than stall the fanout loop.
        let _ = self.tx.try_send(Outbound::Datagram(bytes));
    }

    fn send_control(&self, frame: Bytes) -> BoxFut<'static, ()> {
        let tx = self.tx.clone();
        Box::pin(async move {
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, tx.send(Outbound::Control(frame)))
                .await;
        })
    }

    fn finish_control(&self) -> BoxFut<'static, ()> {
        let tx = self.tx.clone();
        Box::pin(async move {
            let _ = tokio::time::timeout(CONTROL_WRITE_TIMEOUT, tx.send(Outbound::Finish)).await;
        })
    }
}
