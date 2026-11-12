#!/usr/bin/env python3
"""Where to send devnet SOL so the gateway buys a coin for a NEAR account.
  scripts/solana_deposit_address.py <beneficiary.testnet> <ticker> [min_out_raw=1] [--chest]
Prints the derivation path (from the gateway) and the Solana address (from the MPC contract). Send SOL from Phantom
(devnet) to that address, then run scripts/relay_claim.py with the transaction signature."""
import json, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from nview import view

GATEWAY, MPC = "gateway.nme.testnet", "v1.signer-prod.testnet"
args = [a for a in sys.argv[1:] if not a.startswith("--")]
ben, ticker = args[0], args[1]
intent = {"FundChest": {"ticker": ticker}} if "--chest" in sys.argv else {"Buy": {"ticker": ticker, "min_out": args[2] if len(args) > 2 else "1"}}
path = view(GATEWAY, "deposit_path", {"beneficiary": ben, "intent": intent})
key = view(MPC, "derived_public_key", {"path": path, "predecessor": GATEWAY, "domain_id": 1})
print(json.dumps({"intent": intent, "path": path, "solana_address": key.removeprefix("ed25519:")}, indent=1))
