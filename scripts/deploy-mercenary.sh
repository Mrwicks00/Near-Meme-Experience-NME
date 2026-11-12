#!/usr/bin/env bash
# Idempotent: deploys the arena with mercenaries (mercenary.rs). Same state layout (one Task variant appended, side
# maps under their own prefixes): no init call. Run ONLY after `cargo test --workspace` exited 0.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
export NEAR_RPC="${NEAR_RPC:-https://archival-rpc.testnet.fastnear.com}"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" mercenary_sponsor '{"job_id":0}' >/dev/null 2>&1; then echo "arena already has mercenaries"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" without-init-call \
    network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;b=json.load(sys.stdin);print("brain:", b["brain_hash"][:12], b["rpc_url"])'
python3 scripts/lib/nview.py "$ARENA" mercenary_sponsor '{"job_id":0}' >/dev/null && echo "mercenary view: ok"
