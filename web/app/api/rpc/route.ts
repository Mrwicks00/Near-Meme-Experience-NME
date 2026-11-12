// Same-origin JSON-RPC pass-through for accounts that sign in this browser (Google sign-in accounts, lib/play.ts):
// reads and broadcasts go through here, tried on each configured RPC in turn, so a phone never hits a public RPC's
// rate limit (or its missing CORS headers) directly. Only the methods a transaction needs are forwarded.
import { NME } from "@/lib/config";

const METHODS = new Set(["query", "block", "send_tx", "broadcast_tx_commit", "broadcast_tx_async", "tx", "EXPERIMENTAL_tx_status", "gas_price", "status", "EXPERIMENTAL_protocol_config"]);
// Broadcasting the same signed transaction twice is harmless (one nonce), so a failed RPC is retried on the next one.
const SLOW = new Set(["send_tx", "broadcast_tx_commit", "tx", "EXPERIMENTAL_tx_status"]);

export async function POST(req: Request) {
  const text = await req.text();
  if (text.length > 64 * 1024) return Response.json({ error: "request too large" }, { status: 413 });
  let body: { method?: string };
  try {
    body = JSON.parse(text);
  } catch {
    return Response.json({ error: "bad json" }, { status: 400 });
  }
  if (!body.method || !METHODS.has(body.method)) return Response.json({ error: "method not allowed" }, { status: 400 });
  let last = "no RPC answered";
  for (const url of NME.rpcs) {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), SLOW.has(body.method) ? 60_000 : 10_000);
    try {
      const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: text, signal: ctl.signal, cache: "no-store" });
      if (r.status === 429 || r.status >= 500) { last = `${url} answered ${r.status}`; continue; }
      return new Response(await r.text(), { status: 200, headers: { "content-type": "application/json" } });
    } catch (e) {
      last = `${url}: ${(e as Error).message}`;
    } finally {
      clearTimeout(t);
    }
  }
  return Response.json({ jsonrpc: "2.0", id: null, error: { code: -32000, message: last } }, { status: 502 });
}
