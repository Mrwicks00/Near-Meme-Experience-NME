#!/usr/bin/env python3
"""Run one `meme` job of the brain worker through OutLayer and print the result: scripts/meme_probe.py <worker sha256>"""
import json, subprocess, sys, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view
H = sys.argv[1]; URL = f"https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/{H}.wasm"
job = {"mode": "job", "job_id": 99, "kind": "meme", "persona": {"name": "PEPE", "ticker": "PEPE", "vibe": ""}, "coin": "pepe.nme.testnet", "rpc": "https://rpc.testnet.fastnear.com", "target": "BATMAN", "debug_stage": int(sys.argv[2]) if len(sys.argv) > 2 else None, "prompt": "batman hiding in a cave while a frog in sunglasses laughs in the sun"}
calls0 = view("nme-s4.testnet", "last", {})[4]; t0 = time.time()
args = {"source": {"WasmUrl": {"url": URL, "hash": H, "build_target": "wasm32-wasip2"}}, "input": json.dumps(job), "format": "Text", "secrets_ref": {"profile": "default", "account_id": "nme-brain.testnet"},
        "max_seconds": 170, "max_instructions": 5000000000, "max_memory_mb": 128}
subprocess.run(["near", "contract", "call-function", "as-transaction", "nme-s4.testnet", "ask", "json-args", json.dumps(args), "prepaid-gas", "150 Tgas", "attached-deposit", "0.05 NEAR",
                "sign-as", "nme-s4.testnet", "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
for _ in range(80):
    time.sleep(4); last = view("nme-s4.testnet", "last", {})
    if last[4] > calls0 and last[2] > last[1]:
        break
print("blocks", last[2] - last[1], "wall", round(time.time() - t0))
try:
    print(json.dumps(json.loads(json.loads(last[3])), indent=1))
except Exception:  # noqa: BLE001
    print(last[3][:500])
