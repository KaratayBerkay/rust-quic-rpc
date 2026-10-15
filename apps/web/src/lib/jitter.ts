/**
 * Jitter buffer — TypeScript port of the Flutter plugin's `jitter_buffer.dart`.
 *
 * Behaviour locked to the Dart implementation for cross-client parity:
 * - targetDepthMs = 40, maxDepthMs = 80, maxLossFrames = 5, frameMs = 20
 * - capacity = maxDepthMs / frameMs frames (4); overflow drops the OLDEST
 *   buffered frame (fast-forward to live edge)
 * - late/dup drop is wrap-aware (signed int32 delta of u32 sequence)
 * - prebuffer: hold playout (caller emits silence) until bufferedMs >= target
 * - playout: deliver expected seq; missing slot → Loss (PLC) and advance
 * - ≥5 consecutive losses → full reset back to prebuffer
 */

export const FRAME_SAMPLES = 960; // 20 ms @ 48 kHz
export const FRAME_MS = 20;
export const TARGET_DEPTH_MS = 40;
export const MAX_DEPTH_MS = 80;
export const MAX_LOSS_FRAMES = 5;
export const CAPACITY_FRAMES = Math.max(1, Math.floor(MAX_DEPTH_MS / FRAME_MS));

/** Wrap-aware signed delta between two u32 sequence numbers. */
export function seqDelta(a: number, b: number): number {
  return (a - b) | 0;
}

export type TickResult =
  | { kind: "packet"; seq: number; payload: Uint8Array }
  | { kind: "loss"; seq: number }
  | { kind: "prebuffer" }
  | { kind: "idle" };

interface Entry {
  seq: number;
  payload: Uint8Array;
}

export interface JitterStats {
  overflowDropped: number;
  lateDropped: number;
  duplicateDropped: number;
  resets: number;
}

export class JitterBuffer {
  private buffer = new Map<number, Entry>();
  private expected: number | null = null;
  private lastDelivered = -1; // signed "none" sentinel; u32 wrap-safe via delta
  private prebuffering = true;
  private consecutiveLosses = 0;

  readonly overflowDropped = { count: 0 };
  readonly lateDropped = { count: 0 };
  readonly duplicateDropped = { count: 0 };
  readonly resets = { count: 0 };

  constructor(
    private readonly targetDepthMs = TARGET_DEPTH_MS,
    private readonly maxDepthMs = MAX_DEPTH_MS,
    private readonly maxLossFrames = MAX_LOSS_FRAMES,
    private readonly frameMs = FRAME_MS,
  ) {}

  get capacityFrames(): number {
    return Math.max(1, Math.floor(this.maxDepthMs / this.frameMs));
  }

  get bufferedMs(): number {
    return this.buffer.size * this.frameMs;
  }

  stats(): JitterStats {
    return {
      overflowDropped: this.overflowDropped.count,
      lateDropped: this.lateDropped.count,
      duplicateDropped: this.duplicateDropped.count,
      resets: this.resets.count,
    };
  }

  reset(): void {
    this.buffer.clear();
    this.expected = null;
    this.lastDelivered = -1;
    this.prebuffering = true;
    this.consecutiveLosses = 0;
  }

  /** Push an encrypted-then-decrypted Opus frame keyed by its sequence. */
  push(seq: number, payload: Uint8Array): void {
    // Drop already-delivered (late) packets.
    if (this.lastDelivered >= 0 && seqDelta(seq, this.lastDelivered) <= 0) {
      this.lateDropped.count += 1;
      return;
    }
    // Drop packets behind the current playout expectation while live.
    if (
      !this.prebuffering &&
      this.expected !== null &&
      seqDelta(seq, this.expected) < 0
    ) {
      this.lateDropped.count += 1;
      return;
    }
    // Drop duplicates already buffered.
    if (this.buffer.has(seq)) {
      this.duplicateDropped.count += 1;
      return;
    }
    // Overflow: drop the oldest buffered frame (lowest wrap-aware seq).
    while (this.buffer.size >= this.capacityFrames) {
      let oldest: number | null = null;
      for (const key of this.buffer.keys()) {
        if (oldest === null || seqDelta(key, oldest) < 0) {
          oldest = key;
        }
      }
      if (oldest === null) break;
      this.buffer.delete(oldest);
      this.overflowDropped.count += 1;
    }
    this.buffer.set(seq, { seq, payload });
    // First packet: latch expected + leave prebuffer once deep enough.
    if (this.expected === null) {
      this.expected = seq;
    }
    if (this.prebuffering && this.bufferedMs >= this.targetDepthMs) {
      this.prebuffering = false;
    }
  }

  /** One 20 ms playout tick. */
  tick(): TickResult {
    if (this.prebuffering) {
      if (this.buffer.size === 0) return { kind: "idle" };
      return { kind: "prebuffer" };
    }
    if (this.expected === null) return { kind: "idle" };

    const want = this.expected;
    const entry = this.buffer.get(want);
    if (entry) {
      this.buffer.delete(want);
      this.lastDelivered = want;
      this.expected = (want + 1) >>> 0;
      this.consecutiveLosses = 0;
      return { kind: "packet", seq: want, payload: entry.payload };
    }

    // Missing slot → PLC (loss) and advance.
    this.consecutiveLosses += 1;
    if (this.consecutiveLosses >= this.maxLossFrames) {
      // Full reset back to prebuffer (plan §3.3: long gap → buffer reset).
      this.resets.count += 1;
      this.buffer.clear();
      this.lastDelivered = -1;
      this.expected = null;
      this.prebuffering = true;
      this.consecutiveLosses = 0;
      return { kind: "loss", seq: want };
    }
    this.lastDelivered = want;
    this.expected = (want + 1) >>> 0;
    return { kind: "loss", seq: want };
  }
}
