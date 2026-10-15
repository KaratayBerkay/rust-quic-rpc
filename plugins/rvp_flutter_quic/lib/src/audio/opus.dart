/// Opus encode/decode (48 kHz mono, 20 ms frames) over `opus_codec_dart`.
///
/// Native library loading goes through the `opus_codec` package (bundles
/// libopus on Android/iOS/desktop, falls back to the system `libopus.so.0`).
library;

import 'dart:ffi';
import 'dart:typed_data';

import 'package:opus_codec/opus_codec.dart' as opus_loader;
import 'package:opus_codec_dart/opus_codec_dart.dart' as opus;

/// Media clock sample rate (plan §3.2: 48 kHz to match Opus).
const int kOpusSampleRate = 48000;

/// Mono capture (plan §4 isolate model).
const int kOpusChannels = 1;

/// One Opus frame: 20 ms @ 48 kHz mono = 960 samples.
const int kOpusFrameSamples = kOpusSampleRate ~/ 50;

/// Thrown when the native Opus library is unavailable.
class OpusUnavailableException implements Exception {
  OpusUnavailableException(this.message);
  final String message;
  @override
  String toString() => 'OpusUnavailableException: $message';
}

bool _initialized = false;

/// Load the native libopus and initialize `opus_codec_dart` (idempotent).
///
/// Uses the `opus_codec` loader (bundled libopus on device; the Dart plugin
/// registrant supplies the platform implementation there). Under `flutter
/// test` the registrant does not run, so on failure we fall back to opening
/// the system `libopus.so.0` directly.
Future<void> ensureOpusInitialized() async {
  if (_initialized) return;
  Object? lib;
  Object? loadError;
  try {
    lib = await opus_loader.load();
  } catch (e) {
    loadError = e;
  }
  if (lib == null) {
    try {
      lib = DynamicLibrary.open('libopus.so.0');
    } catch (e) {
      throw OpusUnavailableException(
        'could not load libopus (loader: $loadError; system: $e)',
      );
    }
  }
  opus.initOpus(lib);
  _initialized = true;
}

/// Whether [ensureOpusInitialized] has completed.
bool get opusInitialized => _initialized;

/// Native libopus version string (after [ensureOpusInitialized]).
String get opusVersion => opus.getOpusVersion();

/// PCM encoder: 960 Int16 samples (20 ms) → one Opus packet.
class RvpOpusEncoder {
  RvpOpusEncoder._(this._enc);

  final opus.SimpleOpusEncoder _enc;

  /// Create after [ensureOpusInitialized].
  static RvpOpusEncoder create() {
    _requireInit();
    return RvpOpusEncoder._(
      opus.SimpleOpusEncoder(
        sampleRate: kOpusSampleRate,
        channels: kOpusChannels,
        application: opus.Application.audio,
      ),
    );
  }

  /// Encode exactly one 20 ms mono frame.
  Uint8List encodeFrame(Int16List pcm) {
    if (pcm.length != kOpusFrameSamples) {
      throw ArgumentError(
        'expected $kOpusFrameSamples samples, got ${pcm.length}',
      );
    }
    return _enc.encode(input: pcm);
  }

  /// Release the native encoder.
  void dispose() => _enc.destroy();
}

/// Opus packet → PCM decoder (with packet-loss concealment).
class RvpOpusDecoder {
  RvpOpusDecoder._(this._dec);

  final opus.SimpleOpusDecoder _dec;

  /// Create after [ensureOpusInitialized].
  static RvpOpusDecoder create() {
    _requireInit();
    return RvpOpusDecoder._(
      opus.SimpleOpusDecoder(
        sampleRate: kOpusSampleRate,
        channels: kOpusChannels,
      ),
    );
  }

  /// Decode one Opus packet to PCM.
  Int16List decodePacket(Uint8List packet) => _dec.decode(input: packet);

  /// Conceal one missing 20 ms frame (PLC).
  Int16List conceal() {
    // opus_codec_dart documents `loss` as milliseconds but passes it straight
    // through as opus_decode's `frame_size` (samples per channel) — verified
    // against system libopus: frame_size=20 fails, 960 works. Pass the frame
    // in samples; test/opus_test.dart pins this behavior.
    return _dec.decode(input: null, loss: kOpusFrameSamples);
  }

  /// Release the native decoder.
  void dispose() => _dec.destroy();
}

void _requireInit() {
  if (!_initialized) {
    throw StateError('call ensureOpusInitialized() first');
  }
}

/// Convert interleaved s16le bytes (e.g. from a device mic) to Int16 samples.
Int16List pcmS16LeToInt16(List<int> bytes, {int offset = 0, int? length}) {
  final end = length ?? bytes.length - offset;
  final out = Int16List(end ~/ 2);
  for (var i = 0; i < out.length; i++) {
    final lo = bytes[offset + i * 2];
    final hi = bytes[offset + i * 2 + 1];
    var v = (hi << 8) | lo;
    if (v >= 0x8000) v -= 0x10000;
    out[i] = v;
  }
  return out;
}

/// Convert Int16 samples to interleaved s16le bytes (e.g. for a speaker).
Uint8List int16ToPcmS16Le(Int16List samples) {
  final out = Uint8List(samples.length * 2);
  for (var i = 0; i < samples.length; i++) {
    final v = samples[i];
    out[i * 2] = v & 0xff;
    out[i * 2 + 1] = (v >> 8) & 0xff;
  }
  return out;
}
