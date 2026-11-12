#!/usr/bin/env bash
# Creates the "outside" coin for demos: a plain NEP-141 (spike launcher code, NOT from the NME factory) with its own
# DCL pool against wNEAR, seeded with >= 1 NEAR of liquidity-side buys, then enlists it in the arena.
# Idempotent. Needs a funded BUYER (default nme-alice.testnet) holding ~3 NEAR.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; OUT="${NME_OUTSIDER:-nme-batman.testnet}"; BUYER="${NME_BUYER:-nme-alice.testnet}"
WRAP=wrap.testnet; DCL=dclv2.ref-dev.testnet; ARENA="arena.$ROOT"; POOL="$OUT|$WRAP|10000"
WASM=contracts/spikes/s12-launcher/target/near/s12_launcher.wasm
SIGN="network-config testnet sign-with-keychain send"
q() { sed 's/\x1b\[[0-9;]*m//g'; }
call() { near contract call-function as-transaction "$1" "$2" json-args "$3" prepaid-gas "${5:-100} Tgas" attached-deposit "$4" sign-as "$6" $SIGN 2>&1 | q; }
tx() { grep -m1 "Transaction ID" | awk '{print $NF}'; }
exists() { near account view-account-summary "$1" network-config testnet now >/dev/null 2>&1; }
view() { python3 scripts/lib/nview.py "$@"; }

[ -f "$WASM" ] || { echo "build the spike launcher first: (cd contracts/spikes/s12-launcher && cargo near build non-reproducible-wasm)"; exit 2; }
if ! exists "$OUT"; then
  near account create-account sponsor-by-faucet-service "$OUT" autogenerate-new-keypair save-to-keychain network-config testnet create 2>&1 | q | tail -2
  sleep 5
fi
if ! view "$OUT" get_state >/dev/null 2>&1; then
  call "$WRAP" storage_deposit "{\"account_id\":\"$OUT\"}" '0.00125 NEAR' 30 "$ROOT" >/dev/null
  near contract deploy "$OUT" use-file "$WASM" with-init-call new json-args "{\"dcl\":\"$DCL\",\"wrap\":\"$WRAP\",\"supply\":\"1000000000000000000000000000\"}" prepaid-gas '30 Tgas' attached-deposit '0 NEAR' $SIGN 2>&1 | q | grep -E "Transaction ID" | head -1
fi
if [ "$(view "$OUT" get_state | python3 -c 'import json,sys; print(json.load(sys.stdin)["pool_id"])')" = "None" ] || view "$OUT" get_state | grep -q '"pool_id": null'; then
  echo "launch tx: $(call "$OUT" launch '{"init_point":92000,"left":92200,"right":132200}' '1 NEAR' 300 "$BUYER" | tx)"
  sleep 6
fi
view "$OUT" get_state
# buyer: registered on the token and on DCL, then buys ~1.5 NEAR worth so the pool holds wNEAR
call "$OUT" storage_deposit "{\"account_id\":\"$BUYER\"}" '0.00125 NEAR' 30 "$BUYER" >/dev/null || true
call "$DCL" storage_deposit "{\"account_id\":\"$BUYER\",\"registration_only\":true}" '0.5 NEAR' 30 "$BUYER" >/dev/null || true
NEARSIDE=$(view "$DCL" get_pool "{\"pool_id\":\"$POOL\"}" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(int(d["total_y"]))')
if python3 -c "import sys; sys.exit(0 if int('$NEARSIDE') < 1100000000000000000000000 else 1)"; then
  call "$WRAP" near_deposit '{}' '1.5 NEAR' 30 "$BUYER" >/dev/null
  MSG="{\\\"Swap\\\":{\\\"pool_ids\\\":[\\\"$POOL\\\"],\\\"output_token\\\":\\\"$OUT\\\",\\\"min_output_amount\\\":\\\"1\\\"}}"
  echo "swap tx: $(call "$WRAP" ft_transfer_call "{\"receiver_id\":\"$DCL\",\"amount\":\"1500000000000000000000000\",\"msg\":\"$MSG\"}" '1 yoctoNEAR' 150 "$BUYER" | tx)"
  sleep 6
fi
echo "pool NEAR side: $(view "$DCL" get_pool "{\"pool_id\":\"$POOL\"}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["total_y"])')"
echo "enlist tx: $(call "$ARENA" enlist "{\"token\":\"$OUT\",\"pool_id\":\"$POOL\"}" '0.1 NEAR' 300 "$BUYER" | tx)"
sleep 10
view "$ARENA" get_coin "{\"key\":\"$OUT\"}"
