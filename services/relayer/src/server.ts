import { timingSafeEqual } from "node:crypto";
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { Relayer, Reply } from "./relayer.js";
import { BadRequest } from "./validate.js";

const MAX_BODY = 16 * 1024;

function readBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => {
      size += c.length;
      if (size > 1024 * 1024) req.destroy(); // never drain more than 1 MB of an oversized body
      else if (size <= MAX_BODY) chunks.push(c);
    });
    req.on("end", () => {
      if (size > MAX_BODY) return reject(new BadRequest("body too large", 413));
      try {
        resolve(chunks.length ? JSON.parse(Buffer.concat(chunks).toString("utf8")) : {});
      } catch {
        reject(new BadRequest("body is not JSON"));
      }
    });
    req.on("error", reject);
  });
}

/** Constant-time check of the shared secret between the site's server proxy and this service. */
export function tokenOk(expected: string, got: unknown): boolean {
  if (!expected) return true;
  if (typeof got !== "string") return false;
  const a = Buffer.from(expected);
  const b = Buffer.from(got);
  return a.length === b.length && timingSafeEqual(a, b);
}

export function makeServer(relayer: Relayer, opts: { corsOrigin: string; trustProxy: boolean; token?: string }): Server {
  return createServer(async (req, res) => {
    const send = (r: Reply) => {
      res.writeHead(r.status, { "content-type": "application/json", "access-control-allow-origin": opts.corsOrigin, "access-control-allow-headers": "content-type, x-nme-token", "access-control-allow-methods": "GET,POST,OPTIONS" });
      res.end(JSON.stringify(r.body));
    };
    try {
      if (req.method === "OPTIONS") return send({ status: 204, body: {} });
      const url = new URL(req.url ?? "/", "http://x");
      if (url.pathname !== "/health" && !tokenOk(opts.token ?? "", req.headers["x-nme-token"])) return send({ status: 401, body: { error: "unauthorized" } });
      const fwd = opts.trustProxy ? String(req.headers["x-forwarded-for"] ?? "").split(",")[0]?.trim() : "";
      const ip = fwd || req.socket.remoteAddress || "unknown";
      if (req.method === "GET") {
        if (url.pathname === "/health") return send({ status: 200, body: { ok: true } });
        if (url.pathname === "/config") return send(relayer.config());
        const m = /^\/wallet\/([^/]+)$/.exec(url.pathname);
        if (m) return send(await relayer.wallet(decodeURIComponent(m[1]!), url.searchParams.get("token") ?? undefined));
      } else if (req.method === "POST") {
        const body = await readBody(req);
        if (url.pathname === "/action") return send(await relayer.action(body, ip));
        if (url.pathname === "/claim") return send(await relayer.claim(body, ip));
        if (url.pathname === "/claim_deposit") return send(await relayer.claimDeposit(body, ip));
        if (url.pathname === "/execute_claim") return send(await relayer.executeClaim(body, ip));
        if (url.pathname === "/message") return send(await relayer.message(body));
      }
      send({ status: 404, body: { error: "not found" } });
    } catch (e) {
      if (e instanceof BadRequest) return send({ status: e.status, body: { error: e.message } });
      console.error("internal error:", e);
      send({ status: 500, body: { error: "internal error" } });
    }
  });
}
