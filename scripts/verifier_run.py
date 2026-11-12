#!/usr/bin/env python3
"""Run the deposit-verifier worker once through OutLayer:
  scripts/verifier_run.py <wasm sha256> <tx id> <expected deposit address> [solana|evm]   (chain defaults to solana; evm = Ethereum Sepolia)"""
import json, subprocess, sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view
H, sig, to = sys.argv[1:4]
chain = sys.argv[4] if len(sys.argv) > 4 else "solana"
URL = f"https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/{H}.wasm"
calls0 = view("nme-s4.testnet", "last", {})[4]; t0 = time.time()
args = {"source": {"WasmUrl": {"url": URL, "hash": H, "build_target": "wasm32-wasip2"}}, "input": json.dumps({"chain": chain, "tx_id": sig, "expected_to": to}), "format": "Text", "secrets_ref": None, "max_seconds": 60}
subprocess.run(["near", "contract", "call-function", "as-transaction", "nme-s4.testnet", "ask", "json-args", json.dumps(args), "prepaid-gas", "150 Tgas", "attached-deposit", "0.05 NEAR",
                "sign-as", "nme-s4.testnet", "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
for _ in range(60):
    time.sleep(3); last = view("nme-s4.testnet", "last", {})
    if last[4] > calls0 and last[2] > last[1]:
        break
print("blocks", last[2] - last[1], "wall", round(time.time() - t0))
try:
    print(json.dumps(json.loads(json.loads(last[3])), indent=1))
except Exception:  # noqa: BLE001
    print(last[3][:500])
