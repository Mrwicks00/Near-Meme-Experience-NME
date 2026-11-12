#!/usr/bin/env bash
# Idempotent M7 deployment: the gateway (new account gateway.<root>) and arena v9 (`buy_for`). Burns nothing.
# The deposit-verifier worker is pinned by hash: build + upload it (scripts/fastfs_upload.py) and pass its sha256.
#   NME_VERIFIER_HASH=<sha256> scripts/deploy-m7.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; GW="gateway.$ROOT"; WASM=contracts/target/near
HASH="${NME_VERIFIER_HASH:?set NME_VERIFIER_HASH to the sha256 of the uploaded nme-deposit-verifier.wasm}"
UPLOADER="${NME_VERIFIER_UPLOADER:-nme-s4.testnet}"
SIGN="network-config testnet sign-with-keychain send"
[ -f "$WASM/nme_gateway/nme_gateway.wasm" ] || scripts/build-contracts.sh
if ! near account view-account-summary "$GW" network-config testnet now >/dev/null 2>&1; then
  near account create-account fund-myself "$GW" '4 NEAR' autogenerate-new-keypair save-to-keychain sign-as "$ROOT" $SIGN 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error"; sleep 4
fi
if python3 scripts/lib/nview.py "$GW" get_config >/dev/null 2>&1; then echo "gateway already initialized"; else
  near contract deploy "$GW" use-file "$WASM/nme_gateway/nme_gateway.wasm" with-init-call new json-args "{\"outlayer\":\"outlayer.testnet\",\"mpc\":\"v1.signer-prod.testnet\",\"arena\":\"$ARENA\",\"worker\":{\"url\":\"https://test.fastfs.io/$UPLOADER/outlayer.testnet/$HASH.wasm\",\"hash\":\"$HASH\"},\"cfg\":{\"spread_bps\":200,\"max_claim_near\":\"5000000000000000000000000\",\"min_claim_near\":\"100000000000000000000000\"}}" prepaid-gas '100 Tgas' attached-deposit '0 NEAR' $SIGN 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
if python3 scripts/lib/nview.py "$ARENA" get_gateway >/dev/null 2>&1; then echo "arena already has buy_for"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate json-args "{\"gateway\":\"$GW\"}" prepaid-gas '100 Tgas' attached-deposit '0 NEAR' $SIGN 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
near tokens "$ROOT" send-near "$GW" "${NME_RESERVE:-1 NEAR}" network-config testnet sign-with-keychain send >/dev/null 2>&1 || true
python3 scripts/lib/nview.py "$GW" get_config | cut -c1-300
