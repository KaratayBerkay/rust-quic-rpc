# rust-quic-rtc — RVP (Rust Voice Protocol)

Rust + QUIC real-time **voice** server: a DATAGRAM SFU with pairwise end-to-end-encrypted Opus, EdDSA JWT auth via a NestJS control plane, and Flutter + TanStack web clients.

> **Status: P1–P6 shipped** — Rust QUIC SFU (rooms, opaque fanout, JWT auth) + Flutter plugin with full voice pipeline and demo (`apps/flutter`) + NestJS control plane (`services/nest`: login, rooms, media grants, JWKS on Postgres) + WebTransport endpoint and Vite web client (`apps/web`: control handshake, pairwise AES-GCM, AudioWorklet/WebCodecs Opus, Playwright E2E). Design plan: [`docs/plan.md`](docs/plan.md).

## Why not just LiveKit?

Documented honestly in [`docs/plan.md` §12](docs/plan.md): LiveKit is the right buy for standard conferencing. RVP exists for QUIC-native transport, Rust ownership of the media path, **default** pairwise E2E encryption, and protocol-level control. P7's A/B benchmark is the evidence gate.

## Roadmap

| Phase | Scope |
|---|---|
| P0 | Design docs + ADRs ✅ |
| P1–P3 | Rust QUIC server: echo → auth/crypto → SFU rooms ✅ |
| P4 | Flutter plugin + audio pipeline + voice demo ✅ |
| P5 | NestJS control plane (login/rooms/grants/JWKS on Postgres) ✅ |
| P6 | Web client — **P6a** WebTransport endpoint + Vite shell ✅; **P6b** audio pipeline + Playwright E2E ✅ |
| P7–P8 | Hardening, loadgen vs LiveKit, multi-instance |
| P9–P12 | Deferred post-v1: TURN fallback, speaker detection, video, recording |

## Development workflow

- **One PR per phase** on branch `phase/P<N>-<slug>` (e.g. `phase/P1-quic-echo`) — never push code straight to `main`.
- **Husky hooks:** `pre-commit` runs markdownlint; `commit-msg` enforces [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `docs:`, `fix:`, `test:`, `ci:` …).
- **GitHub Actions CI** (`.github/workflows/ci.yml`): docs lint + commitlint on every PR; Rust/Flutter/web/Nest jobs activate automatically when their directories appear.

```bash
npm install        # installs tooling + husky hooks
npm run lint:md    # docs lint
```

## License

**PolyForm Noncommercial 1.0.0** — see [`LICENSE`](LICENSE). Personal/hobby/research and noncommercial-organization use only; no commercial use without a separate agreement.
