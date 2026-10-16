#!/usr/bin/env bash
# Run the rvp-loadgen against an in-process server and commit-ready the JSON
# report under benchmarks/results/ (plan §9, P7d).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS="$ROOT/benchmarks/results"
mkdir -p "$RESULTS"

MODE=crypto
EXTRA_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --plaintext) MODE=plaintext ;;
    *) EXTRA_ARGS+=("$arg") ;;
  esac
done

TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$RESULTS/loopback-$MODE-$TS.json"

cargo run -p rvp-loadgen --release -- run \
  --report "$OUT" \
  "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"

echo "report: $OUT"
