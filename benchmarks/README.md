# Benchmarks (plan §9, P7d)

Artifacts for the RVP media plane. Rule: **no performance claim in the docs without a
committed artifact under `benchmarks/results/`.**

## Scenarios

| Scenario | Tool | What it measures |
| --- | --- | --- |
| RVP loopback (crypto on/off) | `run-rvp.sh` | e2e latency (p50/p95/p99), pps, sent/opened, CPU |
| Netem impairment | `netem-add.sh` + `run-rvp.sh` | loss/jitter behavior |
| RVP vs LiveKit A/B | `run-rvp.sh` + `livekit/` | latency/CPU with and without app-layer crypto |

## Run RVP loadgen (in-process server)

```bash
# From repo root (crypto on = pairwise AES-GCM fanout):
./benchmarks/run-rvp.sh --rooms 2 --clients-per-room 3 --duration 5

# Plaintext mode (no app-layer crypto) for the crypto-cost comparison:
./benchmarks/run-rvp.sh --plaintext
```

The script writes a JSON report to `benchmarks/results/loopback-<mode>-<ts>.json`.

### External server (prometheus scrape)

```bash
cargo run -p rvp-cli --release -- \
  --jwt-public-key loadgen-keys/grant_pub.pem \
  --metrics-listen 127.0.0.1:9100 \
  --ingress-pps 100000 --ingress-bps 100000000 \
  --max-conn-per-ip 10000 --max-conn-per-user 10000 --no-retry &

cargo run -p rvp-loadgen --release -- run \
  --server 127.0.0.1:4433 \
  --grant-key loadgen-keys/grant_key.pem \
  --report benchmarks/results/loopback-external-crypto.json

curl -s http://127.0.0.1:9100/metrics > benchmarks/results/loopback-external-crypto.prom
```

## tc/netem impairment

```bash
./benchmarks/netem-add.sh      # add 1% loss + 20ms delay on lo
./benchmarks/run-rvp.sh --rooms 1 --clients-per-room 4
./benchmarks/netem-clear.sh    # remove impairment
```

## LiveKit A/B (plan §9, §112)

Cannot run in CI (Docker-down locally) — this directory ships the **harness and
methodology only**. Run the LiveKit side on the same box with the same room size and
publish bitrate, then compare against the RVP JSON report. See
[`livekit/README.md`](livekit/README.md).

## Methodology (plan §9)

- Fixed hardware; identical room sizes and publish bitrate on both sides.
- Warm-up period excluded from the report (loadgen `--warmup`).
- Report latency/CPU **with and without** app-layer crypto (crypto fanout is
  publisher uplink ×(N−1); receivers also see sealed copies for other pairs).
- Artifacts live under `benchmarks/results/` and are committed with the PR that
  produced them; label loopback smoke runs as such (not a LiveKit A/B).
