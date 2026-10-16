/// P7c integration: KeyUpdate relay over the control stream + harness
/// `POST /rotate` (plan §5). The media server relays a client's KeyUpdate
/// to every *other* member (no self-echo); the harness rotates pairwise
/// keys under fresh dense kids.
///
/// Requires the test harness (skipped otherwise):
///   cargo build -p rvp_flutter_quic --example p4a_harness
///   RVP_HARNESS=../../target/debug/examples/p4a_harness flutter test
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

void main() {
  final harnessPath = Platform.environment['RVP_HARNESS'];
  final skip = harnessPath == null
      ? 'RVP_HARNESS not set (build the p4a_harness example first)'
      : false;

  test('KeyUpdate is relayed to the other member and not echoed back',
      () async {
    final harness = await Process.start(harnessPath!, ['alice', 'bob']);
    addTearDown(harness.kill);

    final stdout = harness.stdout.transform(utf8.decoder);
    final line = await stdout.first.timeout(const Duration(seconds: 10));
    final info = jsonDecode(line) as Map<String, dynamic>;
    final port = info['port'] as int;
    final grants = (info['grants'] as Map<String, dynamic>).cast<String, String>();
    final pairs = info['pairs'] as List<dynamic>;
    expect(pairs, hasLength(1));
    final initialKid = (pairs[0] as Map<String, dynamic>)['kid'] as int;

    final address = '127.0.0.1:$port';
    await initRustLib();

    final alice = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grants['alice']!,
    );
    final aliceEvents = <ControlMessage>[];
    alice.messages.listen(aliceEvents.add);

    final bob = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grants['bob']!,
    );
    final bobEvents = <ControlMessage>[];
    bob.messages.listen(bobEvents.add);

    // Let Alice see Bob's join before announcing (ordering is not required
    // for relay, but keeps the assertion window tight).
    await _waitFor(() => aliceEvents
        .whereType<ParticipantJoinedMessage>()
        .any((e) => e.userId == 'bob'));

    // Alice announces a rotation to a fresh kid.
    final rotatedKid = initialKid + 1;
    await alice.sendKeyUpdate(rotatedKid);

    // Bob receives the relayed KeyUpdate with the announced kid...
    await _waitFor(() => bobEvents
        .whereType<KeyUpdateMessage>()
        .any((e) => e.kid == rotatedKid));
    // ...while Alice does not see her own announce (no self-echo).
    expect(
      aliceEvents.whereType<KeyUpdateMessage>(),
      isEmpty,
      reason: 'KeyUpdate must not be relayed back to the sender',
    );

    await alice.close();
    await bob.close();
  }, skip: skip, timeout: const Timeout(Duration(minutes: 2)));

  test('harness POST /rotate regenerates pair keys under fresh kids',
      () async {
    // Bind an ephemeral port first so parallel runs do not collide on the
    // harness's default 7860.
    final probe = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    final httpPort = probe.port;
    await probe.close();

    final harness = await Process.start(
      harnessPath!,
      ['alice', 'bob'],
      environment: {'RVP_HARNESS_HTTP': '127.0.0.1:$httpPort'},
    );
    addTearDown(harness.kill);

    final stdout = harness.stdout.transform(utf8.decoder);
    final line = await stdout.first.timeout(const Duration(seconds: 10));
    final info = jsonDecode(line) as Map<String, dynamic>;
    final pairs = (info['pairs'] as List).cast<Map<String, dynamic>>();
    final initialKid = pairs.single['kid'] as int;
    final initialKey = pairs.single['key'] as String;

    final client = HttpClient();
    addTearDown(() => client.close(force: true));

    // Wait for the HTTP endpoint to come up.
    await _waitFor(() async {
      try {
        final req = await client
            .getUrl(Uri.parse('http://127.0.0.1:$httpPort/state'))
            .timeout(const Duration(seconds: 2));
        final resp = await req.close().timeout(const Duration(seconds: 2));
        await resp.transform(utf8.decoder).join();
        return resp.statusCode == HttpStatus.ok;
      } catch (_) {
        return false;
      }
    });

    // Rotate: new kid, new key.
    final rotateReq = await client
        .postUrl(Uri.parse('http://127.0.0.1:$httpPort/rotate'))
        .timeout(const Duration(seconds: 5));
    final rotateResp = await rotateReq.close().timeout(const Duration(seconds: 5));
    expect(rotateResp.statusCode, HttpStatus.ok);
    final rotated =
        jsonDecode(await rotateResp.transform(utf8.decoder).join())
            as Map<String, dynamic>;
    final rotatedPair = (rotated['pairs'] as List).single as Map<String, dynamic>;
    expect(rotatedPair['kid'] as int, initialKid + 1);
    expect(rotatedPair['key'] as String, isNot(initialKey));

    // A subsequent GET /state serves the rotated pair too.
    final stateReq = await client
        .getUrl(Uri.parse('http://127.0.0.1:$httpPort/state'))
        .timeout(const Duration(seconds: 5));
    final stateResp = await stateReq.close().timeout(const Duration(seconds: 5));
    final stateBody =
        jsonDecode(await stateResp.transform(utf8.decoder).join())
            as Map<String, dynamic>;
    final statePair =
        (stateBody['pairs'] as List).single as Map<String, dynamic>;
    expect(statePair['kid'], rotatedPair['kid']);
    expect(statePair['key'], rotatedPair['key']);
  }, skip: skip, timeout: const Timeout(Duration(minutes: 2)));
}

Future<void> _waitFor(FutureOr<bool> Function() condition) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (!(await condition())) {
    if (DateTime.now().isAfter(deadline)) {
      fail('condition not met within 10s');
    }
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
}
