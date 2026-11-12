// Before the relayer starts a deposit claim it checks, on the source chain, that the transaction exists and succeeded. A claim for a
// made-up transaction id would cost the gateway an OutLayer fee; this keeps the relayer from starting such claims. (Anyone can still
// call the contract directly: this protects the relayer path only. The TEE verifier remains the authority.)
import { BadRequest } from "./validate.js";

async function rpc(url: string, body: object): Promise<any> {
  let lastErr: unknown;
  for (let i = 0; i < 2; i++) {
    try {
      const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), signal: AbortSignal.timeout(8000) });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      return await r.json();
    } catch (e) {
      lastErr = e;
    }
  }
  throw new BadRequest(`could not check the transaction on the source chain (${String(lastErr).slice(0, 80)}), try again`, 502);
}

export type Precheck = (chain: "solana" | "evm", txId: string) => Promise<void>;

export function makePrecheck(solanaRpc: string, evmRpc: string): Precheck {
  return async (chain, txId) => {
    if (chain === "solana") {
      const j = await rpc(solanaRpc, { jsonrpc: "2.0", id: 1, method: "getSignatureStatuses", params: [[txId], { searchTransactionHistory: true }] });
      const st = j?.result?.value?.[0];
      if (!st) throw new BadRequest("that Solana transaction was not found (devnet)");
      if (st.err) throw new BadRequest("that Solana transaction failed");
      if (st.confirmationStatus !== "finalized") throw new BadRequest("that Solana transaction is not finalized yet, try again in a minute", 409);
      return;
    }
    const j = await rpc(evmRpc, { jsonrpc: "2.0", id: 1, method: "eth_getTransactionReceipt", params: [txId] });
    const rc = j?.result;
    if (!rc) throw new BadRequest("that EVM transaction was not found (Sepolia) or is not mined yet", 409);
    if (rc.status !== "0x1") throw new BadRequest("that EVM transaction failed");
  };
}
