/// High-level RVP session: transport + control handshake + typed streams.
///
/// [RvpSession.connect] opens the QUIC connection, performs the RVP/1
/// handshake (`Hello` → `JoinRoom` → `Welcome`) and exposes:
/// - [messages] — every control message including the [WelcomeMessage]
///   (buffered until you listen; single subscriber),
/// - [datagrams] — every incoming media DATAGRAM (single subscriber),
/// - [sendDatagram] — outbound media.
///
/// Requires [RustLib.init] to have completed first.
library;

import 'dart:async';
import 'dart:typed_data';

import 'control_codec.dart';
import 'rust/api/transport.dart';

/// Server rejected the handshake before `Welcome`.
class RvpControlException implements Exception {
  RvpControlException({required this.code, required this.message});
  final int code;
  final String message;
  @override
  String toString() => 'RvpControlException($code): $message';
}

/// An open, handshaken RVP connection to one room.
class RvpSession {
  RvpSession._({
    required this._conn,
    required this._controller,
    required this.welcome,
    required this._controlSub,
  });

  final RvpConnection _conn;
  final StreamController<ControlMessage> _controller;
  final StreamSubscription<Uint8List> _controlSub;
  Stream<Uint8List>? _datagrams;

  /// The `Welcome` reply: our participant id, limits, initial roster.
  final WelcomeMessage welcome;

  /// Control messages (the first event is the [WelcomeMessage]).
  /// Completes when the session is closed.
  Stream<ControlMessage> get messages => _controller.stream;

  /// Incoming media DATAGRAMs (raw bytes, media header included).
  /// Single stream, shared by every listener.
  Stream<Uint8List> get datagrams => _datagrams ??= _conn.startDatagramReader();

  /// Connect + handshake. `caPem == null` skips certificate verification
  /// (local testing only).
  static Future<RvpSession> connect({
    required String address,
    required String serverName,
    required String grantJwt,
    String? caPem,
    Duration handshakeTimeout = const Duration(seconds: 5),
  }) async {
    final conn = caPem == null
        ? await connectInsecure(addr: address, serverName: serverName)
        : await connectVerified(
            addr: address,
            serverName: serverName,
            caPem: caPem,
          );
    try {
      await conn.openControl();
    } catch (e) {
      conn.close(code: 0, reason: 'control open failed');
      rethrow;
    }

    // Single-subscriber, buffered until the app listens.
    final controller = StreamController<ControlMessage>();
    final welcomeCompleter = Completer<WelcomeMessage>();
    final controlSub = conn.startControlReader().listen(
      (frame) {
        final ControlMessage msg;
        try {
          msg = decodeControlFrame(frame);
        } catch (e) {
          if (!welcomeCompleter.isCompleted) {
            welcomeCompleter.completeError(e);
          }
          controller.addError(e);
          return;
        }
        if (!welcomeCompleter.isCompleted) {
          switch (msg) {
            case WelcomeMessage():
              welcomeCompleter.complete(msg);
            case ControlErrorMessage(:final code, :final message):
              welcomeCompleter.completeError(
                RvpControlException(code: code, message: message),
              );
            default:
              break;
          }
        }
        controller.add(msg);
      },
      onError: (Object e) {
        if (!welcomeCompleter.isCompleted) welcomeCompleter.completeError(e);
        controller.addError(e);
      },
      onDone: () {
        if (!welcomeCompleter.isCompleted) {
          welcomeCompleter.completeError(
            RvpControlException(code: 0, message: 'connection closed'),
          );
        }
        controller.close();
      },
    );

    try {
      await conn.writeControlFrame(frame: encodeHello(grantJwt));
      await conn.writeControlFrame(frame: encodeJoinRoom());
      final welcome = await welcomeCompleter.future.timeout(handshakeTimeout);
      return RvpSession._(
        conn: conn,
        controller: controller,
        welcome: welcome,
        controlSub: controlSub,
      );
    } catch (_) {
      conn.close(code: 0, reason: 'handshake failed');
      await _cancelQuietly(controlSub);
      final closing = controller.close();
      if (controller.hasListener) await closing;
      rethrow;
    }
  }

  /// Send one media DATAGRAM (raw bytes, media header included).
  Future<void> sendDatagram(List<int> data) =>
      _conn.sendDatagram(data: data);

  /// Announce a pairwise-key rotation on the control stream (plan §5): the
  /// server relays `KeyUpdate` to every other member, who then pull fresh
  /// keys from the control plane and swap their pipeline peers.
  Future<void> sendKeyUpdate(int kid) =>
      _conn.writeControlFrame(frame: encodeKeyUpdate(kid));

  /// Close the connection (both streams complete afterwards).
  Future<void> close({int code = 0, String reason = 'bye'}) async {
    _conn.close(code: code, reason: reason);
    await _cancelQuietly(_controlSub);
    await _closeController();
  }

  /// Detach the internal control listener.
  ///
  /// Called only after the connection is closed: the Rust reader then errors
  /// out, drops its sink and the stream completes, so [StreamSubscription
  /// .cancel] returns promptly. A timeout guards against a stuck reader — the
  /// subscription is then abandoned and cleaned up by garbage collection.
  static Future<void> _cancelQuietly(
    StreamSubscription<Uint8List> sub,
  ) async {
    try {
      await sub.cancel().timeout(const Duration(seconds: 5));
    } on TimeoutException {
      // Abandon: the connection is already closed.
    }
  }

  /// Close the messages stream.
  ///
  /// A single-subscription controller only completes [StreamController.close]
  /// once a listener processed the done event — so if nobody ever listened,
  /// do not await it (the done event stays buffered for a late subscriber).
  Future<void> _closeController() async {
    final closing = _controller.close();
    if (_controller.hasListener) await closing;
  }
}
