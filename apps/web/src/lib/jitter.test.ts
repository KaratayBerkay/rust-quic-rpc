import { describe, expect, it } from "vitest";
import {
  CAPACITY_FRAMES,
  FRAME_MS,
  FRAME_SAMPLES,
  JitterBuffer,
  MAX_LOSS_FRAMES,
  MAX_DEPTH_MS,
  TARGET_DEPTH_MS,
  seqDelta,
} from "./jitter";

const frame = (marker: number): Uint8Array => new Uint8Array([marker]);

describe("constants", () => {
  it("matches the Flutter/Dart parity set", () => {
    expect(FRAME_SAMPLES).toBe(960);
    expect(FRAME_MS).toBe(20);
    expect(TARGET_DEPTH_MS).toBe(40);
    expect(MAX_DEPTH_MS).toBe(80);
    expect(MAX_LOSS_FRAMES).toBe(5);
    expect(CAPACITY_FRAMES).toBe(4);
  });
});

describe("seqDelta (wrap-aware)", () => {
  it("handles forward and backward deltas", () => {
    expect(seqDelta(5, 3)).toBe(2);
    expect(seqDelta(3, 5)).toBe(-2);
  });

  it("handles u32 wrap (0xffffffff → 0)", () => {
    expect(seqDelta(0, 0xffffffff)).toBe(1);
    expect(seqDelta(0xffffffff, 0)).toBe(-1);
    expect(seqDelta(1, 0xffffffff)).toBe(2);
  });
});

function prebuffer(buf: JitterBuffer, start: number, count: number): void {
  for (let i = 0; i < count; i += 1) {
    buf.push(start + i, frame(i));
  }
}

describe("JitterBuffer", () => {
  it("idles with no packets", () => {
    const buf = new JitterBuffer();
    expect(buf.tick()).toEqual({ kind: "idle" });
  });

  it("prebuffers silence until target depth is reached", () => {
    const buf = new JitterBuffer();
    buf.push(0, frame(0));
    // bufferedMs = 20 < 40 target → still prebuffering
    expect(buf.tick()).toEqual({ kind: "prebuffer" });
    buf.push(1, frame(1));
    // bufferedMs = 40 → leave prebuffer on next tick
    expect(buf.tick()).toEqual({ kind: "packet", seq: 0, payload: frame(0) });
  });

  it("delivers consecutive packets and advances expected", () => {
    const buf = new JitterBuffer();
    prebuffer(buf, 0, 2);
    expect(buf.tick().kind).toBe("packet");
    expect(buf.tick().kind).toBe("packet");
  });

  it("emits PLC loss on a single gap", () => {
    const buf = new JitterBuffer();
    // Push 0, then skip 1, then 2 — a single-frame hole.
    buf.push(0, frame(0));
    buf.push(2, frame(2));
    buf.push(3, frame(3));
    buf.push(4, frame(4));
    expect(buf.tick()).toEqual({ kind: "packet", seq: 0, payload: frame(0) });
    expect(buf.tick()).toEqual({ kind: "loss", seq: 1 }); // gap → PLC
    expect(buf.tick()).toEqual({ kind: "packet", seq: 2, payload: frame(2) });
  });

  it("drops late packets behind last delivered", () => {
    const buf = new JitterBuffer();
    prebuffer(buf, 0, 2);
    buf.tick(); // deliver 0
    buf.tick(); // deliver 1
    buf.push(0, frame(99)); // re-push already-delivered seq
    expect(buf.lateDropped.count).toBe(1);
  });

  it("drops duplicate seq already buffered", () => {
    const buf = new JitterBuffer();
    buf.push(0, frame(0));
    buf.push(0, frame(1));
    expect(buf.duplicateDropped.count).toBe(1);
  });

  it("drops oldest on overflow (capacity 4)", () => {
    const buf = new JitterBuffer();
    prebuffer(buf, 0, 5); // 5th push overflows capacity 4
    expect(buf.overflowDropped.count).toBe(1);
    expect(buf.bufferedMs).toBe(MAX_DEPTH_MS);
  });

  it("resets to prebuffer after max consecutive losses", () => {
    const buf = new JitterBuffer();
    prebuffer(buf, 0, 2); // buffered seq 0, 1
    expect(buf.tick().kind).toBe("packet"); // deliver 0
    expect(buf.tick().kind).toBe("packet"); // deliver 1 — buffer now empty
    // expected = 2, nothing follows → consecutive losses.
    for (let i = 0; i < MAX_LOSS_FRAMES; i += 1) {
      buf.tick();
    }
    expect(buf.resets.count).toBe(1);
    // After reset the buffer is empty → idle until new frames arrive.
    expect(buf.tick().kind).toBe("idle");
    buf.push(10, frame(10));
    expect(buf.tick().kind).toBe("prebuffer");
  });

  it("wraps sequence at u32 max", () => {
    const buf = new JitterBuffer();
    // Prebuffer two frames straddling the wrap boundary.
    buf.push(0xfffffffe, frame(0));
    buf.push(0xffffffff, frame(1));
    buf.push(0, frame(2));
    buf.push(1, frame(3));
    expect(buf.tick()).toEqual({
      kind: "packet",
      seq: 0xfffffffe,
      payload: frame(0),
    });
    expect(buf.tick()).toEqual({
      kind: "packet",
      seq: 0xffffffff,
      payload: frame(1),
    });
    expect(buf.tick()).toEqual({ kind: "packet", seq: 0, payload: frame(2) });
  });

  it("reset() clears state", () => {
    const buf = new JitterBuffer();
    prebuffer(buf, 0, 2);
    buf.tick();
    buf.reset();
    expect(buf.tick()).toEqual({ kind: "idle" });
    expect(buf.stats()).toEqual({
      overflowDropped: 0,
      lateDropped: 0,
      duplicateDropped: 0,
      resets: 0,
    });
  });
});
