# rust-quic-rtc — RVP (Rust Voice Protocol)

Rust + QUIC real-time **voice** server: a DATAGRAM SFU with pairwise end-to-end-encrypted Opus, EdDSA JWT auth via a NestJS control plane, and Flutter + TanStack web clients.

> **Status: P1–P8 + P9a shipped** — Rust QUIC SFU (rooms, opaque fanout, JWT auth) + Flutter plugin with full voice pipeline and demo (`apps/flutter`) + NestJS control plane (`services/nest`: login, rooms, media grants, JWKS on Postgres) + WebTransport endpoint and Vite web client (`apps/web`: control handshake, pairwise AES-GCM, AudioWorklet/WebCodecs Opus, Playwright E2E) + Prometheus observability (`crates/rvp-metrics`, `rvp-cli --metrics-listen`) + server hardening (QUIC Retry, per-IP/per-user connection caps, pps/bps + datagram-size limits in `crates/rvp-server/src/limits.rs`) + `KeyUpdate{kid}` relay, pair-key rotation (host rotate → announce → members pull) and grant revocation (host kick → `grant_issues` → `rvp-cli --nest-revoked` → denylist + live kick) + loadgen + LiveKit A/B harness (`crates/rvp-loadgen`, `benchmarks/`) + **P8a media-node pinning** (`media_node` grant claim, `--node-id`, Nest round-robin pin from `MEDIA_NODES`/`RVP_MEDIA_NODES_URL`, `WRONG_NODE` 521) + **P8b multi-instance Redis** (per-node directory `rvp:node:<id>` + shared revocation pubsub via optional `REDIS_URL`/`--redis-url`, `crates/rvp-directory`) + **P9a WebSocket fallback** for UDP-blocked networks (`rvp-cli --ws-listen`, tagged-framing `rvp-server/src/ws.rs`, web WT→WS auto-fallback in `apps/web`) + **Flutter Nest mode** (`apps/flutter` speaks the §5 REST flow — login/rooms/grants/keys — so a Flutter device joins the same room with the same pairwise keys as the web client). Design plan: [`docs/plan.md`](docs/plan.md).

## Why not just LiveKit?

Documented honestly in [`docs/plan.md` §12](docs/plan.md): LiveKit is the right buy for standard conferencing. RVP exists for QUIC-native transport, Rust ownership of the media path, **default** pairwise E2E encryption, and protocol-level control. P7's A/B benchmark is the evidence gate.

## Roadmap

| Phase | Scope |
|---|---|
| P0 | Design docs + ADRs ✅ |
| P1–P3 | Rust QUIC server: echo → auth/crypto → SFU rooms ✅ |
| P4 | Flutter plugin + audio pipeline + voice demo ✅ — demo has Nest (§5, cross-device) + harness modes ✅ |
| P5 | NestJS control plane (login/rooms/grants/JWKS on Postgres) ✅ |
| P6 | Web client — **P6a** WebTransport endpoint + Vite shell ✅; **P6b** audio pipeline + Playwright E2E ✅ |
| P7 | Hardening — **P7a** Prometheus metrics ✅; **P7b** server hardening ✅; **P7c** KeyUpdate relay + revocation ✅ (PR1) + pair-key rotation with client announce/pull ✅ (PR2); **P7d** loadgen + LiveKit A/B harness ✅ |
| P8 | Multi-instance — **P8a** media-node pinning ✅; **P8b** room directory + shared denylist ✅ |
| P9 | Connectivity fallback — **P9a** WS relay (server + web) ✅; **P9b** Flutter WS fallback / TURN deferred |
| P10–P12 | Deferred post-v1: speaker detection, video, recording |

## Development workflow

- **One PR per phase** on branch `phase/P<N>-<slug>` (e.g. `phase/P1-quic-echo`) — never push code straight to `main`.
- **Husky hooks:** `pre-commit` runs markdownlint; `commit-msg` enforces [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `docs:`, `fix:`, `test:`, `ci:` …).
- **GitHub Actions CI** (`.github/workflows/ci.yml`): docs lint + commitlint on every PR; Rust/Flutter/web/Nest jobs activate automatically when their directories appear.
- **Deployment:** single-box runbook (Postgres + Nest + `rvp-cli` + Nginx Proxy Manager + clients) in [`docs/deploy.md`](docs/deploy.md).

```bash
npm install        # installs tooling + husky hooks
npm run lint:md    # docs lint
```

## License

**PolyForm Noncommercial 1.0.0** — see [`LICENSE`](LICENSE). Personal/hobby/research and noncommercial-organization use only; no commercial use without a separate agreement.
