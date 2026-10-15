import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

Uint8List _bytes(List<int> xs) => Uint8List.fromList(xs);

void main() {
  group('control codec', () {
    test('Hello roundtrip', () {
      final frame = encodeHello('eyJhbGciOiJFZERTQSJ9.fake');
      final decoded = decodeControlFrame(frame);
      expect(decoded, isA<HelloMessage>());
      decoded as HelloMessage;
      expect(decoded.protocolVersion, protocolVersion);
      expect(decoded.grantJwt, 'eyJhbGciOiJFZERTQSJ9.fake');
    });

    test('JoinRoom roundtrip', () {
      final frame = encodeJoinRoom();
      expect(frame, _bytes([0, 0, 0, 2, 0, 2]));
      expect(decodeControlFrame(frame), isA<JoinRoomMessage>());
    });

    test('Welcome with roster decodes (Rust test vector)', () {
      // type(2)=3 pid=0xDEAD_BEEF maxDatagram=1200 count=2
      //   pid=1 len=5 "alice", pid=2 len=3 "bob"
      final body = BytesBuilder()
        ..add([0, 3])
        ..add([0, 0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF]) // 0xDEAD_BEEF
        ..add([0x04, 0xB0]) // 1200
        ..add([0, 2])
        ..add([0, 0, 0, 0, 0, 0, 0, 1, 0, 5])
        ..add('alice'.codeUnits)
        ..add([0, 0, 0, 0, 0, 0, 0, 2, 0, 3])
        ..add('bob'.codeUnits);
      final raw = body.toBytes();
      final frame = Uint8List(4 + raw.length);
      ByteData.sublistView(frame).setUint32(0, raw.length);
      frame.setRange(4, frame.length, raw);

      final decoded = decodeControlFrame(frame);
      expect(decoded, isA<WelcomeMessage>());
      decoded as WelcomeMessage;
      expect(decoded.participantId, 0xDEADBEEF);
      expect(decoded.maxDatagram, 1200);
      expect(decoded.roster, hasLength(2));
      expect(decoded.roster[0].participantId, 1);
      expect(decoded.roster[0].userId, 'alice');
      expect(decoded.roster[1].participantId, 2);
      expect(decoded.roster[1].userId, 'bob');
    });

    test('ParticipantJoined / ParticipantLeft / Error decode', () {
      final joined = BytesBuilder()
        ..add([0, 4])
        ..add([0, 0, 0, 0, 0, 0, 0, 9])
        ..add([0, 5])
        ..add('carol'.codeUnits);
      expect(decodeControlBody(joined.toBytes()), isA<ParticipantJoinedMessage>());
      final jm = decodeControlBody(joined.toBytes()) as ParticipantJoinedMessage;
      expect(jm.participantId, 9);
      expect(jm.userId, 'carol');

      final left = BytesBuilder()
        ..add([0, 5])
        ..add([0, 0, 0, 0, 0, 0, 0, 9]);
      expect(decodeControlBody(left.toBytes()), isA<ParticipantLeftMessage>());
      expect(
        (decodeControlBody(left.toBytes()) as ParticipantLeftMessage)
            .participantId,
        9,
      );

      final err = BytesBuilder()
        ..add([0, 15])
        ..add([1, 145]) // 401
        ..add('bad grant'.codeUnits);
      final em = decodeControlBody(err.toBytes()) as ControlErrorMessage;
      expect(em.code, ControlErrorCode.unauthorized);
      expect(em.message, 'bad grant');
    });

    test('rejects bad frames', () {
      expect(() => decodeControlFrame(_bytes([0, 1, 2])),
          throwsA(isA<ControlCodecException>()));
      expect(
        () => decodeControlFrame(_bytes([0, 0, 0, 9, 0, 2, 0])),
        throwsA(isA<ControlCodecException>()), // prefix mismatch
      );
      expect(
        () => decodeControlBody(_bytes([0xEE, 1])),
        throwsA(isA<ControlCodecException>()), // unknown type
      );
    });
  });
}
