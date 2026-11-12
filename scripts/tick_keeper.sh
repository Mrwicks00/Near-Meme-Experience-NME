#!/usr/bin/env bash
# Keeps the arena moving with no one at the keyboard: when anything is due (settle a round, open the next one, burn a
# delivered job, claim fees, defense checks), call the permissionless `arena.tick`. Anyone could run this; the result is
# the same whoever calls (I7). It pays only gas and earns the arena's poke bounty back.
#   scripts/tick_keeper.sh            (signer: $NME_KEEPER, default nme-w2.testnet, from the near-cli keychain)
# Stops on its own when the signer drops below $NME_KEEPER_MIN NEAR (default 0.3).
# Also watches the gateway reserve (the NEAR it pays Phantom / EVM deposits from): below $NME_RESERVE_MIN (default 6)
# it tops the reserve up to $NME_RESERVE_TARGET (default 11) from $NME_RESERVE_FUNDER (default nme-pub.testnet), never
# taking the funder below $NME_FUNDER_KEEP (default 0.5). Set NME_RESERVE_FUNDER=none to only log low reserves.
# The same funder also keeps (a) the arena's protocol balance (pays round storage, bounties and the arena's own AI calls)
# above $NME_PROTO_MIN (default 0.3), topping it to $NME_PROTO_TARGET (default 1) with the permissionless
# `fund_protocol` (a gift: it can never be withdrawn), and (b) the keeper's own wallet above $NME_KEEPER_REFILL
# (default 0.6), topping it to $NME_KEEPER_TARGET (default 2), so the loop never stops for gas.
set -uo pipefail
cd "$(dirname "$0")/.."
KEEPER="${NME_KEEPER:-nme-w2.testnet}"
ARENA="${NME_ARENA:-arena.nme.testnet}"
INTERVAL="${NME_KEEPER_INTERVAL:-15}"
MIN="${NME_KEEPER_MIN:-0.3}"
GATEWAY="${NME_GATEWAY:-gateway.nme.testnet}"
FUNDER="${NME_RESERVE_FUNDER:-nme-pub.testnet}"
RES_MIN="${NME_RESERVE_MIN:-6}"
RES_TARGET="${NME_RESERVE_TARGET:-11}"
FUNDER_KEEP="${NME_FUNDER_KEEP:-0.5}"
# How near-cli signs: the OS keychain on a laptop; on a server use NME_SIGN=sign-with-legacy-keychain (key files in
# ~/.near-credentials/testnet/<account>.json).
SIGN="${NME_SIGN:-sign-with-keychain}"
PROTO_MIN="${NME_PROTO_MIN:-0.3}"
PROTO_TARGET="${NME_PROTO_TARGET:-1}"
KEEPER_REFILL="${NME_KEEPER_REFILL:-0.6}"
KEEPER_TARGET="${NME_KEEPER_TARGET:-2}"
PLAY_PARENT="${NME_PLAY_PARENT:-nmeplay.testnet}"   # pays new Google sign-in accounts their starter NEAR
PLAY_REFILL="${NME_PLAY_REFILL:-3}"
PLAY_TARGET="${NME_PLAY_TARGET:-15}"
RESERVE_EVERY="${NME_RESERVE_EVERY:-10}"   # loops between reserve checks (10 x 15 s = 2.5 min)
export NEAR_RPC="${NEAR_RPC:-https://test.rpc.fastnear.com}"

balance() {
  python3 - "${1:-$KEEPER}" <<'EOF'
import json, os, sys, urllib.request
body = {"jsonrpc": "2.0", "id": "k", "method": "query", "params": {"request_type": "view_account", "finality": "final", "account_id": sys.argv[1]}}
r = json.load(urllib.request.urlopen(urllib.request.Request(os.environ["NEAR_RPC"], json.dumps(body).encode(), {"Content-Type": "application/json"}), timeout=20))
print(int(r["result"]["amount"]) / 1e24)
EOF
}

# Gateway reserve: read it, and top it up from the funder when it is low.
check_reserve() {
  local R NEED FB AMT OUT TX
  R=$(python3 scripts/lib/nview.py "$GATEWAY" get_reserve 2>/dev/null | python3 -c "import json,sys; print(int(json.load(sys.stdin))/1e24)" 2>/dev/null) || return 0
  python3 -c "import sys; sys.exit(0 if float('$R') < float('$RES_MIN') else 1)" || return 0
  if [ "$FUNDER" = "none" ]; then echo "$(date -u +%FT%TZ) RESERVE LOW: $R NEAR (< $RES_MIN). No funder set."; return 0; fi
  FB=$(balance "$FUNDER" 2>/dev/null) || { echo "$(date -u +%FT%TZ) RESERVE LOW: $R NEAR; could not read $FUNDER balance"; return 0; }
  AMT=$(python3 -c "print(f'{max(0.0, min(float('$RES_TARGET') - float('$R'), float('$FB') - float('$FUNDER_KEEP'))):.3f}')")
  if python3 -c "import sys; sys.exit(0 if float('$AMT') < 0.5 else 1)"; then
    echo "$(date -u +%FT%TZ) RESERVE LOW: $R NEAR (< $RES_MIN) and $FUNDER has only $FB NEAR. Send testnet NEAR to $FUNDER."; return 0
  fi
  OUT=$(near contract call-function as-transaction "$GATEWAY" fund_reserve json-args '{}' prepaid-gas '30 Tgas' attached-deposit "$AMT NEAR" \
    sign-as "$FUNDER" network-config testnet $SIGN send 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
  TX=$(echo "$OUT" | grep -oE 'Transaction ID: [A-Za-z0-9]+' | head -1)
  if [ -n "$TX" ]; then echo "$(date -u +%FT%TZ) RESERVE was $R NEAR -> funded $AMT NEAR from $FUNDER (${TX#Transaction ID: })"; else echo "$(date -u +%FT%TZ) RESERVE top-up failed: $(echo "$OUT" | grep -iE 'error|panic' | head -1 | cut -c1-200)"; fi
}

# How much the funder can give toward a target, never taking it below FUNDER_KEEP. Prints 0 when not worth sending.
grant() { # <current> <target> <funder balance>
  python3 -c "a=max(0.0, min(float('$2') - float('$1'), float('$3') - float('$FUNDER_KEEP'))); print(f'{a:.3f}' if a >= 0.1 else '0')"
}

# Arena protocol balance: rounds need ~0.05 NEAR of it to start; the arena's AI calls spend from it too.
check_protocol() {
  local P FB AMT OUT TX
  [ "$FUNDER" = "none" ] && return 0
  P=$(python3 scripts/lib/nview.py "$ARENA" protocol_balance 2>/dev/null | python3 -c "import json,sys; print(int(json.load(sys.stdin))/1e24)" 2>/dev/null) || return 0
  python3 -c "import sys; sys.exit(0 if float('$P') < float('$PROTO_MIN') else 1)" || return 0
  FB=$(balance "$FUNDER" 2>/dev/null) || return 0
  AMT=$(grant "$P" "$PROTO_TARGET" "$FB")
  if [ "$AMT" = "0" ]; then echo "$(date -u +%FT%TZ) PROTOCOL LOW: $P NEAR and $FUNDER has only $FB NEAR."; return 0; fi
  OUT=$(near contract call-function as-transaction "$ARENA" fund_protocol json-args '{}' prepaid-gas '30 Tgas' attached-deposit "$AMT NEAR" \
    sign-as "$FUNDER" network-config testnet $SIGN send 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
  TX=$(echo "$OUT" | grep -oE 'Transaction ID: [A-Za-z0-9]+' | head -1)
  if [ -n "$TX" ]; then echo "$(date -u +%FT%TZ) PROTOCOL was $P NEAR -> funded $AMT NEAR from $FUNDER (${TX#Transaction ID: })"; else echo "$(date -u +%FT%TZ) PROTOCOL top-up failed: $(echo "$OUT" | grep -iE 'error|panic' | head -1 | cut -c1-200)"; fi
}

# The Google sign-in parent account (creates player accounts and pays their starter NEAR).
check_play() {
  local B FB AMT OUT TX
  [ "$FUNDER" = "none" ] && return 0
  B=$(balance "$PLAY_PARENT" 2>/dev/null) || return 0
  python3 -c "import sys; sys.exit(0 if float('$B') < float('$PLAY_REFILL') else 1)" || return 0
  FB=$(balance "$FUNDER" 2>/dev/null) || return 0
  AMT=$(grant "$B" "$PLAY_TARGET" "$FB")
  [ "$AMT" = "0" ] && { echo "$(date -u +%FT%TZ) PLAY LOW: $B NEAR and $FUNDER has only $FB NEAR."; return 0; }
  OUT=$(near tokens "$FUNDER" send-near "$PLAY_PARENT" "$AMT NEAR" network-config testnet $SIGN send 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
  TX=$(echo "$OUT" | grep -oE 'Transaction ID: [A-Za-z0-9]+' | head -1)
  if [ -n "$TX" ]; then echo "$(date -u +%FT%TZ) PLAY was $B NEAR -> sent $AMT NEAR from $FUNDER (${TX#Transaction ID: })"; else echo "$(date -u +%FT%TZ) PLAY top-up failed"; fi
}

# The keeper's own gas wallet.
check_keeper() {
  local B FB AMT OUT TX
  [ "$FUNDER" = "none" ] && return 0
  B=$(balance "$KEEPER" 2>/dev/null) || return 0
  python3 -c "import sys; sys.exit(0 if float('$B') < float('$KEEPER_REFILL') else 1)" || return 0
  FB=$(balance "$FUNDER" 2>/dev/null) || return 0
  AMT=$(grant "$B" "$KEEPER_TARGET" "$FB")
  [ "$AMT" = "0" ] && { echo "$(date -u +%FT%TZ) KEEPER LOW: $B NEAR and $FUNDER has only $FB NEAR."; return 0; }
  OUT=$(near tokens "$FUNDER" send-near "$KEEPER" "$AMT NEAR" network-config testnet $SIGN send 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
  TX=$(echo "$OUT" | grep -oE 'Transaction ID: [A-Za-z0-9]+' | head -1)
  if [ -n "$TX" ]; then echo "$(date -u +%FT%TZ) KEEPER was $B NEAR -> sent $AMT NEAR from $FUNDER (${TX#Transaction ID: })"; else echo "$(date -u +%FT%TZ) KEEPER top-up failed: $(echo "$OUT" | grep -iE 'error|panic' | head -1 | cut -c1-200)"; fi
}

echo "$(date -u +%FT%TZ) keeper up: signer=$KEEPER arena=$ARENA every ${INTERVAL}s, stops below ${MIN} NEAR; reserve floor ${RES_MIN} NEAR, protocol floor ${PROTO_MIN} NEAR, keeper refill below ${KEEPER_REFILL} NEAR, funder ${FUNDER}"
LOOP=0
while true; do
  if [ $((LOOP % RESERVE_EVERY)) -eq 0 ]; then check_reserve; check_protocol; check_keeper; check_play; fi
  LOOP=$((LOOP + 1))
  NEXT=$(python3 scripts/lib/nview.py "$ARENA" next_due_ms 2>/dev/null || echo "err")
  NOW=$(( $(date +%s) * 1000 ))
  if [ "$NEXT" != "err" ] && [ "$NEXT" != "null" ] && [ "$NEXT" -le "$NOW" ]; then
    # A failed balance read is "unknown", never "empty": keep ticking and check again next time.
    BAL=$(balance 2>/dev/null || echo unknown)
    if [ "$BAL" != "unknown" ] && python3 -c "import sys; sys.exit(0 if float('$BAL') < float('$MIN') else 1)"; then
      echo "$(date -u +%FT%TZ) STOP: $KEEPER has $BAL NEAR (< $MIN). Top it up and restart."; exit 1
    fi
    # A tick that starts a TEE request (defense check, commentary) only finishes when the worker answers, up to ~4 min.
    # Run up to $MAX_INFLIGHT ticks at once so one slow request never holds up burns and rounds behind it.
    if [ "$(jobs -rp | wc -l)" -lt "${NME_KEEPER_INFLIGHT:-3}" ]; then
      (
        OUT=$(near contract call-function as-transaction "$ARENA" tick json-args '{"max_tasks":8}' prepaid-gas '300 Tgas' attached-deposit '0 NEAR' \
          sign-as "$KEEPER" network-config testnet $SIGN send 2>&1 | sed 's/\x1b\[[0-9;]*m//g')
        TX=$(echo "$OUT" | grep -oE 'Transaction ID: [A-Za-z0-9]+' | head -1)
        RES=$(echo "$OUT" | grep -oE '"processed":[0-9]+[^}]*' | head -1)
        if [ -n "$TX" ]; then echo "$(date -u +%FT%TZ) tick ${TX#Transaction ID: } ${RES:-} balance=${BAL}"; else echo "$(date -u +%FT%TZ) tick failed: $(echo "$OUT" | grep -iE 'error|panic' | head -1 | cut -c1-200)"; fi
      ) &
    fi
  fi
  sleep "$INTERVAL"
done
