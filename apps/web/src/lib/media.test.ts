import { describe, expect, it } from "vitest";
import {
  HEADER_LEN,
  MediaDecodeError,
  MAX_PAYLOAD,
  PT_OPUS_ENC,
  VERSION,
  buildDatagram,
  decodeHeader,
  defaultHeader,
  encodeHeader,
  parseDatagram,
} from "./media";

/**
 * Wire vectors pinned in both Rust (`crates/rvp-proto/src/lib.rs` tests) and
 * TypeScript. If either side changes encoding, these hex strings diverge.
 */

const VECTOR_HEADER = {
  version: VERSION,
  kind: 0,
  flags: 0b0000_0001,
  pt: PT_OPUS_ENC,
  kid: 7,
  sequence: 0xdeadbeef,
  timestamp: 0x01020304,
  frameDurationMs: 20,
  reserved: [9, 8, 7] as [number, number, number],
};

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

describe("media header codec", () => {
  it("encodes the pinned header wire vector", () => {
    expect(toHex(encodeHeader(VECTOR_HEADER))).toBe(
      "10010107deadbeef0102030414090807",
    );
  });

  it("encodes the default header wire vector", () => {
    expect(toHex(encodeHeader(defaultHeader()))).toBe(
      "10000000000000000000000014000000",
    );
  });

  it("roundtrips the vector header", () => {
    const decoded = decodeHeader(encodeHeader(VECTOR_HEADER));
    expect(decoded).toEqual(VECTOR_HEADER);
  });

  it("roundtrips the default header", () => {
    const decoded = decodeHeader(encodeHeader(defaultHeader()));
    expect(decoded).toEqual(defaultHeader());
  });

  it("rejects short buffers", () => {
    expect(() => decodeHeader(new Uint8Array(8))).toThrow(MediaDecodeError);
    expect(() => decodeHeader(new Uint8Array(8))).toThrow(
      /need 16 bytes, got 8/,
    );
  });

  it("rejects bad versions", () => {
    const bytes = encodeHeader(defaultHeader());
    bytes[0] = 0x90; // version 9
    expect(() => decodeHeader(bytes)).toThrow(/unsupported version 9/);
  });

  it("wraps sequence at u32 max", () => {
    const h = { ...defaultHeader(), sequence: 0xffffffff };
    expect(decodeHeader(encodeHeader(h)).sequence).toBe(0xffffffff);
  });

  it("wraps timestamp at u32 max", () => {
    const h = { ...defaultHeader(), timestamp: 0xffffffff };
    expect(decodeHeader(encodeHeader(h)).timestamp).toBe(0xffffffff);
  });

  it("uses big-endian sequence and timestamp", () => {
    const h = { ...defaultHeader(), sequence: 0x01020304, timestamp: 0x0a0b0c0d };
    const bytes = encodeHeader(h);
    expect(Array.from(bytes.subarray(4, 8))).toEqual([1, 2, 3, 4]);
    expect(Array.from(bytes.subarray(8, 12))).toEqual([10, 11, 12, 13]);
  });
});

describe("media datagram", () => {
  it("builds header ‖ payload and parses back", () => {
    const payload = fromHex("aabbcc");
    const raw = buildDatagram(defaultHeader(), payload);
    expect(raw.length).toBe(HEADER_LEN + payload.length);
    const parsed = parseDatagram(raw);
    expect(parsed.header).toEqual(defaultHeader());
    expect(parsed.payload).toEqual(payload);
  });

  it("parses zero-copy payload (subarray view)", () => {
    const payload = new Uint8Array([1, 2, 3, 4]);
    const raw = buildDatagram(defaultHeader(), payload);
    const parsed = parseDatagram(raw);
    // Same underlying buffer — no copy of the payload bytes.
    expect(parsed.payload.buffer).toBe(raw.buffer);
  });

  it("rejects short datagrams", () => {
    expect(() => parseDatagram(new Uint8Array(15))).toThrow(MediaDecodeError);
  });

  it("rejects bad-version datagrams", () => {
    const raw = buildDatagram(defaultHeader(), new Uint8Array(0));
    raw[0] = 0x00; // version 0
    expect(() => parseDatagram(raw)).toThrow(/unsupported version/);
  });

  it("enforces MAX_PAYLOAD size budget", () => {
    expect(MAX_PAYLOAD).toBe(1184);
    const payload = new Uint8Array(MAX_PAYLOAD);
    const raw = buildDatagram(defaultHeader(), payload);
    expect(raw.length).toBe(1200);
  });

  it("roundtrips an encrypted-shaped datagram (kid + PT=1)", () => {
    const header = { ...defaultHeader(), pt: PT_OPUS_ENC, kid: 42 };
    const payload = new Uint8Array(48).fill(0xab);
    const parsed = parseDatagram(buildDatagram(header, payload));
    expect(parsed.header.pt).toBe(PT_OPUS_ENC);
    expect(parsed.header.kid).toBe(42);
    expect(parsed.payload).toEqual(payload);
  });
});
