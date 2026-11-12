#!/usr/bin/env bash
# Idempotent M2 deployment: arena v2 (migrates M1 state), factory update, graveyard, registrations.
# Burns nothing (no global code is published); only locks storage NEAR on the arena account and spends gas.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; GRAVE="burn.$ROOT"
WRAP="${NME_WRAP:-wrap.testnet}"; DCL="${NME_DCL:-dclv2.ref-dev.testnet}"
NEAR=near; NET="network-config testnet"; SIGN="sign-with-keychain send"; WASM=contracts/target/near
log() { printf '\n== %s\n' "$*"; }
exists() { $NEAR account view-account-summary "$1" $NET now >/dev/null 2>&1; }
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
call() { $NEAR contract call-function as-transaction "$1" "$2" json-args "$3" prepaid-gas '100 Tgas' attached-deposit "$4" sign-as "$5" $NET $SIGN; }

log "top up arena for the bigger wasm"
$NEAR tokens "$ROOT" send-near "$ARENA" '2 NEAR' $NET $SIGN || true

log "graveyard $GRAVE (no keys; created then every key deleted)"
if exists "$GRAVE"; then echo "exists"; else
  $NEAR account create-account fund-myself "$GRAVE" '0.1 NEAR' autogenerate-new-keypair save-to-keychain sign-as "$ROOT" $NET $SIGN; sleep 4
  PK=$($NEAR account list-keys "$GRAVE" $NET now 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -o 'ed25519:[A-Za-z0-9]*' | head -1)
  $NEAR account delete-keys "$GRAVE" public-keys "$PK" $NET $SIGN
fi

log "arena v2 + migrate"
$NEAR contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate \
  json-args "{\"graveyard\":\"$GRAVE\",\"router_fee_bps\":100,\"min_pool_near\":\"1000000000000000000000000\",\"burn_slippage_bps\":500,\"tickers\":[\"pepe\"]}" \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' $NET $SIGN

log "factory update (state unchanged)"
$NEAR contract deploy "$ROOT" use-file "$WASM/nme_factory/nme_factory.wasm" without-init-call $NET $SIGN

log "arena registrations: wNEAR, DCL, pepe"
call "$WRAP" storage_deposit '{"account_id":"'$ARENA'"}' '0.00125 NEAR' "$ROOT" || true
call "$DCL" storage_deposit '{"account_id":"'$ARENA'"}' '0.5 NEAR' "$ROOT" || true
call "pepe.$ROOT" storage_deposit '{"account_id":"'$ARENA'"}' '0.00125 NEAR' "$ROOT" || true
log "done"; python3 scripts/lib/nview.py "$ARENA" get_config; python3 scripts/lib/nview.py "$ARENA" get_coin '{"key":"pepe"}'
