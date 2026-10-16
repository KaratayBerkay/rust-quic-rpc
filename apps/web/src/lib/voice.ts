/**
 * Voice pipeline glue — encode → seal(N−1) → send, and recv → open → jitter
 * → decode/PLC → playback. Mirrors the Flutter `voice_pipeline.dart` send and
 * receive paths so web and Flutter peers interoperate on the same SFU.
 *
 * Codec (WebCodecs Opus) and capture/playback (AudioWorklet) are injected so
 * the pure fanout/decrypt/jitter logic is unit-testable in Node.
 */

import { PairwiseKey, keyFromHex } from "./crypto";
import {
  FRAME_SAMPLES,
  JitterBuffer,
  type TickResult,
} from "./jitter";
import {
  PT_OPUS,
  PT_OPUS_ENC,
  buildDatagram,
  defaultHeader,
  parseDatagram,
} from "./media";

/** Sample rate and frame geometry locked by the RVP media clock (plan §3.2). */
export const SAMPLE_RATE = 48_000;
export const CHANNELS = 1;
export const FRAME_MS = 20;
export { FRAME_SAMPLES } from "./jitter";
export const TIMESTAMP_TICKS_PER_FRAME = FRAME_SAMPLES; // 48 kHz clock

export interface PipelinePeer {
  /** Nest user id of the remote participant. */
  userId: string;
  kid: number;
  key: PairwiseKey;
}

/** Opus encode: one 960-sample Int16 frame → one Opus packet. */
export interface OpusEncoder {
  encodeFrame(pcm: Int16Array): Uint8Array;
  close(): void;
}

/** Opus decode: one Opus packet → 960-sample Int16 frame. */
export interface OpusDecoder {
  decodePacket(packet: Uint8Array): Int16Array;
  /** Packet-loss concealment for one frame (libopus PLC / silence fallback). */
  conceal(): Int16Array;
  close(): void;
}

export interface VoiceStats {
  framesSent: number;
  datagramsSent: number;
  datagramsReceived: number;
  malformedDropped: number;
  missingKeyDropped: number;
  decryptFailures: number;
}

export interface VoicePipelineOptions {
  /** Fire-and-forget media datagram to the SFU. */
  sendDatagram: (payload: Uint8Array) => void;
  /** Opus encoder factory (WebCodecs in the browser). */
  createEncoder: () => OpusEncoder;
  /** Opus decoder factory (WebCodecs in the browser). */
  createDecoder: () => OpusDecoder;
  /** Plaintext interop mode (PT=0, kid=0) — tests / loopback only. */
  plaintext?: boolean;
}

export class VoicePipeline {
  private readonly peers = new Map<string, PipelinePeer>();
  private readonly byKid = new Map<number, PipelinePeer>();
  private readonly jitter = new JitterBuffer();
  private readonly encoder: OpusEncoder;
  private readonly decoder: OpusDecoder;
  private readonly sendDatagram: (payload: Uint8Array) => void;
  private readonly plaintext: boolean;

  private seq = 0;
  private timestamp = 0;
  private muted = false;
  private sendChain: Promise<void> = Promise.resolve();
  private recvChain: Promise<void> = Promise.resolve();
  private ticker: ReturnType<typeof setInterval> | null = null;
  private running = false;

  readonly stats: VoiceStats = {
    framesSent: 0,
    datagramsSent: 0,
    datagramsReceived: 0,
    malformedDropped: 0,
    missingKeyDropped: 0,
    decryptFailures: 0,
  };

  /** Called with every 960-sample Int16 playout frame (or silence/PLC). */
  onPlaybackPcm: ((frame: Int16Array) => void) | null = null;

  constructor(options: VoicePipelineOptions) {
    this.sendDatagram = options.sendDatagram;
    this.encoder = options.createEncoder();
    this.decoder = options.createDecoder();
    this.plaintext = options.plaintext ?? false;
  }

  get isMuted(): boolean {
    return this.muted;
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
  }

  addPeer(peer: PipelinePeer): void {
    this.peers.set(peer.userId, peer);
    // Key rotation (plan §5): a peer swapped to a new kid keeps its old
    // byKid entry, so in-flight datagrams sealed with the old key still
    // decrypt during the switch (bounded: kids are 1..255 per room).
    this.byKid.set(peer.kid, peer);
  }

  /**
   * Install/replace pairwise keys from a Nest `pairs` payload (grant
   * response, `GET /rooms/:id/keys`, or `POST /rooms/:id/keys/rotate`).
   * Used on connect and again on key rotation (plan §5).
   */
  applyPairs(pairs: ReadonlyArray<{ peer: string; kid: number; key: string }>): void {
    for (const pair of pairs) {
      this.addPeer({
        userId: pair.peer,
        kid: pair.kid,
        key: new PairwiseKey(keyFromHex(pair.key)),
      });
    }
  }

  removePeer(userId: string): void {
    const peer = this.peers.get(userId);
    if (peer) {
      this.peers.delete(userId);
      this.byKid.delete(peer.kid);
    }
  }

  peerCount(): number {
    return this.peers.size;
  }

  /**
   * Encode one 960-sample Int16 frame and fan it out (N−1 sealed datagrams,
   * or one plaintext datagram in interop mode). Sequence order is preserved
   * across the async seal chain.
   */
  pushPcm(frame: Int16Array): Promise<void> {
    if (frame.length !== FRAME_SAMPLES) {
      return Promise.reject(
        new Error(`expected ${FRAME_SAMPLES} samples, got ${frame.length}`),
      );
    }
    const next = this.sendChain.then(() => this.encodeAndSend(frame));
    this.sendChain = next.then(
      () => {},
      () => {},
    );
    return next;
  }

  private async encodeAndSend(frame: Int16Array): Promise<void> {
    if (this.muted) return;
    const packet = this.encoder.encodeFrame(frame);
    const seq = this.seq;
    this.seq = (this.seq + 1) >>> 0;
    const ts = this.timestamp;
    this.timestamp = (this.timestamp + frame.length) >>> 0;

    if (this.plaintext) {
      const header = {
        ...defaultHeader(),
        pt: PT_OPUS,
        kid: 0,
        sequence: seq,
        timestamp: ts,
      };
      this.sendDatagram(buildDatagram(header, packet));
      this.stats.datagramsSent += 1;
      this.stats.framesSent += 1;
      return;
    }

    for (const peer of this.peers.values()) {
      const sealed = await peer.key.seal(seq, packet);
      const header = {
        ...defaultHeader(),
        pt: PT_OPUS_ENC,
        kid: peer.kid,
        sequence: seq,
        timestamp: ts,
      };
      this.sendDatagram(buildDatagram(header, sealed));
      this.stats.datagramsSent += 1;
    }
    this.stats.framesSent += 1;
  }

  /** Feed one received SFU datagram (from WT `onData`). */
  receiveDatagram(bytes: Uint8Array): void {
    this.stats.datagramsReceived += 1;
    this.recvChain = this.recvChain
      .then(() => this.processDatagram(bytes))
      .catch(() => {});
  }

  /** Await all queued receive processing (decrypt + jitter push). */
  flush(): Promise<void> {
    return this.recvChain;
  }

  private async processDatagram(bytes: Uint8Array): Promise<void> {
    let parsed;
    try {
      parsed = parseDatagram(bytes);
    } catch {
      this.stats.malformedDropped += 1;
      return;
    }
    const { header, payload } = parsed;
    let packet: Uint8Array;
    if (header.pt === PT_OPUS_ENC) {
      const peer = this.byKid.get(header.kid);
      if (!peer) {
        this.stats.missingKeyDropped += 1;
        return;
      }
      try {
        packet = await peer.key.open(header.sequence, payload);
      } catch {
        this.stats.decryptFailures += 1;
        return;
      }
    } else {
      packet = payload;
    }
    this.jitter.push(header.sequence, packet);
  }

  /** Subscribe to the playout ticker (20 ms). Returns an unsubscribe fn. */
  start(): () => void {
    if (this.running) return () => {};
    this.running = true;
    this.ticker = setInterval(() => this.tick(), FRAME_MS);
    return () => this.stop();
  }

  stop(): void {
    this.running = false;
    if (this.ticker !== null) {
      clearInterval(this.ticker);
      this.ticker = null;
    }
  }

  /** One playout slot: decode the due frame, conceal a loss, or emit silence. */
  tick(): TickResult {
    const result = this.jitter.tick();
    let frame: Int16Array;
    switch (result.kind) {
      case "packet":
        frame = this.decoder.decodePacket(result.payload);
        break;
      case "loss":
        frame = this.decoder.conceal();
        break;
      case "prebuffer":
      case "idle":
        frame = new Int16Array(FRAME_SAMPLES);
        break;
    }
    this.onPlaybackPcm?.(frame);
    return result;
  }

  dispose(): void {
    this.stop();
    void this.recvChain;
    this.encoder.close();
    this.decoder.close();
  }
}

/** Int16 (s16le) ↔ Float32 helpers for AudioWorklet bridging. */
export function int16ToFloat32(pcm: Int16Array): Float32Array {
  const out = new Float32Array(pcm.length);
  for (let i = 0; i < pcm.length; i += 1) {
    out[i] = pcm[i] / 32768;
  }
  return out;
}

export function float32ToInt16(pcm: Float32Array): Int16Array {
  const out = new Int16Array(pcm.length);
  for (let i = 0; i < pcm.length; i += 1) {
    const s = Math.max(-1, Math.min(1, pcm[i]));
    out[i] = s < 0 ? Math.round(s * 32768) : Math.round(s * 32767);
  }
  return out;
}
