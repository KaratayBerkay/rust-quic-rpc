/// RVP/1 media datagram: fixed 16-byte header + payload (plan §3.2).
///
/// Byte-for-byte mirror of `rvp-proto`'s `MediaHeader` — the Dart and Rust
/// encoders must agree (cross-language vector tests in both test suites).
library;

import 'dart:typed_data';

/// Fixed media-header length in bytes.
const int kMediaHeaderLen = 16;

/// Current protocol version (`Ver` nibble).
const int kMediaVersion = 1;

/// Payload type: plaintext Opus (tests/interop only).
const int kPtOpus = 0;

/// Payload type: pairwise AES-GCM encrypted Opus (ADR-004).
const int kPtOpusEnc = 1;

/// Conservative payload cap (plan §3.2: `min(peer max_datagram, 1200)`).
const int kMaxMediaPayload = 1200 - kMediaHeaderLen;

/// Thrown when a media datagram cannot be decoded.
class MediaHeaderException implements Exception {
  MediaHeaderException(this.message);
  final String message;
  @override
  String toString() => 'MediaHeaderException: $message';
}

/// Fixed 16-byte RVP/1 media header.
class MediaHeader {
  MediaHeader({
    this.version = kMediaVersion,
    this.kind = 0,
    this.flags = 0,
    this.pt = kPtOpus,
    this.kid = 0,
    required this.sequence,
    required this.timestamp,
    this.frameDurationMs = 20,
    List<int> reserved = const [0, 0, 0],
  }) : reserved = List<int>.unmodifiable(reserved) {
    if (this.reserved.length != 3) {
      throw MediaHeaderException('reserved must be 3 bytes');
    }
  }

  /// Datagram type nibble (reserved; 0 for media).
  final int kind;
  final int flags;
  final int pt;

  /// Pairwise key id; 0 = plaintext payload.
  final int kid;

  /// u32 wrapping sequence (per publisher).
  final int sequence;

  /// Publisher media clock, 48 kHz ticks.
  final int timestamp;
  final int frameDurationMs;
  final int version;
  final List<int> reserved;

  /// Encode into a fixed 16-byte array (big-endian integers).
  Uint8List encode() {
    final buf = Uint8List(kMediaHeaderLen);
    buf[0] = ((version & 0x0f) << 4) | (kind & 0x0f);
    buf[1] = flags;
    buf[2] = pt;
    buf[3] = kid;
    buf[4] = (sequence >> 24) & 0xff;
    buf[5] = (sequence >> 16) & 0xff;
    buf[6] = (sequence >> 8) & 0xff;
    buf[7] = sequence & 0xff;
    buf[8] = (timestamp >> 24) & 0xff;
    buf[9] = (timestamp >> 16) & 0xff;
    buf[10] = (timestamp >> 8) & 0xff;
    buf[11] = timestamp & 0xff;
    buf[12] = frameDurationMs;
    buf[13] = reserved[0];
    buf[14] = reserved[1];
    buf[15] = reserved[2];
    return buf;
  }

  /// Decode from the first [kMediaHeaderLen] bytes of `buf`.
  static MediaHeader decode(List<int> buf) {
    if (buf.length < kMediaHeaderLen) {
      throw MediaHeaderException(
        'buffer too short: need $kMediaHeaderLen bytes, got ${buf.length}',
      );
    }
    final version = (buf[0] >> 4) & 0x0f;
    if (version != kMediaVersion) {
      throw MediaHeaderException('unsupported version $version');
    }
    return MediaHeader(
      version: version,
      kind: buf[0] & 0x0f,
      flags: buf[1],
      pt: buf[2],
      kid: buf[3],
      sequence: _u32(buf, 4),
      timestamp: _u32(buf, 8),
      frameDurationMs: buf[12],
      reserved: [buf[13], buf[14], buf[15]],
    );
  }

  static int _u32(List<int> b, int off) =>
      ((b[off] << 24) | (b[off + 1] << 16) | (b[off + 2] << 8) | b[off + 3]) &
      0xffffffff;

  @override
  bool operator ==(Object other) =>
      other is MediaHeader &&
      other.version == version &&
      other.kind == kind &&
      other.flags == flags &&
      other.pt == pt &&
      other.kid == kid &&
      other.sequence == sequence &&
      other.timestamp == timestamp &&
      other.frameDurationMs == frameDurationMs &&
      other.reserved[0] == reserved[0] &&
      other.reserved[1] == reserved[1] &&
      other.reserved[2] == reserved[2];

  @override
  int get hashCode => Object.hash(
        version,
        kind,
        flags,
        pt,
        kid,
        sequence,
        timestamp,
        frameDurationMs,
        reserved[0],
        reserved[1],
        reserved[2],
      );
}

/// A received/sending media datagram: header + payload.
class MediaDatagram {
  const MediaDatagram({required this.header, required this.payload});

  final MediaHeader header;
  final Uint8List payload;

  /// Serialize header ‖ payload.
  Uint8List build() {
    final out = Uint8List(kMediaHeaderLen + payload.length);
    out.setAll(0, header.encode());
    out.setAll(kMediaHeaderLen, payload);
    return out;
  }

  /// Parse a full datagram buffer.
  static MediaDatagram parse(List<int> buf) {
    if (buf.length < kMediaHeaderLen) {
      throw MediaHeaderException(
        'buffer too short: need $kMediaHeaderLen bytes, got ${buf.length}',
      );
    }
    return MediaDatagram(
      header: MediaHeader.decode(buf),
      payload: Uint8List.sublistView(
        buf is Uint8List ? buf : Uint8List.fromList(buf),
        kMediaHeaderLen,
      ),
    );
  }
}
