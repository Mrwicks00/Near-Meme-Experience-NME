#!/usr/bin/env python3
"""Live Self-Defending demo on testnet (coin `frog`, a shallow pool so a few NEAR move the price):
buy through the arena (fees fill the treasury, price rises = the rolling high), dump it all back, then poke `tick`
until the coin's brain (real OutLayer worker + NEAR AI) decides, and show the capped buyback & burn.
  scripts/demo_defense.py [ticker]     (the coin must have been launched with a shallow pool, e.g. --start-point -60000 --range 2000)"""
import json, re, subprocess, sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view

TICKER = sys.argv[1] if len(sys.argv) > 1 else "toad"
ARENA, COIN, W, POKER = "arena.nme.testnet", f"{TICKER}.nme.testnet", "nme-w1.testnet", "nme-w2.testnet"

def call(contract, method, args, deposit, signer, gas="300 Tgas"):
    o = subprocess.run(["near", "contract", "call-function", "as-transaction", contract, method, "json-args", json.dumps(args), "prepaid-gas", gas, "attached-deposit", deposit,
                        "sign-as", signer, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
    t = re.sub(r"\x1b\[[0-9;]*m", "", o.stdout + o.stderr)
    tx = re.search(r"Transaction ID: (\w+)", t)
    print(f"  {'ERR' if 'Error' in t else 'ok '} {method:<14} {signer:<16} {tx.group(1) if tx else '?'}")
    if "Error" in t:
        print("     ", re.sub(r"\s+", " ", t[t.find("Error"):])[:240])
    return t

def pool_point():
    return view("dclv2.ref-dev.testnet", "get_pool", {"pool_id": f"{COIN}|wrap.testnet|10000"})["current_point"]

def treasury():
    return int(view(ARENA, "get_coin", {"key": TICKER})["treasury_near"])

START_POINT = None
print("== wallet registration"); call(COIN, "storage_deposit", {"account_id": W}, "0.00125 NEAR", W, "30 Tgas")
START_POINT = pool_point()
print(f"point at start {START_POINT}, treasury {treasury()/1e24:.4f} NEAR")
print("== buys through the arena (fills the treasury with router fees, pushes the price up)")
for _ in range(3):
    call(ARENA, "buy", {"key": TICKER, "min_out": "1"}, "1 NEAR", W)
time.sleep(8)
print(f"point after buys {pool_point()}, treasury {treasury()/1e24:.4f} NEAR")
print("== first defense check records the rolling high (tick runs it when due)")
t0 = time.time()
for _ in range(12):
    call(ARENA, "tick", {"max_tasks": 5}, "0 NEAR", POKER)
    time.sleep(10)
    if view(ARENA, "get_defense_state", {"ticker": TICKER})["high_point"] > START_POINT + 100:
        break  # the rolling high was recorded AFTER the buys pushed the price up
st = view(ARENA, "get_defense_state", {"ticker": TICKER}); print("   state:", st)
print("== dump: sell everything back through the arena")
held = int(view(COIN, "ft_balance_of", {"account_id": W}))
call(COIN, "ft_transfer_call", {"receiver_id": ARENA, "amount": str(held), "msg": json.dumps({"sell": {"min_out": "1"}})}, "1 yoctoNEAR", W)
time.sleep(8)
print(f"point after the dump {pool_point()} (rolling high {st['high_point']})")
tr0 = treasury()
print("== tick until the brain decides and (if it buys back) the burn runs")
decided = burned = None
for i in range(30):
    t = call(ARENA, "tick", {"max_tasks": 5}, "0 NEAR", POKER)
    for m in re.finditer(r'EVENT_JSON:(\{[^\n]*"event":"(defense|defense_rejected|defense_miss|treasury_buyback)"[^\n]*\})', t):
        ev = json.loads(m.group(1)); print("   EVENT", m.group(2), json.dumps(ev["data"][0])[:330])
        if m.group(2) == "defense" and ev["data"][0]["action"] == "buyback":
            decided = (ev["data"][0]["bps"], treasury())
        if m.group(2) == "treasury_buyback" and ev["data"][0]["key"] == TICKER and decided:
            burned = ev["data"][0]
    time.sleep(12)
    if burned:
        break
stt = view(ARENA, "get_defense_state", {"ticker": TICKER})
print(f"decision {decided}; buyback {burned}; state {stt}")
ok = bool(burned) and int(burned["near_spent"]) * 10_000 <= decided[1] * decided[0] + 10**21 and int(burned["near_spent"]) * 10_000 <= tr0 * 500 + 10**21 * 10
print("PASS" if ok else "FAIL", " the brain's buyback burned at most the decided share, and never above the 5% cap")
sys.exit(0 if ok else 1)
