#!/usr/bin/env bash
# Idempotent M6 deployment: arena v6 (Self-Defending Memes) migrating the M5 state. Burns nothing.
# The brain worker gained a `defense` mode: build + upload it, store its secret, then pass its sha256:
#   NME_BRAIN_HASH=<sha256> scripts/deploy-m6.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
HASH="${NME_BRAIN_HASH:?set NME_BRAIN_HASH to the sha256 of the uploaded nme-brain.wasm}"
UPLOADER="${NME_BRAIN_UPLOADER:-nme-s4.testnet}"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" get_defense_config >/dev/null 2>&1; then echo "arena already at M6"; exit 0; fi
near tokens "$ROOT" send-near "$ARENA" "${NME_ARENA_TOPUP:-1 NEAR}" network-config testnet sign-with-keychain send >/dev/null 2>&1 || true
CFG=$(python3 - <<PY
import json
print(json.dumps({
  "defense": {"per_action_bps": 500, "daily_bps": 1000, "cooldown_ms": 300000, "interval_ms": 120000, "look_points": 50,
              "trigger_points": 2232, "fallback_after": 3, "min_treasury": "10000000000000000000000",
              "outlayer_fee": "20000000000000000000000", "fee_cap_per_day": "200000000000000000000000"},
  "tickers": ["${NME_TICKERS:-pepe}"],
  "brain_url": "https://test.fastfs.io/$UPLOADER/outlayer.testnet/$HASH.wasm", "brain_hash": "$HASH"}))
PY
)
near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate json-args "$CFG" \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
python3 scripts/lib/nview.py "$ARENA" get_defense_config
