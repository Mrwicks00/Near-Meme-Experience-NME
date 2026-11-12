#!/usr/bin/env bash
# Idempotent v10 deployment: arena v10 (per-coin job prices, defense commentary, `meme` job kind + delivery media) migrating the
# M7 state, then factory v2 (job_price in create_coin) migrating its state. Burns nothing. Run ONLY after the full workspace tests
# exited 0 (lessons 9) and `cargo test -p nme-it --test migrate_real_state` passed (lessons 8).
#   NME_BRAIN_HASH   sha256 of the brain worker (default: the verified meme-capable build)
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; ARENA="arena.$ROOT"; WASM=contracts/target/near
BRAIN_HASH="${NME_BRAIN_HASH:-e022e3fefc69b5da7b92a90cf2c54dc0780564f6867db52400325cd77b7da839}"
BRAIN_URL="https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/$BRAIN_HASH.wasm"
[ -f "$WASM/nme_arena/nme_arena.wasm" ] && [ -f "$WASM/nme_factory/nme_factory.wasm" ] || scripts/build-contracts.sh
# The coin and vault code are pinned by hash: they must not have changed (lessons 10).
want_coin=$(python3 -c "import json;print(json.load(open('deployments/testnet.json'))['code']['coin_sha256'])")
want_vault=$(python3 -c "import json;print(json.load(open('deployments/testnet.json'))['code']['vault_sha256'])")
[ "$(sha256sum $WASM/nme_coin/nme_coin.wasm | cut -d' ' -f1)" = "$want_coin" ] || { echo "coin wasm hash changed - stop"; exit 1; }
[ "$(sha256sum $WASM/nme_crown_vault/nme_crown_vault.wasm | cut -d' ' -f1)" = "$want_vault" ] || { echo "vault wasm hash changed - stop"; exit 1; }
# The worker must be served by FastFS with exactly this hash.
[ "$(curl -s "$BRAIN_URL" | sha256sum | cut -d' ' -f1)" = "$BRAIN_HASH" ] || { echo "brain worker at $BRAIN_URL does not hash to $BRAIN_HASH - stop"; exit 1; }

if python3 scripts/lib/nview.py "$ARENA" get_job_price '{"ticker":"pepe"}' >/dev/null 2>&1; then echo "arena already at v10"; else
  near contract deploy "$ARENA" use-file "$WASM/nme_arena/nme_arena.wasm" with-init-call migrate \
    json-args "{\"brain_url\":\"$BRAIN_URL\",\"brain_hash\":\"$BRAIN_HASH\"}" \
    prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
echo "arena job price (pepe): $(python3 scripts/lib/nview.py "$ARENA" get_job_price '{"ticker":"pepe"}')"
echo "arena brain pin: $(python3 scripts/lib/nview.py "$ARENA" get_brain_config | python3 -c 'import json,sys;print(json.load(sys.stdin)["brain_hash"])')"

# Factory: compare the deployed code hash with the local wasm (no view tells the version).
here=$(python3 - "$WASM/nme_factory/nme_factory.wasm" "$ROOT" <<'E'
import hashlib, json, sys, urllib.request
ALPHA = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58(b):
    n = int.from_bytes(b, "big"); s = ""
    while n: n, r = divmod(n, 58); s = ALPHA[r] + s
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + s
local = b58(hashlib.sha256(open(sys.argv[1], "rb").read()).digest())
req = urllib.request.Request("https://rpc.testnet.fastnear.com", json.dumps({"jsonrpc": "2.0", "id": 1, "method": "query", "params": {"request_type": "view_account", "finality": "optimistic", "account_id": sys.argv[2]}}).encode(), {"Content-Type": "application/json"})
print("same" if json.load(urllib.request.urlopen(req, timeout=20))["result"]["code_hash"] == local else "different")
E
)
if [ "$here" = "same" ]; then echo "factory already at v2"; else
  near contract deploy "$ROOT" use-file "$WASM/nme_factory/nme_factory.wasm" with-init-call migrate json-args '{}' \
    prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-keychain send 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "Transaction ID|Error|panick"
fi
python3 scripts/lib/nview.py "$ROOT" get_config
python3 scripts/lib/nview.py "$ROOT" list_coins '{"from_index":0,"limit":10}' | python3 -c 'import json,sys;print("coins:", [c[0] for c in json.load(sys.stdin)])'
