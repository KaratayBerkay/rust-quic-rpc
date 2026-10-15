import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

void main() {
  group('media header', () {
    test('matches the rvp-proto Rust encode byte-for-byte', () {
      // Same fixture as crates/rvp-proto tests (plan §3.2).
      final h = MediaHeader(
        flags: 0x01,
        pt: kPtOpusEnc,
        kid: 7,
        sequence: 0xDEADBEEF,
        timestamp: 0x01020304,
        frameDurationMs: 20,
        reserved: [9, 8, 7],
      );
      final bytes = h.encode();
      expect(bytes, <int>[
        0x10, // ver=1, kind=0
        0x01, // flags
        0x01, // pt = PT_OPUS_ENC
        0x07, // kid
        0xDE, 0xAD, 0xBE, 0xEF, // seq BE
        0x01, 0x02, 0x03, 0x04, // ts BE
        0x14, // frame_duration_ms = 20
        0x09, 0x08, 0x07, // reserved
      ]);
      expect(MediaHeader.decode(bytes), h);
    });

    test('roundtrips a plaintext default header', () {
      final h = MediaHeader(sequence: 1, timestamp: 960);
      final decoded = MediaHeader.decode(h.encode());
      expect(decoded, h);
      expect(decoded.pt, kPtOpus);
      expect(decoded.kid, 0);
      expect(decoded.version, kMediaVersion);
    });

    test('sequence survives u32 wrap (0xFFFFFFFF)', () {
      final h = MediaHeader(sequence: 0xFFFFFFFF, timestamp: 0);
      expect(MediaHeader.decode(h.encode()).sequence, 0xFFFFFFFF);
      // Values above u32 mask on encode.
      final wrapped = MediaHeader(sequence: 0x1_0000_0001, timestamp: 0);
      expect(MediaHeader.decode(wrapped.encode()).sequence, 1);
    });

    test('decode rejects a short buffer', () {
      expect(
        () => MediaHeader.decode(Uint8List(8)),
        throwsA(isA<MediaHeaderException>()),
      );
      expect(
        () => MediaDatagram.parse(Uint8List(kMediaHeaderLen - 1)),
        throwsA(isA<MediaHeaderException>()),
      );
    });

    test('decode rejects an unsupported version', () {
      final bytes = MediaHeader(sequence: 0, timestamp: 0).encode();
      bytes[0] = 0x90; // version 9
      expect(
        () => MediaHeader.decode(bytes),
        throwsA(isA<MediaHeaderException>()),
      );
    });

    test('datagram build/parse splits header and payload', () {
      final payload = Uint8List.fromList(List<int>.generate(40, (i) => i));
      final dg = MediaDatagram(
        header: MediaHeader(pt: kPtOpusEnc, kid: 3, sequence: 42, timestamp: 7),
        payload: payload,
      );
      final raw = dg.build();
      expect(raw.length, kMediaHeaderLen + 40);
      final parsed = MediaDatagram.parse(raw);
      expect(parsed.header, dg.header);
      expect(parsed.payload, payload);
    });
  });
}
