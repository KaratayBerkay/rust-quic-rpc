import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_voice/src/harness_client.dart';

void main() {
  group('HarnessState.parse', () {
    const sample = '''
{
  "port": 47011,
  "grants": {
    "alice": "jwt-alice",
    "bob": "jwt-bob"
  },
  "pairs": [
    {"a": "alice", "b": "bob", "kid": 1, "key": "000102030405060708090a0b0c0d0e0f"}
  ]
}
''';

    test('parses port, grants and the pairwise key', () {
      final state = HarnessState.parse(sample);
      expect(state.port, 47011);
      expect(state.grants['alice'], 'jwt-alice');
      expect(state.grants['bob'], 'jwt-bob');
      expect(state.mediaAddress('10.0.0.2'), '10.0.0.2:47011');

      final pair = state.pairs.single;
      expect(pair.a, 'alice');
      expect(pair.b, 'bob');
      expect(pair.kid, 1);
      expect(pair.key, [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
      ]);
    });

    test('pairFor resolves either side of the pair', () {
      final state = HarnessState.parse(sample);
      final forAlice = state.pairFor('alice')!;
      final forBob = state.pairFor('bob')!;
      expect(forAlice.kid, forBob.kid);
      expect(forAlice.peerOf('alice'), 'bob');
      expect(forBob.peerOf('bob'), 'alice');
      expect(forAlice.involves('carol'), isFalse);
      expect(state.pairFor('carol'), isNull);
    });

    test('rejects malformed json', () {
      expect(() => HarnessState.parse('{"port": 1}'), throwsA(isA<TypeError>()));
    });
  });
}
