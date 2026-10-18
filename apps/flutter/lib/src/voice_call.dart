/// Orchestrates a demo voice call: sessions + [VoicePipeline]s + device audio.
///
/// Three modes:
/// - **loopback** (harness): one device runs both `alice` and `bob` through
///   the SFU — mic → alice → network → bob → speaker.
/// - **two-device** (harness): this device runs one harness user; the peer
///   joins from another device with the same harness grants.
/// - **nest** (plan §5): grants + pairwise keys come from the NestJS control
///   plane, so this device joins the **same room with the same keys as the
///   web client** — the real cross-device call path.
///
/// Key rotation (P7c, plan §5): harness modes hit the harness
/// `POST /rotate`; nest mode hits Nest `POST /rooms/:id/keys/rotate`. Either
/// way the fresh keys land in the pipeline and `KeyUpdate{kid}` is announced
/// on the control stream; a received `KeyUpdate` re-pulls keys (harness
/// `GET /state`, nest `GET /rooms/:id/keys`) and swaps them in (old kids
/// stay receivable for in-flight datagrams).
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

import 'audio_io.dart';
import 'harness_client.dart';
import 'nest_client.dart';

typedef LogFn = void Function(String message);

class VoiceCall {
  VoiceCall._({
    required this.user,
    required this.session,
    required this.pipeline,
    required this.mic,
    required this.speaker,
    required this.log,
    required this.rotate,
    required this.refresh,
    this.role,
  });

  final String user;
  final RvpSession session;
  final VoicePipeline pipeline;
  final Mic mic;
  final Speaker speaker;
  final LogFn log;

  /// Grant role (`host`/`member`) in nest mode; null in harness modes.
  final String? role;

  /// Rotate keys, apply them, announce `KeyUpdate` (wired per mode).
  final Future<void> Function() rotate;

  /// Re-pull keys from the control plane and apply them (on remote
  /// `KeyUpdate`).
  final Future<void> Function() refresh;

  final List<StreamSubscription<ControlMessage>> _keySubs = [];

  /// Whether the local mic is muted.
  bool get muted => pipeline.muted;
  set muted(bool value) => pipeline.muted;

  /// Host-only key rotation (nest mode; harness modes always allow it).
  bool get canRotate => role != 'member';

  /// Rotate every pairwise key, apply the fresh ones, and announce
  /// `KeyUpdate{kid}` on the control stream (plan §5, P7c).
  Future<void> rotateKeys() => rotate();

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
      rotate: () async {
        final fresh = await HarnessState.rotate(stateUrl);
        await applyKeys(fresh);
        final maxKid =
            fresh.pairs.fold<int>(0, (m, p) => p.kid > m ? p.kid : m);
        if (maxKid > 0) {
          await alice.sendKeyUpdate(maxKid);
          log('rotated keys, announced kid=$maxKid');
        }
      },
      refresh: () async {
        await applyKeys(await HarnessState.fetch(stateUrl));
      },
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
      rotate: () async {
        final fresh = await HarnessState.rotate(stateUrl);
        await applyKeys(fresh);
        final maxKid =
            fresh.pairs.fold<int>(0, (m, p) => p.kid > m ? p.kid : m);
        if (maxKid > 0) {
          await session.sendKeyUpdate(maxKid);
          log('rotated keys, announced kid=$maxKid');
        }
      },
      refresh: () async {
        await applyKeys(await HarnessState.fetch(stateUrl));
      },
    );
    call._watchKeyUpdates([session]);
    return call;
  }

  /// Plan §5 Nest mode: join the room from [grant] (fetched by the caller
  /// via [NestClient.createGrant]) with pairwise keys mapped onto whoever is
  /// in the welcome roster; late joiners and `KeyUpdate` pull fresh keys
  /// from Nest so the call works regardless of join order.
  static Future<VoiceCall> nest({
    required NestClient client,
    required NestSession control,
    required NestGrant grant,
    required String address,
    required LogFn log,
  }) async {
    await initRustLib();
    await ensureOpusInitialized();

    log('connecting ${grant.userId} to $address (room ${grant.roomId})');
    final media = await RvpSession.connect(
      address: address,
      serverName: 'localhost',
      grantJwt: grant.grant,
    );
    log('joined room ${grant.roomId}: pid=${media.welcome.participantId}, '
        'role=${grant.role}, pairs=${grant.pairs.length}');

    final speaker = Speaker();
    await speaker.open();

    final pipeline = VoicePipeline(
      sendDatagram: media.sendDatagram,
      datagrams: media.datagrams,
      onPlaybackPcm: speaker.play,
    );

    // Latest known pairwise keys (grant snapshot, refreshed from Nest on
    // join/KeyUpdate) and userId → participantId from roster + events.
    var latest = List<NestPairKey>.of(grant.pairs);
    final pidOf = <String, int>{
      for (final e in media.welcome.roster) e.userId: e.participantId,
    };

    Future<void> applyKeys() async {
      final slots = resolvePeerKeys(pairs: latest, userIdToParticipantId: pidOf);
      for (final slot in slots) {
        await pipeline.addPeerBytes(
          slot.participantId,
          kid: slot.kid,
          key: slot.key,
        );
      }
      log('keys applied: ${slots.length} peer(s)');
    }

    Future<void> refresh() async {
      latest = await client.fetchPairKeys(control, grant.roomId);
      await applyKeys();
    }

    await applyKeys();
    pipeline.start();

    final mic = Mic(onFrame: (frame) {
      pipeline.pushPcm(frame).catchError((Object e) {
        log('send error: $e');
      });
    });
    await mic.start();

    log('live as ${grant.userId} (role=${grant.role})');

    final call = VoiceCall._(
      user: grant.userId,
      session: media,
      pipeline: pipeline,
      mic: mic,
      speaker: speaker,
      log: log,
      role: grant.role,
      rotate: () async {
        // Host-only on the Nest side; members get an HTTP 403.
        final fresh = await client.rotatePairKeys(control, grant.roomId);
        latest = fresh;
        await applyKeys();
        final maxKid = fresh.fold<int>(0, (m, p) => p.kid > m ? p.kid : m);
        if (maxKid > 0) {
          await media.sendKeyUpdate(maxKid);
          log('rotated keys, announced kid=$maxKid');
        }
      },
      refresh: refresh,
    );

    // Late joiner: pair keys follow room membership, so pull the authoritative
    // set from Nest instead of trusting the grant-time snapshot.
    call._keySubs.add(media.messages.listen((msg) {
      if (msg is ParticipantJoinedMessage) {
        pidOf[msg.userId] = msg.participantId;
        log('participantJoined ${msg.userId} (pid=${msg.participantId})');
        refresh().catchError((Object e) {
          log('key pull on join failed: $e');
        });
      }
    }));
    call._watchKeyUpdates([media]);
    return call;
  }

  /// On a relayed `KeyUpdate` from a peer: re-pull keys and swap them in.
  /// The old kid stays receivable for in-flight datagrams.
  void _watchKeyUpdates(List<RvpSession> sessions) {
    for (final s in sessions) {
      _keySubs.add(
        s.messages.listen((msg) {
          if (msg is KeyUpdateMessage) {
            log('keyUpdate received (kid=${msg.kid}), pulling fresh keys');
            refresh().catchError((Object e) {
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
