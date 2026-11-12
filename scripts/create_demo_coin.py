#!/usr/bin/env python3
"""Create a demo coin on testnet through the NME factory and drive its launch to Live.

  scripts/create_demo_coin.py --ticker pepe --creator you.testnet [--first-buy-tokens 1000]

Needs the `near` CLI with the creator's key in the keychain, and deployments/testnet.json (scripts/deploy-m1.sh).
The launch pauses when it runs out of gas; anyone can resume it with `launch()`, this script just does that for you.
"""
import argparse, json, math, re, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from nview import view  # noqa: E402

# 1x1 PNG
ICON = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
GRID, RANGE = 200, 40_000


def near(args, **kw):
    return subprocess.run(["near", *args, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True, **kw)


def call(contract, method, payload, signer, deposit="0 NEAR", gas="300 Tgas"):
    r = near(["contract", "call-function", "as-transaction", contract, method, "json-args", json.dumps(payload),
              "prepaid-gas", gas, "attached-deposit", deposit, "sign-as", signer])
    out = r.stdout + r.stderr
    ok = r.returncode == 0 and "Error" not in out
    m = re.search(r"Transaction ID:\s*([1-9A-HJ-NP-Za-km-z]{43,44})", out)
    if m:
        print(f"  tx {method}: {m.group(1)}")
    return ok, out


START = 92_000  # overridden by --start-point; a lower start = a lower price = a pool the price of which moves with small trades

def start_point(coin_is_x):
    return START if coin_is_x else -START


def first_buy_max_near(tokens_whole, coin_is_x):
    """Upper bound of the NEAR (yocto) needed to buy `tokens_whole` tokens right after launch (X-side coins only)."""
    if not coin_is_x:
        raise SystemExit("first buy in this script supports X-side tickers (sorting before 'wrap.testnet'); pick another ticker")
    left = start_point(True) + GRID
    raw_price = 1.0001 ** left            # raw Y per raw X at the bottom of the range
    return int(tokens_whole * 10**18 * raw_price * 1.05)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ticker", required=True)
    ap.add_argument("--creator", required=True, help="testnet account with a key in the near-cli keychain")
    ap.add_argument("--first-buy-tokens", type=int, default=0, help="whole tokens the creator buys at launch (<= 5%% of supply)")
    ap.add_argument("--vibe", default="degen frog")
    ap.add_argument("--start-point", type=int, default=92_000, help="DCL start point (multiple of 200). Default 92000 = a very deep, slow pool; try 30000 for a demo coin whose price moves with a few NEAR")
    ap.add_argument("--range", type=int, default=RANGE, help="range width in points (multiple of 200)")
    a = ap.parse_args()
    globals()["START"] = a.start_point

    st = json.load(open(ROOT / "deployments" / "testnet.json"))
    factory, wrap = st["accounts"]["factory"], st["accounts"]["wrap"]
    ticker = a.ticker.lower()
    coin = f"{ticker}.{factory}"
    coin_is_x = coin < wrap
    fb = None
    if a.first_buy_tokens:
        fb = {"tokens": str(a.first_buy_tokens * 10**18), "max_near": str(first_buy_max_near(a.first_buy_tokens, coin_is_x))}
    launch = {"start_point": start_point(coin_is_x), "range_points": a.range, "first_buy": fb}
    required = int(view(factory, "required_deposit", {"icon_len": len(ICON), "first_buy_max_near": fb["max_near"] if fb else None}))
    print(f"coin {coin} ({'X' if coin_is_x else 'Y'} side of its DCL pool), deposit {required / 1e24:.4f} NEAR")

    args = {"args": {"ticker": ticker, "name": f"{ticker} coin", "icon_data_url": ICON, "description": "NME demo coin", "socials": "",
                     "persona": {"vibe": a.vibe, "rivals": []}, "launch": launch,
                     "crown": {"cliff_ms": 60_000, "vest_ms": 600_000, "inactivity_ms": 300_000}, "colosseum_opt_in": True}}
    ok, out = call(factory, "create_coin", args, a.creator, deposit=f"{required} yoctoNEAR")
    print("create_coin:", "ok" if ok else "FAILED\n" + out[-600:])
    if not ok:
        sys.exit(1)

    for i in range(12):
        state = view(coin, "get_launch", {})
        if isinstance(state, dict) and "Live" in state:
            break
        if state == "Failed":
            sys.exit("launch failed (every DCL fee tier was taken); the creator can call refund_failed on the coin")
        print(f"launch state: {json.dumps(state)} -> calling launch() (attempt {i + 1})")
        time.sleep(4)
        call(coin, "launch", {}, a.creator)
        time.sleep(3)
    state = view(coin, "get_launch", {})
    print("launch:", json.dumps(state))
    print("run: scripts/verify.py", ticker)


if __name__ == "__main__":
    main()
