// Local signature check (the contract verifies again; this only stops the relayer paying gas for a forgery).
import { createPublicKey, verify } from "node:crypto";
import { secp256k1 } from "@noble/curves/secp256k1";
import { keccak_256 } from "@noble/hashes/sha3";
import bs58 from "bs58";

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export function verifySolana(owner: string, text: string, sig: Uint8Array): boolean {
  try {
    const pk = bs58.decode(owner.slice(4));
    if (pk.length !== 32 || sig.length !== 64) return false;
    const key = createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, Buffer.from(pk)]), format: "der", type: "spki" });
    return verify(null, Buffer.from(text, "utf8"), key, Buffer.from(sig));
  } catch {
    return false;
  }
}

/** EIP-191 personal_sign: signature = r || s || v (v is 27/28 or 0/1). Recovers the address and compares it to the owner. */
export function verifyEvm(owner: string, text: string, sig: Uint8Array): boolean {
  try {
    if (sig.length !== 65) return false;
    const body = Buffer.from(text, "utf8");
    const digest = keccak_256(Buffer.concat([Buffer.from(`\x19Ethereum Signed Message:\n${body.length}`, "utf8"), body]));
    let v = sig[64]!;
    if (v >= 27) v -= 27;
    if (v !== 0 && v !== 1) return false;
    const s = secp256k1.Signature.fromCompact(Buffer.from(sig.slice(0, 64)).toString("hex")).addRecoveryBit(v);
    const pub = s.recoverPublicKey(digest).toRawBytes(false); // 65 bytes, 0x04 || x || y
    const addr = "0x" + Buffer.from(keccak_256(pub.slice(1)).slice(12)).toString("hex");
    return `evm:${addr}` === owner;
  } catch {
    return false;
  }
}

export function verifyOwner(owner: string, text: string, sig: Uint8Array): boolean {
  return owner.startsWith("sol:") ? verifySolana(owner, text, sig) : verifyEvm(owner, text, sig);
}
