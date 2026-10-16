/// End-to-end voice pipeline (plan §4):
///
/// ```text
/// pushPcm → Opus encode → seal per peer (N−1 datagrams) → send
/// recv → parse header → open (kid) → jitter buffer → decode/PLC → speaker
/// ```
///
/// One [VoicePipeline] per [RvpSession]. Timing: the app feeds exactly
/// [kOpusFrameSamples] of 48 kHz mono PCM per 20 ms frame on the send side;
/// on the receive side [start] runs a 20 ms playout ticker (tests call
/// [tick] manually for determinism).
library;

import 'dart:async';
import 'dart:typed_data';

import '../rust/api/crypto.dart';
import 'jitter_buffer.dart';
import 'media_header.dart';
import 'opus.dart';

/// Send seam: `RvpSession.sendDatagram` in production, a fake in tests.
typedef SendDatagram = Future<void> Function(List<int> data);

/// One recipient pair: outbound key + the `kid` stamped on its datagrams.
class PipelinePeer {
  PipelinePeer({
    required this.participantId,
    required this.kid,
    required this.key,
  });

  final int participantId;
  final int kid;
  final PairwiseKey key;
}

class VoicePipeline {
  VoicePipeline({
    required this.sendDatagram,
    required this.datagrams,
    required this.onPlaybackPcm,
    this.plaintext = false,
    RvpOpusEncoder? encoder,
    RvpOpusDecoder? decoder,
    JitterBuffer? jitter,
  }) : _jitter = jitter ?? JitterBuffer() {
    _encoder = encoder;
    _decoder = decoder;
  }

  /// Send seam: `RvpSession.sendDatagram` in production, a fake in tests.
  final SendDatagram sendDatagram;

  /// Incoming datagram stream: `RvpSession.datagrams` in production.
  final Stream<Uint8List> datagrams;

  /// Playback sink: feed device speaker with 960-sample PCM frames.
  final void Function(Int16List pcm) onPlaybackPcm;

  /// Interop/tests only: send plaintext Opus (PT=0, kid=0), no per-peer
  /// copies (plan §3.2). Production always uses pairwise encryption.
  final bool plaintext;

  RvpOpusEncoder? _encoder;
  RvpOpusDecoder? _decoder;
  final JitterBuffer _jitter;

  final Map<int, PipelinePeer> _peers = {};
  final Map<int, PipelinePeer> _byKid = {};

  StreamSubscription<Uint8List>? _recvSub;
  Timer? _ticker;
  Future<void> _sendChain = Future<void>.value();
  Future<void> _recvChain = Future<void>.value();
  bool _running = false;

  int _seq = 0;
  int _timestamp = 0;

  /// Mute: stop sending local frames (remote audio keeps playing).
  bool muted = false;

  /// Whether [start] has been called (recv + playout ticker running).
  bool get running => _running;

  JitterBuffer get jitter => _jitter;

  // --- stats ---
  int framesSent = 0;
  int datagramsReceived = 0;
  int malformedDropped = 0;
  int missingKeyDropped = 0;
  int decryptFailures = 0;

  /// Register the pairwise key for one recipient (`kid` stamped per packet).
  Future<void> addPeerBytes(
    int participantId, {
    required int kid,
    required List<int> key,
  }) async {
    final pairwise = await PairwiseKey.newInstance(key: key);
    addPeer(participantId, kid: kid, key: pairwise);
  }

  /// Register an already-created pairwise key for one recipient.
  ///
  /// Key rotation (plan §5): re-registering the same participant with a new
  /// [kid] keeps the old kid in the byKid index, so in-flight datagrams
  /// sealed with the old key still decrypt during the switch (bounded: kids
  /// are 1..255 per room).
  void addPeer(int participantId, {required int kid, required PairwiseKey key}) {
    final peer = PipelinePeer(
      participantId: participantId,
      kid: kid,
      key: key,
    );
    _peers[participantId] = peer;
    _byKid[kid] = peer;
  }

  /// Drop a participant (e.g. on ParticipantLeft).
  void removePeer(int participantId) {
    final peer = _peers.remove(participantId);
    if (peer != null) _byKid.remove(peer.kid);
  }

  RvpOpusEncoder get encoder => _encoder ??= RvpOpusEncoder.create();
  RvpOpusDecoder get decoder => _decoder ??= RvpOpusDecoder.create();

  /// Encode one 20 ms PCM frame and fan it out (serialized: sequence order
  /// per connection is preserved even though sealing is async).
  Future<void> pushPcm(Int16List frame) {
    final next = _sendChain.then((_) => _encodeAndSend(frame));
    _sendChain = next.then<void>((_) {}, onError: (_) {});
    return next;
  }

  Future<void> _encodeAndSend(Int16List frame) async {
    if (muted) return;
    final packet = encoder.encodeFrame(frame);
    final seq = _seq;
    _seq = (_seq + 1) & 0xffffffff;
    final ts = _timestamp;
    _timestamp = (_timestamp + frame.length) & 0xffffffff;

    if (plaintext) {
      final header = MediaHeader(pt: kPtOpus, kid: 0, sequence: seq, timestamp: ts);
      await sendDatagram(MediaDatagram(header: header, payload: packet).build());
      framesSent++;
      return;
    }

    for (final peer in _peers.values) {
      final sealed = await peer.key.seal(seq: seq, plaintext: packet);
      final header = MediaHeader(
        pt: kPtOpusEnc,
        kid: peer.kid,
        sequence: seq,
        timestamp: ts,
      );
      await sendDatagram(
        MediaDatagram(header: header, payload: sealed).build(),
      );
    }
    framesSent++;
  }

  /// Begin receiving datagrams; run the playout ticker unless [autoTick] is
  /// false (tests drive [tick] manually for determinism).
  void start({bool autoTick = true}) {
    if (_running) return;
    _running = true;
    _recvSub = datagrams.listen(
      _enqueueDatagram,
      onError: (_) {},
      onDone: () {},
    );
    if (autoTick) {
      _ticker = Timer.periodic(
        Duration(milliseconds: _jitter.frameMs),
        (_) => tick(),
      );
    }
  }

  /// Stop receiving/playing (does not dispose codec resources).
  Future<void> stop() async {
    if (!_running) return;
    _running = false;
    _ticker?.cancel();
    _ticker = null;
    await _recvSub?.cancel();
    _recvSub = null;
    await _recvChain;
  }

  /// Release native codec resources (after [stop]).
  void dispose() {
    _encoder?.dispose();
    _decoder?.dispose();
    _encoder = null;
    _decoder = null;
  }

  void _enqueueDatagram(Uint8List bytes) {
    datagramsReceived++;
    _recvChain = _recvChain.then((_) => _processDatagram(bytes));
  }

  Future<void> _processDatagram(Uint8List bytes) async {
    final MediaDatagram dg;
    try {
      dg = MediaDatagram.parse(bytes);
    } catch (_) {
      malformedDropped++;
      return;
    }
    final h = dg.header;
    Uint8List packet;
    if (h.pt == kPtOpusEnc) {
      final peer = _byKid[h.kid];
      if (peer == null) {
        missingKeyDropped++;
        return;
      }
      try {
        packet = await peer.key.open(seq: h.sequence, ciphertext: dg.payload);
      } catch (_) {
        decryptFailures++;
        return;
      }
    } else {
      packet = dg.payload;
    }
    _jitter.push(h.sequence, packet);
  }

  /// One playout slot: decode the due frame, conceal a loss, or play silence
  /// while prebuffering. Called by the internal ticker (or manually in tests).
  void tick() {
    final pop = _jitter.pop();
    final Int16List pcm;
    switch (pop.gap) {
      case null:
        pcm = decoder.decodePacket(pop.frame!.payload);
      case JitterGap.loss:
        pcm = decoder.conceal();
      case JitterGap.prebuffer:
        pcm = Int16List(kOpusFrameSamples);
    }
    onPlaybackPcm(pcm);
  }
}
