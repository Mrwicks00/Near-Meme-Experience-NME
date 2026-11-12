import { NME } from "./config";

export class RpcError extends Error {
  constructor(message: string, readonly kind: "network" | "contract") {
    super(message);
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
let preferred = 0;
// Shared back-off after a 429 from a public RPC (5 s doubling to 60 s).
let coolUntil = 0;
let coolMs = 0;
// The same-origin proxy (app/api/view) is tried first; if it's missing (static hosting) we stop asking it.
let proxyOk = typeof window !== "undefined";

function b64(s: string) {
  if (typeof window === "undefined") return Buffer.from(s).toString("base64");
  return btoa(unescape(encodeURIComponent(s)));
}

async function post(url: string, body: unknown, timeoutMs: number) {
  const ctl = new AbortController();
  const t = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), signal: ctl.signal });
    if (r.status === 429) {
      coolMs = Math.min(60000, coolMs ? coolMs * 2 : 5000);
      coolUntil = Date.now() + coolMs;
      throw new RpcError("RPC is rate limiting us, slowing down", "network");
    }
    if (r.status === 404 && url.startsWith("/")) { proxyOk = false; throw new RpcError("no proxy", "network"); }
    if (!r.ok) throw new RpcError(`RPC ${r.status}`, "network");
    coolMs = 0;
    return await r.json();
  } finally {
    clearTimeout(t);
  }
}

/** Raw `query` with 8 s timeout, retries with backoff, proxy first, then public RPC fallback. */
async function query(proxyBody: object, params: object): Promise<{ result?: Record<string, unknown> & { error?: string } }> {
  let lastErr: unknown;
  for (let attempt = 0; attempt < 4; attempt++) {
    try {
      if (proxyOk && attempt < 2) {
        const j = await post("/api/view", proxyBody, 10000);
        if (j.error && !j.result) throw new RpcError(String(j.error), "network");
        return j;
      }
      if (coolUntil > Date.now()) await sleep(coolUntil - Date.now());
      const idx = (preferred + attempt) % NME.rpcs.length;
      const j = await post(NME.rpcs[idx], { jsonrpc: "2.0", id: "nme", method: "query", params }, 8000);
      preferred = idx;
      return j;
    } catch (e) {
      lastErr = e;
      await sleep(400 * (attempt + 1));
    }
  }
  throw new RpcError(`Can't reach NEAR testnet RPC (${String((lastErr as Error)?.message ?? lastErr)})`, "network");
}

/** Contract view call. Contract errors (panics, missing methods) throw RpcError("contract") and are not retried. */
export async function view<T>(contractId: string, method: string, args: object = {}): Promise<T> {
  const params = { request_type: "call_function", finality: "optimistic", account_id: contractId, method_name: method, args_base64: b64(JSON.stringify(args)) };
  const j = await query({ contract: contractId, method, args }, params) as { error?: unknown; result?: { error?: unknown; result: number[] } };
  const err = j.error ?? j.result?.error;
  if (err) throw new RpcError(`${contractId}.${method}: ${typeof err === "string" ? err : JSON.stringify(err)}`.slice(0, 300), "contract");
  return JSON.parse(new TextDecoder().decode(new Uint8Array(j.result!.result))) as T;
}

/** Number of access keys on an account. NME coin accounts are created with none: nobody can sign as the coin. */
export async function accessKeyCount(accountId: string): Promise<number> {
  const params = { request_type: "view_access_key_list", finality: "final", account_id: accountId };
  const j = await query({ keys: accountId }, params) as { error?: unknown; result?: { keys: unknown[] } };
  if (j.error || !j.result?.keys) throw new RpcError("access key query failed", "contract");
  return j.result.keys.length;
}
