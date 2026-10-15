/// Client jitter buffer (plan §3.3): 20 ms slots keyed by wrapping sequence,
/// prebuffer to a target depth, late-drop, gaps → PLC, long gap → reset.
library;

import 'dart:typed_data';
import 'dart:math' as math;

/// One buffered media frame ready for playout.
class JitterFrame {
  const JitterFrame({required this.sequence, required this.payload});
  final int sequence;
  final Uint8List payload;
}

/// Why a playout tick produced no frame.
enum JitterGap {
  /// Still prebuffering (not started yet) — do NOT run PLC, emit silence.
  prebuffer,

  /// Started, but the next frame is missing — run PLC.
  loss,
}

/// Result of [JitterBuffer.pop].
class JitterPop {
  const JitterPop.frame(JitterFrame this.frame) : gap = null;
  const JitterPop.gap(this.frame, this.gap);
  const JitterPop.prebuffer() : frame = null, gap = JitterGap.prebuffer;

  /// The frame to decode, or null.
  final JitterFrame? frame;

  /// Why [frame] is null (null means a normal frame).
  final JitterGap? gap;
}

/// Deterministic jitter buffer for tests and the real pipeline.
///
/// State machine:
/// - **Prebuffer:** collect packets until [targetDepthMs] is buffered (and at
///   least one packet arrived); pops [JitterGap.prebuffer] (emit silence).
/// - **Playout:** deliver `expected_seq`, then `expected_seq+1`, …; a missing
///   slot pops [JitterGap.loss] (call Opus PLC) and advances the expectation.
/// - **Late:** any packet with `seq ≤ last_delivered`, or behind the playout
///   expectation, is dropped (wrap-aware).
/// - **Reset:** after [maxLossFrames] consecutive losses the buffer returns to
///   prebuffer (plan: "long gap → buffer reset + control Resync").
class JitterBuffer {
  JitterBuffer({
    this.targetDepthMs = 40,
    this.maxDepthMs = 80,
    this.maxLossFrames = 5,
    this.frameMs = 20,
  });

  /// Initial playout depth (plan: adaptive 40–80 ms).
  final int targetDepthMs;

  /// Hard cap on buffered frames ahead of playout.
  final int maxDepthMs;

  /// Consecutive PLC pops before a reset.
  final int maxLossFrames;

  /// Frame duration in ms (20 for this profile).
  final int frameMs;

  final Map<int, Uint8List> _buffer = {};
  int? _lastDelivered;
  int? _expected;
  bool _started = false;
  int _consecutiveLosses = 0;

  /// Packets late-dropped (behind last delivered or behind playout).
  int lateDropped = 0;

  /// Frames dropped because the buffer was at [maxDepthMs] capacity.
  int overflowDropped = 0;

  /// Playout ticks that hit a missing frame (PLC used).
  int losses = 0;

  /// Resets back to prebuffer.
  int resets = 0;

  /// Whether playout has started (prebuffer satisfied).
  bool get started => _started;

  /// Currently buffered frame count.
  int get buffered => _buffer.length;

  /// Currently buffered duration in ms.
  int get bufferedMs => _buffer.length * frameMs;

  /// Store one received frame (wrap-aware; late/out-of-order safe).
  void push(int sequence, Uint8List payload) {
    final seq = sequence & 0xffffffff;

    if (_lastDelivered != null && _signedDelta(seq, _lastDelivered!) <= 0) {
      lateDropped++;
      return;
    }
    if (_started && _expected != null && _signedDelta(seq, _expected!) < 0) {
      lateDropped++;
      return;
    }
    if (_buffer.containsKey(seq)) {
      lateDropped++;
      return;
    }

    // Over capacity: drop the oldest buffered frame (fast-forward toward the
    // live edge) rather than the incoming one — playout delay never exceeds
    // maxDepthMs, which is the whole point of the 40–80 ms window.
    final cap = math.max(1, maxDepthMs ~/ frameMs);
    if (_buffer.length >= cap) {
      final oldest = _oldestSeq();
      if (oldest != null) {
        _buffer.remove(oldest);
        overflowDropped++;
      }
    }
    _buffer[seq] = payload;
  }

  /// Pop the next playout slot.
  JitterPop pop() {
    if (!_started) {
      if (_buffer.isEmpty || bufferedMs < targetDepthMs) {
        return const JitterPop.prebuffer();
      }
      _started = true;
      _expected = _oldestSeq();
      _consecutiveLosses = 0;
    }

    final expected = _expected!;
    final payload = _buffer.remove(expected);
    if (payload != null) {
      _lastDelivered = expected;
      _expected = (expected + 1) & 0xffffffff;
      _consecutiveLosses = 0;
      return JitterPop.frame(
        JitterFrame(sequence: expected, payload: payload),
      );
    }

    losses++;
    _consecutiveLosses++;
    _expected = (expected + 1) & 0xffffffff;
    if (_consecutiveLosses >= maxLossFrames) {
      reset();
      return const JitterPop.gap(null, JitterGap.loss);
    }
    return const JitterPop.gap(null, JitterGap.loss);
  }

  /// Return to prebuffer (plan: long gap → reset).
  void reset() {
    _started = false;
    _expected = null;
    _lastDelivered = null;
    _consecutiveLosses = 0;
    _buffer.clear();
    resets++;
  }

  int? _oldestSeq() {
    if (_buffer.isEmpty) return null;
    int? best;
    for (final seq in _buffer.keys) {
      if (best == null || _signedDelta(seq, best) < 0) best = seq;
    }
    return best;
  }

  /// Wrap-aware signed distance `a - b` in int32 space.
  static int _signedDelta(int a, int b) {
    var d = (a - b) & 0xffffffff;
    if (d >= 0x80000000) d -= 0x100000000;
    return d;
  }
}
