#!/usr/bin/env python3
"""Run the Operator worker once through OutLayer (any account may pay; the worker holds the function-call-only key).
  scripts/operator_run.py [wasm sha256]   (default: hash recorded by the last upload in /tmp/claude-1000/op_hash.txt)"""
import json, subprocess, sys, time, re
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view

CALLER = PAYER = "nme-s4.testnet"
H = sys.argv[1] if len(sys.argv) > 1 else Path("/tmp/claude-1000/op_hash.txt").read_text().strip()
URL = f"https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/{H}.wasm"

def bal():
    o = subprocess.run(["near", "account", "view-account-summary", PAYER, "network-config", "testnet", "now"], capture_output=True, text=True).stdout
    return float(re.search(r"Native account balance\s+([\d.]+)", re.sub(r"\x1b\[[0-9;]*m", "", o)).group(1))

inp = {"rpc": "https://rpc.testnet.fastnear.com", "arena": "arena.nme.testnet", "account": "nme-operator.testnet", "max_tasks": 5}
calls0 = view(CALLER, "last", {})[4]; b0 = bal(); t0 = time.time()
args = {"source": {"WasmUrl": {"url": URL, "hash": H, "build_target": "wasm32-wasip2"}}, "input": json.dumps(inp), "format": "Text",
        "secrets_ref": {"profile": "default", "account_id": "nme-brain.testnet"}, "max_seconds": 60}
r = subprocess.run(["near", "contract", "call-function", "as-transaction", CALLER, "ask", "json-args", json.dumps(args), "prepaid-gas", "150 Tgas", "attached-deposit", "0.05 NEAR",
                    "sign-as", PAYER, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
tx = re.search(r"Transaction ID: (\w+)", re.sub(r"\x1b\[[0-9;]*m", "", r.stdout + r.stderr))
for _ in range(60):
    time.sleep(3)
    last = view(CALLER, "last", {})
    if last[4] > calls0 and last[2] > last[1]:
        break
ok, req, res, raw, _ = last
try:
    out = json.loads(json.loads(raw))
except Exception:  # noqa: BLE001
    out = {"ok": False, "error": f"unparsable callback: {raw[:200]}"}
print(json.dumps({"ask_tx": tx.group(1) if tx else "?", "blocks": res - req, "wall_s": round(time.time() - t0, 1), "payer_cost_near": round(b0 - bal(), 5), "worker": out}, indent=1))
