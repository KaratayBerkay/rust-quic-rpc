import { describe, expect, it } from "vitest";
import {
  ControlDecodeError,
  ControlFrameReader,
  decodeBody,
  encode,
  PROTOCOL_VERSION,
  type ControlMessage,
} from "./control";

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function fromHex(value: string): Uint8Array {
  const out = new Uint8Array(value.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(value.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * Wire vectors generated from `rvp-proto::control` encode output and pinned
 * in the Rust unit tests (`crates/rvp-proto/src/control.rs`). A mismatch here
 * means the TS codec drifted from the server.
 */
const VECTORS: Array<[string, ControlMessage, string]> = [
  [
    "hello",
    { type: "hello", protocolVersion: 1, grantJwt: "j" },
    "00000005000100016a",
  ],
  ["joinRoom", { type: "joinRoom" }, "000000020002"],
  [
    "welcome_empty",
    { type: "welcome", participantId: 1, maxDatagram: 1200, roster: [] },
    "0000000e0003000000000000000104b00000",
  ],
  [
    "welcome_roster",
    {
      type: "welcome",
      participantId: 1,
      maxDatagram: 1200,
      roster: [
        { participantId: 1, userId: "alice" },
        { participantId: 2, userId: "bob" },
      ],
    },
    "0000002a0003000000000000000104b0000200000000000000010005616c69636500000000000000020003626f62",
  ],
  [
    "participantJoined",
    { type: "participantJoined", participantId: 2, userId: "bob" },
    "0000000f000400000000000000020003626f62",
  ],
  [
    "participantLeft",
    { type: "participantLeft", participantId: 1 },
    "0000000a00050000000000000001",
  ],
  [
    "error",
    { type: "error", code: 401, message: "x" },
    "00000005000f019178",
  ],
];

describe("control codec — Rust-anchored wire vectors", () => {
  for (const [name, message, expected] of VECTORS) {
    it(`encodes ${name}`, () => {
      expect(hex(encode(message))).toBe(expected);
    });

    it(`decodes ${name}`, () => {
      expect(decodeBody(fromHex(expected).subarray(4))).toEqual(message);
    });

    it(`roundtrips ${name}`, () => {
      const frame = encode(message);
      const view = new DataView(frame.buffer, frame.byteOffset);
      expect(view.getUint32(0, false)).toBe(frame.length - 4);
      expect(decodeBody(frame.subarray(4))).toEqual(message);
    });
  }
});

describe("ControlFrameReader", () => {
  it("reassembles a frame split across chunks", () => {
    const frame = encode({
      type: "participantLeft",
      participantId: 1,
    });
    const reader = new ControlFrameReader();
    reader.push(frame.subarray(0, 3));
    expect(reader.drain()).toEqual([]);
    reader.push(frame.subarray(3));
    expect(reader.drain()).toEqual([
      { type: "participantLeft", participantId: 1 },
    ]);
  });

  it("drains multiple frames from one chunk", () => {
    const joined = encode({
      type: "participantJoined",
      participantId: 2,
      userId: "bob",
    });
    const left = encode({ type: "participantLeft", participantId: 2 });
    const reader = new ControlFrameReader();
    const merged = new Uint8Array(joined.length + left.length);
    merged.set(joined, 0);
    merged.set(left, joined.length);
    reader.push(merged);
    expect(reader.drain()).toEqual([
      { type: "participantJoined", participantId: 2, userId: "bob" },
      { type: "participantLeft", participantId: 2 },
    ]);
  });

  it("rejects oversized frames", () => {
    const reader = new ControlFrameReader();
    reader.push(fromHex("ffffffff"));
    expect(() => reader.drain()).toThrow(ControlDecodeError);
  });
});

describe("PROTOCOL_VERSION", () => {
  it("matches the Rust constant", () => {
    expect(PROTOCOL_VERSION).toBe(1);
  });
});
