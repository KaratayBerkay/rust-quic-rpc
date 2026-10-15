/// RVP QUIC transport plugin — in-house `flutter_rust_bridge` over quinn (P4a).
///
/// Layers:
/// - generated FFI: [RustLib], [RvpConnection], [connectInsecure]/[connectVerified]
/// - [control_codec.dart] — pure-Dart RVP/1 control frames
/// - [RvpSession] — connect + handshake + typed streams
library;

export 'src/control_codec.dart';
export 'src/init.dart';
export 'src/session.dart';
export 'src/rust/api/transport.dart';
export 'src/rust/frb_generated.dart' show RustLib;
