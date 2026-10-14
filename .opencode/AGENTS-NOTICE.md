# Agent attribution

Agents in this directory are converted from
[msitarzewski/agency-agents](https://github.com/msitarzewski/agency-agents)
(MIT License) via the upstream `scripts/convert.sh --tool opencode`.

Selected roster for this project (Rust + QUIC voice server):

| Agent | Purpose in this project |
|---|---|
| software-architect | System design, ADRs (Phase 0) |
| backend-architect | Rust/NestJS API & server design |
| technical-writer | Protocol and design docs |
| rust-refactoring-specialist | Rust code quality |
| realtime-collaboration-engineer | Realtime presence/sync patterns |
| video-streaming-engineer | Low-latency transport, codecs |
| voice-ai-integration-engineer | Audio/Opus pipelines |
| mobile-app-builder | Flutter client |
| identity-access-engineer | JWT/OAuth auth design |
| security-architect | Threat model (Phase 0) |
| application-security-engineer | Secure implementation review |
| database-optimizer | PostgreSQL schema/queries (NestJS) |
| sre-site-reliability-engineer | Metrics, observability, scaling (P6/P7) |
| api-platform-engineer | Rate limiting, API surface |
| code-reviewer | Code review gates |
| performance-benchmarker | Benchmarks vs LiveKit (P6) |
| api-tester | Protocol/API tests |
| test-automation-engineer | Integration test suites |

Regenerate/refresh: clone agency-agents, run `./scripts/convert.sh --tool opencode`,
copy selected files from `integrations/opencode/agents/` here.
