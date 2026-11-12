// Proof of Thought, checked in the browser: recover who signed NEAR AI's statement `{model}:{request_hash}:{response_hash}`
// (an Ethereum-style personal-message signature by a key inside the model's TEE) and compare it with the keys the model's
// hardware attestation vouches for and with the hashes the arena recorded on chain.
import { secp256k1 } from "@noble/curves/secp256k1.js";
import { keccak_256 } from "@noble/hashes/sha3";
import { NME } from "./config";

export type ProofData = {
  chat_id: string; model: string; text: string; signature: string; signing_address: string; signature_kind: string | null;
  attested_addresses: string[]; attestation_nonce: string; intel_quote: string | null;
};

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const unhex = (h: string) => Uint8Array.from((h.replace(/^0x/, "").match(/../g) ?? []).map((x) => parseInt(x, 16)));

/** EIP-191 personal_sign recovery: the 0x address that signed `text`. */
export function recoverSigner(text: string, signature: string): string {
  const body = new TextEncoder().encode(text);
  const prefix = new TextEncoder().encode(`\x19Ethereum Signed Message:\n${body.length}`);
  const msg = new Uint8Array(prefix.length + body.length);
  msg.set(prefix);
  msg.set(body, prefix.length);
  const digest = keccak_256(msg);
  const sig = unhex(signature);
  if (sig.length !== 65) throw new Error("signature is not 65 bytes");
  const v = sig[64] >= 27 ? sig[64] - 27 : sig[64];
  const pub = secp256k1.Signature.fromBytes(sig.slice(0, 64), "compact").addRecoveryBit(v).recoverPublicKey(digest).toBytes(false);
  return `0x${hex(keccak_256(pub.slice(1)).slice(-20))}`;
}

export type Check = { ok: boolean | null; label: string };
/** The checks shown to the user; `null` = not applicable (e.g. an older delivery without recorded hashes). */
export function checkProof(p: ProofData, onChain: { model: string; request_hash?: string; response_hash?: string }): Check[] {
  let recovered = "";
  try {
    recovered = recoverSigner(p.text, p.signature).toLowerCase();
  } catch {
    recovered = "";
  }
  const parts = p.text.split(":");
  const [rq, rs] = [parts[parts.length - 2] ?? "", parts[parts.length - 1] ?? ""];
  const hasChain = !!onChain.request_hash && !!onChain.response_hash;
  return [
    { ok: recovered === p.signing_address.toLowerCase(), label: "NEAR AI's signature checks out (verified in your browser)" },
    { ok: p.attested_addresses.includes(recovered), label: "The signing key belongs to an attested TEE (Intel TDX + NVIDIA) running this model" },
    { ok: parts.length < 3 || parts[0] === onChain.model, label: `The signed run used ${onChain.model}` },
    {
      ok: hasChain ? rq === onChain.request_hash && rs === onChain.response_hash : null,
      label: hasChain ? "The signed request and answer match the hashes the arena stored on chain" : "Older delivery: its hashes were not recorded on chain",
    },
  ];
}

export async function fetchProof(chatId: string, model: string): Promise<ProofData> {
  const r = await fetch(`${NME.intentUrl}/proof/${encodeURIComponent(chatId)}?model=${encodeURIComponent(model)}`, { signal: AbortSignal.timeout(60_000) });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j?.error ?? `HTTP ${r.status}`);
  return j as ProofData;
}
