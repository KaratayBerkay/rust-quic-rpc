#!/usr/bin/env bash
# Remove netem impairment from loopback.
set -euo pipefail
sudo tc qdisc del dev lo root netem 2>/dev/null || \
  sudo tc qdisc del dev lo root
echo "netem cleared on lo"
