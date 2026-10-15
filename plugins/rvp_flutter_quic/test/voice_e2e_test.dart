/// P4b E2E voice: two QUIC sessions in one room, real Opus + AES-GCM through
/// the SFU fanout, jitter playout with energy assertions (plan §4).
///
/// Requires the test harness (skipped otherwise):
///   cargo build -p rvp_flutter_quic --example p4a_harness
///   RVP_HARNESS=../../target/debug/examples/p4a_harness flutter test
library;

import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

import 'audio_test_helpers.dart';

List<int> _hex(String s) => [
      for (var i = 0; i < s.length; i += 2)
        int.parse(s.substring(i, i + 2), radix: 16),
    ];

void main() {
  final harnessPath = Platform.environment['RVP_HARNESS'];
  final skip = harnessPath == null
      ? 'RVP_HARNESS not set (build the p4a_harness example first)'
      : false;

  test('voice over QUIC SFU: sine in → encrypted fanout → decoded audio out',
      () async {
    final harness = await Process.start(harnessPath!, ['alice', 'bob']);
    addTearDown(harness.kill);

    // {"port": int, "grants": {...}, "pairs": [{"a","b","kid","key"}]}
    final line = await harness.stdout
        .transform(utf8.decoder)
        .first
        .timeout(const Duration(seconds: 10));
    final info = jsonDecode(line) as Map<String, dynamic>;
    final address = '127.0.0.1:${info['port'] as int}';
    final grants =
        (info['grants'] as Map<String, dynamic>).cast<String, String>();
    final pair = (info['pairs'] as List).single as Map<String, dynamic>;
    final kid = pair['kid'] as int;
    final key = _hex(pair['key'] as String);
    expect(pair['a'], 'alice');
    expect(pair['b'], 'bob');

    await initRustLib();
    await ensureOpusInitialized();

    final alice = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grants['alice']!,
    );
    addTearDown(alice.close);
    final aliceEvents = <ControlMessage>[];
    alice.messages.listen(aliceEvents.add);

    final bob = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grants['bob']!,
    );
    addTearDown(bob.close);

    // Wait until Alice has seen Bob (fanout target is established).
    await _waitFor(() => aliceEvents
        .whereType<ParticipantJoinedMessage>()
        .any((e) => e.userId == 'bob'));

    final playback = <Int16List>[];
    final alicePipe = VoicePipeline(
      sendDatagram: alice.sendDatagram,
      datagrams: alice.datagrams,
      onPlaybackPcm: (_) {},
    );
    final bobPipe = VoicePipeline(
      sendDatagram: bob.sendDatagram,
      datagrams: bob.datagrams,
      onPlaybackPcm: playback.add,
      // Transport test, not a jitter-policy test: allow the whole burst in
      // (policy is covered by test/jitter_buffer_test.dart).
      jitter: JitterBuffer(targetDepthMs: 40, maxDepthMs: 200),
    );
    addTearDown(alicePipe.dispose);
    addTearDown(bobPipe.dispose);
    await alicePipe.addPeerBytes(bob.welcome.participantId, kid: kid, key: key);
    await bobPipe.addPeerBytes(alice.welcome.participantId, kid: kid, key: key);
    bobPipe.start(autoTick: false);

    // Phase 1: stream frames across QUIC + SFU, each fully decrypting into
    // the jitter buffer before playout begins.
    const frames = 6;
    for (var i = 0; i < frames; i++) {
      await alicePipe.pushPcm(sineFrame(offsetSamples: i * kOpusFrameSamples));
      await _waitFor(() => bobPipe.datagramsReceived == i + 1);
    }
    await _waitFor(() => bobPipe.jitter.buffered == frames);

    // Phase 2: drain the buffer through decode.
    while (bobPipe.jitter.buffered > 0) {
      bobPipe.tick();
    }

    expect(alicePipe.framesSent, frames);
    expect(bobPipe.datagramsReceived, frames);
    expect(bobPipe.decryptFailures, 0);
    expect(bobPipe.missingKeyDropped, 0);
    expect(bobPipe.malformedDropped, 0);
    expect(bobPipe.jitter.losses, 0);
    expect(bobPipe.jitter.buffered, 0);

    // One decoded audio frame per input frame, all carrying energy.
    expect(playback, hasLength(frames));
    final loud = [for (final f in playback) if (rms(f) > 200) f];
    expect(loud.length, greaterThan(4),
        reason: 'decoded audio carries real energy after seal/open/decode');
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
