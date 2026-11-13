#!/usr/bin/env bash
# Idempotent: re-pins the arena's brain worker (same code layout; `migrate_brain` changes only brain_url + brain_hash).
# 2026-11-11: worker with Qwen3.6-35B-A3B and thinking off (the old model ran past OutLayer's ~30 s limit) and an upload-RPC fallback.
# 2026-11-13: meme jobs return once the picture upload is in a block (no FastFS wait), every network wait capped, step timings.
# 2026-11-13: brain rules cut to two (no sexual content, no harassment of real people); cartoon coin-vs-coin roasts allowed.
# Run ONLY after `cargo test --workspace` exited 0. The worker must be on FastFS with its secrets stored for this exact hash.
#   NME_BRAIN_HASH   sha256 of the brain worker
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
export NEAR_RPC="${NEAR_RPC:-https://archival-rpc.testnet.fastnear.com}"
HASH="${NME_BRAIN_HASH:-8264a8bfa4dd0c7d585d7c4afb6a2c0e9e7bd0e765dd2dcb669c2288d3e087ee}"
URL="https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/$HASH.wasm"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
[ "$(curl -s "$URL" | sha256sum | cut -d' ' -f1)" = "$HASH" ] || { echo "worker at $URL does not hash to $HASH - stop"; exit 1; }
current=$(python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;print(json.load(sys.stdin)["brain_hash"])')
if [ "$current" = "$HASH" ]; then echo "arena brain already pinned to $HASH"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate_brain \
    json-args "{\"brain_url\":\"$URL\",\"brain_hash\":\"$HASH\"}" \
    prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;b=json.load(sys.stdin);print("brain:", b["brain_hash"][:12], "| rpc:", b["rpc_url"])'
