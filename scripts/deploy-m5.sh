#!/usr/bin/env bash
# Idempotent M5 deployment: arena v5 (Working Memes) migrating the M8 state. Burns nothing.
# The brain worker is pinned by hash. Build + upload it first (scripts/fastfs_upload.py) and store its secret:
#   outlayer secrets set '{"NEAR_AI_API_KEY":"..."}' --wasm-hash <sha> --build <sha>      (account nme-brain.testnet)
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
HASH="${NME_BRAIN_HASH:?set NME_BRAIN_HASH to the sha256 of the uploaded nme-brain.wasm}"
UPLOADER="${NME_BRAIN_UPLOADER:-nme-s4.testnet}"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" get_brain_config >/dev/null 2>&1; then echo "arena already at M5"; exit 0; fi
near tokens "$ROOT" send-near "$ARENA" "${NME_ARENA_TOPUP:-1 NEAR}" network-config testnet sign-with-keychain send >/dev/null 2>&1 || true
CFG=$(python3 - <<PY
import json
print(json.dumps({"brain": {
  "outlayer": "outlayer.testnet",
  "brain_url": "https://test.fastfs.io/$UPLOADER/outlayer.testnet/$HASH.wasm", "brain_hash": "$HASH",
  "secrets_account": "nme-brain.testnet", "secrets_profile": "default",
  "rpc_url": "https://rpc.testnet.fastnear.com",
  "min_price": "100000000000000000000000", "outlayer_fee": "20000000000000000000000", "record_reserve": "3000000000000000000000",
  "cut_bps": 1000, "deadline_ms": 600000}}))
PY
)
near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate json-args "$CFG" \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
python3 scripts/lib/nview.py "$ARENA" get_brain_config
