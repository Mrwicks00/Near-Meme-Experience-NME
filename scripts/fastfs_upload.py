#!/usr/bin/env python3
"""Upload a wasm to FastFS via OutLayer (docs/spikes/S4.md finding 2) and print its URL + sha256.
  scripts/fastfs_upload.py <file.wasm> <signer.testnet>
The payload is Borsh: [0u8] ++ str(relative_path) ++ Some(str(mime) ++ Vec<u8>); the call is expected to report
'Exceeded the prepaid gas' (gas is 1 Ggas on purpose): FastFS indexes the call data, not the execution."""
import hashlib, os, struct, subprocess, sys, re, tempfile

def s(b: bytes) -> bytes:
    return struct.pack("<I", len(b)) + b

def main():
    path, signer = sys.argv[1], sys.argv[2]
    data = open(path, "rb").read()
    sha = hashlib.sha256(data).hexdigest()
    rel = f"{sha}.wasm"
    payload = b"\x00" + s(rel.encode()) + b"\x01" + s(b"application/wasm") + s(data)
    if len(payload) > 1_000_000:
        sys.exit("file > 1 MB: needs the chunked Partial variant (not implemented)")
    with tempfile.NamedTemporaryFile(delete=False) as f:  # the CLI limits one argument to 128 KB: pass the payload as a file
        f.write(payload)
    out = subprocess.run(["near", "contract", "call-function", "as-transaction", "outlayer.testnet", "__fastdata_fastfs", "file-args",
                          f.name, "prepaid-gas", "1 Ggas", "attached-deposit", "0 NEAR", "sign-as", signer,
                          "network-config", "testnet", "sign-with-keychain", "send"], capture_output=True, text=True)
    os.unlink(f.name)
    text = re.sub(r"\x1b\[[0-9;]*m", "", out.stdout + out.stderr)
    m = re.search(r"Transaction ID: (\w+)", text)
    print(f"sha256 {sha}\nurl    https://test.fastfs.io/{signer}/outlayer.testnet/{rel}\ntx     {m.group(1) if m else '?'}")

main()
