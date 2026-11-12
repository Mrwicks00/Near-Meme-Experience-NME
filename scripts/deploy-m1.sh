#!/usr/bin/env bash
# Idempotent M1 deployment to NEAR testnet (minimal launch on Rhea DCL). Safe to re-run: every step checks first.
#
#   NME_ROOT       factory account (default nme.testnet)    - must exist and hold ~10 NEAR (more for the sub-accounts)
#   NME_PUBLISHER  account that publishes the global code   - must hold the burn cost printed by the dry run
#
# PUBLISHING GLOBAL CODE BURNS NEAR (10 NEAR per 100 KB, not refundable) and every wasm change is a new burn.
# So this script is a DRY RUN by default: it prints the plan and the exact burn. Set NME_CONFIRM_BURN=yes to execute.
# State (addresses, published hashes) is recorded in deployments/testnet.json.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="${NME_ROOT:-nme.testnet}"; PUB="${NME_PUBLISHER:-nme-pub.testnet}"
WRAP="${NME_WRAP:-wrap.testnet}"; DCL="${NME_DCL:-dclv2.ref-dev.testnet}"
ARENA="arena.$ROOT"; CROWN="crown.$ROOT"
NEAR="near"; NET="network-config testnet"; SIGN="sign-with-keychain send"
STATE=deployments/testnet.json; WASM=contracts/target/near
mkdir -p deployments; [ -f "$STATE" ] || echo '{"published":{}}' > "$STATE"
log() { printf '\n== %s\n' "$*"; }
exists() { $NEAR account view-account-summary "$1" $NET now >/dev/null 2>&1; }
inited() { python3 scripts/lib/nview.py "$1" get_config >/dev/null 2>&1; }
is_published() { python3 -c "import json,sys; sys.exit(0 if '$1' in json.load(open('$STATE'))['published'] else 1)"; }
setstate() { python3 - "$STATE" "$1" "$2" <<'PY'
import json,sys; p,k,v=sys.argv[1:4]; d=json.load(open(p)); d[k]=json.loads(v); json.dump(d,open(p,'w'),indent=2); open(p,'a').write("\n")
PY
}

for c in nme_coin nme_crown_vault nme_factory nme_arena nme_crown; do
  [ -f "$WASM/$c/$c.wasm" ] || { scripts/build-contracts.sh; break; }; done
read -r COIN_HEX COIN_B58 < <(python3 scripts/lib/hash.py "$WASM/nme_coin/nme_coin.wasm")
read -r VAULT_HEX VAULT_B58 < <(python3 scripts/lib/hash.py "$WASM/nme_crown_vault/nme_crown_vault.wasm")
COIN_BYTES=$(stat -c %s "$WASM/nme_coin/nme_coin.wasm"); VAULT_BYTES=$(stat -c %s "$WASM/nme_crown_vault/nme_crown_vault.wasm")

log "plan (dry run unless NME_CONFIRM_BURN=yes)"
BURN=0
plan_publish() { # name hex bytes
  if is_published "$2"; then echo "  [skip] $1 code already published ($2)"; else
    cost=$(python3 -c "print(round($3/100000*10,2))"); BURN=$(python3 -c "print(round($BURN+$cost,2))")
    echo "  [BURN] publish $1 global code: $3 bytes -> ~$cost NEAR burned (hash $2)"; fi
}
plan_publish coin "$COIN_HEX" "$COIN_BYTES"
plan_publish vault "$VAULT_HEX" "$VAULT_BYTES"
for a in "$ARENA" "$CROWN"; do exists "$a" && echo "  [skip] account $a exists" || echo "  [create] account $a"; done
for a in "$ARENA" "$CROWN" "$ROOT"; do inited "$a" && echo "  [skip] $a already initialized" || echo "  [deploy] contract + init on $a"; done
PUBBAL=$($NEAR account view-account-summary "$PUB" $NET now 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -i "native account" | awk '{print $4}')
echo "  publisher $PUB balance: $PUBBAL NEAR; total burn needed: $BURN NEAR"
if [ "$BURN" != "0" ] && [ "${NME_CONFIRM_BURN:-no}" != "yes" ]; then
  echo; echo "DRY RUN: nothing was spent. To publish and deploy, re-run with NME_CONFIRM_BURN=yes"; exit 3
fi
python3 -c "import sys; sys.exit(0 if float('$PUBBAL') > float('$BURN') + 0.5 else 1)" || { echo "publisher needs more than $BURN NEAR (+0.5 margin)"; exit 4; }

log "publish global code by hash (burns NEAR, once per hash)"
publish() { # name hex file
  if is_published "$2"; then echo "$1 already published"; return; fi
  echo "publishing $1 ($(stat -c %s "$3") bytes) from $PUB ..."
  $NEAR contract deploy-as-global use-file "$3" as-global-hash "$PUB" $NET $SIGN
  python3 - "$STATE" "$2" "$1" <<'PY'
import json,sys; p,h,n=sys.argv[1:4]; d=json.load(open(p)); d["published"][h]=n; json.dump(d,open(p,'w'),indent=2); open(p,'a').write("\n")
PY
}
publish coin "$COIN_HEX" "$WASM/nme_coin/nme_coin.wasm"
publish vault "$VAULT_HEX" "$WASM/nme_crown_vault/nme_crown_vault.wasm"

log "infrastructure accounts"
for pair in "$ARENA:2.4 NEAR" "$CROWN:2.9 NEAR"; do
  acct="${pair%%:*}"; amt="${pair#*:}"
  if exists "$acct"; then echo "$acct exists"; else
    $NEAR account create-account fund-myself "$acct" "$amt" autogenerate-new-keypair save-to-keychain sign-as "$ROOT" $NET $SIGN; sleep 4
  fi
done

deploy() { # account wasm-file init-method json-args
  if inited "$1"; then echo "$1 already initialized"; return; fi
  $NEAR contract deploy "$1" use-file "$2" with-init-call "$3" json-args "$4" prepaid-gas '30 Tgas' attached-deposit '0 NEAR' $NET $SIGN
}
log "arena";   deploy "$ARENA" "$WASM/nme_arena/nme_arena.wasm" new \
  "{\"factory\":\"$ROOT\",\"wrap\":\"$WRAP\",\"dcl\":\"$DCL\",\"cfg\":{\"bounty_per_task\":\"0\",\"bounty_cap_per_tick\":\"0\",\"inline_tick_min_gas_tgas\":0,\"price_check_interval_ms\":0,\"fee_crown_bps\":4000,\"fee_treasury_bps\":4000,\"claim_interval_ms\":600000}}"
log "crown";   deploy "$CROWN" "$WASM/nme_crown/nme_crown.wasm" new "{\"factory\":\"$ROOT\",\"arena\":\"$ARENA\"}"
log "factory"; deploy "$ROOT" "$WASM/nme_factory/nme_factory.wasm" new \
  "{\"arena\":\"$ARENA\",\"crown\":\"$CROWN\",\"wrap\":\"$WRAP\",\"dcl\":\"$DCL\",\"coin_code_hash\":\"$COIN_B58\",\"vault_code_hash\":\"$VAULT_B58\"}"

setstate network '"testnet"'
setstate accounts "{\"factory\":\"$ROOT\",\"arena\":\"$ARENA\",\"crown\":\"$CROWN\",\"wrap\":\"$WRAP\",\"dcl\":\"$DCL\",\"publisher\":\"$PUB\"}"
setstate code "{\"coin_sha256\":\"$COIN_HEX\",\"coin_b58\":\"$COIN_B58\",\"vault_sha256\":\"$VAULT_HEX\",\"vault_b58\":\"$VAULT_B58\"}"
log "done"; python3 scripts/lib/nview.py "$ROOT" get_config
