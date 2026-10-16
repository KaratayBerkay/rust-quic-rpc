/**
 * WebTransport session client (plan §7, P6a).
 *
 * Browser WebTransport requires `serverCertificateHashes` for self-signed
 * server certs (ECDSA P-256, ≤14-day validity). The hash is the SHA-256 of
 * the leaf certificate DER — rvp-cli logs it at boot as colon-hex.
 */

import {
  ControlFrameReader,
  encode,
  type ControlMessage,
} from "./control";

export interface WtConnectOptions {
  /** e.g. `https://127.0.0.1:4434/` — HTTPS origin of the WT endpoint. */
  url: string;
  /** Colon-separated hex SHA-256 of the server's leaf certificate. */
  certHashHex: string;
  /** Media-grant JWT (iss=nest, aud=rvp). */
  grantJwt: string;
}

export interface WtSession {
  participantId: number;
  maxDatagram: number;
  roster: Array<{ participantId: number; userId: string }>;
  /** Fire-and-forget media datagram (live audio; drops on congestion). */
  sendDatagram(payload: Uint8Array): void;
  /**
   * Send one control frame after the handshake (e.g. `keyUpdate` on key
   * rotation, plan §5). Hello/JoinRoom are already consumed by connect.
   */
  sendControl(msg: ControlMessage): void;
  /** Subscribe to server-push control events after Welcome. */
  onControl(listener: (msg: ControlMessage) => void): () => void;
  /** Subscribe to incoming media datagrams. */
  onData(listener: (payload: Uint8Array) => void): () => void;
  close(): void;
}

export class WtError extends Error {
  constructor(message: string, readonly detail?: unknown) {
    super(message);
    this.name = "WtError";
  }
}

function hexToBytes(colonHex: string): Uint8Array {
  const clean = colonHex.replace(/:/g, "");
  if (!/^[0-9a-fA-F]{64}$/.test(clean)) {
    throw new WtError(`cert hash must be 32 hex bytes, got: ${colonHex}`);
  }
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * Connect, complete the RVP control handshake (Hello + JoinRoom → Welcome),
 * and return a live session. Rejects on transport or handshake error.
 */
export async function connectWt(options: WtConnectOptions): Promise<WtSession> {
  if (typeof WebTransport === "undefined") {
    throw new WtError(
      "WebTransport is not available in this browser (Chrome/Edge 97+ required)",
    );
  }

  const hashBytes = hexToBytes(options.certHashHex);
  const hashBuffer = hashBytes.buffer.slice(
    hashBytes.byteOffset,
    hashBytes.byteOffset + hashBytes.byteLength,
  );
  const transport = new WebTransport(options.url, {
    serverCertificateHashes: [
      {
        algorithm: "sha-256",
        value: hashBuffer as ArrayBuffer,
      },
    ],
  });
  await transport.ready;

  const control = await transport.createBidirectionalStream();
  const writer = control.writable.getWriter();
  const reader = control.readable.getReader();

  // Hello + JoinRoom (plan §3.4 steps 2–4).
  await writer.write(
    encode({
      type: "hello",
      protocolVersion: 1,
      grantJwt: options.grantJwt,
    }),
  );
  await writer.write(encode({ type: "joinRoom" }));

  const frameReader = new ControlFrameReader();
  let welcome: Extract<ControlMessage, { type: "welcome" }> | undefined;
  let handshakeError: WtError | undefined;

  const controlListeners = new Set<(msg: ControlMessage) => void>();
  const dataListeners = new Set<(payload: Uint8Array) => void>();

  const pumpControl = async () => {
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value) frameReader.push(value);
        for (const msg of frameReader.drain()) {
          if (!welcome) {
            if (msg.type === "welcome") {
              welcome = msg;
            } else if (msg.type === "error") {
              handshakeError = new WtError(
                `handshake rejected (${msg.code}): ${msg.message}`,
                msg,
              );
              return;
            } else {
              handshakeError = new WtError(
                `expected Welcome, got ${msg.type}`,
                msg,
              );
              return;
            }
          } else {
            for (const listener of controlListeners) {
              listener(msg);
            }
          }
        }
      }
    } catch {
      // Stream closed; session end is handled by datagram/connection loops.
    }
  };

  const pumpDatagrams = async () => {
    const recv = transport.datagrams.readable.getReader();
    try {
      for (;;) {
        const { done, value } = await recv.read();
        if (done) break;
        if (value) {
          for (const listener of dataListeners) {
            listener(value);
          }
        }
      }
    } catch {
      // Connection closed.
    }
  };

  void pumpControl();
  void pumpDatagrams();

  // Wait for Welcome (or a handshake error) with a timeout.
  const deadline = Date.now() + 5_000;
  while (!welcome && !handshakeError) {
    if (Date.now() > deadline) {
      transport.close();
      throw new WtError("handshake timeout: no Welcome within 5s");
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  if (handshakeError) {
    transport.close();
    throw handshakeError;
  }
  const w = welcome!;

  return {
    participantId: w.participantId,
    maxDatagram: w.maxDatagram,
    roster: w.roster,
    sendDatagram(payload: Uint8Array) {
      const writer = transport.datagrams.writable.getWriter();
      writer.write(payload).catch(() => {
        // Congestion or closed session — live audio drops by design (plan §4).
      });
      writer.releaseLock();
    },
    sendControl(msg: ControlMessage) {
      writer.write(encode(msg)).catch(() => {
        // Control stream closed — session teardown handles the rest.
      });
    },
    onControl(listener) {
      controlListeners.add(listener);
      return () => controlListeners.delete(listener);
    },
    onData(listener) {
      dataListeners.add(listener);
      return () => dataListeners.delete(listener);
    },
    close() {
      transport.close();
    },
  };
}
