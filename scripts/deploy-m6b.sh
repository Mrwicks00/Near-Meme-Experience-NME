#!/usr/bin/env bash
# Idempotent deployment of the commentary feed: arena v7 migrating the M6 state. Burns nothing.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" get_commentary_config >/dev/null 2>&1; then echo "arena already has commentary"; exit 0; fi
near tokens "$ROOT" send-near "$ARENA" "${NME_ARENA_TOPUP:-0.5 NEAR}" network-config testnet sign-with-keychain send >/dev/null 2>&1 || true
near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate \
  json-args '{"commentary":{"min_treasury":"10000000000000000000000","per_day_per_coin":6,"outlayer_fee":"20000000000000000000000","fee_cap_per_day":"200000000000000000000000"}}' \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
python3 scripts/lib/nview.py "$ARENA" get_commentary_config
