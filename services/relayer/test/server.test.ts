import assert from "node:assert/strict";
import type { AddressInfo } from "node:net";
import { test } from "node:test";
import { Relayer } from "../src/relayer.js";
import { makeServer } from "../src/server.js";
import { FakeChain, SOL_OWNER, signText } from "./helpers.js";

test("HTTP: health, config, bad input, big bodies, unknown paths, and a signed action end to end", async () => {
  const chain = new FakeChain();
  const relayer = new Relayer(chain, { ratePerMin: 5, ratePerDay: 50, ipPerMin: 50, globalPerMin: 50, gateway: "gateway.nme.testnet", arena: "arena.nme.testnet", factory: "nme.testnet", registrationYocto: 1n }, async () => {});
  const server = makeServer(relayer, { corsOrigin: "*", trustProxy: false });
  await new Promise<void>((r) => server.listen(0, r));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const post = (p: string, body: unknown) => fetch(base + p, { method: "POST", headers: { "content-type": "application/json" }, body: typeof body === "string" ? body : JSON.stringify(body) });
  try {
    assert.deepEqual(await (await fetch(base + "/health")).json(), { ok: true });
    assert.equal(((await (await fetch(base + "/config")).json()) as any).gateway, "gateway.nme.testnet");
    assert.equal((await fetch(base + "/nope")).status, 404);
    assert.equal((await post("/action", "not json")).status, 400);
    assert.equal((await post("/action", { owner: "alice.testnet" })).status, 400);
    assert.equal((await post("/action", "x".repeat(20_000))).status, 413);
    const exp = Date.now() + 60_000;
    const action = { FundChest: { ticker: "pepe", amount: "5" } };
    const text = `TEXT|${SOL_OWNER}|${JSON.stringify(action)}|0|${exp}`;
    const res = await post("/action", { owner: SOL_OWNER, action, nonce: 0, expires_ms: exp, signature: signText(SOL_OWNER, text) });
    assert.equal(res.status, 200);
    assert.equal(chain.calls.at(-1)!.method, "wallet_action");
    const w: any = await (await fetch(`${base}/wallet/${encodeURIComponent(SOL_OWNER)}`)).json();
    assert.equal(w.nonce, 0);
    assert.equal((await fetch(`${base}/wallet/alice.testnet`)).status, 400);
  } finally {
    server.close();
  }
});
