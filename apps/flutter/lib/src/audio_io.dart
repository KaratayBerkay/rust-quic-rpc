/// Device audio I/O over `flutter_sound` (plan §4 model): 48 kHz mono s16le
/// capture → 960-sample frames, playback from frames.
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:flutter_sound/flutter_sound.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

/// Microphone: streams exactly [kOpusFrameSamples]-sized PCM frames.
class Mic {
  Mic({this.onFrame});

  /// Called once per complete 20 ms frame (never with a partial frame).
  final void Function(Int16List frame)? onFrame;

  final FlutterSoundRecorder _rec = FlutterSoundRecorder();
  late final StreamController<List<Int16List>> _pcm =
      StreamController<List<Int16List>>();
  final List<int> _pending = <int>[];
  bool _opened = false;

  Future<void> open() async {
    if (_opened) return;
    await _rec.openRecorder();
    _pcm.stream.listen(_handleChunks);
    _opened = true;
  }

  Future<void> start() async {
    await open();
    await _rec.startRecorder(
      codec: Codec.pcm16,
      toStreamInt16: _pcm.sink,
      sampleRate: kOpusSampleRate,
      numChannels: 1,
      bufferSize: 4096,
      enableEchoCancellation: true,
    );
  }

  Future<void> stop() async {
    if (!_opened) return;
    await _rec.stopRecorder();
    _pending.clear();
  }

  Future<void> close() async {
    if (!_opened) return;
    await stop();
    await _pcm.close();
    await _rec.closeRecorder();
    _opened = false;
  }

  // flutter_sound delivers List<Int16List> (one entry per channel).
  void _handleChunks(List<Int16List> chunks) {
    if (chunks.isEmpty) return;
    _pending.addAll(chunks.first);
    while (_pending.length >= kOpusFrameSamples) {
      final frame = Int16List(kOpusFrameSamples);
      for (var i = 0; i < kOpusFrameSamples; i++) {
        frame[i] = _pending[i];
      }
      _pending.removeRange(0, kOpusFrameSamples);
      onFrame?.call(frame);
    }
  }
}

/// Speaker: plays 48 kHz mono s16le frames.
class Speaker {
  Speaker();

  final FlutterSoundPlayer _player = FlutterSoundPlayer();
  bool _started = false;

  Future<void> open() async {
    if (_started) return;
    await _player.openPlayer();
    await _player.startPlayerFromStream(
      codec: Codec.pcm16,
      interleaved: true,
      numChannels: 1,
      sampleRate: kOpusSampleRate,
      bufferSize: 4096,
    );
    _started = true;
  }

  void play(Int16List frame) {
    if (!_started) return;
    _player.uint8ListSink?.add(int16ToPcmS16Le(frame));
  }

  Future<void> close() async {
    if (!_started) return;
    _started = false;
    await _player.stopPlayer();
    await _player.closePlayer();
  }
}
