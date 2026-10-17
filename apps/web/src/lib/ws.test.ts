import { describe, expect, it } from "vitest";
import { HEADER_LEN } from "./media";
import {
  frameControl,
  frameDatagram,
  parseWsMessage,
  TAG_CONTROL,
  TAG_DATAGRAM,
  WsError,
} from "./ws";

/**
 * Anchored vectors mirrored from `crates/rvp-proto/src/ws.rs` — the Rust
 * unit tests pin the same bytes (`anchored_control_vector`). A mismatch
 * means the TS framing drifted from the server.
 */
describe("ws framing (mirrors rvp_proto::ws)", () => {
  it("uses the same tag bytes", () => {
    expect(TAG_CONTROL).toBe(0);
    expect(TAG_DATAGRAM).toBe(1);
  });

  it("matches the anchored control vector", () => {
    // Rust: frame_control(&[0, 0, 0, 2, 0, 2]) == [0, 0, 0, 0, 2, 0, 2]
    const framed = frameControl(new Uint8Array([0, 0, 0, 2, 0, 2]));
    expect(Array.from(framed)).toEqual([0, 0, 0, 0, 2, 0, 2]);
  });

  it("round-trips a control chunk", () => {
    const chunk = new Uint8Array([0, 0, 0, 5, 0, 1, 0, 1, 106]); // hello, ver=1, "j"
    const parsed = parseWsMessage(frameControl(chunk));
    expect(parsed.kind).toBe("control");
    expect(Array.from(parsed.kind === "control" ? parsed.chunk : [])).toEqual(
      Array.from(chunk),
    );
  });

  it("round-trips a datagram", () => {
    const dgram = new Uint8Array(HEADER_LEN + 4).fill(0x11);
    const framed = frameDatagram(dgram);
    expect(framed[0]).toBe(TAG_DATAGRAM);
    const parsed = parseWsMessage(framed);
    expect(parsed.kind).toBe("datagram");
    expect(
      Array.from(parsed.kind === "datagram" ? parsed.datagram : []),
    ).toEqual(Array.from(dgram));
  });

  it("rejects a datagram shorter than the header", () => {
    expect(() => parseWsMessage(frameDatagram(new Uint8Array(HEADER_LEN - 1)))).toThrow(
      WsError,
    );
  });

  it("rejects empty messages and unknown tags", () => {
    expect(() => parseWsMessage(new Uint8Array(0))).toThrow(WsError);
    expect(() => parseWsMessage(new Uint8Array([0xff, 1]))).toThrow(/unknown ws message tag/);
  });
});
