#!/usr/bin/env python3
"""Live Colosseum demo on testnet: NME coin `pepe` vs the outside coin nme-batman.testnet.
Spends testnet NEAR from the wallets below (all in the near-cli keychain). Prints every tx id.
  usage: scripts/demo_war.py            (full run: fund chests, start, trade, wait, settle, verify)"""
import json, re, subprocess, sys, time
AUTO = "--auto" in sys.argv   # no start_round / settle_round: the arena's tick does it and pays the poker a bounty
POKER = "nme-w2.testnet"
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from nview import view  # noqa: E402

ARENA, PEPE, BAT = "arena.nme.testnet", "pepe.nme.testnet", "nme-batman.testnet"
FUNDER, PEPE_W, BAT_W = "nme-alice.testnet", ["nme-w1.testnet", "nme-w2.testnet"], ["nme-relayer.testnet", "nme-alice.testnet"]
TXS = {}

def call(contract, method, args, deposit, signer, gas="300 Tgas", label=None):
    out = subprocess.run(["near", "contract", "call-function", "as-transaction", contract, method, "json-args", json.dumps(args), "prepaid-gas", gas,
                          "attached-deposit", deposit, "sign-as", signer, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
    text = re.sub(r"\x1b\[[0-9;]*m", "", out.stdout + out.stderr)
    m = re.search(r"Transaction ID: (\w+)", text)
    tx = m.group(1) if m else "?"
    ok = "Error" not in text and "panicked" not in text
    print(f"  {'ok ' if ok else 'ERR'} {label or method:<22} {signer:<22} {tx}")
    if not ok:
        print(text[-600:])
    if label:
        TXS[label] = tx
    return text

def bal(token, who):
    return int(view(token, "ft_balance_of", {"account_id": who}))

def coin(key):
    return view(ARENA, "get_coin", {"key": key})

def main():
    print("== register wallets on the tokens")
    for w in PEPE_W:
        call(PEPE, "storage_deposit", {"account_id": w}, "0.00125 NEAR", w, "30 Tgas", f"reg_pepe_{w}")
    call(BAT, "storage_deposit", {"account_id": "nme-relayer.testnet"}, "0.00125 NEAR", "nme-relayer.testnet", "30 Tgas", "reg_bat_relayer")
    print("== fund chests"); call(ARENA, "fund_chest", {"key": "pepe"}, "0.8 NEAR", FUNDER, "30 Tgas", "chest_pepe"); call(ARENA, "fund_chest", {"key": BAT}, "0.5 NEAR", FUNDER, "30 Tgas", "chest_batman")
    time.sleep(3)
    c0 = {k: int(coin(k)["chest_near"]) for k in ("pepe", BAT)}
    print("   chests:", c0)
    sup0 = int(view(PEPE, "ft_total_supply", {})); grave0 = bal(BAT, "burn.nme.testnet")
    if AUTO:
        print("== fund the protocol bucket, then let tick start the round")
        call(ARENA, "fund_protocol", {}, "0.2 NEAR", FUNDER, "30 Tgas", "fund_protocol")
        print("   protocol bucket:", view(ARENA, "protocol_balance", {}))
        due = [t["due_ms"] for t in view(ARENA, "due_tasks", {"limit": 20}) if t["task"] == "StartRound"]
        wait = (min(due) / 1000 + 3 - time.time()) if due else 0
        print(f"   StartRound is polled every 60 s; next poll due in {max(wait, 0):.0f}s"); time.sleep(max(wait, 0))
        call(ARENA, "tick", {"max_tasks": 5}, "0 NEAR", POKER, label="tick_start")
    else:
        print("== start round"); call(ARENA, "start_round", {}, "0.05 NEAR", FUNDER, "100 Tgas", "start_round")
    time.sleep(3)
    rnd = view(ARENA, "current_round", {}); pair = rnd["pairs"][0]
    print(f"   round {rnd['id']}: {pair['a']} vs {pair['b']}, stake {int(pair['stake'])/1e24} NEAR each, ends in {(rnd['end_ms'] - time.time()*1000)/1000:.0f}s")
    print("== wallets trade through the arena")
    for w in PEPE_W:
        call(ARENA, "buy", {"key": "pepe", "min_out": "1"}, "0.3 NEAR", w, label=f"buy_pepe_{w}")
    alice_before = bal(BAT, FUNDER)
    for w in BAT_W:
        call(ARENA, "buy", {"key": BAT, "min_out": "1"}, "0.3 NEAR", w, label=f"buy_batman_{w}")
    time.sleep(8)
    # alice dumps 80% of what she bought on the batman side: her counted score shrinks
    bought = bal(BAT, FUNDER) - alice_before; dump = bought * 8 // 10  # only tokens bought in this round matter
    print(f"== {FUNDER} sells 80% of her batman buy back through the arena ({dump} tokens)")
    call(BAT, "ft_transfer_call", {"receiver_id": ARENA, "amount": str(dump), "msg": json.dumps({"sell": {"min_out": "1"}})}, "1 yoctoNEAR", FUNDER, label="alice_sells")
    time.sleep(10)
    p = view(ARENA, "current_round", {})["pairs"][0]
    print(f"   scores: pepe {int(p['score_a'])/1e24:.4f} ({p['unique_a']} wallets)  batman {int(p['score_b'])/1e24:.4f} ({p['unique_b']} wallets)")
    end = view(ARENA, "current_round", {})["end_ms"]
    wait = end / 1000 + 6 - time.time()
    print(f"== waiting {max(wait,0):.0f}s for the round to end"); time.sleep(max(wait, 0))
    if AUTO:
        print("== tick settles (the poker earns a bounty from the protocol bucket)")
        b0 = view(ARENA, "protocol_balance", {}); out = call(ARENA, "tick", {"max_tasks": 5}, "0 NEAR", POKER, label="tick_settle")
        print("   protocol bucket:", b0, "->", view(ARENA, "protocol_balance", {}))
    else:
        print("== settle (anyone)"); out = call(ARENA, "settle_round", {"round_id": rnd["id"]}, "0 NEAR", "nme-w1.testnet", label="settle_round")
    time.sleep(12)
    p = view(ARENA, "get_round", {"id": rnd["id"]})["pairs"][0]
    sup1 = int(view(PEPE, "ft_total_supply", {})); c1 = {k: int(coin(k)["chest_near"]) for k in ("pepe", BAT)}
    print(f"   winner side: {p['winner']} ({p['note'] or 'clear win'})")
    print(f"   pepe supply burned: {(sup0 - sup1)/1e18:.4f} tokens; chests now {c1}; batman tokens in graveyard +{(bal(BAT, 'burn.nme.testnet') - grave0)}")
    # In auto mode the same tick also starts the NEXT round, which locks new stakes out of the chests: count them.
    cur = view(ARENA, "current_round", {}); locked = {k: 0 for k in c1}
    if cur["id"] != rnd["id"] and cur["settled_pairs"] < len(cur["pairs"]):
        print(f"   the machine already started round {cur['id']} (stakes locked: {[int(x['stake'])/1e24 for x in cur['pairs']]} NEAR each)")
        for x in cur["pairs"]:
            for side in ("a", "b"):
                if x[side] in locked: locked[x[side]] += int(x["stake"])
    ok = p["winner"] == "a" and sup1 < sup0 and c1[BAT] + locked[BAT] == c0[BAT] - int(pair["stake"])
    print("PASS" if ok else "FAIL", " the loser's stake bought and burned the winner; winner's stake returned")
    print(json.dumps(TXS, indent=1))
    sys.exit(0 if ok else 1)

main()
