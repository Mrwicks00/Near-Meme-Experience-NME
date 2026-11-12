#!/usr/bin/env python3
"""Tiny read-only NEAR RPC helper: nview.py <contract> <method> [json-args]  -> prints the JSON result.
Exit 1 if the view fails. Uses NEAR_RPC (default https://rpc.testnet.fastnear.com)."""
import base64, json, os, sys, urllib.request

def view(contract, method, args=None, rpc=None):
    rpc = rpc or os.environ.get("NEAR_RPC", "https://rpc.testnet.fastnear.com")
    body = {"jsonrpc": "2.0", "id": "x", "method": "query", "params": {
        "request_type": "call_function", "finality": "optimistic", "account_id": contract,
        "method_name": method, "args_base64": base64.b64encode(json.dumps(args or {}).encode()).decode()}}
    req = urllib.request.Request(rpc, json.dumps(body).encode(), {"Content-Type": "application/json"})
    r = json.load(urllib.request.urlopen(req, timeout=20))
    if "error" in r or "error" in r.get("result", {}):
        raise RuntimeError(json.dumps(r.get("error") or r["result"])[:300])
    return json.loads(bytes(r["result"]["result"]))

if __name__ == "__main__":
    try:
        print(json.dumps(view(sys.argv[1], sys.argv[2], json.loads(sys.argv[3]) if len(sys.argv) > 3 else {})))
    except Exception as e:
        print(str(e), file=sys.stderr); sys.exit(1)
