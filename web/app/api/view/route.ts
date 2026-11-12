// Same-origin view proxy: every browser asks us, we ask NEAR once per 2.5 s per (contract, method, args).
// Keeps public RPCs from rate limiting viewers (their 429 replies carry no CORS headers, so browsers can't even read them).
import { NME } from "@/lib/config";

type Entry = { at: number; body: string; status: number };
const cache = new Map<string, Entry>();
const inflight = new Map<string, Promise<Entry>>();
const TTL = 2500;
const ALLOWED = /^[a-z0-9_.-]{2,64}$/;

async function rpc(payload: object): Promise<Entry> {
  let last = "no RPC answered";
  for (const url of NME.rpcs) {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), 8000);
    try {
      const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(payload), signal: ctl.signal, cache: "no-store" });
      if (r.status === 429 || r.status >= 500) { last = `${url} answered ${r.status}`; continue; }
      return { at: Date.now(), body: await r.text(), status: 200 };
    } catch (e) {
      last = `${url}: ${(e as Error).message}`;
    } finally {
      clearTimeout(t);
    }
  }
  return { at: Date.now(), body: JSON.stringify({ error: last }), status: 502 };
}

export async function POST(req: Request) {
  let q: { contract?: string; method?: string; args?: unknown; keys?: string };
  try { q = await req.json(); } catch { return Response.json({ error: "bad json" }, { status: 400 }); }
  const target = q.keys ?? q.contract;
  if (!target || !ALLOWED.test(target) || (!q.keys && (!q.method || !/^[a-z_][a-z0-9_]{0,63}$/.test(q.method)))) {
    return Response.json({ error: "bad request" }, { status: 400 });
  }
  const args = JSON.stringify(q.args ?? {});
  if (args.length > 2000) return Response.json({ error: "args too large" }, { status: 400 });

  const key = q.keys ? `keys:${q.keys}` : `${q.contract}|${q.method}|${args}`;
  const hit = cache.get(key);
  if (hit && Date.now() - hit.at < TTL) return new Response(hit.body, { status: hit.status, headers: { "content-type": "application/json" } });

  let p = inflight.get(key);
  if (!p) {
    const params = q.keys
      ? { request_type: "view_access_key_list", finality: "final", account_id: q.keys }
      : { request_type: "call_function", finality: "optimistic", account_id: q.contract, method_name: q.method, args_base64: Buffer.from(args).toString("base64") };
    p = rpc({ jsonrpc: "2.0", id: "nme", method: "query", params }).finally(() => inflight.delete(key));
    inflight.set(key, p);
  }
  const e = await p;
  if (e.status === 200) cache.set(key, e);
  if (cache.size > 2000) cache.clear();
  return new Response(e.body, { status: e.status, headers: { "content-type": "application/json" } });
}
