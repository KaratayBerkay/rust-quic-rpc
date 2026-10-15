import 'dart:math' as math;
import 'dart:typed_data';

import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

/// 440 Hz sine, 960 samples (20 ms @ 48 kHz), amplitude 8000.
Int16List sineFrame({double freq = 440, int offsetSamples = 0}) {
  final out = Int16List(kOpusFrameSamples);
  for (var i = 0; i < out.length; i++) {
    final t = (offsetSamples + i) / kOpusSampleRate;
    out[i] = (math.sin(2 * math.pi * freq * t) * 8000).round();
  }
  return out;
}

/// Root-mean-square of a PCM frame.
double rms(Int16List pcm) {
  if (pcm.isEmpty) return 0;
  var sum = 0;
  for (final s in pcm) {
    sum += s * s;
  }
  return math.sqrt(sum / pcm.length);
}

/// Let async receive chains (StreamController → listen → future chain) settle.
Future<void> flush([int rounds = 8]) async {
  for (var i = 0; i < rounds; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}
