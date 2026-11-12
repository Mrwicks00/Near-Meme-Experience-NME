#!/usr/bin/env bash
# Idempotent M3 deployment: arena v3 (Colosseum) migrating the M2 state. Burns nothing.
# Demo war parameters (immutable once migrated): 4 min rounds, stake = 50% of the weaker chest, wallet cap 50% of
# the stake, at least 2 scoring wallets per side, min stake 0.05 NEAR, up to 2 pairs per round.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" get_war_config >/dev/null 2>&1; then echo "arena already at M3"; exit 0; fi
near tokens "$ROOT" send-near "$ARENA" "${NME_ARENA_TOPUP:-1 NEAR}" network-config testnet sign-with-keychain send >/dev/null 2>&1 || true
near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate \
  json-args "{\"war\":{\"round_duration_ms\":${NME_ROUND_MS:-240000},\"round_interval_ms\":0,\"chest_bps\":5000,\"wallet_cap_bps\":5000,\"min_unique\":2,\"min_stake\":\"50000000000000000000000\",\"max_pairs\":2},\"roster\":[\"pepe\",\"${NME_OUTSIDER:-nme-batman.testnet}\"]}" \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
python3 scripts/lib/nview.py "$ARENA" get_war_config; python3 scripts/lib/nview.py "$ARENA" roster '{"from":0,"limit":10}'
