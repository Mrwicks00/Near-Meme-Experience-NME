// Same-origin pass-through to the two local NME services, so the browser never makes a cross-port request
// (CORS, extensions and Chrome's local-network rules can block those). Only the known routes are forwarded.
const TARGETS: Record<string, { base: string; routes: RegExp }> = {
  intent: { base: process.env.INTENT_URL || "http://localhost:8788", routes: /^(launch\/(parse|icon|preview)|trade\/parse|health|proof\/[A-Za-z0-9_-]{8,80})$/ },
  relayer: { base: process.env.RELAYER_URL || "http://localhost:8787", routes: /^(health|config|message|action|claim|claim_deposit|execute_claim|play\/(session|topup)|wallet\/[A-Za-z0-9:]{10,80})$/ },
};

async function forward(req: Request, ctx: { params: Promise<{ svc: string; path: string[] }> }) {
  const { svc, path } = await ctx.params;
  const t = TARGETS[svc];
  const p = path.join("/");
  if (!t || !t.routes.test(p)) return Response.json({ error: "not found" }, { status: 404 });
  const body = req.method === "POST" ? await req.text() : undefined;
  if (body && body.length > 64 * 1024) return Response.json({ error: "request too large" }, { status: 413 });
  try {
    const r = await fetch(`${t.base}/${p}${new URL(req.url).search}`, {
      method: req.method,
      headers: {
        ...(body ? { "content-type": "application/json" } : {}),
        // Shared secret with the services (server env only, never sent to the browser) and the visitor's IP for their rate limits.
        ...(process.env.SVC_TOKEN ? { "x-nme-token": process.env.SVC_TOKEN } : {}),
        "x-forwarded-for": (req.headers.get("x-forwarded-for") ?? "").split(",")[0]!.trim() || "unknown",
      },
      body,
      signal: AbortSignal.timeout(svc === "relayer" ? 300_000 : 90_000),
      cache: "no-store",
    });
    return new Response(await r.text(), { status: r.status, headers: { "content-type": "application/json" } });
  } catch (e) {
    const down = /ECONNREFUSED|fetch failed/i.test(String((e as Error)?.cause ?? e));
    const hint = svc === "intent" ? "cd services/intent && npm start" : "cd services/relayer && npm start";
    return Response.json(
      { error: down ? `The ${svc === "intent" ? "AI helper" : "relayer"} isn't running. Start it with: ${hint}` : `The ${svc === "intent" ? "AI helper" : "relayer"} took too long to answer. Try again.` },
      { status: 503 },
    );
  }
}

export const GET = forward;
export const POST = forward;
