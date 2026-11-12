#!/usr/bin/env python3
"""hash.py <file> -> prints "<sha256-hex> <base58>" (base58 is what contract args use for CryptoHash)."""
import hashlib, sys
A = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
d = hashlib.sha256(open(sys.argv[1], "rb").read()).digest()
n = int.from_bytes(d, "big"); out = ""
while n: n, r = divmod(n, 58); out = A[r] + out
out = "1" * (len(d) - len(d.lstrip(b"\0"))) + out
print(d.hex(), out)
