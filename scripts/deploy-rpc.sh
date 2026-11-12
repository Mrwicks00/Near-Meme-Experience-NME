#!/usr/bin/env bash
# Idempotent: moves the arena's brain RPC off the public FastNear endpoint (it stalls requests from OutLayer's enclaves, so
# every brain call timed out). Same code layout; `migrate_rpc` changes only `brain.rpc_url`. Burns nothing.
# Run ONLY after `cargo test --workspace` exited 0 and `cargo test -p nme-it --test migrate_rpc_real_state` passed.
#   NME_BRAIN_RPC   keyless https RPC the worker should use (default https://near-testnet.drpc.org)
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
NEW_RPC="${NME_BRAIN_RPC:-https://near-testnet.drpc.org}"
export NEAR_RPC="${NEAR_RPC:-https://archival-rpc.testnet.fastnear.com}"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh

current=$(python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;print(json.load(sys.stdin)["rpc_url"])')
if [ "$current" = "$NEW_RPC" ]; then echo "arena brain rpc already $NEW_RPC"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate_rpc \
    json-args "{\"rpc_url\":\"$NEW_RPC\"}" \
    prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;b=json.load(sys.stdin);print("brain rpc:", b["rpc_url"], "| worker:", b["brain_hash"][:12])'
