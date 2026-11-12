#!/usr/bin/env bash
# Idempotent: deploys the local arena build WITHOUT an init call when the on-chain code differs. Only for changes that
# keep the state layout (new side maps, appended Task variants, new methods): prove it first with
#   cargo test -p nme-it --test migrate_rpc_real_state the_deployed_arena_takes_raids_without_a_migration
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near/nme_arena/nme_arena.wasm
RPC="${NEAR_RPC:-https://archival-rpc.testnet.fastnear.com}"
[ -f "$WASM" ] || scripts/build-contracts.sh
same=$(python3 - "$WASM" "$ARENA" "$RPC" <<'E'
import hashlib, json, sys, urllib.request
ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58(b):
    n = int.from_bytes(b, "big"); s = ""
    while n: n, r = divmod(n, 58); s = ALPHA[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s
local = b58(hashlib.sha256(open(sys.argv[1], "rb").read()).digest())
req = urllib.request.Request(sys.argv[3], json.dumps({"jsonrpc": "2.0", "id": 1, "method": "query", "params": {"request_type": "view_account", "finality": "final", "account_id": sys.argv[2]}}).encode(), {"Content-Type": "application/json", "User-Agent": "nme-deploy"})
print("same" if json.load(urllib.request.urlopen(req, timeout=20))["result"]["code_hash"] == local else "different")
E
)
if [ "$same" = "same" ]; then echo "arena code already deployed"; else
  near contract deploy "$ARENA" use-file "$WASM" without-init-call network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
