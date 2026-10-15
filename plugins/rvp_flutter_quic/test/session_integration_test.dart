/// P4a integration: two sessions in one room, SFU fanout + roster/events.
///
/// Requires the test harness (skipped otherwise):
///   cargo build -p rvp_flutter_quic --example p4a_harness
///   RVP_HARNESS=../../target/debug/examples/p4a_harness flutter test
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

void main() {
  final harnessPath = Platform.environment['RVP_HARNESS'];
  final skip = harnessPath == null
      ? 'RVP_HARNESS not set (build the p4a_harness example first)'
      : false;

  test('two sessions: welcome, roster, fanout, join/leave events', () async {
    final harness = await Process.start(harnessPath!, ['alice', 'bob']);
    addTearDown(harness.kill);

    // One JSON line: {"port": int, "grants": {user: jwt}}
    final stdout = harness.stdout.transform(utf8.decoder);
    final line = await stdout.first.timeout(const Duration(seconds: 10));
    final info = jsonDecode(line) as Map<String, dynamic>;
    final port = info['port'] as int;
    final grants = (info['grants'] as Map<String, dynamic>).cast<String, String>();
    expect(grants.keys, containsAll(['alice', 'bob']));

    final address = '127.0.0.1:$port';
    await initRustLib();

    // --- Alice joins first (empty roster) ---
    final alice = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grants['alice']!,
    );
    expect(alice.welcome.roster, isEmpty);
    final aliceEvents = <ControlMessage>[];
    final aliceEventDone = Completer<void>();
    alice.messages.listen(
      aliceEvents.add,
      onDone: aliceEventDone.complete,
    );

    final aliceDatagrams = <Uint8List>[];
    alice.datagrams.listen(aliceDatagrams.add);

    // --- Bob joins, sees Alice ---
    final bob = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grants['bob']!,
    );
    expect(bob.welcome.roster, hasLength(1));
    expect(bob.welcome.roster.single.userId, 'alice');
    final bobDatagrams = <Uint8List>[];
    bob.datagrams.listen(bobDatagrams.add);

    // Alice learns Bob joined.
    await _waitFor(() => aliceEvents
        .whereType<ParticipantJoinedMessage>()
        .any((e) => e.userId == 'bob'));

    // --- Fanout: Alice → Bob (no self-echo for Alice) ---
    final payload = Uint8List.fromList([9, 8, 7, 6, 5]);
    await alice.sendDatagram(payload);
    await _waitFor(() => bobDatagrams.isNotEmpty);
    expect(bobDatagrams.single, payload);
    expect(aliceDatagrams, isEmpty);

    // --- Bob → Alice ---
    await bob.sendDatagram(Uint8List.fromList([1, 2, 3]));
    await _waitFor(() => aliceDatagrams.isNotEmpty);
    expect(aliceDatagrams.single, [1, 2, 3]);

    // --- Bob leaves: Alice gets ParticipantLeft ---
    final bobParticipantId = bob.welcome.participantId;
    await bob.close();
    await _waitFor(
      () => aliceEvents
          .whereType<ParticipantLeftMessage>()
          .any((e) => e.participantId == bobParticipantId),
    );

    await alice.close();
    await aliceEventDone.future.timeout(const Duration(seconds: 2));
  }, skip: skip, timeout: const Timeout(Duration(minutes: 2)));
}

Future<void> _waitFor(bool Function() condition) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) {
      fail('condition not met within 10s');
    }
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
}
