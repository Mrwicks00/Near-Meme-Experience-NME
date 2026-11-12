import assert from "node:assert/strict";
import { test } from "node:test";
import { hiredJobId, Relayer } from "../src/relayer.js";
import { BadRequest } from "../src/validate.js";
import { EVM_OWNER, FakeChain, SOL_OWNER, signText } from "./helpers.js";

const NOW = 1_794_000_000_000;
const cfg = { ratePerMin: 3, ratePerDay: 100, ipPerMin: 30, globalPerMin: 100, gateway: "gateway.nme.testnet", arena: "arena.nme.testnet", factory: "nme.testnet", registrationYocto: 1_250_000_000_000_000_000_000n };
const ok = async () => {};
const mk = (chain = new FakeChain(), precheck = ok) => ({ chain, r: new Relayer(chain, cfg, precheck, () => NOW, 5) });

function req(chain: FakeChain, owner: string, action: any, over: Record<string, unknown> = {}) {
  const nonce = (over.nonce as number) ?? chain.state.wallet.nonce;
  const expires_ms = (over.expires_ms as number) ?? NOW + 60_000;
  const text = `TEXT|${owner}|${JSON.stringify(action)}|${nonce}|${expires_ms}`;
  return { owner, action, nonce, expires_ms, signature: signText(owner, text), ...over };
}
const buy = { Buy: { key: "pepe", amount: "2000000000000000000000000", min_out: "1" } };
const sell = { Sell: { token: "pepe.nme.testnet", amount: "400", min_out: "1" } };
const fails = (p: Promise<unknown>, status: number, re: RegExp) => assert.rejects(p, (e: unknown) => e instanceof BadRequest && e.status === status && re.test(e.message));

test("a signed Buy registers the gateway on the token once, then submits exactly what was signed", async () => {
  for (const owner of [SOL_OWNER, EVM_OWNER]) {
    const { chain, r } = mk();
    const body = req(chain, owner, buy);
    const out = await r.action(body, "1.1.1.1");
    assert.equal(out.status, 200);
    assert.deepEqual(chain.calls.map((c) => c.method), ["register_token", "wallet_action"]);
    assert.deepEqual(chain.calls[0]!.args, { token: "pepe.nme.testnet" });
    assert.equal(chain.calls[0]!.deposit, 1_250_000_000_000_000_000_000n);
    assert.deepEqual(chain.calls[1]!.args, { owner, action: buy, nonce: 0, expires_ms: NOW + 60_000, signature: body.signature });
    assert.equal(chain.calls[1]!.gas, 300);
    await r.action(req(chain, owner, buy, { nonce: 0 }), "1.1.1.1");
    assert.equal(chain.calls.filter((c) => c.method === "register_token").length, 1, "registered once, then cached");
  }
});

test("a Sell makes sure the gateway can receive wNEAR; a NEAR-balance action needs no registration", async () => {
  const { chain, r } = mk();
  await r.action(req(chain, SOL_OWNER, sell), "ip");
  assert.deepEqual(chain.calls.map((c) => [c.method, c.args.token]), [["register_token", "wrap.testnet"], ["wallet_action", undefined]]);
  const c2 = mk();
  await c2.r.action(req(c2.chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip");
  assert.deepEqual(c2.chain.calls.map((c) => c.method), ["wallet_action"]);
});

test("nothing is submitted (no gas) for requests that are certain to fail", async () => {
  const cases: [string, (c: FakeChain) => any, number, RegExp][] = [
    ["forged: signed by the other wallet", (c) => req(c, SOL_OWNER, buy, { signature: signText(EVM_OWNER, "x") }), 403, /signature/],
    ["forged: another action than signed", (c) => ({ ...req(c, SOL_OWNER, buy), action: { Buy: { key: "pepe", amount: "9", min_out: "1" } } }), 403, /signature/],
    ["wrong nonce", (c) => req(c, SOL_OWNER, buy, { nonce: 5 }), 409, /nonce/],
    ["expired", (c) => req(c, SOL_OWNER, buy, { expires_ms: NOW - 1 }), 400, /expired/],
    ["expiry too far", (c) => req(c, SOL_OWNER, buy, { expires_ms: NOW + 25 * 3600_000 }), 400, /24 hours/],
    ["not enough NEAR", (c) => { c.state.wallet.near = "5"; return req(c, SOL_OWNER, buy); }, 409, /not enough NEAR/],
    ["not enough tokens", (c) => { c.state.tokens = "399"; return req(c, SOL_OWNER, sell); }, 409, /not enough tokens/],
    ["token operation running", (c) => { c.state.lock = { owner: "x", until_ms: 1 }; return req(c, SOL_OWNER, buy); }, 409, /running/],
    ["unknown coin", (c) => { c.state.coin = null; return req(c, SOL_OWNER, buy); }, 404, /unknown coin/],
    ["extra field", (c) => ({ ...req(c, SOL_OWNER, buy), admin: true }), 400, /unexpected field/],
  ];
  for (const [name, build, status, re] of cases) {
    const { chain, r } = mk();
    const body = build(chain);
    await fails(r.action(body, "ip"), status, re).catch((e) => { throw new Error(`${name}: ${e.message}`); });
    assert.equal(chain.calls.length, 0, `${name}: no transaction was sent`);
  }
});

test("the relay fee is required for NEAR-balance actions, not for buys, sells or token transfers", async () => {
  const chain = new FakeChain();
  chain.state.wallet.near = "5"; // exactly the amount, no room for the fee
  const { r } = mk(chain);
  await fails(r.action(req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip"), 409, /not enough NEAR/);
  chain.state.wallet.near = "2000000000000000000000000"; // exactly the buy amount
  await r.action(req(chain, SOL_OWNER, buy), "ip");
  assert.equal(chain.calls.at(-1)!.method, "wallet_action");
});

test("per-wallet rate limit: only requests that would cost gas count; other wallets are unaffected", async () => {
  const { chain, r } = mk();
  for (let i = 0; i < 3; i++) await r.action(req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip");
  await fails(r.action(req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip"), 429, /rate limit/);
  await r.action(req(chain, EVM_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip2"); // another wallet, another ip
  const before = chain.calls.length;
  const c2 = mk();
  for (let i = 0; i < 10; i++) await fails(c2.r.action(req(c2.chain, SOL_OWNER, buy, { nonce: 9 }), "ip"), 409, /nonce/); // refused requests are free
  await c2.r.action(req(c2.chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip");
  assert.equal(before, 4);
});

test("a gateway refusal is reported, not hidden", async () => {
  const { chain, r } = mk();
  chain.state.failWith = '{"ActionError":"boom"}';
  await fails(r.action(req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "5" } }), "ip"), 422, /gateway refused/);
});

const SOL_TX = "5mkfv677EVWqTZrsDsdY9sMQQpraHtc2vFUx4GVWZfB5yCvyRfgghP6AxqA4hvAH7M8cxyCQ5Ca26NaEbWKBVEMd";
const EVM_TX = "0x" + "ab".repeat(32);

test("deposit claims: shape per chain, source-chain pre-check first, then claim_wallet_deposit", async () => {
  const { chain, r } = mk();
  await r.claim({ owner: SOL_OWNER, tx_id: SOL_TX }, "ip");
  await r.claim({ owner: EVM_OWNER, tx_id: EVM_TX }, "ip");
  assert.deepEqual(chain.calls.map((c) => [c.method, c.args.owner.slice(0, 3)]), [["claim_wallet_deposit", "sol"], ["claim_wallet_deposit", "evm"]]);
  await fails(r.claim({ owner: EVM_OWNER, tx_id: SOL_TX }, "ip"), 400, /0x/);
  await fails(r.claim({ owner: SOL_OWNER, tx_id: EVM_TX }, "ip"), 400, /Solana/);
  await fails(r.claim({ owner: EVM_OWNER, tx_id: "0x" + "AB".repeat(32) }, "ip"), 400, /lowercase/);
  const c2 = mk(new FakeChain(), async () => { throw new BadRequest("that Solana transaction was not found (devnet)"); });
  await fails(c2.r.claim({ owner: SOL_OWNER, tx_id: SOL_TX }, "ip"), 400, /not found/);
  assert.equal(c2.chain.calls.length, 0, "a made-up transaction id costs the gateway nothing");
});

test("the claim watcher executes a deposit once it is Verified, and never a rejected one", async () => {
  const a = mk();
  a.chain.state.claimSeq = [{ Pending: { since_ms: 1 } }, { Verified: { near_pay: "1" } }];
  await a.r.claimDeposit({ tx_id: SOL_TX, beneficiary: "alice.testnet", intent: { Buy: { ticker: "pepe", min_out: "1" } } }, "ip");
  await a.r.watch(SOL_TX + "x").catch(() => {}); // a different id would be separate; the first watcher is already running
  await new Promise((res) => setTimeout(res, 60));
  assert.deepEqual(a.chain.calls.map((c) => c.method), ["claim_deposit", "execute_claim"]);
  const b = mk();
  b.chain.state.claimSeq = [{ Pending: { since_ms: 1 } }, { Rejected: { reason: "x" } }];
  await b.r.claimDeposit({ tx_id: SOL_TX, beneficiary: "alice.testnet", intent: { FundChest: { ticker: "pepe" } } }, "ip");
  await new Promise((res) => setTimeout(res, 60));
  assert.deepEqual(b.chain.calls.map((c) => c.method), ["claim_deposit"], "nothing to execute");
});

test("execute_claim only fires for a verified deposit", async () => {
  const { chain, r } = mk();
  chain.state.claim = { Done: {} };
  await fails(r.executeClaim({ tx_id: SOL_TX }, "ip"), 409, /no verified deposit/);
  chain.state.claim = { Verified: {} };
  await r.executeClaim({ tx_id: SOL_TX }, "ip");
  assert.deepEqual(chain.calls.map((c) => c.method), ["execute_claim"]);
});

test("views: wallet and message are read-only", async () => {
  const { chain, r } = mk();
  const w: any = (await r.wallet(SOL_OWNER, "pepe.nme.testnet")).body;
  assert.deepEqual([w.near, w.nonce, w.tokens, w.lock], ["10000000000000000000000000", 0, "1000", null]);
  const m: any = (await r.message({ owner: SOL_OWNER, action: buy, nonce: 0, expires_ms: 5 })).body;
  assert.match(m.text, /^TEXT\|sol:/);
  assert.equal(chain.calls.length, 0);
});

const argsJson = (ticker = "froggy", extra: object = {}) => JSON.stringify({ ticker, name: "Froggy", icon_data_url: "data:image/webp;base64,AAAA", launch: { first_buy: null }, ...extra });

test("a signed CreateCoin is submitted only with the factory's exact price, and the relayer then pokes the launch until Live", async () => {
  const { chain, r } = mk();
  chain.state.coinLaunch = [{ Creating: { step: "Announce" } }, { Creating: { step: "Reclaim" } }, { Live: {} }];
  const good = { CreateCoin: { args_json: argsJson(), deposit: chain.state.quote } };
  const out = await r.action(req(chain, SOL_OWNER, good), "1.1.1.1");
  assert.equal(out.status, 200);
  assert.equal(chain.calls[0]!.method, "wallet_action");
  assert.equal(chain.calls[0]!.gas, 300);
  await new Promise((res) => setTimeout(res, 60));
  assert.ok(chain.calls.filter((c) => c.method === "launch" && c.contract === "froggy.nme.testnet").length >= 1, "the relayer poked launch()");
  // a different deposit, a taken ticker, a first buy and a malformed ticker are refused for free
  for (const [body, status, re] of [
    [{ CreateCoin: { args_json: argsJson(), deposit: "1" } }, 409, /factory's price/],
    [{ CreateCoin: { args_json: argsJson("Froggy"), deposit: chain.state.quote } }, 400, /normalized/],
    [{ CreateCoin: { args_json: argsJson("froggy", { launch: { first_buy: { tokens: "1", max_near: "1" } } }), deposit: chain.state.quote } }, 400, /first buy/],
  ] as const) {
    const { chain: c2, r: r2 } = mk();
    await fails(r2.action(req(c2, SOL_OWNER, body), "1.1.1.1"), status, re);
    assert.equal(c2.calls.length, 0);
  }
  const { chain: c3, r: r3 } = mk();
  c3.state.launch = { owner: "x" };
  await fails(r3.action(req(c3, SOL_OWNER, good), "1.1.1.1"), 409, /already launched/);
});

test("PumpBag actions need the gateway to hold that PumpBag for this wallet; claims also register the gateway on the coin", async () => {
  const { chain, r } = mk();
  await fails(r.action(req(chain, SOL_OWNER, { CrownHeartbeat: { ticker: "froggy" } }), "1.1.1.1"), 404, /no PumpBag/);
  chain.state.crownOwner = SOL_OWNER;
  await r.action(req(chain, SOL_OWNER, { CrownClaimVested: { ticker: "froggy" } }), "1.1.1.1");
  assert.deepEqual(chain.calls.map((c) => c.method), ["register_token", "wallet_action"]);
  assert.deepEqual(chain.calls[0]!.args, { token: "froggy.nme.testnet" });
  chain.state.wallet.nonce = 1;
  const { chain: c2, r: r2 } = mk();
  c2.state.crownOwner = EVM_OWNER; // someone else's PumpBag
  await fails(r2.action(req(c2, SOL_OWNER, { CrownSend: { ticker: "froggy", to: "bob.testnet" } }), "1.1.1.1"), 404, /no PumpBag/);
});

const hire = { Hire: { ticker: "toad", kind: "shill", target: "axolotl", prompt: "shill it", amount: "100000000000000000000000" } };
const hiredLog = (owner: string, job_id: number) => `EVENT_JSON:${JSON.stringify({ standard: "nme", version: "1.0.0", event: "wallet_hired", data: [{ owner, job_id, amount: "1" }] })}`;
const settle = () => new Promise((r) => setTimeout(r, 60));

test("hiredJobId reads only this owner's wallet_hired event", () => {
  assert.equal(hiredJobId(["noise", hiredLog(SOL_OWNER, 6)], SOL_OWNER), 6);
  assert.equal(hiredJobId([hiredLog(EVM_OWNER, 6)], SOL_OWNER), undefined);
  assert.equal(hiredJobId(["EVENT_JSON:{bad"], SOL_OWNER), undefined);
});

test("a hire the brain could not answer: the relayer claims the refund back to the wallet once", async () => {
  const { chain, r } = mk();
  chain.state.logs = [hiredLog(SOL_OWNER, 6)];
  chain.state.jobSeq = [{ status: "Requested" }, { status: "Refunded" }];
  chain.state.hire = { owner: SOL_OWNER, credited: false };
  await r.action(req(chain, SOL_OWNER, hire), "ip");
  await settle();
  const claims = chain.calls.filter((c) => c.method === "claim_hire_refund");
  assert.equal(claims.length, 1);
  assert.deepEqual(claims[0]!.args, { job_id: 6 });
});

test("no refund claim for a delivered job or one already credited", async () => {
  for (const [job, hireRec] of [[{ status: "Delivered" }, { credited: false }], [{ status: "Refunded" }, { credited: true }]] as const) {
    const { chain, r } = mk();
    chain.state.logs = [hiredLog(SOL_OWNER, 7)];
    chain.state.jobSeq = [job];
    chain.state.hire = hireRec;
    await r.action(req(chain, SOL_OWNER, hire), "ip");
    await settle();
    assert.equal(chain.calls.filter((c) => c.method === "claim_hire_refund").length, 0);
  }
});

test("the same signed action twice while the first is in flight: only one is submitted", async () => {
  const { chain, r } = mk();
  let release!: () => void;
  const gate = new Promise<void>((res) => (release = res));
  const orig = chain.call.bind(chain);
  chain.call = async (...a: Parameters<typeof orig>) => { if (a[1] === "wallet_action") await gate; return orig(...a); };
  const body = req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "1000" } });
  const first = r.action(body, "ip");
  await new Promise((res) => setTimeout(res, 10));
  await fails(r.action(body, "ip"), 409, /already being submitted/);
  release();
  assert.equal((await first).status, 200);
  assert.equal(chain.calls.filter((c) => c.method === "wallet_action").length, 1);
});

test("a duplicate refused with 'wrong nonce' after the first copy landed is reported as success", async () => {
  const { chain, r } = mk();
  chain.state.failWith = "wrong nonce (the next valid one is in get_wallet)";
  chain.state.walletAfter = { near: chain.state.wallet.near, nonce: 1 };
  const out = await r.action(req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "1000" } }), "ip");
  assert.equal(out.status, 200);
  chain.state.walletAfter = null;
  await fails(r.action(req(chain, SOL_OWNER, { FundChest: { ticker: "pepe", amount: "1000" } }), "ip2"), 422, /wrong nonce/);
});

test("the refund sweep claims only uncredited gateway refunds from the feed, once", async () => {
  const { chain, r } = mk();
  const gw = "gateway.nme.testnet";
  chain.state.feed = [
    { id: 300, kind: "job_hired", actor: gw, extra: "job=11 kind=meme" },
    { id: 301, kind: "job_refunded", actor: gw, extra: "job=11 timeout" },
    { id: 302, kind: "job_refunded", actor: "alice.testnet", extra: "job=12 x" },
    { id: 303, kind: "job_refunded", actor: gw, extra: "job=13 x" },
  ];
  chain.state.hires = { 11: { credited: false }, 13: { credited: true } };
  assert.equal(await r.sweepRefunds(), 1);
  assert.deepEqual(chain.calls.filter((c) => c.method === "claim_hire_refund").map((c) => c.args), [{ job_id: 11 }]);
  assert.equal(await r.sweepRefunds(), 0, "the cursor moved past what was swept");
});
