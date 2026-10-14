# Approach B: Rust + QUIC Real-Time Voice Server — Design Plan

Status: **Design-only (Phase 0). No application code until sign-off.**
Research date: 2026-10-14 (against current crates.io/pub.dev/docs.rs).

---

## 1. Library research (verified)

| Layer | Choice | Evidence / risk |
|---|---|---|
| QUIC server | **quinn 0.11.12** + rustls 0.23 + tokio 1.x | Active (updated 2026-09). DATAGRAM API: `Connection::send_datagram(Bytes)`, `read_datagram()`, `max_datagram_size()`, `datagram_send_buffer_space()`. **Must set** `TransportConfig::datagram_receive_buffer_size(Some(n))` or datagrams are disabled. |
| JWT (server) | **jsonwebtoken 10.x**, EdDSA/Ed25519 | NestJS signs; Rust verifies offline with public PEM. No HS256 shared secret on media path. |
| Flutter QUIC | **In-house `rvp_flutter_quic`** (`flutter_rust_bridge` over quinn) | Q1 resolved: third-party `flutter_quic` spike **disabled** — thin datagram surface, single maintainer, last release ~14 mo ago. Pure-Dart `quic_lib` too young. We own datagram APIs, CI, versions. |
| Flutter Opus | **opus_codec_dart** (libopus 1.5.2) + **opus_codec** loader | Or `flutter_recorder` (streams Opus natively, miniaudio + Speex AEC). Decision Q2. |
| Web client | **TanStack (React)** + **WebTransport** (HTTP/3 datagrams) + **WebCodecs** Opus + **WebCrypto** AES-GCM | Browser's QUIC: datagrams = RVP media, bi-stream = RVP control. Server must expose a WebTransport endpoint beside raw-QUIC (verify `wtransport`/quinn+h3 in P6 spike). |
| Server Opus | **Not needed for SFU path** | Server forwards encoded Opus; decode only for recording/mix later (`opus-rs` / `rusty_opus` pure Rust). |
| NestJS | `@nestjs/jwt` or **jose** with EdDSA | Shared public key with Rust via config. |
| Postgres / Redis | sqlx or TypeORM; Redis only for cross-instance events | NestJS owns DB; Rust does not query Postgres on the media path. |

**Explicit non-claims:** QUIC is not an automatic win over WebRTC; no HTTP/3 WebSockets; no custom cryptography (app-layer crypto uses vetted AES-GCM crates, never hand-rolled primitives).

---

## 2. Target architecture

```text
Flutter A ──QUIC/UDP──► ┌─────────────────────────┐
                        │  Rust RVP Media Server   │
Flutter B ──QUIC/UDP──► │  • TLS 1.3 (rustls)     │
                        │  • Auth (JWT verify)     │
                        │  • Rooms / participants  │
                        │  • DATAGRAM SFU forward  │
                        └───────────┬─────────────┘
                                    │ internal events (optional Redis)
                        ┌───────────▼─────────────┐
                        │ NestJS Control Plane     │
                        │  auth, users, rooms,     │
                        │  grants (short-lived JWT)│
                        │  Postgres (+ Redis)      │
                        └─────────────────────────┘
```

Trust boundary: NestJS is **issuer only**. Rust never trusts client-supplied room/participant IDs without a signed grant. NestJS never sees audio packets.

---

## 3. Media protocol design: RVP/1 (Rust Voice Protocol)

Deliverable: `docs/protocol-rvp-v1.md` before any implementation.

### 3.1 Planes

- **Control:** QUIC bidirectional streams. One dedicated control stream per connection. Length-prefixed frames (`u32` BE length + type + body). Hand-written binary encoding for Phase 1–3 (no protobuf dependency); version the envelope for later encoding upgrades.
- **Media:** QUIC DATAGRAMs only. Server never retransmits media.

### 3.2 Media datagram layout (fixed 16-byte header + Opus payload)

```text
0        1        2        3        4        5        6        7
+--------+--------+--------+--------+--------+--------+--------+--------+
| Ver(4) | Type(4)| Flags  | PT     |        Sequence (u32)             |
+--------+--------+--------+--------+-----------------------------------+
|          Timestamp (u32, 48 kHz ticks)      | FrameDurationMs | Rsvd  |
+-----------------------------------------+-------------------+---------+
| Opus payload (≤ max_datagram − 16)                                  |
```

Design decisions to lock in Phase 0:

- **Clock:** client media clock at 48 kHz (matches Opus); server stamps **arrival time** separately on the metrics path only.
- **Max size:** `min(peer max_datagram, 1200)` conservative payload cap to reduce PMTUD edge loss; 20 ms frames leave headroom to ~48–64 kb/s+.
- **PT:** `0 = Opus`, reserved for future codecs; DTX/silence via empty payload or flag.
- **Flags:** lifecycle events (`MUTE`, `START`, `STOP`) live on the **control** stream; per-packet `DTX` flag optional.
- **Dup/reorder:** u32 sequence wrap; receiver drops `seq ≤ last_delivered` (sliding window); gaps trigger PLC.
- **Payload encryption (app-layer E2E, pairwise — ADR-004):** the Opus payload is AES-128-GCM-encrypted **per recipient pair** before send; QUIC TLS 1.3 stays as hop protection. The server forwards ciphertext it cannot read and never holds keys.
  - Header stays plaintext (server needs it for room routing/Seq); `kid` (key id, 1 byte) lives in `Rsvd`; `PT=1` marks Opus-Enc (0 = plaintext Opus, tests/interop only).
  - Nonce = u32 `Seq`; rekey via `kid` long before wrap.
  - **Cost:** publisher emits **N−1 ciphertext datagrams per 20 ms frame** (one per peer). Fine for small rooms; revisit at scale (open decision 7).
  - Breaks future server-side recording/mixing (already out of scope for the media path).

### 3.3 Application-layer behaviors (beyond QUIC transport)

- **Jitter buffer (client):** adaptive 40–80 ms target; ring of slots indexed by seq; late-drop threshold.
- **Playback scheduling:** 20 ms playout ticks; PLC on gaps; comfort noise on DTX.
- **Congestion/adaptation:** QUIC CC + app feedback on control stream (`ReceiverReport`: loss %, inter-arrival jitter, requested bitrate). Sender adapts Opus bitrate; server may send `RateHint`.
- **Late/missing:** late → drop + count; short gap → PLC; long gap → buffer reset + control `Resync`.
- **Lifecycle control messages:** `Hello`, `JoinRoom`, `Leave`, `Mute/Unmute`, `ParticipantJoined/Left`, `AudioStart/Stop`, `Ping/Pong`, `ReceiverReport`, `RateHint`, `KeyUpdate` (pairwise `kid` rollover), `Error`.

### 3.4 Auth handshake on QUIC

1. TLS 1.3 to media endpoint (server cert; public CA in prod, pinning option for Phase 1).
2. Client opens control bi-stream immediately after connect.
3. `Hello { protocol_version, grant_jwt }` → server validates JWT (`iss`, `aud=rvp`, `exp`, `room_id`, `user_id`, optional `jti`), applies rate limits, assigns `participant_id`.
4. `JoinRoom` → grant + capacity check → `Welcome { participant_id, roster, max_datagram, stream_id }`.
5. Only after `Welcome` are DATAGRAMs accepted from that connection.

---

## 4. Rust server design

Planned workspace layout (names only until Phase 1):

```text
crates/
  rvp-proto/     # packet + control message encode/decode, zero-copy parsers
  rvp-auth/      # JWT verify, claims, rate-limit policy
  rvp-crypto/    # pairwise AES-GCM seal/open, kid lookup (clients + loadgen ONLY)
  rvp-server/    # endpoint, connection task, room registry, SFU forwarder
  rvp-cli/       # binary: config load, run server
  rvp-loadgen/   # Phase 6 load tool (pure-Rust quinn client)
apps/
  web/           # TanStack (React) phone-call web client (P6)
  flutter/       # Flutter mobile client (P4) — location TBD at Phase 1
```

### Hot-path principles

- Per-connection tokio task reads DATAGRAMs → parse header (no alloc; `Bytes` slice) → room lookup → fanout.
- Room state: `Arc<Room>` + `DashMap` or sharded `RwLock<HashMap<ParticipantId, ParticipantHandle>>`; handles hold `quinn::Connection`.
- Fanout: `send_datagram(bytes.clone())` — `Bytes` refcount, not buffer copy. Quinn's bounded send buffer drops oldest on overflow (correct for voice).
- Bounded ingress: per-participant pps + bps token buckets; drop + metric on exceed.
- No Opus decode/re-encode on the media path.
- No seal/open on the server hot path: `rvp-crypto` is **not** linked into `rvp-server`; ciphertext is forwarded opaquely.
- Observability: `tracing` + `metrics`/Prometheus; counters sampled; never log tokens.

### Failure handling

- Connection lost → remove participant, broadcast `ParticipantLeft`, GC empty rooms.
- Reconnect = new QUIC connection + fresh grant (grant TTL 60–120 s; app re-fetches).
- Transport: `keep_alive_interval` + `max_idle_timeout` configured.

---

## 5. NestJS design

- `POST /auth/login` → long-lived API session JWT (separate from media grant).
- `POST /rooms`, `GET /rooms/:id`, membership tables in Postgres.
- `POST /rooms/:id/grants` → media grant JWT:
  - Claims: `iss=nest`, `aud=rvp`, `sub=user_id`, `room_id`, `role`, `exp ≤ 2 min`, `jti`, optional `media_node`.
- Revocation: short TTL is primary; optional Redis `jti` denylist for force-kick (Rust checks at Join).
- Rust gets the EdDSA public key via config file or boot-time fetch from `/auth/jwks`; support rotation.
- **Pairwise media keys (ADR-004):** NestJS generates one AES-128 key per participant pair per room, delivers it to **both** members over the authenticated REST/session channel (never via the media server). Rotation every N minutes → new `kid`; clients announce `KeyUpdate{kid}` on their control streams. Rust never receives keys. Missing key for a peer blocks audio with that peer (not the whole `JoinRoom`).

---

## 6. Flutter client design

```text
ui → CallController (Dart)
        ├─ RestApiClient → NestJS (login, grant)
        ├─ QuicTransport → (platform bridge) → quinn
        ├─ Capture (mic, 48 kHz/16 kHz mono PCM)
        ├─ OpusEncoder (20 ms frames)
        ├─ PairwiseCrypto (AES-GCM seal/open per peer; kid from NestJS)
        ├─ RvpPacketizer → DATAGRAM send (N−1 ciphertexts per frame)
        ├─ JitterBuffer ← DATAGRAM recv
        └─ OpusDecoder → Playback
```

**Transport strategies (Q1 — resolved):**

- **A (chosen):** in-house `rvp_flutter_quic` plugin — `flutter_rust_bridge` over quinn; we control datagram APIs, CI, versions.
- **B (disabled):** third-party `flutter_quic` spike **cancelled for now**. Revisit only if the in-house plugin slips badly, and only after a fresh datagram/isolate spike.

**Audio strategies (Q2):**

- **A:** `flutter_recorder` streaming Opus (less Dart code, Android AEC).
- **B (recommended):** PCM stream + `opus_codec_dart` encoder — classic WebRTC-style pipeline, better testability.
- Platform AEC/VoIP processing: iOS VoiceProcessingIO; Android AAudio path; document Bluetooth SCO limits; background calling is OS-limited.

---

## 7. TanStack web client design (phone-call web app)

Second real client: a browser **phone-call app** (login, room list, roster, mute, in-call controls) so demos and QA don't need a device. Shares the RVP protocol, pairwise crypto, and NestJS API with Flutter.

**Stack (TanStack family):**

- **TanStack Start** (or Vite SPA — open decision 9) + React + TypeScript
- **TanStack Router** — type-safe routes: `/login`, `/rooms`, `/rooms/:id` (in-call)
- **TanStack Query** — all NestJS REST (login, rooms, grants, pairwise keys) with retry/reconnect policy
- UI kit in the Radix/shadcn style — owned by the UI Designer agent (§7.1)

**Media path (browser):**

- **WebTransport over HTTP/3** — the browser's QUIC: unreliable DATAGRAMs carry RVP media; one bidirectional stream carries the RVP control protocol. Framing mirrors `rvp-proto` (TS port, or WASM bindings later).
- **Server change:** `rvp-server` exposes a WebTransport (HTTP/3) endpoint **alongside** the raw-QUIC endpoint (`wtransport` or quinn + h3) — same room registry, same JWT checks, same fanout, same datagram forward path.
- **Opus via WebCodecs** (`AudioEncoder`/`AudioDecoder`) — 20 ms frames; **AudioWorklet** captures/plays 48 kHz mono PCM.
- **Pairwise crypto via WebCrypto** AES-GCM — same NestJS-delivered per-pair keys and `kid` rotation as Flutter (ADR-004). Pairwise cost applies too: N−1 datagrams per 20 ms frame per publisher.
- **Fallback (open decision 8):** WebSocket + same RVP frames if WebTransport is unavailable — degraded (no QUIC CC), not v1-blocking.

```text
ui → CallController (React hooks)
        ├─ TanStack Query → NestJS REST (login, rooms, grant, pairwise keys)
        ├─ WebTransport (HTTP/3) → RVP control bi-stream + media DATAGRAMs
        ├─ AudioWorklet (mic capture, 48 kHz PCM)
        ├─ WebCodecs AudioEncoder (Opus, 20 ms)
        ├─ WebCrypto AES-GCM seal per peer (kid)
        ├─ RvpPacketizer → DATAGRAM send (N−1 ciphertexts)
        ├─ JitterBuffer ← DATAGRAM recv (seq window, 40–80 ms)
        └─ WebCodecs AudioDecoder → AudioWorklet playback
```

**Why it matters:** fastest end-to-end demo of the whole stack (P6), manual QA of pairwise crypto against Flutter/Rust peers, and a second RVP implementation that pressure-tests the protocol spec before P7 hardening.

### 7.1 Agency skills to activate ([agency-agents](https://github.com/msitarzewski/agency-agents))

Install subset (OpenCode caps registered agents): `./scripts/install.sh --tool opencode --agent <slug>,<slug>`

**Core:**

- `frontend-developer` — React/TanStack UI implementation, rendering performance
- `realtime-collaboration-engineer` — presence, join/leave, live mute/hold state
- `ui-designer` — call-control design system (mute button, roster, room list)
- `ux-architect` — CSS/styling architecture for a component-heavy call UI
- `test-automation-engineer` — Playwright E2E for join/mute/leave flows

**Supporting:**

- `rapid-prototyper` — fast MVP screens before hardening
- `voice-ai-integration-engineer` — voice/audio domain (preprocessing, later transcripts)
- `video-streaming-engineer` — media-plane/codec/latency patterns
- `webassembly-engineer` — optional `rvp-proto` → WASM bindings for the TS client
- `section-508-specialist` + `accessibility-auditor` — accessible call controls (ARIA live mute/join)
- `i18n-engineer` — localized call UI, duration formatting, RTL
- `ui-finish-gate-reviewer` + `persona-walkthrough` — pre-ship polish/friction gates

Note: the roster has **no TanStack-specific agent** — `frontend-developer` covers it.

---

## 8. Security model

| Property | Source |
|---|---|
| Confidentiality/integrity (media + control) | QUIC TLS 1.3 (AES-GCM / ChaCha20-Poly1305) |
| Confidentiality **from the media server** | App-layer pairwise AES-128-GCM (ADR-004); server holds no media keys |
| Identity | NestJS-issued EdDSA JWT, short TTL |
| Room authz | Grant claims + server-side capacity/membership checks |
| Replay | `exp` + optional `jti` denylist; media-seq anti-replay not needed for live audio |
| DoS | QUIC retry tokens, per-IP/per-user connection caps, pps/bps limits, room caps, datagram size caps, malformed-header drops |
| Secrets | Cert/key via env/files, never in git; no token logging |

---

## 9. Performance & benchmarking plan (Phase 7, designed now)

- Metrics: CPU/participant, RSS/connection, pps, e2e latency (encode→playout), loss/jitter, bandwidth, p50/p95/p99.
- Loadgen: pure-Rust quinn client simulating N participants × rooms; tc/netem for impairment scenarios.
- Methodology: fixed hardware, warm-up period, identical Opus bitrate/room sizes; A/B vs **LiveKit** on same box.
- Loadgen must model pairwise ciphertext fanout (publisher uplink ×(N−1)); report latency/CPU with and without app-layer crypto.
- Rule: no performance claims in docs without benchmark artifacts under `benchmarks/`.

---

## 10. Workflow (design-first phases)

### Phase 0 — Design only (current)

Deliverables (docs + decisions, no production code):

1. `docs/architecture.md` — diagram + trust boundaries
2. `docs/protocol-rvp-v1.md` — full wire format + state machines
3. `docs/auth.md` — JWT schema, examples, rotation, pairwise media-key issuance
4. `docs/client-flutter.md` — transport/audio decision + isolate model
5. `docs/client-web.md` — TanStack stack, WebTransport media path, WebSocket fallback
6. `docs/security-threat-model.md`
7. `docs/benchmark-plan.md`
8. ADRs: ADR-001 transport binding, ADR-002 control encoding, ADR-003 fanout model, ADR-004 pairwise app-layer payload encryption
9. Repo skeleton tree (directories + planned crate/package names only)

### Phase 1+ — Implementation (after Phase 0 sign-off)

1. **P1** Rust QUIC echo + DATAGRAM loopback (Rust test client)
2. **P2** JWT auth + RVP control handshake + packet format tests + pairwise key handoff + AES-GCM test vectors
3. **P3** Rooms + SFU forward (two+ Rust clients talking; server forwards ciphertext opaquely)
4. **P4** In-house Flutter plugin (`rvp_flutter_quic`) → capture/Opus/crypto/jitter/playback (no `flutter_quic` spike)
5. **P5** NestJS rooms/grants wired
6. **P6** TanStack web client — WebTransport spike (HTTP/3 endpoint crate) → AudioWorklet/WebCodecs/WebCrypto pipeline + Playwright E2E
7. **P7** Hardening, metrics, loadgen, LiveKit comparison (includes web client)
8. **P8** Multi-instance routing (Redis room directory)

**Post-v1 (deferred — in plan, not scheduled until v1 ships; see §12):**

9. **P9** Connectivity fallback — TURN-like QUIC/WS relay for UDP-blocked networks
10. **P10** Audio quality parity — active speaker detection, Opus RED, richer ReceiverReport
11. **P11** RVP/2 video + screen share (separate design doc before implementation)
12. **P12** Recording/Egress — requires group-key mode decision (conflicts with pairwise E2E)

Exit criteria per phase: builds green, integration test list, runnable demo command, features marked implemented vs planned.

---

## 11. Open decisions

1. ~~**Flutter QUIC:** in-house FFI plugin vs spike `flutter_quic` first?~~ **Resolved:** in-house `rvp_flutter_quic`; `flutter_quic` spike disabled.
2. **Flutter audio:** PCM + `opus_codec_dart` (recommended) vs `flutter_recorder` Opus stream?
3. **Control encoding:** hand-rolled binary (recommended for P1–3) vs protobuf from day one?
4. **Target platforms P4:** Android + iOS only, or + desktop for faster CI?
5. **Media grant TTL:** 60 s vs 120 s; `jti` denylist yes/no for v1?
6. **Phase 0 output:** markdown docs only, or also mermaid sequence diagrams?
7. **Pairwise crypto scale limit:** at what room size (N) do we drop pairwise E2E, switch to a group key, or accept publisher uplink ×(N−1)?
8. **Web media transport:** WebTransport/HTTP3 datagrams (recommended, real QUIC semantics) vs WebSocket binary fallback for v1?
9. **Web app shape:** TanStack Start (SSR) vs pure Vite SPA (simpler deploy, call app is client-heavy anyway)?
10. ~~**Scope freeze:** commit to audio-only v1 or schedule gap phases P9–P11 now?~~ **Resolved:** audio-only v1 (P1–P8); connectivity fallback, speaker detection, video, and recording are **in the plan as deferred post-v1 phases P9–P12** (§10) — start only after v1 ships and product asks for them.

---

## 12. LiveKit comparison & gap roadmap

LiveKit = mature open-source **WebRTC** SFU (Go/Pion) with a large SDK/ecosystem. Honest framing: **QUIC is not an automatic win over WebRTC** (§1). RVP's differentiators are deliberate and narrow: Rust media plane, QUIC 1-RTT + native datagrams, **pairwise app-layer E2E by default** (LiveKit's E2EE is optional/off), and tight NestJS control-plane integration. Everything below is the price of that choice.

### 12.1 Feature matrix (audio product scope)

| Capability | LiveKit | RVP (this plan) | Status |
|---|---|---|---|
| Audio rooms / SFU forward | ✅ WebRTC | ✅ QUIC DATAGRAM (P3) | **Have (planned)** |
| Opus 20 ms real-time | ✅ | ✅ | Have (planned) |
| JWT auth | ✅ API key/secret tokens | ✅ EdDSA grants from NestJS | Have (planned) |
| E2E payload encryption | ⚠️ optional frame E2EE | ✅ pairwise AES-GCM by default (ADR-004) | **RVP advantage** |
| Multi-instance / multi-region | ✅ distributed, K8s, Redis | ⚠️ P8 (single-region Redis directory) | Partial |
| Recording (Egress) | ✅ composites + per-track | ❌ none (pairwise E2E makes server recording impossible) | **Gap → P12** |
| Ingress (RTMP/WHIP/HLS) | ✅ | ❌ | Out of scope |
| Active speaker detection | ✅ server-side | ❌ | **Gap → P10** |
| RED (redundant audio encoding) | ✅ | ❌ (plain Opus + PLC) | Gap (P10) |
| Noise cancellation | ✅ WebRTC built-in; Krisp/BVC on cloud | ⚠️ platform AEC only (iOS VOIP/AudioUnit, Android AAudio) | Partial |
| DTX / comfort noise | ✅ | ✅ flag (§3.2) | Have (planned) |
| Data channels / byte streams | ✅ data tracks, byte streams | ⚠️ control stream only (binary frames) | Partial (P3+) |
| Webhooks / room events API | ✅ | ⚠️ Redis events → NestJS (P8) | Partial |
| Moderation APIs | ✅ | ⚠️ Mute/Leave + kick via jti denylist | Partial |
| Prometheus / observability | ✅ | ✅ tracing + metrics (P7) | Have (planned) |
| TURN / TCP-TLS fallback for blocked networks | ✅ ICE + TURN UDP/TCP/TLS | ❌ QUIC/UDP only; web WS fallback is decision 8 | **Gap → P9** |
| Client SDK ecosystem | ✅ JS, Flutter, RN, Rust, C++, Unity, Swift/Kotlin | ⚠️ own Flutter plugin + TanStack web + Rust test clients | Gap (by design) |
| AI Agents framework | ✅ | ❌ | Out of scope |
| Video (simulcast, SVC VP9/AV1, dynacast) | ✅ | ❌ audio-only | **Gap → P11 (RVP/2)** |
| Screen share | ✅ | ❌ | Gap (P11) |
| Production maturity | ✅ years, cloud offering | ❌ Phase 0 | Gap (earned over P6–P8) |

### 12.2 Gap phases (deferred to post-v1 — added to §10 as P9–P12)

- **P9 — Connectivity fallback:** a TURN-like relay (QUIC relay or WS media relay) for UDP-blocked/CGNAT networks; without it some mobile networks simply won't connect. Highest product risk of all gaps.
- **P10 — Audio quality parity:** active speaker detection (server measures arrival energy or clients send level in `Rsvd`/flags), Opus RED, richer `ReceiverReport`.
- **P11 — RVP/2 video (only if product demands it):** new packet types for keyframe/simulcast layers, client-side encode; server remains forwarder (no transcode). Large effort — treat as a separate design doc, not an extension.
- **P12 — Recording:** requires a **group-key mode** (server can decrypt) or client-side upload; conflicts with pairwise E2E by design — decide explicitly if ever needed.

### 12.3 Build-vs-buy note

If the product goal is "standard video conferencing tomorrow", **LiveKit is the correct buy** and this plan is a research/custom-stack track. RVP is justified when QUIC-native transport, Rust ownership of the media path, default E2EE, or protocol-level control outweigh ecosystem maturity. P7's LiveKit A/B benchmark is the evidence gate for that claim.

---

## 13. Development workflow (production-grade)

Repo: `https://github.com/KaratayBerkay/rust-quic-rpc` · Branch: `main` · Hooks: husky · CI: GitHub Actions.

- **One PR per phase.** Branch naming: `phase/P<N>-<slug>` (e.g. `phase/P0-design`, `phase/P1-quic-echo`, `phase/P2-auth`). `main` is protected by convention: no direct pushes of phase work.
- **PR gate:** CI green (docs lint + commitlint now; per-phase jobs activate when code lands: `crates/**` → Rust fmt/clippy/test, `apps/flutter` → analyze/test, `apps/web` → lint/build, `services/nest` → lint/test/build), human review, squash-merge with conventional title.
- **Husky (local):** `pre-commit` = markdownlint on docs; `commit-msg` = commitlint (`config-conventional`). Bypass only via explicit `--no-verify` with review note.
- **Commit/PR title convention:** `type(scope): summary` — scopes: `docs`, `proto`, `auth`, `server`, `flutter`, `web`, `nest`, `ci`.

### 13.1 GitHub/CI agency skills ([agency-agents](https://github.com/msitarzewski/agency-agents))

Curated roster for the delivery pipeline (`.github/workflows/ci.yml`, PR gates, releases). Installed under `.opencode/agents/`.

**Core — CI/GitHub (installed):**

- `devops-automator` — owns GitHub Actions workflows (docs lint, fmt/clippy/test, phase gates)
- `git-workflow-master` — conventional commits, `phase/P<N>` branch strategy, history hygiene
- `code-reviewer` — PR review gates and merge criteria
- `test-automation-engineer` — deterministic CI jobs, flake elimination, parallelization
- `test-results-analyzer` — cargo/clippy/job-log signals into actionable output
- `reality-checker` — evidence-based release certification before production
- `senior-secops` — per-PR secret scanning, secure-by-default checks
- `application-security-engineer` — SAST/DAST (CodeQL-class) jobs as code lands
- `platform-engineer` — monorepo golden paths so every crate/app follows one paved road
- `sre-site-reliability-engineer` — SLOs/observability for the media server (P7+)

**Supporting (installed, adjacent duties):**

- `performance-benchmarker` — P7 LiveKit A/B regression gates
- `rust-refactoring-specialist` — behavior-safe refactors under green CI
- `api-tester` — NestJS/QUIC endpoint integration jobs (P5+)
- `technical-writer` — docs quality behind the docs-lint job

**Not installed — activate if needed:**

- `minimal-change-engineer` — keep phase PRs scoped, no creep
- `developer-tooling-engineer` — local↔CI parity scripts
- `mobile-release-engineer` — Flutter signing/staged rollout (P4+)
- `secrets-credential-engineer` — Actions secrets/token hygiene
- `finops-engineer` — runner/matrix cost control

Install subset: `./scripts/install.sh --tool opencode --agent <slug>,...`

Note: the roster has no GitHub-Actions/Docker/SBOM specialist — those duties are covered by `devops-automator` + `application-security-engineer`.
