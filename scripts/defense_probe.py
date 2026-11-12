#!/usr/bin/env python3
"""Run the brain worker's `defense` mode through OutLayer on a few price scenarios and print its decisions.
  scripts/defense_probe.py <wasm sha256>"""
import json, subprocess, sys, time, re
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view

CALLER = PAYER = "nme-s4.testnet"
H = sys.argv[1]
URL = f"https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/{H}.wasm"
BASE = {"mode": "defense", "high_age_secs": 300, "ticker": "pepe", "coin": "pepe.nme.testnet", "rpc": "https://rpc.testnet.fastnear.com", "treasury_near": "5000000000000000000000000", "per_action_bps": 500, "daily_left_bps": 1500}
# DCL points: price = 1.0001^point, so -1054 points = -10%, -2877 = -25%, -6931 = -50%.
SCENARIOS = [
    ("calm dip -3%", {"high_point": 20000, "current_point": 19700}),
    ("dump -10%", {"high_point": 20000, "current_point": 20000 - 1054}),
    ("panic dump -25%", {"high_point": 20000, "current_point": 20000 - 2877}),
    ("crash -50%", {"high_point": 20000, "current_point": 20000 - 6931}),
    ("panic dump -25%, only 60 bps left today", {"high_point": 20000, "current_point": 20000 - 2877, "daily_left_bps": 60}),
]
for name, extra in SCENARIOS:
    calls0 = view(CALLER, "last", {})[4]; t0 = time.time()
    args = {"source": {"WasmUrl": {"url": URL, "hash": H, "build_target": "wasm32-wasip2"}}, "input": json.dumps({**BASE, **extra}), "format": "Text",
            "secrets_ref": {"profile": "default", "account_id": "nme-brain.testnet"}, "max_seconds": 90}
    subprocess.run(["near", "contract", "call-function", "as-transaction", CALLER, "ask", "json-args", json.dumps(args), "prepaid-gas", "150 Tgas", "attached-deposit", "0.05 NEAR",
                    "sign-as", PAYER, "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
    for _ in range(60):
        time.sleep(3)
        last = view(CALLER, "last", {})
        if last[4] > calls0 and last[2] > last[1]:
            break
    try:
        out = json.loads(json.loads(last[3]))
    except Exception:  # noqa: BLE001
        out = {"error": last[3][:150]}
    print(f"{name:<30} blocks={last[2]-last[1]:<4} {out.get('action')} bps={out.get('bps')} drop={out.get('drop_pct_x100')} err={out.get('error')}\n   {out.get('reason')!r}")
