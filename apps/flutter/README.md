# rvp_voice — RVP voice demo (P4b / Flutter Nest mode)

Flutter demo client for [`rust-quic-rtc`](../../README.md): QUIC SFU voice with
pairwise AES-GCM payload encryption and Opus audio, via the in-house
[`rvp_flutter_quic`](../../plugins/rvp_flutter_quic) plugin.

## Modes

### Nest (plan §5) — cross-device calls with the web app

The app speaks the real NestJS control plane (same room, same pairwise keys as
`apps/web`), so a Flutter device and a browser can join the **same room** and
hear each other.

```bash
# terminal 1 — Postgres + Nest (auto-migrates at boot)
docker compose up -d postgres && npm --prefix services/nest run start:dev

# terminal 2 — media server (JWKS from Nest; QUIC on UDP 4433)
cargo run -p rvp-cli -- --listen 0.0.0.0:4433 \
  --nest-jwks http://127.0.0.1:3000/auth/jwks

# terminal 3 — Flutter app
cd apps/flutter && flutter run
```

In the app, pick **Nest (plan §5)** and set:

- **Nest URL** — control plane base (e.g. `http://<server>:3000`; your LAN IP
  for a physical device).
- **User ID** — the id you will log in as (must match the membership the peer
  invites, e.g. `phone1`).
- **Room id** — either tap **Create room** (you become host) or paste the id
  shown by the peer and tap **Add member** to invite your user.
- **Media address (QUIC)** — the `rvp-cli --listen` address (`host:port`,
  e.g. `1.2.3.4:4433`). Flutter is QUIC-only (no WS fallback yet — P9b); the
  grant's advertised `mediaAddress` may point at the WT port, so enter it
  manually.

Order of operations for a phone ↔ browser call:

1. Browser (or phone) **Create room** → note the room id.
2. Host taps **Add member** for the other user's id.
3. Both sides **Connect**.

Key rotation: the host's rotate button calls `POST /rooms/:id/keys/rotate`,
applies the fresh keys, and announces `KeyUpdate{kid}`; members auto-pull on
the announce and on `participantJoined`.

### Harness (dev stand-in) — no Nest/Postgres needed

- **Loopback** (default): one device joins the room twice (`alice` + `bob`) —
  mic → alice → SFU → bob → speaker. Full duplex on a single device.
- **Two-device**: this device is `alice` or `bob`; the peer joins from another
  device with the same harness grants.

```bash
# terminal 1 — server + grants/keys (GET /state on 127.0.0.1:7860)
cargo build -p rvp_flutter_quic --example p4a_harness
RVP_HARNESS_HTTP=127.0.0.1:7860 ./target/debug/examples/p4a_harness alice bob

# terminal 2 — app (emulator uses 127.0.0.1; a real device uses your LAN IP
# in both "Harness state URL" and "Media host")
cd apps/flutter && flutter run
```

The app fetches `GET /state` (`{port, grants, pairs}`) — a stand-in for the
NestJS grants/keys API for offline dev.

## Tests

```bash
flutter analyze && flutter test   # pure-Dart (no device needed)
```

Microphone permission: Android `RECORD_AUDIO` is declared in the manifest and
requested at runtime; iOS uses `NSMicrophoneUsageDescription`.
