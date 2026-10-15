/// RVP/1 control-stream codec (Dart mirror of `rvp-proto::control`).
///
/// Frames are `u32 BE length` ‖ `u16 BE type` ‖ body, where `length` covers
/// type + body. Pure Dart (no FFI) so it unit-tests without a server or
/// native library.
library;

import 'dart:convert';
import 'dart:typed_data';

/// Current RVP protocol version (`Hello.protocolVersion`).
const int protocolVersion = 1;

/// Hard cap on control frames (matches `rvp-proto`, JWT grants are a few KB).
const int maxControlFrameLen = 16 * 1024;

const int _typeHello = 1;
const int _typeJoinRoom = 2;
const int _typeWelcome = 3;
const int _typeParticipantJoined = 4;
const int _typeParticipantLeft = 5;
const int _typeKeyUpdate = 6;
const int _typeError = 15;

/// Error codes carried by [ControlErrorMessage] (mirror of `error_code`).
abstract final class ControlErrorCode {
  static const int badRequest = 400;
  static const int unauthorized = 401;
  static const int forbidden = 403;
  static const int timeout = 408;
  static const int roomFull = 429;
  static const int internal = 500;
  static const int connectionLimit = 503;
  static const int versionUnsupported = 505;
}

/// Thrown when a frame cannot be decoded.
class ControlCodecException implements Exception {
  ControlCodecException(this.message);
  final String message;
  @override
  String toString() => 'ControlCodecException: $message';
}

/// One decoded control message.
sealed class ControlMessage {
  const ControlMessage();
}

/// `Hello { protocolVersion, grantJwt }` — client → server, first frame.
final class HelloMessage extends ControlMessage {
  const HelloMessage({required this.protocolVersion, required this.grantJwt});
  final int protocolVersion;
  final String grantJwt;
}

/// `JoinRoom` — client → server (room comes from the verified grant).
final class JoinRoomMessage extends ControlMessage {
  const JoinRoomMessage();
}

/// A room member advertised in [WelcomeMessage] / [ParticipantJoinedMessage].
final class RosterEntry {
  const RosterEntry({required this.participantId, required this.userId});
  final int participantId;
  final String userId;
}

/// `Welcome` — server → client; only after this are DATAGRAMs accepted.
final class WelcomeMessage extends ControlMessage {
  const WelcomeMessage({
    required this.participantId,
    required this.maxDatagram,
    required this.roster,
  });
  final int participantId;
  final int maxDatagram;
  final List<RosterEntry> roster;
}

/// A peer joined the room.
final class ParticipantJoinedMessage extends ControlMessage {
  const ParticipantJoinedMessage({
    required this.participantId,
    required this.userId,
  });
  final int participantId;
  final String userId;
}

/// A peer left the room.
final class ParticipantLeftMessage extends ControlMessage {
  const ParticipantLeftMessage({required this.participantId});
  final int participantId;
}

/// `KeyUpdate { kid }` — client → server announce; relayed to other members.
final class KeyUpdateMessage extends ControlMessage {
  const KeyUpdateMessage({required this.kid});
  final int kid;
}

/// Server-side error (also sent instead of [WelcomeMessage] on rejection).
final class ControlErrorMessage extends ControlMessage {
  const ControlErrorMessage({required this.code, required this.message});
  final int code;
  final String message;
}

/// Encode `Hello` as a complete frame (prefix included).
Uint8List encodeHello(String grantJwt) {
  final jwt = utf8.encode(grantJwt);
  final body = Uint8List(4 + jwt.length);
  final bd = ByteData.sublistView(body);
  bd.setUint16(0, _typeHello);
  bd.setUint16(2, protocolVersion);
  body.setRange(4, body.length, jwt);
  return _frame(body);
}

/// Encode `JoinRoom` as a complete frame (prefix included).
Uint8List encodeJoinRoom() {
  final body = Uint8List(2);
  ByteData.sublistView(body).setUint16(0, _typeJoinRoom);
  return _frame(body);
}

/// Encode `KeyUpdate` as a complete frame (prefix included).
Uint8List encodeKeyUpdate(int kid) {
  final body = Uint8List(3);
  final bd = ByteData.sublistView(body);
  bd.setUint16(0, _typeKeyUpdate);
  body[2] = kid;
  return _frame(body);
}

/// Decode a complete frame (prefix included).
ControlMessage decodeControlFrame(Uint8List frame) {
  if (frame.length < 6) {
    throw ControlCodecException('frame too short: ${frame.length} bytes');
  }
  final bd = ByteData.sublistView(frame);
  final len = bd.getUint32(0);
  if (len != frame.length - 4) {
    throw ControlCodecException(
      'length prefix $len != body ${frame.length - 4}',
    );
  }
  if (len > maxControlFrameLen) {
    throw ControlCodecException('frame too large: $len > $maxControlFrameLen');
  }
  return decodeControlBody(frame.sublist(4));
}

/// Decode a frame body (after the `u32` length prefix).
ControlMessage decodeControlBody(Uint8List body) {
  if (body.length < 2) {
    throw ControlCodecException('body too short');
  }
  final bd = ByteData.sublistView(body);
  final type = bd.getUint16(0);
  switch (type) {
    case _typeHello:
      if (body.length < 4) throw ControlCodecException('bad Hello');
      return HelloMessage(
        protocolVersion: bd.getUint16(2),
        grantJwt: utf8.decode(body.sublist(4)),
      );
    case _typeJoinRoom:
      return const JoinRoomMessage();
    case _typeWelcome:
      // type(2) pid(8) maxDatagram(2) count(2) entries...
      if (body.length < 14) throw ControlCodecException('bad Welcome');
      final count = bd.getUint16(12);
      var offset = 14;
      final roster = <RosterEntry>[];
      for (var i = 0; i < count; i++) {
        if (body.length < offset + 10) {
          throw ControlCodecException('bad Welcome roster');
        }
        final pid = _u64(bd, offset);
        final uidLen = bd.getUint16(offset + 8);
        offset += 10;
        if (body.length < offset + uidLen) {
          throw ControlCodecException('bad Welcome roster user');
        }
        roster.add(
          RosterEntry(
            participantId: pid,
            userId: utf8.decode(body.sublist(offset, offset + uidLen)),
          ),
        );
        offset += uidLen;
      }
      return WelcomeMessage(
        participantId: _u64(bd, 2),
        maxDatagram: bd.getUint16(10),
        roster: roster,
      );
    case _typeParticipantJoined:
      // type(2) pid(8) len(2) user(len)
      if (body.length < 12) throw ControlCodecException('bad ParticipantJoined');
      final pid = _u64(bd, 2);
      final uidLen = bd.getUint16(10);
      if (body.length < 12 + uidLen) {
        throw ControlCodecException('bad ParticipantJoined user');
      }
      return ParticipantJoinedMessage(
        participantId: pid,
        userId: utf8.decode(body.sublist(12, 12 + uidLen)),
      );
    case _typeParticipantLeft:
      if (body.length < 10) throw ControlCodecException('bad ParticipantLeft');
      return ParticipantLeftMessage(participantId: _u64(bd, 2));
    case _typeKeyUpdate:
      if (body.length < 3) throw ControlCodecException('bad KeyUpdate');
      return KeyUpdateMessage(kid: body[2]);
    case _typeError:
      if (body.length < 4) throw ControlCodecException('bad Error');
      return ControlErrorMessage(
        code: bd.getUint16(2),
        message: utf8.decode(body.sublist(4)),
      );
    default:
      throw ControlCodecException('unknown frame type $type');
  }
}

Uint8List _frame(Uint8List body) {
  final out = Uint8List(4 + body.length);
  ByteData.sublistView(out).setUint32(0, body.length);
  out.setRange(4, out.length, body);
  return out;
}

int _u64(ByteData bd, int offset) =>
    (bd.getUint32(offset) << 32) | bd.getUint32(offset + 4);
