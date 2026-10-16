/// Orchestrates a demo voice call: sessions + [VoicePipeline]s + device audio.
///
/// Two modes (plan §4 / P4b demo):
/// - **loopback**: one device runs both `alice` and `bob` through the SFU —
///   mic → alice → network → bob → speaker (self-echo over the wire).
/// - **two-device**: this device runs one user; the peer joins from elsewhere
///   with the same harness grants.
///
/// Key rotation (P7c, plan §5): [VoiceCall.rotateKeys] hits the harness
/// `POST /rotate`, re-applies the fresh pairwise keys, and announces
/// `KeyUpdate` on the control stream; a `KeyUpdate` received from the peer
/// re-fetches `/state` and swaps the pipeline keys (old kids stay receivable
/// for in-flight datagrams).
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

import 'audio_io.dart';
import 'harness_client.dart';

typedef LogFn = void Function(String message);

class VoiceCall {
  VoiceCall._({
    required this.user,
    required this.session,
    required this.pipeline,
    required this.mic,
    required this.speaker,
    required this.log,
    required this.stateUrl,
    required this.applyKeys,
    required this.secondaryApplyKeys,
  });

  final String user;
  final RvpSession session;
  final VoicePipeline pipeline;
  final Mic mic;
  final Speaker speaker;
  final LogFn log;
  final String stateUrl;
  final Future<void> Function(HarnessState) applyKeys;
  final Future<void> Function(HarnessState)? secondaryApplyKeys;
  final List<StreamSubscription<ControlMessage>> _keySubs = [];

  /// Whether the local mic is muted.
  bool get muted => pipeline.muted;
  set muted(bool value) => pipeline.muted;

  /// Loopback on this device: alice sends, bob plays (plan §4 demo).
  static Future<VoiceCall> loopback({
    required HarnessState state,
    String host = '127.0.0.1',
    required String stateUrl,
    required LogFn log,
  }) async {
    await initRustLib();
    await ensureOpusInitialized();

    final pair = state.pairs.singleOrNull ??
        (state.pairFor('alice') ??
            (throw StateError('harness state has no alice/bob pair')));
    final address = state.mediaAddress(host);
    log('connecting alice + bob to $address');

    final alice = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: state.grants['alice']!,
    );
    final bob = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: state.grants['bob']!,
    );
    log('joined room demo: alice pid=${alice.welcome.participantId}, '
        'bob pid=${bob.welcome.participantId}');

    final speaker = Speaker();
    await speaker.open();

    final alicePipe = VoicePipeline(
      sendDatagram: alice.sendDatagram,
      datagrams: const Stream<Uint8List>.empty(),
      onPlaybackPcm: (_) {},
    );
    final bobPipe = VoicePipeline(
      sendDatagram: bob.sendDatagram,
      datagrams: bob.datagrams,
      onPlaybackPcm: speaker.play,
    );
    await alicePipe.addPeerBytes(
      bob.welcome.participantId,
      kid: pair.kid,
      key: pair.key,
    );
    await bobPipe.addPeerBytes(
      alice.welcome.participantId,
      kid: pair.kid,
      key: pair.key,
    );
    bobPipe.start();

    final mic = Mic(onFrame: (frame) {
      alicePipe.pushPcm(frame).catchError((Object e) {
        log('send error: $e');
      });
    });
    await mic.start();

    log('loopback live: mic → alice → SFU → bob → speaker '
        '(kid=${pair.kid})');

    Future<void> applyKeys(HarnessState s) async {
      final p = s.pairs.singleOrNull ?? s.pairFor('alice');
      if (p == null) return;
      await alicePipe.addPeerBytes(
        bob.welcome.participantId,
        kid: p.kid,
        key: p.key,
      );
      await bobPipe.addPeerBytes(
        alice.welcome.participantId,
        kid: p.kid,
        key: p.key,
      );
      log('keys applied: kid=${p.kid}');
    }

    final call = VoiceCall._(
      user: 'alice+bob (loopback)',
      session: alice,
      pipeline: alicePipe,
      mic: mic,
      speaker: speaker,
      log: log,
      stateUrl: stateUrl,
      applyKeys: applyKeys,
      secondaryApplyKeys: applyKeys,
    )._withSecondary(bob, bobPipe);
    call._watchKeyUpdates([alice, bob]);
    return call;
  }

  /// One user on this device; the peer joins from another device.
  static Future<VoiceCall> twoDevice({
    required HarnessState state,
    required String me,
    String host = '127.0.0.1',
    required String stateUrl,
    required LogFn log,
  }) async {
    await initRustLib();
    await ensureOpusInitialized();

    final other = me == 'alice' ? 'bob' : 'alice';
    final pair = state.pairFor(me) ??
        (throw StateError('harness state has no pair for $me'));
    final address = state.mediaAddress(host);
    log('connecting $me to $address');

    final session = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: state.grants[me]!,
    );
    final peer = session.welcome.roster.firstWhere(
      (e) => e.userId == other,
      orElse: () => throw StateError(
        '$other is not in the room yet (start the other device first)',
      ),
    );

    final speaker = Speaker();
    await speaker.open();

    final pipeline = VoicePipeline(
      sendDatagram: session.sendDatagram,
      datagrams: session.datagrams,
      onPlaybackPcm: speaker.play,
    );
    await pipeline.addPeerBytes(
      peer.participantId,
      kid: pair.kid,
      key: pair.key,
    );
    pipeline.start();

    final mic = Mic(onFrame: (frame) {
      pipeline.pushPcm(frame).catchError((Object e) {
        log('send error: $e');
      });
    });
    await mic.start();

    log('live as $me (kid=${pair.kid}), peer $other pid=${peer.participantId}');

    Future<void> applyKeys(HarnessState s) async {
      final p = s.pairFor(me);
      if (p == null) return;
      await pipeline.addPeerBytes(
        peer.participantId,
        kid: p.kid,
        key: p.key,
      );
      log('keys applied: kid=${p.kid}');
    }

    final call = VoiceCall._(
      user: me,
      session: session,
      pipeline: pipeline,
      mic: mic,
      speaker: speaker,
      log: log,
      stateUrl: stateUrl,
      applyKeys: applyKeys,
      secondaryApplyKeys: null,
    );
    call._watchKeyUpdates([session]);
    return call;
  }

  /// Rotate every pairwise key via the harness `POST /rotate` (P7c), swap
  /// the fresh keys into the local pipelines, and announce `KeyUpdate` on
  /// the control stream so the peer re-fetches and swaps too (plan §5).
  Future<void> rotateKeys() async {
    final fresh = await HarnessState.rotate(stateUrl);
    await applyKeys(fresh);
    await secondaryApplyKeys?.call(fresh);
    final maxKid = fresh.pairs.fold<int>(0, (m, p) => p.kid > m ? p.kid : m);
    if (maxKid > 0) {
      await session.sendKeyUpdate(maxKid);
      log('rotated keys, announced kid=$maxKid');
    }
  }

  /// On a relayed `KeyUpdate` from the peer: re-fetch `/state` and swap the
  /// pipeline keys. The old kid stays receivable for in-flight datagrams.
  void _watchKeyUpdates(List<RvpSession> sessions) {
    for (final s in sessions) {
      _keySubs.add(
        s.messages.listen((msg) {
          if (msg is KeyUpdateMessage) {
            log('keyUpdate received (kid=${msg.kid}), pulling fresh keys');
            HarnessState.fetch(stateUrl).then((fresh) async {
              await applyKeys(fresh);
              await secondaryApplyKeys?.call(fresh);
            }).catchError((Object e) {
              log('key pull failed: $e');
            });
          }
        }),
      );
    }
  }

  // Loopback keeps a second session/pipeline to tear down (alice is the
  // "primary" fields above; bob rides along).
  RvpSession? _secondarySession;
  VoicePipeline? _secondaryPipeline;

  VoiceCall _withSecondary(RvpSession session, VoicePipeline pipeline) {
    _secondarySession = session;
    _secondaryPipeline = pipeline;
    return this;
  }

  Future<void> stop() async {
    log('stopping call ($user)');
    for (final sub in _keySubs) {
      await sub.cancel();
    }
    _keySubs.clear();
    try {
      await mic.close();
    } catch (e) {
      log('mic close: $e');
    }
    await pipeline.stop();
    await _secondaryPipeline?.stop();
    await session.close();
    await _secondarySession?.close();
    await speaker.close();
    pipeline.dispose();
    _secondaryPipeline?.dispose();
    log('stopped');
  }
}

extension<T> on Iterable<T> {
  T? get singleOrNull {
    final it = iterator;
    if (!it.moveNext()) return null;
    final first = it.current;
    return it.moveNext() ? null : first;
  }
}
