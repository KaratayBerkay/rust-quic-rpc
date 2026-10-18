/// NestJS control-plane client — plan §5 endpoints (P5), the same flow the
/// web app uses: login → rooms/membership → media grant + pairwise keys.
///
/// Replaces the dev `p4a_harness` `GET /state` stand-in so the Flutter app
/// joins the **same rooms with the same keys** as the browser (plan §5 note:
/// "switching apps/flutter to Nest" — this file is that switch).
///
/// Wire shapes verified against `services/nest/src`:
/// - `POST /auth/login` → `{token, userId, ttlSeconds}`
/// - `POST /rooms` → `{id, name, createdBy, createdAt, ...}`
/// - `POST /rooms/:id/members` → member record
/// - `POST /rooms/:id/grants` → `{grant, expiresAt, roomId, userId, role,
///   pairs: [{peer, kid, key}], mediaAddress?}`
/// - `GET /rooms/:id/keys` / `POST /rooms/:id/keys/rotate` → `{pairs: [...]}`
library;

import 'dart:convert';
import 'dart:io';

/// HTTP error from the Nest API (non-2xx or transport failure).
class NestApiError implements Exception {
  const NestApiError(this.message, {this.statusCode});

  final String message;
  final int? statusCode;

  @override
  String toString() => statusCode == null
      ? 'NestApiError: $message'
      : 'NestApiError(HTTP $statusCode): $message';
}

/// Long-lived API session (not the short-lived media grant).
class NestSession {
  const NestSession({
    required this.token,
    required this.userId,
    required this.ttlSeconds,
  });

  final String token;
  final String userId;
  final int ttlSeconds;

  static NestSession fromJson(Map<String, dynamic> json) => NestSession(
        token: json['token']! as String,
        userId: json['userId']! as String,
        ttlSeconds: (json['ttlSeconds']! as num).toInt(),
      );
}

/// Room record (id is what the other client types to join).
class NestRoom {
  const NestRoom({
    required this.id,
    required this.name,
    required this.createdBy,
  });

  final String id;
  final String name;
  final String createdBy;

  static NestRoom fromJson(Map<String, dynamic> json) => NestRoom(
        id: json['id']! as String,
        name: (json['name'] as String?) ?? '',
        createdBy: (json['createdBy'] as String?) ?? '',
      );
}

/// One pairwise payload key as delivered to one user: AES-128-GCM key shared
/// with exactly [peer], room-wide unique [kid] (plan §3.2/ADR-004).
class NestPairKey {
  const NestPairKey({
    required this.peer,
    required this.kid,
    required this.key,
  });

  final String peer;
  final int kid;

  /// Hex-encoded 16-byte key (Nest sends `keyBytes.toString('hex')`).
  final String key;

  List<int> get keyBytes => _hex(key);

  static NestPairKey fromJson(Map<String, dynamic> json) => NestPairKey(
        peer: json['peer']! as String,
        kid: (json['kid']! as num).toInt(),
        key: json['key']! as String,
      );
}

/// Response of `POST /rooms/:id/grants`: the media grant JWT + the caller's
/// pairwise keys at grant time.
class NestGrant {
  const NestGrant({
    required this.grant,
    required this.roomId,
    required this.userId,
    required this.role,
    required this.pairs,
    this.mediaAddress,
  });

  final String grant;
  final String roomId;
  final String userId;
  final String role;
  final List<NestPairKey> pairs;

  /// P8a pin's `host:port` when the room is pinned and resolvable; null on
  /// a single-node setup (the caller supplies the media address instead).
  final String? mediaAddress;

  static NestGrant fromJson(Map<String, dynamic> json) => NestGrant(
        grant: json['grant']! as String,
        roomId: json['roomId']! as String,
        userId: json['userId']! as String,
        role: (json['role'] as String?) ?? 'member',
        pairs: [
          for (final raw in (json['pairs'] as List?) ?? const [])
            NestPairKey.fromJson(raw as Map<String, dynamic>),
        ],
        mediaAddress: json['mediaAddress'] as String?,
      );
}

/// A pairwise key resolved onto a connected participant.
class PeerKeySlot {
  const PeerKeySlot({
    required this.participantId,
    required this.kid,
    required this.key,
  });

  final int participantId;
  final int kid;
  final List<int> key;
}

/// Map Nest per-peer keys onto pipeline peer slots by participant id.
///
/// Peers not yet in [userIdToParticipantId] are skipped — they'll be applied
/// when their `participantJoined` event arrives (pair keys follow room
/// membership, so a pull at that point gets them).
List<PeerKeySlot> resolvePeerKeys({
  required List<NestPairKey> pairs,
  required Map<String, int> userIdToParticipantId,
}) {
  final out = <PeerKeySlot>[];
  for (final p in pairs) {
    final pid = userIdToParticipantId[p.peer];
    if (pid == null) continue;
    out.add(PeerKeySlot(participantId: pid, kid: p.kid, key: p.keyBytes));
  }
  return out;
}

/// Thin client for the plan §5 REST endpoints (pure `dart:io`, no deps).
class NestClient {
  NestClient(String baseUrl) : _base = _strip(baseUrl);

  static String _strip(String url) =>
      url.endsWith('/') ? url.substring(0, url.length - 1) : url;

  final String _base;

  /// Normalized base URL (trailing slash stripped) — used for cache checks.
  String get baseUrl => _base;

  /// `POST /auth/login` — any non-empty userId in dev (plan §5).
  Future<NestSession> login(String userId) async {
    final json = await _request(
      'POST',
      '/auth/login',
      body: {'userId': userId},
    );
    return NestSession.fromJson(json);
  }

  /// `POST /rooms` — the creator becomes the host.
  Future<NestRoom> createRoom(NestSession session, String name) async {
    final json = await _request('POST', '/rooms', session: session, body: {
      'name': name,
    });
    return NestRoom.fromJson(json);
  }

  /// `POST /rooms/:id/members` — any member can invite (dev-grade, plan §5).
  Future<void> addMember(
    NestSession session,
    String roomId,
    String userId,
  ) async {
    await _request(
      'POST',
      '/rooms/$roomId/members',
      session: session,
      body: {'userId': userId},
    );
  }

  /// `POST /rooms/:id/grants` — media grant JWT + pairwise keys for the room
  /// members known at this moment.
  Future<NestGrant> createGrant(NestSession session, String roomId) async {
    final json = await _request(
      'POST',
      '/rooms/$roomId/grants',
      session: session,
    );
    return NestGrant.fromJson(json);
  }

  /// `GET /rooms/:id/keys` — pull the caller's current pairwise keys (e.g.
  /// after a relayed `KeyUpdate`, or when a new member joins).
  Future<List<NestPairKey>> fetchPairKeys(
    NestSession session,
    String roomId,
  ) async {
    final json = await _request('GET', '/rooms/$roomId/keys', session: session);
    return [
      for (final raw in (json['pairs'] as List?) ?? const [])
        NestPairKey.fromJson(raw as Map<String, dynamic>),
    ];
  }

  /// `POST /rooms/:id/keys/rotate` — host-only: fresh keys under new dense
  /// kids; the caller then announces `KeyUpdate{kid}` on the media stream.
  Future<List<NestPairKey>> rotatePairKeys(
    NestSession session,
    String roomId,
  ) async {
    final json = await _request(
      'POST',
      '/rooms/$roomId/keys/rotate',
      session: session,
    );
    return [
      for (final raw in (json['pairs'] as List?) ?? const [])
        NestPairKey.fromJson(raw as Map<String, dynamic>),
    ];
  }

  Future<Map<String, dynamic>> _request(
    String method,
    String path, {
    NestSession? session,
    Map<String, dynamic>? body,
  }) async {
    final client = HttpClient();
    try {
      final uri = Uri.parse('$_base$path');
      final req = await (method == 'POST'
          ? client.postUrl(uri)
          : client.getUrl(uri));
      req.headers.contentType = ContentType.json;
      if (session != null) {
        req.headers.set(HttpHeaders.authorizationHeader, 'Bearer ${session.token}');
      }
      if (body != null) {
        req.write(jsonEncode(body));
      }
      final resp = await req.close().timeout(const Duration(seconds: 5));
      final text = await resp.transform(utf8.decoder).join();
      if (resp.statusCode != HttpStatus.ok) {
        throw NestApiError(
          '$method $path → ${text.isEmpty ? 'no body' : text}',
          statusCode: resp.statusCode,
        );
      }
      final decoded = jsonDecode(text);
      if (decoded is! Map<String, dynamic>) {
        throw NestApiError('$method $path → unexpected JSON shape');
      }
      return decoded;
    } on SocketException catch (e) {
      throw NestApiError('cannot reach $_base$path: ${e.message}');
    } on HandshakeException catch (e) {
      throw NestApiError('TLS handshake to $_base failed: $e');
    } on HttpException catch (e) {
      throw NestApiError(e.message);
    } finally {
      client.close(force: true);
    }
  }
}

List<int> _hex(String s) {
  if (s.length.isOdd || !RegExp(r'^[0-9a-fA-F]*$').hasMatch(s)) {
    throw FormatException('invalid hex key: $s');
  }
  return [
    for (var i = 0; i < s.length; i += 2)
      int.parse(s.substring(i, i + 2), radix: 16),
  ];
}
