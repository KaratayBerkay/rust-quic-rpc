#!/usr/bin/env bash
# Add netem impairment on loopback for a load scenario (plan §9).
# Requires: sudo, iproute2. Run netem-clear.sh afterwards.
set -euo pipefail

# 1% loss, 20ms delay, 5ms jitter on egress of lo.
sudo tc qdisc replace dev lo root netem \
  delay 20ms 5ms distribution normal loss 1%
echo "netem applied on lo (delay 20ms ±5ms, loss 1%) — run ./netem-clear.sh to remove"
