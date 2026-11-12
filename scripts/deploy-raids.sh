#!/usr/bin/env bash
# Idempotent: deploys the arena with raids (docs/13-RAIDS.md). Raid state lives under its own storage prefixes and only
# one `Task` variant was appended, so the code goes in WITHOUT an init call (checked against the live state by
# `cargo test -p nme-it --test migrate_rpc_real_state the_deployed_arena_takes_raids_without_a_migration`).
# Run ONLY after `cargo test --workspace` exited 0.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
export NEAR_RPC="${NEAR_RPC:-https://archival-rpc.testnet.fastnear.com}"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] || scripts/build-contracts.sh
if python3 scripts/lib/nview.py "$ARENA" raid_rules >/dev/null 2>&1; then echo "arena already has raids"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" without-init-call \
    network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
python3 scripts/lib/nview.py "$ARENA" raid_rules
python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;b=json.load(sys.stdin);print("brain still:", b["brain_hash"][:12], b["rpc_url"])'
