/// Rust-library initialization with native-assets resolution.
///
/// `flutter test` / `flutter run` build `librvp_flutter_quic` through the build
/// hook in `hook/build.dart`, which registers the artifact as a Dart code
/// asset. Flutter under Dart 3.13 cannot open code assets by id
/// (`DynamicLibrary.codeAsset` is unsupported by flutter_tester), so resolve
/// the hook output path directly and otherwise fall back to FRB's own loader
/// (e.g. bundled apps where the library is loadable by name).
library;

import 'dart:io';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import 'rust/frb_generated.dart';

/// The build-hook artifact names per platform.
List<String> get _libNames => Platform.isWindows
    ? const ['rvp_flutter_quic.dll']
    : Platform.isMacOS
        ? const ['librvp_flutter_quic.dylib']
        : const ['librvp_flutter_quic.so'];

/// Initialize the Rust side of the plugin (call once before first use).
Future<void> initRustLib() async {
  final path = await _findHookBuiltLibrary();
  if (path != null) {
    await RustLib.init(externalLibrary: ExternalLibrary.open(path));
    return;
  }
  await RustLib.init();
}

/// Search ancestor `.dart_tool/hooks_runner` directories for the build-hook
/// output of this package's Rust crate.
Future<String?> _findHookBuiltLibrary() async {
  final names = _libNames;
  Directory dir = Directory.current;
  for (var i = 0; i < 10; i++) {
    final runner = Directory('${dir.path}/.dart_tool/hooks_runner');
    if (runner.existsSync()) {
      await for (final entity in runner.list(recursive: true)) {
        if (entity is! File) continue;
        if (!names.contains(entity.uri.pathSegments.last)) continue;
        if (entity.path.contains('/deps/')) continue;
        return entity.path;
      }
    }
    final parent = dir.parent;
    if (parent.path == dir.path) break;
    dir = parent;
  }
  return null;
}
