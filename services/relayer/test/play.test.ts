import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { test } from "node:test";
import { GoogleVerifier, Play, type PlayChain } from "../src/play.js";
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
}
const PK1 = "ed25519:" + "1".repeat(44);
const PK2 = "ed25519:" + "2".repeat(44);

function mk() {
  const chain = new FakeChain();
  const data: Record<string, { account: string; created_ms: number }> = {};
  const store = { get: (s: string) => data[s], set: (s: string, a: string, n: number) => { data[s] = { account: a, created_ms: n }; } };
  const play = new Play(new GoogleVerifier(CLIENT, () => NOW, jwks), store, chain, { parent: "nmeplay.testnet", starterYocto: 1n, signupsPerIpPerDay: 2 }, () => NOW);
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
