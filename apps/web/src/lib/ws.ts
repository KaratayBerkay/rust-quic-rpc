/**
 * WebSocket transport client (plan §7 fallback, open decision 8).
 *
 * Degraded path for networks that block UDP (WebTransport needs QUIC): same
 * RVP control handshake and media fanout over plain `ws://`, framed with the
 * 1-byte tags from `rvp-proto::ws` (mirrored here with the same anchored
 * vectors the Rust unit tests pin).
 *
 * Production note: browsers refuse `ws://` from an https page (mixed content)
 * and WebSockets have no certificate-hash pinning API — terminate TLS at a
 * reverse proxy and use `wss://`.
 */

import {
  ControlFrameReader,
  encode,
  type ControlMessage,
} from "./control";
import { HEADER_LEN } from "./media";
import type { WtSession } from "./wt";

/** Tag byte for a chunk of the control byte-stream. */
export const TAG_CONTROL = 0;
/** Tag byte for one complete media datagram (16-byte header + payload). */
export const TAG_DATAGRAM = 1;

/** Mirrors `rvp_proto::ws::frame_control`. */
export function frameControl(chunk: Uint8Array): Uint8Array {
  const out = new Uint8Array(1 + chunk.length);
  out[0] = TAG_CONTROL;
  out.set(chunk, 1);
  return out;
}

/** Mirrors `rvp_proto::ws::frame_datagram`. */
export function frameDatagram(datagram: Uint8Array): Uint8Array {
  const out = new Uint8Array(1 + datagram.length);
  out[0] = TAG_DATAGRAM;
  out.set(datagram, 1);
  return out;
}

/** Mirrors `rvp_proto::ws::parse_ws_message` (Ok = payload after the tag). */
export function parseWsMessage(
  msg: Uint8Array,
): { kind: "control"; chunk: Uint8Array } | { kind: "datagram"; datagram: Uint8Array } {
  if (msg.length < 1) {
    throw new WsError("empty ws message (missing tag byte)");
  }
  const tag = msg[0];
  const rest = msg.subarray(1);
  if (tag === TAG_CONTROL) {
    return { kind: "control", chunk: rest };
  }
  if (tag === TAG_DATAGRAM) {
    if (rest.length < HEADER_LEN) {
      throw new WsError(`datagram shorter than header (${rest.length} bytes)`);
    }
    return { kind: "datagram", datagram: rest };
  }
  throw new WsError(`unknown ws message tag: ${tag}`);
}

export interface WsConnectOptions {
  /** e.g. `ws://127.0.0.1:4435/` (or `wss://…` behind a TLS proxy). */
  url: string;
  /** Media-grant JWT (iss=nest, aud=rvp). */
  grantJwt: string;
}

export class WsError extends Error {
  constructor(message: string, readonly detail?: unknown) {
    super(message);
    this.name = "WsError";
  }
}

/** Above this queued byte count we drop outgoing media (live audio; the
 * server does the same on its outbound queue — plan §4). */
const MAX_BUFFERED = 32 * 1024;

/**
 * Connect over WebSocket, complete the RVP control handshake (Hello +
 * JoinRoom → Welcome) and return a session with the same shape as
 * `connectWt`, so callers swap transports without caring which one won.
 */
export async function connectWs(options: WsConnectOptions): Promise<WtSession> {
  const ws = new WebSocket(options.url);
  ws.binaryType = "arraybuffer";

  await new Promise<void>((resolve, reject) => {
    const onOpen = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new WsError(`websocket connection failed: ${options.url}`));
    };
    const cleanup = () => {
      ws.removeEventListener("open", onOpen);
      ws.removeEventListener("error", onError);
    };
    ws.addEventListener("open", onOpen);
    ws.addEventListener("error", onError);
  });

  const send = (bytes: Uint8Array) => {
    try {
      ws.send(bytes);
    } catch {
      // Session teardown handles the rest (mirrors wt.ts sendControl).
    }
  };

  // Hello + JoinRoom (plan §3.4 steps 2–4) as tagged control messages.
  send(
    frameControl(
      encode({
        type: "hello",
        protocolVersion: 1,
        grantJwt: options.grantJwt,
      }),
    ),
  );
  send(frameControl(encode({ type: "joinRoom" })));

  const frameReader = new ControlFrameReader();
  let welcome: Extract<ControlMessage, { type: "welcome" }> | undefined;
  let handshakeError: WsError | undefined;
  let closed = false;

  const controlListeners = new Set<(msg: ControlMessage) => void>();
  const dataListeners = new Set<(payload: Uint8Array) => void>();
  ws.addEventListener("message", (event) => {
    if (!(event.data instanceof ArrayBuffer)) return;
    let parsed;
    try {
      parsed = parseWsMessage(new Uint8Array(event.data));
    } catch (e) {
      // Framing violation: stop trusting the stream (server closes anyway).
      handshakeError ??= e instanceof WsError ? e : new WsError(String(e));
      return;
    }
    if (parsed.kind === "datagram") {
      for (const listener of dataListeners) {
        listener(parsed.datagram);
      }
      return;
    }
    frameReader.push(parsed.chunk);
    for (const msg of frameReader.drain()) {
      if (!welcome) {
        if (msg.type === "welcome") {
          welcome = msg;
        } else if (msg.type === "error") {
          handshakeError = new WsError(
            `handshake rejected (${msg.code}): ${msg.message}`,
            msg,
          );
        } else {
          handshakeError = new WsError(`expected Welcome, got ${msg.type}`, msg);
        }
      } else {
        for (const listener of controlListeners) {
          listener(msg);
        }
      }
    }
  });

  ws.addEventListener("close", () => {
    closed = true;
    if (!welcome && !handshakeError) {
      handshakeError = new WsError("socket closed during handshake");
    }
  });
  ws.addEventListener("error", () => {
    if (!welcome && !handshakeError) {
      handshakeError = new WsError("socket error during handshake");
    }
  });

  // Wait for Welcome (or a handshake error) with the same 5s deadline as wt.ts.
  const deadline = Date.now() + 5_000;
  while (!welcome && !handshakeError && !closed) {
    if (Date.now() > deadline) {
      ws.close();
      throw new WsError("handshake timeout: no Welcome within 5s");
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  if (handshakeError) {
    ws.close();
    throw handshakeError;
  }
  const w = welcome!;

  return {
    participantId: w.participantId,
    maxDatagram: w.maxDatagram,
    roster: w.roster,
    sendDatagram(payload: Uint8Array) {
      if (ws.readyState !== WebSocket.OPEN) return;
      // Congestion drop: a live-audio sender must never queue unbounded.
      if (ws.bufferedAmount > MAX_BUFFERED) return;
      ws.send(frameDatagram(payload));
    },
    sendControl(msg: ControlMessage) {
      if (ws.readyState !== WebSocket.OPEN) return;
      send(frameControl(encode(msg)));
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
      ws.close();
    },
  };
}
