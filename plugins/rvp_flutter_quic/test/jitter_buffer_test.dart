import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

Uint8List _payload(int tag) => Uint8List.fromList([tag, tag, tag, tag]);

void main() {
  group('jitter buffer', () {
    test('prebuffers to target depth before starting playout', () {
      final jb = JitterBuffer(targetDepthMs: 40, frameMs: 20);
      expect(jb.pop().gap, JitterGap.prebuffer);
      jb.push(0, _payload(0));
      expect(jb.pop().gap, JitterGap.prebuffer); // only 20 ms buffered
      jb.push(1, _payload(1));
      final pop = jb.pop();
      expect(pop.gap, isNull);
      expect(pop.frame!.sequence, 0);
      expect(jb.started, isTrue);
    });

    test('delivers in sequence order, reordering on arrival', () {
      final jb = JitterBuffer(targetDepthMs: 40, frameMs: 20);
      jb.push(1, _payload(1)); // out of order arrival
      jb.push(0, _payload(0));
      jb.push(2, _payload(2));
      jb.push(3, _payload(3));
      expect([jb.pop(), jb.pop(), jb.pop(), jb.pop()].map((p) => p.frame!.sequence),
          [0, 1, 2, 3]);
    });

    test('gap at the playout point pops a loss (PLC slot)', () {
      final jb = JitterBuffer(targetDepthMs: 40, frameMs: 20, maxLossFrames: 5);
      for (var i = 0; i < 4; i++) {
        if (i == 2) continue; // lose seq 2
        jb.push(i, _payload(i));
      }
      expect(jb.pop().frame!.sequence, 0);
      expect(jb.pop().frame!.sequence, 1);
      final gap = jb.pop();
      expect(gap.gap, JitterGap.loss);
      expect(gap.frame, isNull);
      expect(jb.pop().frame!.sequence, 3); // resyncs after the gap
      expect(jb.losses, 1);
    });

    test('late packet behind last delivered is dropped', () {
      final jb = JitterBuffer(targetDepthMs: 20, frameMs: 20);
      jb.push(10, _payload(10));
      jb.push(11, _payload(11));
      expect(jb.pop().frame!.sequence, 10);
      jb.push(10, _payload(99)); // late retransmit
      expect(jb.lateDropped, 1);
      expect(jb.buffered, 1);
    });

    test('long gap resets back to prebuffer', () {
      final jb = JitterBuffer(
        targetDepthMs: 20,
        frameMs: 20,
        maxLossFrames: 3,
      );
      jb.push(0, _payload(0));
      expect(jb.pop().frame!.sequence, 0);
      jb.push(5, _payload(5)); // a jump, but buffer was drained by losses
      // seqs 1..4 missing: three consecutive loss pops → reset
      expect(jb.pop().gap, JitterGap.loss);
      expect(jb.pop().gap, JitterGap.loss);
      final third = jb.pop();
      expect(third.gap, JitterGap.loss);
      expect(jb.resets, 1);
      expect(jb.started, isFalse);
      // fresh prebuffer round
      expect(jb.pop().gap, JitterGap.prebuffer);
    });

    test('sequence wrap from 0xFFFFFFFF → 0 is not late', () {
      final jb = JitterBuffer(targetDepthMs: 40, frameMs: 20);
      jb.push(0xFFFFFFFE, _payload(1));
      jb.push(0xFFFFFFFF, _payload(2));
      jb.push(0, _payload(3));
      jb.push(1, _payload(4));
      expect(
        [jb.pop(), jb.pop(), jb.pop(), jb.pop()]
            .map((p) => p.frame!.sequence),
        [0xFFFFFFFE, 0xFFFFFFFF, 0, 1],
      );
      expect(jb.lateDropped, 0);
    });

    test('overflow beyond maxDepthMs drops the oldest (stays live)', () {
      final jb = JitterBuffer(targetDepthMs: 40, maxDepthMs: 60, frameMs: 20);
      jb.push(0, _payload(0));
      jb.push(1, _payload(1));
      jb.push(2, _payload(2)); // at capacity (3 × 20 = 60 ms)
      jb.push(3, _payload(3)); // overflow → evict seq 0 (the oldest)
      expect(jb.overflowDropped, 1);
      jb.push(4, _payload(4)); // overflow → evict seq 1
      expect(jb.overflowDropped, 2);
      expect(jb.buffered, 3);
      expect(jb.pop().frame!.sequence, 2); // playout starts at what survived
    });

    test('duplicate push is dropped', () {
      final jb = JitterBuffer(targetDepthMs: 40, frameMs: 20);
      jb.push(0, _payload(0));
      jb.push(0, _payload(0));
      expect(jb.lateDropped, 1);
      expect(jb.buffered, 1);
    });
  });
}
