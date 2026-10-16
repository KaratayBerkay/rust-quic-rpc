# LiveKit A/B harness (plan §9, §112, P7d)

Docker Compose + config to run a **LiveKit SFU on the same box** as the RVP
loadgen, for an apples-to-apples latency/CPU comparison. CI cannot run this
(Docker-down locally), so this ships as a harness + procedure only — **no
fabricated numbers**.

## Run LiveKit

```bash
cd benchmarks/livekit
docker compose up -d
# LiveKit on ws://127.0.0.1:7880, dev API key/secret in livekit.yaml.
```

## Load-test LiveKit

Use the official `lk` CLI loadtest publisher (same room size / publish bitrate
as the RVP scenario):

```bash
# 3 publishers, 50fps, ~80 byte video payloads (mirrors the loadgen defaults):
lk loadtest --url ws://127.0.0.1:7880 \
  --api-key devkey --api-secret secret \
  --room benchmark --identity loadgen \
  --publishers 3 --subscribers 0 --fps 50 --bitrate 80000
```

Adjust `--publishers` / `--subscribers` to match the RVP `--clients-per-room`
count; keep Opus bitrate and room size identical on both sides (plan §9).

## Compare

1. Run `../run-rvp.sh` (and a `--plaintext` pass) for the RVP side.
2. Run the LiveKit loadtest above for the LiveKit side.
3. Record latency/CPU side by side; commit both artifacts under
   `../results/` with the same room size and publish bitrate noted.

RVP's differentiators to weigh in the A/B (plan §1, §112): default pairwise
app-layer E2E, QUIC 1-RTT + native datagrams, Rust media plane. LiveKit wins
on ecosystem maturity — see plan §12.
