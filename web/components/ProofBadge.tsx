"use client";
// "Proof of Thought": one click checks that an AI text came from a NEAR AI model running in an attested TEE and that
// it is the very answer the arena recorded on chain. The signature is recovered here, in the visitor's browser.
import { useState } from "react";
import { Check, ProofData, checkProof, fetchProof } from "@/lib/proof";
import { NME } from "@/lib/config";
import { view } from "@/lib/rpc";

/** `commentarySeq`: a war comment, whose hashes are read from the arena's `commentary_proof` on click. */
export function ProofBadge({ chatId, model, requestHash, responseHash, commentarySeq }: { chatId: string; model: string; requestHash?: string; responseHash?: string; commentarySeq?: number }) {
  const [state, setState] = useState<"idle" | "loading" | "done" | "error">("idle");
  const [checks, setChecks] = useState<Check[]>([]);
  const [proof, setProof] = useState<ProofData | null>(null);
  const [err, setErr] = useState("");
  if (!chatId) return null;

  const run = async () => {
    setState("loading");
    try {
      const [p, onChain] = await Promise.all([
        fetchProof(chatId, model),
        commentarySeq !== undefined
          ? view<{ request_hash: string; response_hash: string } | null>(NME.arena, "commentary_proof", { seq: commentarySeq }).catch(() => null)
          : Promise.resolve(requestHash && responseHash ? { request_hash: requestHash, response_hash: responseHash } : null),
      ]);
      setProof(p);
      setChecks(checkProof(p, { model, request_hash: onChain?.request_hash, response_hash: onChain?.response_hash }));
      setState("done");
    } catch (e) {
      setErr((e as Error).message);
      setState("error");
    }
  };

  const allOk = checks.length > 0 && checks.every((c) => c.ok !== false);
  return (
    <div className="mt-2 text-xs">
      {state === "idle" && (
        <button onClick={run} className="chip bg-lilac text-ink hover:brightness-110" title="Check that this text came from a NEAR AI model in an attested TEE">
          🔒 Proof of Thought · verify
        </button>
      )}
      {state === "loading" && <span className="chip bg-paper">Checking NEAR AI&apos;s signature…</span>}
      {state === "error" && (
        <span className="chip bg-paper" title={err}>Proof not available right now ({err.slice(0, 60)}) · <button className="underline" onClick={run}>retry</button></span>
      )}
      {state === "done" && (
        <div className="mt-1 rounded-xl border border-ink/40 bg-paper p-3">
          <p className={`font-semibold ${allOk ? "" : "text-red"}`}>{allOk ? "🔒 Verified: written by a NEAR AI model inside an attested TEE" : "Not verified"}</p>
          <ul className="mt-2 space-y-1">
            {checks.map((c) => (
              <li key={c.label} className="flex gap-2">
                <span aria-hidden>{c.ok === true ? "✅" : c.ok === false ? "❌" : "➖"}</span>
                <span>{c.label}</span>
              </li>
            ))}
          </ul>
          {proof && (
            <details className="mt-2">
              <summary className="cursor-pointer font-semibold">Raw proof</summary>
              <dl className="mt-1 space-y-1 break-all font-mono text-[0.68rem]">
                <div><dt className="font-sans font-semibold">Signed statement</dt><dd>{proof.text}</dd></div>
                <div><dt className="font-sans font-semibold">Signer (TEE key)</dt><dd>{proof.signing_address}</dd></div>
                <div><dt className="font-sans font-semibold">Signature</dt><dd>{proof.signature}</dd></div>
                <div><dt className="font-sans font-semibold">NEAR AI chat id</dt><dd>{proof.chat_id}</dd></div>
              </dl>
              {proof.intel_quote && (
                <p className="mt-2 font-sans">
                  Check the hardware yourself: <button className="underline" onClick={() => navigator.clipboard?.writeText(proof.intel_quote!).catch(() => {})}>copy the Intel TDX quote</button> and paste it at{" "}
                  <a className="underline" href="https://proof.t16z.com/" target="_blank" rel="noreferrer">proof.t16z.com</a>.
                </p>
              )}
            </details>
          )}
        </div>
      )}
    </div>
  );
}
