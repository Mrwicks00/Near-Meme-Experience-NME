#!/usr/bin/env python3
"""Check the claims for a coin on testnet and print PASS/FAIL per claim:  scripts/verify.py <ticker>\n   scripts/verify.py --sell-test <ticker> <seller.testnet> <raw token amount>   (spends tokens: proves a sell delivers NEAR)"""
import json, re, subprocess, sys, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
from nview import view  # noqa: E402

SUPPLY = 10**27
RPC = "https://rpc.testnet.fastnear.com"


def rpc(method, params):
    r = urllib.request.Request(RPC, json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(), {"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(r, timeout=30))["result"]


def sell_test(ticker, seller, tokens):
    """Sell `tokens` (raw units) of `ticker` through the arena from `seller` and check the NEAR arrives: the seller's
    balance delta plus the gas it paid must equal the DCL quote (sells carry no router fee, D-39). Spends real testnet tokens."""
    st = json.load(open(ROOT / "deployments" / "testnet.json"))
    acc = st["accounts"]
    coin = ticker if "." in ticker else f"{ticker}.{acc['factory']}"
    key = ticker
    entry = view(acc["arena"], "get_coin", {"key": key})
    quote = view(acc["dcl"], "quote", {"pool_ids": [entry["pool_id"]], "input_token": coin, "output_token": acc["wrap"], "input_amount": str(tokens), "tag": None})
    expect = int(quote["amount"])
    def bal():
        return int(rpc("query", {"request_type": "view_account", "finality": "final", "account_id": seller})["amount"])
    b0 = bal()
    msg = json.dumps({"sell": {"min_out": str(expect * 95 // 100)}})
    args = json.dumps({"receiver_id": acc["arena"], "amount": str(tokens), "msg": msg})
    out = subprocess.run(["near", "contract", "call-function", "as-transaction", coin, "ft_transfer_call", "json-args", args, "prepaid-gas", "300 Tgas",
                          "attached-deposit", "1 yoctoNEAR", "sign-as", seller, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
    text = re.sub(r"\x1b\[[0-9;]*m", "", out.stdout + out.stderr)
    txid = re.search(r"Transaction ID: (\w+)", text).group(1)
    time.sleep(12)
    tx = rpc("EXPERIMENTAL_tx_status", {"tx_hash": txid, "sender_account_id": seller, "wait_until": "FINAL"})
    fee = sum(int(o["outcome"]["tokens_burnt"]) for o in [tx["transaction_outcome"]] + tx["receipts_outcome"])
    received = bal() - b0 + fee + 1  # +1 yocto deposit
    ok = abs(received - expect) <= expect // 50 and received > 0
    print(f"{'PASS' if ok else 'FAIL'}  a sell through the arena delivers NEAR to the seller, no router fee  [sold {tokens}, quote {expect}, received {received}, gas paid {fee}, tx {txid}]")
    sys.exit(0 if ok else 1)


def main():
    if sys.argv[1] == "--sell-test":
        return sell_test(sys.argv[2].lower(), sys.argv[3], int(sys.argv[4]))
    ticker = sys.argv[1].lower()
    st = json.load(open(ROOT / "deployments" / "testnet.json"))
    acc = st["accounts"]
    coin, vault = f"{ticker}.{acc['factory']}", f"crown.{ticker}.{acc['factory']}"
    results = []

    def check(name, fn):
        try:
            ok, detail = fn()
        except Exception as e:  # noqa: BLE001
            ok, detail = False, f"{type(e).__name__}: {str(e)[:120]}"
        results.append(ok)
        print(f"{'PASS' if ok else 'FAIL'}  {name}  [{detail}]")

    acct = rpc("query", {"request_type": "view_account", "finality": "final", "account_id": coin})
    keys = rpc("query", {"request_type": "view_access_key_list", "finality": "final", "account_id": coin})["keys"]
    check("coin account has NO access keys (I2)", lambda: (len(keys) == 0, f"{len(keys)} keys"))
    check("coin runs global code by hash (D-15)", lambda: (bool(acct.get("global_contract_hash")), str(acct.get("global_contract_hash"))))
    check("supply never exceeds the fixed 1,000,000,000 (only burns reduce it)", lambda: (0 < int(view(coin, "ft_total_supply", {})) <= SUPPLY, f"{view(coin, 'ft_total_supply', {})} (burned: {SUPPLY - int(view(coin, 'ft_total_supply', {}))})"))
    launch = view(coin, "get_launch", {})
    check("launch is Live", lambda: (isinstance(launch, dict) and "Live" in launch, json.dumps(launch)[:90]))
    check("5% of supply sits in the PumpBag vault (I4)", lambda: (int(view(coin, "ft_balance_of", {"account_id": vault})) == SUPPLY // 20, view(coin, "ft_balance_of", {"account_id": vault})))
    vkeys = rpc("query", {"request_type": "view_access_key_list", "finality": "final", "account_id": vault})["keys"]
    check("PumpBag vault has NO access keys", lambda: (len(vkeys) == 0, f"{len(vkeys)} keys"))
    if isinstance(launch, dict) and "Live" in launch:
        pool_id, lpt_id = launch["Live"]["pool_id"], launch["Live"]["lpt_id"]
        rows = view(acc["dcl"], "list_liquidities", {"account_id": coin})
        check("the coin owns exactly one DCL position", lambda: (len(rows) == 1 and rows[0]["lpt_id"] == lpt_id, f"{len(rows)} position(s)"))
        pool = view(acc["dcl"], "get_pool", {"pool_id": pool_id})
        check("the DCL pool exists and is Running", lambda: (pool is not None and pool["state"] == "Running", pool_id))
        dcl_bal = int(view(coin, "ft_balance_of", {"account_id": acc["dcl"]}))
        check("DCL holds the pool side (95% minus any first buy)", lambda: (SUPPLY * 95 // 100 - dcl_bal >= 0 and dcl_bal > SUPPLY * 90 // 100, dcl_bal))
        check("the coin holds no loose tokens", lambda: (int(view(coin, "ft_balance_of", {"account_id": coin})) == 0, "0"))
    entry = view(acc["arena"], "get_coin", {"key": ticker})
    check("the arena registered the coin and recorded its launch", lambda: (bool(entry) and entry["launched"], json.dumps(entry)[:90]))
    nft = view(acc["crown"], "nft_token", {"token_id": ticker})
    check("the PumpBag NFT exists", lambda: (bool(nft), (nft or {}).get("owner_id")))
    print(f"\n{sum(results)}/{len(results)} claims verified for ${ticker.upper()}")
    sys.exit(0 if all(results) else 1)


if __name__ == "__main__":
    main()
