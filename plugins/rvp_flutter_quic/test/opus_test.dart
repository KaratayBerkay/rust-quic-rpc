import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

import 'audio_test_helpers.dart';

void main() {
  group('pcm conversion', () {
    test('s16le roundtrip', () {
      final pcm = sineFrame();
      final bytes = int16ToPcmS16Le(pcm);
      expect(bytes.length, kOpusFrameSamples * 2);
      expect(pcmS16LeToInt16(bytes), pcm);
    });

    test('s16le byte-order and sign extension', () {
      expect(pcmS16LeToInt16([0x34, 0x12]), [0x1234]);
      expect(pcmS16LeToInt16([0xFF, 0x7F]), [0x7FFF]);
      expect(pcmS16LeToInt16([0x00, 0x80]), [-32768]);
    });
  });

  // Requires the native libopus (bundled by the opus_codec loader, or the
  // system libopus.so.0 on Linux; CI installs libopus0 in the plugin job).
  // Fails loudly when unavailable — that is a CI/environment regression.
  group('opus codec', () {
    test('loads and reports a version', () async {
      await ensureOpusInitialized();
      expect(opusInitialized, isTrue);
      expect(opusVersion, isNotEmpty);
    });

    test('encode → decode preserves a sine frame', () async {
      await ensureOpusInitialized();
      final enc = RvpOpusEncoder.create();
      final dec = RvpOpusDecoder.create();
      addTearDown(enc.dispose);
      addTearDown(dec.dispose);

      final packet = enc.encodeFrame(sineFrame());
      expect(packet, isNotEmpty);
      expect(packet.length, lessThan(kMaxMediaPayload));

      final decoded = dec.decodePacket(packet);
      expect(decoded.length, kOpusFrameSamples);
      expect(rms(decoded), greaterThan(200));
    });

    test('conceal (PLC) returns one 20 ms frame', () async {
      await ensureOpusInitialized();
      final enc = RvpOpusEncoder.create();
      final dec = RvpOpusDecoder.create();
      addTearDown(enc.dispose);
      addTearDown(dec.dispose);

      final packet = enc.encodeFrame(sineFrame());
      dec.decodePacket(packet); // prime decoder state
      final concealed = dec.conceal();
      expect(concealed.length, kOpusFrameSamples);
    });

    test('encoder rejects a wrong frame size', () async {
      await ensureOpusInitialized();
      final enc = RvpOpusEncoder.create();
      addTearDown(enc.dispose);
      expect(
        () => enc.encodeFrame(Int16List(100)),
        throwsA(isA<ArgumentError>()),
      );
    });
  });
}
