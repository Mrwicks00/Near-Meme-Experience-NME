import { createServer, type IncomingMessage, type Server } from "node:http";
import { parseTrade } from "./trade.js";
import { DraftError } from "./draft.js";
import { parseLaunch, preview, type Chain, type Llm } from "./launch.js";
import type { IconMaker } from "./nearai.js";

const MAX_BODY = 64 * 1024;

export interface Deps { llm: Llm; icon: IconMaker; chain: Chain; root: string; wrap: string; corsOrigin: string; perMin: number }

function readBody(req: IncomingMessage): Promise<any> {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => { size += c.length; if (size > 1024 * 1024) req.destroy(); else if (size <= MAX_BODY) chunks.push(c); });
    req.on("end", () => {
      if (size > MAX_BODY) return reject(new DraftError("body too large"));
      try { resolve(chunks.length ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : {}); } catch { reject(new DraftError("body is not JSON")); }
    });
    req.on("error", reject);
  });
}

/** Sliding one-minute window per client address (these routes cost model credits). */
function limiter(perMin: number, now: () => number) {
  const hits = new Map<string, number[]>();
  return (ip: string) => {
    const t = now(), w = (hits.get(ip) ?? []).filter((x) => t - x < 60_000);
    if (w.length >= perMin) { hits.set(ip, w); return false; }
    w.push(t); hits.set(ip, w); return true;
  };
}

export function makeServer(d: Deps, now: () => number = Date.now): Server {
  const allow = limiter(d.perMin, now);
  return createServer(async (req, res) => {
    const send = (status: number, body: object) => {
      res.writeHead(status, { "content-type": "application/json", "access-control-allow-origin": d.corsOrigin, "access-control-allow-headers": "content-type" });
      res.end(JSON.stringify(body));
    };
    try {
      if (req.method === "OPTIONS") return send(204, {});
      const url = req.url ?? "";
      if (req.method === "GET" && url === "/health") return send(200, { ok: true });
      if (req.method !== "POST") return send(404, { error: "not found" });
      if (!["/launch/parse", "/launch/icon", "/launch/preview", "/trade/parse"].includes(url)) return send(404, { error: "not found" });
      if (!allow(req.socket.remoteAddress ?? "?")) return send(429, { error: "too many requests, wait a minute" });
      const b = await readBody(req);
      if (url === "/launch/parse") return send(200, await parseLaunch(b?.text, d.llm));
      if (url === "/trade/parse") return send(200, await parseTrade(b?.text, d.llm));
      if (url === "/launch/icon") {
        if (typeof b?.prompt !== "string" || !b.prompt.trim() || b.prompt.length > 300) throw new DraftError("prompt must be 1 to 300 characters");
        return send(200, { icon_data_url: await d.icon(b.prompt.trim()) });
      }
      return send(200, await preview(b?.draft, b?.icon_data_url, d.chain, d.root, d.wrap));
    } catch (e) {
      if (e instanceof DraftError) return send(400, { error: e.message });
      console.error("internal error:", e);
      send(502, { error: "upstream service failed, try again" });
    }
  });
}
