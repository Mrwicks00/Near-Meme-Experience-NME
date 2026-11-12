#!/usr/bin/env bash
# Idempotent M8 deployment: arena v4 (tick, bounties, inline ticks) migrating the M3 state. Burns nothing.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" protocol_balance >/dev/null 2>&1; then echo "arena already at M8"; else
  near tokens "$ROOT" send-near "$ARENA" "${NME_ARENA_TOPUP:-1 NEAR}" network-config testnet sign-with-keychain send >/dev/null 2>&1 || true
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate \
    json-args "{\"bounty_per_task\":\"${NME_BOUNTY:-2000000000000000000000}\",\"bounty_cap_per_tick\":\"${NME_BOUNTY_CAP:-10000000000000000000000}\",\"inline_tick_min_gas_tgas\":250}" \
    prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
echo "protocol bucket: $(python3 scripts/lib/nview.py "$ARENA" protocol_balance)"
python3 scripts/lib/nview.py "$ARENA" due_tasks '{"limit":10}'
