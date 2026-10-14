# rust-quic-rtc — RVP (Rust Voice Protocol)

Rust + QUIC real-time **voice** server: a DATAGRAM SFU with pairwise end-to-end-encrypted Opus, EdDSA JWT auth via a NestJS control plane, and Flutter + TanStack web clients.

> **Status: Phase 0 — design only.** Full design plan: [`docs/plan.md`](docs/plan.md). No production media code until sign-off.

## Why not just LiveKit?

Documented honestly in [`docs/plan.md` §12](docs/plan.md): LiveKit is the right buy for standard conferencing. RVP exists for QUIC-native transport, Rust ownership of the media path, **default** pairwise E2E encryption, and protocol-level control. P7's A/B benchmark is the evidence gate.

## Roadmap

| Phase | Scope |
|---|---|
| P0 | Design docs + ADRs (current) |
| P1–P3 | Rust QUIC server: echo → auth/crypto → SFU rooms |
| P4–P6 | Clients: Flutter plugin, NestJS wiring, TanStack web app |
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

TBD.
