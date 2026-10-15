/**
 * RVP/1 media datagram codec — TypeScript port of `rvp-proto::lib`.
 *
 * Wire format (plan §3.2): fixed 16-byte plaintext header + opaque payload
 * (raw Opus when PT=0, ciphertext‖GCM-tag when PT=1). The header stays
 * plaintext so the SFU can route without media keys (ADR-004).
 *
 * ```text
 *  0                   1                   2                   3
 *  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
 * +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 * | Ver(4)  |Ty(4)|    Flags      |     PT        |     KID       |
 * +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 * |                          Sequence (u32 BE)                    |
 * +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 * |                    Timestamp (u32 BE, 48 kHz)                 |
 * +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 * | FrameDur(ms)  |            Reserved (3 bytes)                 |
 * +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 * |                     Opus / ciphertext payload ...             |
 * +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 * ```
 */

export const HEADER_LEN = 16;
export const VERSION = 1;
export const PT_OPUS = 0;
export const PT_OPUS_ENC = 1;
export const MAX_PAYLOAD = 1200 - HEADER_LEN;

export class MediaDecodeError extends Error {
  constructor(
    message: string,
    readonly detail?: unknown,
  ) {
    super(message);
    this.name = "MediaDecodeError";
  }
}

export interface MediaHeader {
  version: number;
  kind: number;
  flags: number;
  pt: number;
  kid: number;
  sequence: number;
  timestamp: number;
  frameDurationMs: number;
  reserved: [number, number, number];
}

export function defaultHeader(): MediaHeader {
  return {
    version: VERSION,
    kind: 0,
    flags: 0,
    pt: PT_OPUS,
    kid: 0,
    sequence: 0,
    timestamp: 0,
    frameDurationMs: 20,
    reserved: [0, 0, 0],
  };
}

/** Encode into a fixed 16-byte array (big-endian integers). */
export function encodeHeader(header: MediaHeader): Uint8Array {
  const buf = new Uint8Array(HEADER_LEN);
  const view = new DataView(buf.buffer);
  buf[0] = ((header.version & 0x0f) << 4) | (header.kind & 0x0f);
  buf[1] = header.flags;
  buf[2] = header.pt;
  buf[3] = header.kid;
  view.setUint32(4, header.sequence >>> 0, false);
  view.setUint32(8, header.timestamp >>> 0, false);
  buf[12] = header.frameDurationMs;
  buf[13] = header.reserved[0];
  buf[14] = header.reserved[1];
  buf[15] = header.reserved[2];
  return buf;
}

/** Decode from the first [`HEADER_LEN`] bytes of `buf`. */
export function decodeHeader(buf: Uint8Array): MediaHeader {
  if (buf.length < HEADER_LEN) {
    throw new MediaDecodeError(
      `buffer too short: need ${HEADER_LEN} bytes, got ${buf.length}`,
    );
  }
  const version = buf[0] >> 4;
  if (version !== VERSION) {
    throw new MediaDecodeError(`unsupported version ${version}`, version);
  }
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  return {
    version,
    kind: buf[0] & 0x0f,
    flags: buf[1],
    pt: buf[2],
    kid: buf[3],
    sequence: view.getUint32(4, false),
    timestamp: view.getUint32(8, false),
    frameDurationMs: buf[12],
    reserved: [buf[13], buf[14], buf[15]],
  };
}

/** Build an owned datagram buffer (header ‖ payload). */
export function buildDatagram(header: MediaHeader, payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(HEADER_LEN + payload.length);
  out.set(encodeHeader(header), 0);
  out.set(payload, HEADER_LEN);
  return out;
}

export interface ParsedDatagram {
  header: MediaHeader;
  payload: Uint8Array;
}

/** Parse a datagram buffer (payload is a zero-copy view). */
export function parseDatagram(buf: Uint8Array): ParsedDatagram {
  if (buf.length < HEADER_LEN) {
    throw new MediaDecodeError(
      `buffer too short: need ${HEADER_LEN} bytes, got ${buf.length}`,
    );
  }
  const header = decodeHeader(buf);
  return { header, payload: buf.subarray(HEADER_LEN) };
}
