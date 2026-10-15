import 'dart:async';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

import 'audio_test_helpers.dart';

/// End-to-end pipeline tests: Opus encode → AES-GCM seal → datagram fan-out →
/// open → jitter → decode, over an in-memory transport (plan §4).
///
/// Requires the Rust native lib (built by the Flutter native-assets hooks
/// during `flutter test`) and the native libopus.
void main() {
  final keyBytes = List<int>.generate(16, (i) => i);

  late StreamController<Uint8List> toBob;
  late List<Uint8List> sentByAlice;
  late VoicePipeline alice;
  late VoicePipeline bob;
  late List<Int16List> playback;

  setUpAll(() async {
    await initRustLib();
    await ensureOpusInitialized();
  });

  setUp(() async {
    toBob = StreamController<Uint8List>();
    sentByAlice = <Uint8List>[];
    playback = <Int16List>[];

    bob = VoicePipeline(
      sendDatagram: (_) async {},
      datagrams: toBob.stream,
      onPlaybackPcm: playback.add,
    );
    await bob.addPeerBytes(1, kid: 1, key: keyBytes);
    bob.start(autoTick: false);

    alice = VoicePipeline(
      sendDatagram: (data) async {
        final copy = Uint8List.fromList(data);
        sentByAlice.add(copy);
        toBob.add(copy);
      },
      datagrams: const Stream<Uint8List>.empty(),
      onPlaybackPcm: (_) {},
    );
    await alice.addPeerBytes(1, kid: 1, key: keyBytes);
  });

  tearDown(() async {
    await alice.stop();
    await bob.stop();
    await toBob.close();
    alice.dispose();
    bob.dispose();
  });

  test('audio path: encode → seal → send → open → jitter → decode', () async {
    const frames = 6;
    // Realistic cadence: one frame arrives, one playout tick consumes it.
    for (var i = 0; i < frames; i++) {
      await alice.pushPcm(sineFrame(offsetSamples: i * kOpusFrameSamples));
      await flush();
      bob.tick();
    }
    while (bob.jitter.buffered > 0) {
      bob.tick();
    }

    // One ciphertext datagram per frame, kid=1, PT=OPUS_ENC, seq order.
    expect(sentByAlice.length, frames);
    expect(alice.framesSent, frames);
    final headers = [
      for (final raw in sentByAlice) MediaDatagram.parse(raw).header,
    ];
    expect([for (final h in headers) h.sequence], [0, 1, 2, 3, 4, 5]);
    expect([for (final h in headers) h.pt], List.filled(frames, kPtOpusEnc));
    expect([for (final h in headers) h.kid], List.filled(frames, 1));

    // All frames decrypted and played out on this clean link: one silence
    // tick for prebuffer, then `frames` decoded audio frames, no PLC.
    expect(bob.datagramsReceived, frames);
    expect(bob.decryptFailures, 0);
    expect(bob.missingKeyDropped, 0);
    expect(bob.jitter.buffered, 0);
    expect(bob.jitter.losses, 0);
    expect(playback.length, frames + 1);
    expect(playback.first.every((s) => s == 0), isTrue,
        reason: 'prebuffer tick plays silence');
    final loud = [for (final f in playback.skip(1)) if (rms(f) > 200) f];
    expect(loud.length, greaterThan(4), reason: 'decoded audio has energy');
  });

  test('concurrent pushPcm calls serialize into seq order', () async {
    final futures = [
      for (var i = 0; i < 5; i++) alice.pushPcm(sineFrame(offsetSamples: i * 960)),
    ];
    await Future.wait(futures);
    await flush();
    final seqs = [
      for (final raw in sentByAlice) MediaDatagram.parse(raw).header.sequence,
    ];
    expect(seqs, [0, 1, 2, 3, 4]);
  });

  test('tampered ciphertext is dropped by AEAD', () async {
    await alice.pushPcm(sineFrame());
    await flush();
    expect(bob.jitter.buffered, 1);

    final tampered = Uint8List.fromList(sentByAlice[0]);
    tampered[tampered.length - 1] ^= 0xff;
    toBob.add(tampered);
    await flush();

    expect(bob.decryptFailures, 1);
    expect(bob.jitter.buffered, 1); // nothing corrupt reached the buffer
  });

  test('unknown kid is dropped (no key registered)', () async {
    final header = MediaHeader(
      pt: kPtOpusEnc,
      kid: 9,
      sequence: 0,
      timestamp: 0,
    );
    toBob.add(MediaDatagram(header: header, payload: Uint8List(32)).build());
    await flush();
    expect(bob.missingKeyDropped, 1);
    expect(bob.jitter.buffered, 0);
  });

  test('removed peer loses its key', () async {
    bob.removePeer(1);
    await alice.pushPcm(sineFrame());
    await flush();
    expect(bob.missingKeyDropped, 1);
    expect(bob.jitter.buffered, 0);
  });

  test('malformed datagrams are dropped', () async {
    toBob.add(Uint8List(4)); // shorter than the header
    final badVersion = Uint8List(kMediaHeaderLen);
    badVersion[0] = 0x90;
    toBob.add(badVersion);
    await flush();
    expect(bob.malformedDropped, 2);
    expect(bob.jitter.buffered, 0);
  });

  test('plaintext mode (interop) sends one PT=0 datagram, no seal', () async {
    final plain = VoicePipeline(
      sendDatagram: (data) async {
        sentByAlice.add(Uint8List.fromList(data));
        toBob.add(Uint8List.fromList(data));
      },
      datagrams: const Stream<Uint8List>.empty(),
      onPlaybackPcm: (_) {},
      plaintext: true,
    );
    addTearDown(plain.dispose);

    await plain.pushPcm(sineFrame());
    await flush();

    expect(sentByAlice.length, 1);
    final header = MediaDatagram.parse(sentByAlice[0]).header;
    expect(header.pt, kPtOpus);
    expect(header.kid, 0);
    expect(bob.jitter.buffered, 1);
    expect(bob.datagramsReceived, 1);
  });

  test('muted pipeline sends nothing', () async {
    alice.muted = true;
    await alice.pushPcm(sineFrame());
    await flush();
    expect(alice.framesSent, 0);
    expect(sentByAlice, isEmpty);
    expect(bob.datagramsReceived, 0);
  });

  test('stop() cancels the receive subscription', () async {
    await alice.pushPcm(sineFrame());
    await flush();
    final before = bob.datagramsReceived;
    await bob.stop();
    expect(bob.running, isFalse);

    await alice.pushPcm(sineFrame());
    await flush();
    expect(bob.datagramsReceived, before);
  });
}
