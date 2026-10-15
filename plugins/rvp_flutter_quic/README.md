# rvp_flutter_quic

In-house RVP/1 QUIC transport for Flutter — `flutter_rust_bridge` over [quinn].
Part of the RVP voice stack (see `docs/plan.md`, phase **P4a**).

* **Control:** one reliable bi-directional stream, frames prefixed with a
  `u32` big-endian length.
* **Media:** unreliable QUIC DATAGRAMs, raw payload bytes.
* **TLS:** `connectInsecure` (local testing) or `connectVerified`
  (CA-pinned, ALPN `rvp/1`).

The high-level Dart API is `RvpSession`: it connects, performs the
`Hello → JoinRoom → Welcome` handshake and exposes typed control messages
(`messages`), incoming media (`datagrams`) and `sendDatagram`.

```dart
import 'package:rvp_flutter_quic/rvp_flutter_quic.dart';

await initRustLib(); // resolves the build-hook output, falls back to FRB default

final session = await RvpSession.connect(
  address: '127.0.0.1:7842',
  serverName: 'localhost',
  grantJwt: grant, // JWT grant from rvp-server --jwt-public-key
);

session.messages.listen((msg) => print(msg));
session.datagrams.listen((bytes) => print('${bytes.length} bytes'));
await session.sendDatagram(payload);
await session.close();
```

## Project structure

* `rust/` — Rust crate (quinn client, `flutter_rust_bridge` API in
  `rust/src/api/transport.rs`).
* `hook/` — native-assets build hook (`flutter_rust_bridge_hooks`).
* `lib/` — Dart API (`src/session.dart`, `src/control_codec.dart`) and
  generated bridge code (`lib/src/rust/`).
* `test/` — codec unit tests + host integration test against the Rust
  harness.
* `example/` — minimal Flutter app (Android/iOS).

## Building native code

Flutter invokes `hook/build.dart` during builds; it compiles the Rust crate
with Cargo and bundles the dynamic library as a Dart/Flutter code asset.

> Note: under Flutter/Dart 3.13, `flutter test` cannot open code assets by id
> (`DynamicLibrary.codeAsset` is unsupported by flutter_tester), so
> `initRustLib()` resolves the hook output under `.dart_tool/hooks_runner`
> itself.

## Testing

```bash
# Unit tests only
flutter test

# With the in-process RVP server harness (rooms + SFU fanout)
cargo build -p rvp_flutter_quic --example p4a_harness
RVP_HARNESS=../../target/debug/examples/p4a_harness flutter test
```

The integration test starts the harness, joins two sessions to one room and
checks welcome/roster, SFU datagram fanout (no self-echo) and
join/leave events.

## Regenerating bridge code

After changing Rust APIs in `rust/src/api`, run from the package root:

```bash
flutter_rust_bridge_codegen generate
```
