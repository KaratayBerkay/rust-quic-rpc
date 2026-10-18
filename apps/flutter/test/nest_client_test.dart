import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_voice/src/nest_client.dart';

void main() {
  group('NestPairKey / NestGrant parsing (wire shapes from services/nest)', () {
    const grantSample = '''
{
  "grant": "jwt-media",
  "expiresAt": 1760000000,
  "roomId": "room-1",
  "userId": "bob",
  "role": "member",
  "pairs": [
    {"peer": "alice", "kid": 3, "key": "000102030405060708090a0b0c0d0e0f"}
  ],
  "mediaAddress": null
}
''';

    test('parses grant, role and the hex pairwise key', () {
      final grant = NestGrant.fromJson(
        jsonDecode(grantSample) as Map<String, dynamic>,
      );
      expect(grant.grant, 'jwt-media');
      expect(grant.roomId, 'room-1');
      expect(grant.userId, 'bob');
      expect(grant.role, 'member');
      expect(grant.mediaAddress, isNull);

      final pair = grant.pairs.single;
      expect(pair.peer, 'alice');
      expect(pair.kid, 3);
      expect(pair.keyBytes, [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
      ]);
    });

    test('rejects malformed hex keys', () {
      const bad = '''
{"grant":"j","roomId":"r","userId":"u","role":"host",
 "pairs":[{"peer":"p","kid":1,"key":"xyz"}]}
''';
      final grant = NestGrant.fromJson(
        jsonDecode(bad) as Map<String, dynamic>,
      );
      expect(() => grant.pairs.single.keyBytes, throwsFormatException);
    });
  });

  group('resolvePeerKeys', () {
    const pairs = [
      NestPairKey(peer: 'alice', kid: 2, key: '00'),
      NestPairKey(peer: 'carol', kid: 3, key: '11'),
    ];

    test('maps known peers onto participant ids, skips strangers', () {
      final slots = resolvePeerKeys(
        pairs: pairs,
        userIdToParticipantId: {'alice': 41},
      );
      expect(slots, hasLength(1));
      expect(slots.single.participantId, 41);
      expect(slots.single.kid, 2);
      expect(slots.single.key, [0x00]);
    });

    test('empty roster resolves nothing (late joiners get keys on event)', () {
      expect(
        resolvePeerKeys(pairs: pairs, userIdToParticipantId: const {}),
        isEmpty,
      );
    });
  });

  group('NestClient against a local stub server', () {
    late HttpServer server;
    late NestClient client;
    final requests = <String>[];

    setUp(() async {
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      client = NestClient('http://127.0.0.1:${server.port}');
      requests.clear();
      server.listen((req) async {
        requests.add('${req.method} ${req.uri.path}');
        final segments = req.uri.pathSegments;
        Object body;
        if (req.method == 'POST' && segments.join('/') == 'auth/login') {
          body = {'token': 'tok', 'userId': 'bob', 'ttlSeconds': 86400};
        } else if (req.method == 'POST' && segments.join('/') == 'rooms') {
          body = {
            'id': 'room-9',
            'name': 'demo',
            'createdBy': 'bob',
            'createdAt': '2026-01-01T00:00:00Z',
          };
        } else if (req.method == 'POST' && segments.length == 3 &&
            segments[2] == 'members') {
          body = {
            'roomId': segments[1],
            'userId': 'alice',
            'role': 'member',
            'joinedAt': '2026-01-01T00:00:00Z',
          };
        } else if (req.method == 'POST' && segments.length == 3 &&
            segments[2] == 'grants') {
          if (segments[1] == 'room-404') {
            req.response.statusCode = HttpStatus.notFound;
            req.response.write('{"message":"not a member"}');
            await req.response.close();
            return;
          }
          body = {
            'grant': 'jwt-media',
            'expiresAt': 1760000000,
            'roomId': segments[1],
            'userId': 'bob',
            'role': 'member',
            'pairs': [
              {'peer': 'alice', 'kid': 1, 'key': 'aa' * 16},
            ],
          };
        } else if (req.uri.path.endsWith('/keys/rotate')) {
          body = {
            'pairs': [
              {'peer': 'alice', 'kid': 2, 'key': 'bb' * 16},
            ],
          };
        } else if (req.uri.path.endsWith('/keys')) {
          body = {
            'pairs': [
              {'peer': 'alice', 'kid': 7, 'key': 'cc' * 16},
            ],
          };
        } else {
          req.response.statusCode = HttpStatus.notFound;
          await req.response.close();
          return;
        }
        req.response.statusCode = HttpStatus.ok;
        req.response.headers.contentType = ContentType.json;
        req.response.write(jsonEncode(body));
        await req.response.close();
      });
    });

    tearDown(() => server.close(force: true));

    test('login → createRoom → addMember → createGrant flow', () async {
      final session = await client.login('bob');
      expect(session.token, 'tok');
      expect(session.userId, 'bob');

      final room = await client.createRoom(session, 'demo');
      expect(room.id, 'room-9');

      await client.addMember(session, room.id, 'alice');

      final grant = await client.createGrant(session, room.id);
      expect(grant.grant, 'jwt-media');
      expect(grant.roomId, 'room-9');
      expect(grant.pairs.single.peer, 'alice');
      expect(grant.pairs.single.keyBytes, List.filled(16, 0xaa));

      expect(requests, [
        'POST /auth/login',
        'POST /rooms',
        'POST /rooms/room-9/members',
        'POST /rooms/room-9/grants',
      ]);
    });

    test('fetchPairKeys and rotatePairKeys return dense kids', () async {
      final session = await client.login('bob');
      final pulled = await client.fetchPairKeys(session, 'room-9');
      expect(pulled.single.kid, 7);
      final rotated = await client.rotatePairKeys(session, 'room-9');
      expect(rotated.single.kid, 2);
      expect(rotated.single.keyBytes, List.filled(16, 0xbb));
    });

    test('non-2xx becomes NestApiError with status', () async {
      final session = await client.login('bob');
      expect(
        () => client.createGrant(session, 'room-404'),
        throwsA(
          isA<NestApiError>().having((e) => e.statusCode, 'statusCode', 404),
        ),
      );
    });
  });
}
