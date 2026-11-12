#!/usr/bin/env bash
# The timer for the Operator. OutLayer has no scheduler (docs/spikes/S11.md), so something must wake the TEE worker.
# This loop does it: look at the arena's due index (a free RPC view) and only pay for an Operator run when something is due.
set -euo pipefail
cd "$(dirname "$0")/.."
INTERVAL="${NME_KEEPER_INTERVAL:-30}"
while true; do
  NEXT=$(python3 scripts/lib/nview.py arena.nme.testnet next_due_ms || echo null)
  NOW=$(( $(date +%s) * 1000 ))
  if [ "$NEXT" != "null" ] && [ "$NEXT" -le "$NOW" ]; then
    echo "$(date -u +%FT%TZ) due -> running the Operator"; python3 scripts/operator_run.py | python3 -c "import json,sys; d=json.load(sys.stdin); print(json.dumps(d['worker']))"
  fi
  sleep "$INTERVAL"
done
