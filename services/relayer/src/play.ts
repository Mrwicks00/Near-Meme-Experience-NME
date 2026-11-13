// "Sign in with Google" accounts (web2-style onboarding). The phone makes its own ed25519 key; after verifying the
// Google ID token, the relayer creates `<name>.<parent>` with that key (full access, the user signs everything on the
// device: no wallet app, no popups) plus a recovery key the relayer holds, and sends starter NEAR. On a new phone,
// signing in with Google again makes the relayer add that phone's key with the recovery key.
// Honest trust note: recovery is custodial (the recovery key can add keys); the README says so. Testnet only.
import { createPublicKey, verify as cryptoVerify, verify as rsaVerify } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync, renameSync } from "node:fs";
import { dirname } from "node:path";
import { Account, JsonRpcProvider, KeyPairSigner, PublicKey, actions, type KeyPairString } from "near-api-js";
import { BadRequest } from "./validate.js";

export type GoogleUser = { sub: string; email?: string };

/** Verifies a Google Sign-In ID token (RS256, Google's published keys, issuer, audience, expiry). */
export class GoogleVerifier {
  private keys: { at: number; jwks: Record<string, JsonWebKey> } | null = null;
  constructor(private clientId: string, private now: () => number = Date.now, private fetchJwks = defaultJwks) {}

  async verify(token: unknown): Promise<GoogleUser> {
    if (!this.clientId) throw new BadRequest("Google sign-in is not configured on this relayer", 503);
    if (typeof token !== "string" || token.length > 4096) throw new BadRequest("missing Google ID token");
    const parts = token.split(".");
    if (parts.length !== 3) throw new BadRequest("malformed Google ID token");
    const [h, p, s] = parts as [string, string, string];
    let header: { alg?: string; kid?: string };
    let claims: { iss?: string; aud?: string; exp?: number; sub?: string; email?: string; email_verified?: boolean };
    try {
      header = JSON.parse(Buffer.from(h, "base64url").toString("utf8"));
      claims = JSON.parse(Buffer.from(p, "base64url").toString("utf8"));
    } catch {
      throw new BadRequest("malformed Google ID token");
    }
    if (header.alg !== "RS256" || !header.kid) throw new BadRequest("unexpected token algorithm");
    const jwk = await this.key(header.kid);
    const ok = rsaVerify("RSA-SHA256", Buffer.from(`${h}.${p}`), createPublicKey({ key: jwk as any, format: "jwk" }), Buffer.from(s, "base64url"));
    if (!ok) throw new BadRequest("Google ID token signature is invalid", 401);
    if (claims.iss !== "accounts.google.com" && claims.iss !== "https://accounts.google.com") throw new BadRequest("token was not issued by Google", 401);
    if (claims.aud !== this.clientId) throw new BadRequest("token is for another app", 401);
    if (!claims.exp || claims.exp * 1000 < this.now()) throw new BadRequest("Google sign-in expired, sign in again", 401);
    if (!claims.sub) throw new BadRequest("token has no user id", 401);
    return { sub: claims.sub, email: claims.email };
  }

  private async key(kid: string): Promise<JsonWebKey> {
    if (!this.keys || this.now() - this.keys.at > 3_600_000 || !this.keys.jwks[kid]) {
      this.keys = { at: this.now(), jwks: await this.fetchJwks() };
    }
    const k = this.keys.jwks[kid];
    if (!k) throw new BadRequest("unknown Google signing key", 401);
    return k;
  }
}

async function defaultJwks(): Promise<Record<string, JsonWebKey>> {
  const r = await fetch("https://www.googleapis.com/oauth2/v3/certs", { signal: AbortSignal.timeout(10_000) });
  if (!r.ok) throw new BadRequest("could not reach Google to check the sign-in", 502);
  const j = (await r.json()) as { keys: (JsonWebKey & { kid: string })[] };
  return Object.fromEntries(j.keys.map((k) => [k.kid, k]));
}

/** Google user -> NEAR account, kept in a JSON file next to the relayer. */
export class PlayStore {
  private data: Record<string, { account: string; created_ms: number }>;
  constructor(private file: string) {
    try {
      this.data = JSON.parse(readFileSync(file, "utf8"));
    } catch {
      this.data = {};
    }
  }
  get(sub: string) {
    return this.data[sub];
  }
  set(sub: string, account: string, now: number) {
    this.data[sub] = { account, created_ms: now };
    mkdirSync(dirname(this.file), { recursive: true });
    writeFileSync(`${this.file}.tmp`, JSON.stringify(this.data, null, 1), { mode: 0o600 });
    renameSync(`${this.file}.tmp`, this.file);
  }
}

/** What the play flow needs from NEAR. Injected so the logic is testable without a network. */
export interface PlayChain {
  exists(accountId: string): Promise<boolean>;
  hasKey(accountId: string, publicKey: string): Promise<boolean>;
  create(accountId: string, devicePk: string, starterYocto: bigint): Promise<string>;
  addDeviceKey(accountId: string, devicePk: string): Promise<string>;
  balance(accountId: string): Promise<bigint>;
  send(accountId: string, yocto: bigint): Promise<string>;
}

export class NearPlayChain implements PlayChain {
  private provider: JsonRpcProvider;
  private parent: Account;
  private recoverySigner: KeyPairSigner;
  private recoveryPk: string;
  constructor(rpc: string, parentId: string, parentKey: string, recoveryKey: string) {
    this.provider = new JsonRpcProvider({ url: rpc });
    this.parent = new Account(parentId, this.provider, KeyPairSigner.fromSecretKey(parentKey as KeyPairString));
    this.recoverySigner = KeyPairSigner.fromSecretKey(recoveryKey as KeyPairString);
    this.recoveryPk = "";
  }
  private async recovery(): Promise<string> {
    if (!this.recoveryPk) this.recoveryPk = (await this.recoverySigner.getPublicKey()).toString();
    return this.recoveryPk;
  }
  async exists(accountId: string) {
    try {
      await this.provider.viewAccount({ accountId });
      return true;
    } catch (e) {
      if (/does not exist|UNKNOWN_ACCOUNT/i.test(String(e))) return false;
      throw e;
    }
  }
  async hasKey(accountId: string, publicKey: string) {
    try {
      await this.provider.viewAccessKey({ accountId, publicKey });
      return true;
    } catch {
      return false;
    }
  }
  async create(accountId: string, devicePk: string, starterYocto: bigint) {
    const out: any = await this.parent.signAndSendTransaction({
      receiverId: accountId,
      actions: [actions.createAccount(), actions.transfer(starterYocto), actions.addFullAccessKey(PublicKey.from(devicePk)), actions.addFullAccessKey(PublicKey.from(await this.recovery()))],
    });
    return out?.transaction?.hash ?? "";
  }
  async balance(accountId: string) {
    const a = (await this.provider.viewAccount({ accountId })) as unknown as { amount: string | bigint };
    return BigInt(a.amount);
  }
  async send(accountId: string, yocto: bigint) {
    const out: any = await this.parent.signAndSendTransaction({ receiverId: accountId, actions: [actions.transfer(yocto)] });
    return out?.transaction?.hash ?? "";
  }
  async addDeviceKey(accountId: string, devicePk: string) {
    const user = new Account(accountId, this.provider, this.recoverySigner);
    const out: any = await user.signAndSendTransaction({ receiverId: accountId, actions: [actions.addFullAccessKey(PublicKey.from(devicePk))] });
    return out?.transaction?.hash ?? "";
  }
}

const NAME = /^[a-z0-9](?:[a-z0-9]|[-_](?=[a-z0-9])){2,19}$/;
const NEAR = 10n ** 24n;
export const TOPUP = { below: (6n * NEAR) / 10n, to: NEAR, everyMs: 12 * 3_600_000, perDay: 10n * NEAR, maxAgeMs: 5 * 60_000 };

/** The text the phone signs to ask for a top-up (no Google popup: proves it holds a key of the account). */
export const topupMessage = (account: string, ts: number) => `nme-topup:${account}:${ts}`;

function ed25519Ok(publicKey: string, message: string, sigB64: string): boolean {
  try {
    const raw = PublicKey.from(publicKey).data;
    const key = createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: Buffer.from(raw).toString("base64url") }, format: "jwk" });
    return cryptoVerify(null, Buffer.from(message), key, Buffer.from(sigB64, "base64"));
  } catch {
    return false;
  }
}
const PK = /^ed25519:[1-9A-HJ-NP-Za-km-z]{43,44}$/;

export class Play {
  private signups = new Map<string, number[]>();
  constructor(
    private google: Pick<GoogleVerifier, "verify">,
    private store: Pick<PlayStore, "get" | "set">,
    private chain: PlayChain,
    private cfg: { parent: string; starterYocto: bigint; signupsPerIpPerDay: number },
    private now: () => number = Date.now,
  ) {}

  /** POST /play/session {id_token, device_pk, name?}: sign up (with a name) or add this device to the existing account. */
  async session(raw: unknown, ip: string): Promise<{ status: number; body: unknown }> {
    const o = (raw ?? {}) as Record<string, unknown>;
    const user = await this.google.verify(o.id_token);
    const devicePk = typeof o.device_pk === "string" ? o.device_pk : "";
    if (!PK.test(devicePk)) throw new BadRequest("device_pk must be an ed25519 public key");
    const known = this.store.get(user.sub);
    if (known) {
      if (!(await this.chain.hasKey(known.account, devicePk))) await this.chain.addDeviceKey(known.account, devicePk);
      return { status: 200, body: { account: known.account, created: false } };
    }
    const name = typeof o.name === "string" ? o.name.trim().toLowerCase() : "";
    if (!name) return { status: 200, body: { account: null, needs_name: true } };
    if (!NAME.test(name)) throw new BadRequest("pick 3-20 lowercase letters, digits, - or _");
    const accountId = `${name}.${this.cfg.parent}`;
    if (await this.chain.exists(accountId)) throw new BadRequest("that name is taken, pick another", 409);
    this.rate(ip);
    const tx = await this.chain.create(accountId, devicePk, this.cfg.starterYocto);
    this.store.set(user.sub, accountId, this.now());
    return { status: 200, body: { account: accountId, created: true, tx } };
  }

  private topped = new Map<string, number>();
  private day = { start: 0, sent: 0n };

  /** POST /play/topup {account, public_key, ts, signature}: refills a low sign-in account with free test NEAR.
   *  The phone signs `nme-topup:<account>:<ts>` with its key; that key must be on the account. Once per 12 h per account,
   *  only under 0.6 NEAR, up to 1 NEAR, 10 NEAR a day across everyone. */
  async topup(raw: unknown, verify: (pk: string, msg: string, sig: string) => boolean = ed25519Ok): Promise<{ status: number; body: unknown }> {
    const o = (raw ?? {}) as Record<string, unknown>;
    const account = typeof o.account === "string" ? o.account : "";
    const pk = typeof o.public_key === "string" ? o.public_key : "";
    const ts = typeof o.ts === "number" ? o.ts : 0;
    const sig = typeof o.signature === "string" ? o.signature : "";
    const name = account.endsWith(`.${this.cfg.parent}`) ? account.slice(0, -this.cfg.parent.length - 1) : "";
    if (!NAME.test(name)) throw new BadRequest("only Google sign-in accounts get free test NEAR");
    if (!PK.test(pk)) throw new BadRequest("public_key must be an ed25519 public key");
    const t = this.now();
    if (Math.abs(t - ts) > TOPUP.maxAgeMs) throw new BadRequest("the request is too old, try again");
    if (!verify(pk, topupMessage(account, ts), sig)) throw new BadRequest("signature does not match", 401);
    if (!(await this.chain.hasKey(account, pk))) throw new BadRequest("that key is not on this account", 401);
    const last = this.topped.get(account) ?? 0;
    if (t - last < TOPUP.everyMs) throw new BadRequest(`you got free NEAR recently; try again in ${Math.ceil((TOPUP.everyMs - (t - last)) / 3_600_000)} h`, 429);
    const bal = await this.chain.balance(account);
    if (bal >= TOPUP.below) throw new BadRequest("you still have enough NEAR to play", 409);
    if (t - this.day.start > 86_400_000) this.day = { start: t, sent: 0n };
    const amount = TOPUP.to - bal;
    if (this.day.sent + amount > TOPUP.perDay) throw new BadRequest("today's free NEAR is all given out, come back tomorrow", 429);
    this.topped.set(account, t);
    this.day.sent += amount;
    try {
      const tx = await this.chain.send(account, amount);
      return { status: 200, body: { account, sent: amount.toString(), tx } };
    } catch (e) {
      this.topped.delete(account);
      this.day.sent -= amount;
      throw e;
    }
  }

  private rate(ip: string) {
    const day = 86_400_000;
    const t = this.now();
    const w = (this.signups.get(ip) ?? []).filter((x) => t - x < day);
    if (w.length >= this.cfg.signupsPerIpPerDay) throw new BadRequest("too many new accounts from this network today", 429);
    w.push(t);
    this.signups.set(ip, w);
  }
}
