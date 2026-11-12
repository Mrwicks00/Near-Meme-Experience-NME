#!/usr/bin/env bash
# Idempotent M4 deployment: PumpBag collection v2 (crown.<root>) and arena v8, both migrating their state. Burns nothing.
# Afterwards `crown_sync` registers the deadlines of the PumpBags minted before M4 (pepe, frog, toad ... NME_TICKERS).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; CROWN="crown.$ROOT"; WASM=contracts/target/near
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
SIGN="network-config testnet sign-with-keychain send"
if python3 scripts/lib/nview.py "$CROWN" crown_status '{"token_id":"x"}' 2>&1 | grep -q "unknown PumpBag\|owner"; then echo "crown already at M4"; else
  near contract deploy "$CROWN" use-file "$WASM/nme_crown/nme_crown.wasm" with-init-call migrate json-args '{}' prepaid-gas '100 Tgas' attached-deposit '0 NEAR' $SIGN 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
if python3 scripts/lib/nview.py "$ARENA" get_crown_deadline '{"ticker":"x"}' >/dev/null 2>&1; then echo "arena already at M4"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate json-args "{\"crown\":\"$CROWN\"}" prepaid-gas '100 Tgas' attached-deposit '0 NEAR' $SIGN 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
for t in ${NME_TICKERS:-pepe frog toad}; do
  near contract call-function as-transaction "$CROWN" crown_sync json-args "{\"token_id\":\"$t\"}" prepaid-gas '30 Tgas' attached-deposit '0 NEAR' sign-as "${NME_SYNC_SIGNER:-nme-w2.testnet}" $SIGN 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID" | sed "s/^/crown_sync $t: /"
done
for t in ${NME_TICKERS:-pepe frog toad}; do echo "$t deadline: $(python3 scripts/lib/nview.py "$ARENA" get_crown_deadline "{\"ticker\":\"$t\"}")"; done
