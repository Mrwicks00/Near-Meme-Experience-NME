#!/usr/bin/env python3
"""The relayer for Solana deposits (anyone can run it): claim a deposit and spend it for the beneficiary.
  scripts/relay_claim.py <solana tx signature> <beneficiary.testnet> <ticker> [min_out_raw=1] [--chest]
claim_deposit -> the TEE verifies the transfer (finalized, to exactly that address) and prices it from two sources ->
the gateway sets the NEAR value aside -> execute_claim spends it (arena.buy_for / fund_chest)."""
import json, re, subprocess, sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view

GATEWAY, SIGNER = "gateway.nme.testnet", "nme-w2.testnet"
args = [a for a in sys.argv[1:] if not a.startswith("--")]
tx, ben, ticker = args[0], args[1], args[2]
intent = {"FundChest": {"ticker": ticker}} if "--chest" in sys.argv else {"Buy": {"ticker": ticker, "min_out": args[3] if len(args) > 3 else "1"}}

def call(method, payload, gas="300 Tgas"):
    o = subprocess.run(["near", "contract", "call-function", "as-transaction", GATEWAY, method, "json-args", json.dumps(payload), "prepaid-gas", gas, "attached-deposit", "0 NEAR",
                        "sign-as", SIGNER, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
    t = re.sub(r"\x1b\[[0-9;]*m", "", o.stdout + o.stderr)
    m = re.search(r"Transaction ID: (\w+)", t)
    print(f"  {method:<14} {'ERR ' + t[t.find('Error'):][:200].strip() if 'Error' in t else 'ok'}  {m.group(1) if m else '?'}")
    return t

print("== claim_deposit"); call("claim_deposit", {"tx_id": tx, "beneficiary": ben, "intent": intent})
for _ in range(40):
    time.sleep(5)
    c = view(GATEWAY, "get_claim", {"tx_id": tx})
    if c and "Pending" not in c:
        break
print("   claim:", json.dumps(c)[:300])
if c and "Verified" in c:
    print("== execute_claim"); t = call("execute_claim", {"tx_id": tx})
    for m in re.finditer(r'EVENT_JSON:(\{[^\n]*"event":"(gateway_deposit_claimed|trade|chest_funded|trade_refused)"[^\n]*\})', t):
        print("   EVENT", m.group(1)[:300])
    time.sleep(8); print("   final:", json.dumps(view(GATEWAY, "get_claim", {"tx_id": tx}))[:300])
