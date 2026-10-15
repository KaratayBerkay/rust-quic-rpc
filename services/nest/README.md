# @rvp/nest — RVP control plane

NestJS service implementing plan §5: session login, rooms + membership
(Postgres), media grants, pairwise AES-128 key distribution, and the JWKS
endpoint the media server consumes.

## Endpoints

| Method | Path                  | Auth   | Purpose |
| ------ | --------------------- | ------ | ------- |
| POST   | `/auth/login`         | —      | Issue a long-lived session JWT (`iss=nest-api`, `aud=rvp-api`) |
| GET    | `/auth/jwks`          | —      | Ed25519 public key set (`rvp-cli --nest-jwks`) |
| POST   | `/rooms`              | bearer | Create a room (creator becomes `host`) |
| GET    | `/rooms/:id`          | bearer | Room + members (members only) |
| POST   | `/rooms/:id/members`  | bearer | Add a member (members only) |
| POST   | `/rooms/:id/grants`   | bearer | Media grant + pairwise keys (members only) |

Media grants carry `iss=nest`, `aud=rvp`, `sub`, `room_id`, `role`, `exp`
(≤ 2 min), `jti` — verified offline by `rvp-auth::Verifier` inside the Rust
media server. Pairwise keys are AES-128 (16 bytes, hex), one per other
member, with dense per-room kids (P4b media-header byte3, unique per room).

## Run locally

```sh
docker compose up -d          # Postgres 16 on :5432
cp .env.example .env          # or export DATABASE_URL directly
npm ci
npm run start:dev
```

Consume the grant path from the Rust server:

```sh
cargo run -p rvp-cli -- --nest-jwks http://127.0.0.1:3000/auth/jwks
```

## Tests

e2e (supertest + real Postgres, plan §5 flow: login → JWKS → rooms →
membership → grants with shared pairwise keys):

```sh
DATABASE_URL=postgres://rvp:rvp@127.0.0.1:5432/rvp npm test
```

CI runs the same suite against a `postgres:16` service container with a
deterministic `NEST_EDDSA_SEED` (the Rust side pins the resulting JWKS as an
interop fixture in `crates/rvp-auth`).

## Security notes (dev-grade until P7)

- Login accepts any non-empty `userId` — no password/identity proof yet.
- Any room member can add further members.
- Without `NEST_EDDSA_SEED`, an ephemeral signing key is generated at boot
  (media grants stop verifying after restart). Set the seed in any shared
  environment.
