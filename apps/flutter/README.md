# rvp_voice — RVP voice demo (P4b)

Flutter demo client for [`rust-quic-rtc`](../../README.md): QUIC SFU voice with
pairwise AES-GCM payload encryption and Opus audio, via the in-house
[`rvp_flutter_quic`](../../plugins/rvp_flutter_quic) plugin.

## Modes

- **Loopback** (default): one device joins the room twice (`alice` + `bob`) —
  mic → alice → SFU → bob → speaker. Full duplex on a single device.
- **Two-device**: this device is `alice` or `bob`; the peer joins from another
  device with the same harness grants.

## Running against the dev harness

```bash
# terminal 1 — server + grants/keys (GET /state on 127.0.0.1:7860)
cargo build -p rvp_flutter_quic --example p4a_harness
RVP_HARNESS_HTTP=127.0.0.1:7860 ./target/debug/examples/p4a_harness alice bob

# terminal 2 — app (emulator uses 127.0.0.1; a real device uses your LAN IP
# in both "Harness state URL" and "Media host")
cd apps/flutter && flutter run
```

The app fetches `GET /state` (`{port, grants, pairs}`) — the stand-in for the
NestJS grants/keys API until P5.

## Tests

```bash
flutter analyze && flutter test   # pure-Dart (no device needed)
```

Microphone permission: Android `RECORD_AUDIO` is declared in the manifest and
requested at runtime; iOS uses `NSMicrophoneUsageDescription`.
