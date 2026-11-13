import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { test } from "node:test";
import { GoogleVerifier, Play, TOPUP, topupMessage, type PlayChain } from "../src/play.js";
import { BadRequest } from "../src/validate.js";

const CLIENT = "123-abc.apps.googleusercontent.com";
const NOW = 1_794_000_000_000;
const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = { ...(publicKey.export({ format: "jwk" }) as JsonWebKey), kid: "k1", alg: "RS256", use: "sig" };
const jwks = async () => ({ k1: jwk });

function token(claims: Record<string, unknown>, kid = "k1", key = privateKey): string {
  const h = Buffer.from(JSON.stringify({ alg: "RS256", kid, typ: "JWT" })).toString("base64url");
  const p = Buffer.from(JSON.stringify({ iss: "https://accounts.google.com", aud: CLIENT, exp: NOW / 1000 + 600, sub: "g-1", email: "a@b.c", ...claims })).toString("base64url");
  const s = sign("RSA-SHA256", Buffer.from(`${h}.${p}`), key).toString("base64url");
  return `${h}.${p}.${s}`;
}
const fails = (p: Promise<unknown>, status: number, re: RegExp) => assert.rejects(p, (e: unknown) => e instanceof BadRequest && e.status === status && re.test(e.message));

test("Google ID tokens: only a fresh token signed by Google for this app passes", async () => {
  const g = new GoogleVerifier(CLIENT, () => NOW, jwks);
  assert.deepEqual(await g.verify(token({})), { sub: "g-1", email: "a@b.c" });
  await fails(g.verify(token({ aud: "other-app" })), 401, /another app/);
  await fails(g.verify(token({ iss: "https://evil.example" })), 401, /not issued by Google/);
  await fails(g.verify(token({ exp: NOW / 1000 - 1 })), 401, /expired/);
  const other = generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey;
  await fails(g.verify(token({}, "k1", other)), 401, /signature is invalid/);
  await fails(g.verify(token({}, "k9")), 401, /unknown Google signing key/);
  await fails(g.verify("not.a.jwt.at.all"), 400, /malformed/);
  await fails(new GoogleVerifier("", () => NOW, jwks).verify(token({})), 503, /not configured/);
});

class FakeChain implements PlayChain {
  accounts = new Map<string, Set<string>>();
  created: string[] = [];
  async exists(id: string) { return this.accounts.has(id); }
  async hasKey(id: string, pk: string) { return this.accounts.get(id)?.has(pk) ?? false; }
  async create(id: string, pk: string) { this.accounts.set(id, new Set([pk, "recovery"])); this.created.push(id); return "tx-create"; }
  async addDeviceKey(id: string, pk: string) { this.accounts.get(id)!.add(pk); return "tx-add"; }
  balances = new Map<string, bigint>();
  sent: [string, bigint][] = [];
  async balance(id: string) { return this.balances.get(id) ?? 0n; }
  async send(id: string, y: bigint) { this.sent.push([id, y]); this.balances.set(id, (this.balances.get(id) ?? 0n) + y); return "tx-send"; }
}
const PK1 = "ed25519:" + "1".repeat(44);
const PK2 = "ed25519:" + "2".repeat(44);

function mk(now = () => NOW) {
  const chain = new FakeChain();
  const data: Record<string, { account: string; created_ms: number }> = {};
  const store = { get: (s: string) => data[s], set: (s: string, a: string, n: number) => { data[s] = { account: a, created_ms: n }; } };
  const play = new Play(new GoogleVerifier(CLIENT, () => NOW, jwks), store, chain, { parent: "nmeplay.testnet", starterYocto: 1n, signupsPerIpPerDay: 2 }, now);
  return { chain, play };
}

test("first sign-in asks for a name, then creates <name>.nmeplay.testnet with the phone's key", async () => {
  const { chain, play } = mk();
  assert.deepEqual((await play.session({ id_token: token({}), device_pk: PK1 }, "ip")).body, { account: null, needs_name: true });
  const r = await play.session({ id_token: token({}), device_pk: PK1, name: "DegenFrog" }, "ip");
  assert.deepEqual(r.body, { account: "degenfrog.nmeplay.testnet", created: true, tx: "tx-create" });
  assert.ok(await chain.hasKey("degenfrog.nmeplay.testnet", PK1));
});

test("signing in again on a new phone adds that phone's key to the same account, once", async () => {
  const { chain, play } = mk();
  await play.session({ id_token: token({}), device_pk: PK1, name: "frog" }, "ip");
  const r = await play.session({ id_token: token({}), device_pk: PK2 }, "ip2");
  assert.deepEqual(r.body, { account: "frog.nmeplay.testnet", created: false });
  assert.ok(await chain.hasKey("frog.nmeplay.testnet", PK2));
  await play.session({ id_token: token({}), device_pk: PK2 }, "ip2");
  assert.equal(chain.created.length, 1, "never a second account for the same Google user");
});

test("bad names, taken names, bad keys and too many sign-ups are refused", async () => {
  const { play } = mk();
  await fails(play.session({ id_token: token({}), device_pk: PK1, name: "a" }, "ip"), 400, /3-20/);
  await fails(play.session({ id_token: token({}), device_pk: PK1, name: "bad..name" }, "ip"), 400, /3-20/);
  await fails(play.session({ id_token: token({}), device_pk: "ed25519:short" }, "ip"), 400, /device_pk/);
  await play.session({ id_token: token({ sub: "u1" }), device_pk: PK1, name: "taken" }, "ip");
  await fails(play.session({ id_token: token({ sub: "u2" }), device_pk: PK1, name: "taken" }, "ip"), 409, /taken/);
  await play.session({ id_token: token({ sub: "u3" }), device_pk: PK1, name: "third" }, "ip");
  await fails(play.session({ id_token: token({ sub: "u4" }), device_pk: PK1, name: "fourth" }, "ip"), 429, /too many/);
});

test("free test NEAR: only a low sign-in account, signed by its own key, once per 12 h, within the daily cap", async () => {
  let t = NOW;
  const { chain, play } = mk(() => t);
  await play.session({ id_token: token({}), device_pk: PK1, name: "frog" }, "ip");
  const acc = "frog.nmeplay.testnet";
  const NEAR = 10n ** 24n;
  chain.balances.set(acc, NEAR / 10n); // 0.1 NEAR left
  const good = (pk: string, msg: string, sig: string) => sig === `signed:${msg}` && pk === PK1;
  const req = (o: Record<string, unknown> = {}) => ({ account: acc, public_key: PK1, ts: t, signature: `signed:${topupMessage(acc, t)}`, ...o });
  const r = await play.topup(req(), good);
  assert.equal((r.body as { sent: string }).sent, (NEAR - NEAR / 10n).toString(), "refilled to exactly 1 NEAR");
  await fails(play.topup(req(), good), 429, /recently/);
  t += TOPUP.everyMs + 1;
  await fails(play.topup(req(), good), 409, /enough NEAR/);
  chain.balances.set(acc, 0n);
  await fails(play.topup(req({ signature: "forged" }), good), 401, /signature/);
  await fails(play.topup(req({ public_key: PK2 }), (pk, m, s) => s === `signed:${m}`), 401, /not on this account/);
  await fails(play.topup(req({ ts: t - TOPUP.maxAgeMs - 1 }), good), 400, /too old/);
  await fails(play.topup(req({ account: "abc123.testnet" }), good), 400, /Google sign-in accounts/);
  assert.equal(chain.sent.length, 1);
  // the daily cap across everyone
  for (let i = 0; i < 12; i++) {
    const a = `user${i}x.nmeplay.testnet`;
    chain.accounts.set(a, new Set([PK1]));
    const o = { account: a, public_key: PK1, ts: t, signature: `signed:${topupMessage(a, t)}` };
    if (i < 9) await play.topup(o, good);
    else { await fails(play.topup(o, good), 429, /all given out/); break; }
  }
});

test("free test NEAR: a real ed25519 signature from the phone's NEAR key passes, a tampered one fails", async () => {
  const { KeyPair } = await import("near-api-js");
  const kp = KeyPair.fromRandom("ed25519");
  const pk = kp.getPublicKey().toString();
  const { chain, play } = mk();
  await play.session({ id_token: token({}), device_pk: pk, name: "real" }, "ip");
  const acc = "real.nmeplay.testnet";
  const sign = (msg: string) => Buffer.from(kp.sign(new TextEncoder().encode(msg)).signature).toString("base64");
  const r = await play.topup({ account: acc, public_key: pk, ts: NOW, signature: sign(topupMessage(acc, NOW)) });
  assert.equal(r.status, 200);
  assert.equal(chain.sent.length, 1);
  await fails(play.topup({ account: acc, public_key: pk, ts: NOW, signature: sign(topupMessage(acc, NOW + 1)) }), 401, /signature/);
});
