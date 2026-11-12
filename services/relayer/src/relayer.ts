import type { Chain } from "./chain.js";
import type { Config } from "./config.js";
import type { Precheck } from "./precheck.js";
import { RateLimit } from "./ratelimit.js";
import { verifyOwner } from "./sigcheck.js";
import { BadRequest, costOf, isTokenOp, launchArgs, parseAction, parseOwner, parseSigned, type Action } from "./validate.js";

export interface Reply {
  status: number;
  body: unknown;
}

type Limits = Pick<Config, "ratePerMin" | "ratePerDay" | "ipPerMin" | "globalPerMin" | "gateway" | "arena" | "factory" | "registrationYocto">;

const MAX_AGE_MS = 24 * 60 * 60 * 1000;
const SOL_SIG = /^[1-9A-HJ-NP-Za-km-z]{64,100}$/;
const EVM_TX = /^0x[0-9a-f]{64}$/;

/** The courier. Everything it submits is checked again by the gateway contract, which alone decides what happens (I7). */
export class Relayer {
  private perMin: RateLimit;
  private perDay: RateLimit;
  private perIp: RateLimit;
  private global: RateLimit;
  private registered = new Set<string>();
  private watching = new Set<string>();
  private hiresWatched = new Set<number>();
  private inFlight = new Set<string>();
  private feedCursor: number | null = null;

  constructor(private chain: Chain, private cfg: Limits, private precheck: Precheck, private now: () => number = Date.now, private pollMs = 5000) {
    this.perMin = new RateLimit(cfg.ratePerMin, 60_000, now);
    this.perDay = new RateLimit(cfg.ratePerDay, 86_400_000, now);
    this.perIp = new RateLimit(cfg.ipPerMin, 60_000, now);
    this.global = new RateLimit(cfg.globalPerMin, 60_000, now);
  }

  private limit(owner: string, ip: string) {
    const checks: [RateLimit, string, string][] = [
      [this.perMin, owner, "this wallet"],
      [this.perDay, owner, "this wallet today"],
      [this.perIp, ip, "this address"],
      [this.global, "all", "the relayer"],
    ];
    for (const [rl, key, who] of checks) {
      if (rl.retryAfter(key) > 0) throw new BadRequest(`rate limit reached for ${who}, retry in ${rl.retryAfter(key)} s`, 429);
    }
    for (const [rl, key] of checks) rl.take(key);
  }

  private async ensureRegistered(token: string) {
    if (this.registered.has(token)) return;
    const bal = await this.chain.view<unknown>(token, "storage_balance_of", { account_id: this.cfg.gateway });
    if (bal === null || bal === undefined) {
      const r = await this.chain.call(this.cfg.gateway, "register_token", { token }, 60, this.cfg.registrationYocto);
      if (r.failure) throw new BadRequest(`could not register the gateway on ${token}: ${r.failure}`, 502);
      // the registration lands a few blocks later (cross-contract): wait for it before the buy depends on it
      for (let i = 0; i < 20; i++) {
        const b = await this.chain.view<unknown>(token, "storage_balance_of", { account_id: this.cfg.gateway });
        if (b !== null && b !== undefined) break;
        await new Promise((r) => setTimeout(r, 1000));
      }
    }
    this.registered.add(token);
  }

  /** POST /action: a wallet-signed action. */
  async action(raw: unknown, ip: string): Promise<Reply> {
    const req = parseSigned(raw);
    const { owner, action } = req;
    const t = this.now();
    if (t > req.expires_ms) throw new BadRequest("this signed message has expired");
    if (req.expires_ms > t + MAX_AGE_MS) throw new BadRequest("the expiry is more than 24 hours away");
    const wallet = await this.chain.view<{ near: string; nonce: number }>(this.cfg.gateway, "get_wallet", { owner });
    if (req.nonce !== wallet.nonce) throw new BadRequest(`wrong nonce: the next valid one is ${wallet.nonce}`, 409);
    const text = await this.chain.view<string>(this.cfg.gateway, "wallet_message", { owner, action, nonce: req.nonce, expires_ms: req.expires_ms });
    const sig = Buffer.from(req.signature, "base64");
    if (!verifyOwner(owner, text, sig)) throw new BadRequest("the signature does not match this owner and message", 403);

    const cost = costOf(action);
    if (cost.near > 0n || cost.fee) {
      const cfg = await this.chain.view<{ relay_fee: string }>(this.cfg.gateway, "get_config", {});
      const need = cost.near + (cost.fee ? BigInt(cfg.relay_fee) : 0n);
      if (BigInt(wallet.near) < need) throw new BadRequest("not enough NEAR in the wallet balance for this action", 409);
    }
    if (cost.token) {
      const tk = await this.chain.view<{ tokens: string }>(this.cfg.gateway, "get_wallet_tokens", { owner, token: cost.token.account });
      if (BigInt(tk.tokens) < cost.token.amount) throw new BadRequest("not enough tokens in the wallet balance", 409);
    }
    if (isTokenOp(action)) {
      const lock = await this.chain.view<unknown>(this.cfg.gateway, "get_lock", {});
      if (lock) throw new BadRequest("another wallet token operation is running, retry in a few seconds", 409);
    }
    await this.prepare(action, owner);

    // The same signed message twice (a double click) passes the nonce check above while the first is still queued: refuse it here.
    const slot = `${owner}#${req.nonce}`;
    if (this.inFlight.has(slot)) throw new BadRequest("this action is already being submitted", 409);
    this.limit(owner, ip); // only requests that would really cost gas count against the limits
    this.inFlight.add(slot);
    let r: Awaited<ReturnType<Chain["call"]>>;
    try {
      r = await this.chain.call(this.cfg.gateway, "wallet_action", { owner, action, nonce: req.nonce, expires_ms: req.expires_ms, signature: req.signature }, 300);
    } finally {
      this.inFlight.delete(slot);
    }
    if (r.failure) {
      // The relayer re-sends after a dropped connection; if the first copy already landed, the copy is refused as a
      // duplicate. The wallet's nonce moving past this one proves the action went through.
      if (/wrong nonce/i.test(r.failure)) {
        const now = await this.chain.view<{ nonce: number }>(this.cfg.gateway, "get_wallet", { owner }).catch(() => null);
        if (now && now.nonce > req.nonce) {
          return { status: 200, body: { ok: true, tx: undefined, token_operation: isTokenOp(action), note: "submitted; the transaction link is on the explorer under the relayer account", next: "poll GET /wallet/<owner>?token=<account>" } };
        }
      }
      throw new BadRequest(`the gateway refused: ${r.failure}`, 422);
    }
    if ("CreateCoin" in action) void this.pokeLaunch(launchArgs(action.CreateCoin.args_json).ticker);
    if ("Hire" in action) {
      const jobId = hiredJobId(r.logs ?? [], owner);
      if (jobId !== undefined) void this.watchHire(jobId);
    }
    return { status: 200, body: { ok: true, tx: r.txHash, token_operation: isTokenOp(action), next: "poll GET /wallet/<owner>?token=<account>" } };
  }

  /**
   * A hire the brain could not answer is refunded by the arena to the gateway; `claim_hire_refund` (permissionless)
   * moves it back to the wallet's balance. Timing only (I7): the gateway checks the job record itself.
   */
  async watchHire(jobId: number): Promise<void> {
    if (this.hiresWatched.has(jobId)) return;
    this.hiresWatched.add(jobId);
    try {
      // The brain answers within the yield/resume window (~200 blocks, about 4 min); poll a little longer than that.
      for (let i = 0; i < 72; i++) {
        const job = await this.chain.view<{ status: string } | null>(this.cfg.arena, "get_job", { job_id: jobId }).catch(() => undefined);
        if (job === null) return;
        if (job && job.status === "Refunded") {
          const hire = await this.chain.view<{ credited: boolean } | null>(this.cfg.gateway, "get_hire", { job_id: jobId }).catch(() => undefined);
          if (hire && !hire.credited) await this.chain.call(this.cfg.gateway, "claim_hire_refund", { job_id: jobId }, 60).catch(() => undefined);
          return;
        }
        if (job && job.status !== "Requested") return; // Delivered or Burned: nothing to refund
        await new Promise((r) => setTimeout(r, this.pollMs));
      }
    } finally {
      this.hiresWatched.delete(jobId);
    }
  }

  /**
   * Claims every hire refund the gateway has not credited yet, found from the arena feed (`job_refunded` paid to the
   * gateway). Catches what `watchHire` can miss: a restart, a dropped connection. Timing only (I7). Returns claims made.
   */
  async sweepRefunds(): Promise<number> {
    if (this.feedCursor === null) {
      const last = await this.chain.view<number>(this.cfg.arena, "feed_last_id", {});
      this.feedCursor = Math.max(0, last - 200);
    }
    let made = 0;
    for (let page = 0; page < 5; page++) {
      const items: { id: number; kind: string; actor: string; extra: string }[] = await this.chain.view(this.cfg.arena, "feed_since", { id: this.feedCursor, limit: 100 });
      if (!items.length) break;
      for (const f of items) {
        const job = /^job=(\d+)/.exec(f.extra)?.[1];
        if (f.kind !== "job_refunded" || f.actor !== this.cfg.gateway || !job) continue;
        const hire = await this.chain.view<{ credited: boolean } | null>(this.cfg.gateway, "get_hire", { job_id: Number(job) });
        if (hire && !hire.credited) {
          const r = await this.chain.call(this.cfg.gateway, "claim_hire_refund", { job_id: Number(job) }, 60);
          if (!r.failure) made++;
        }
      }
      this.feedCursor = items[items.length - 1]!.id;
      if (items.length < 100) break;
    }
    return made;
  }

  startRefundSweep(everyMs = 60_000) {
    const run = () => this.sweepRefunds().catch(() => 0).finally(() => setTimeout(run, everyMs).unref());
    void run();
  }

  /** Makes sure the gateway can receive what the action brings: tokens of a bought coin, wNEAR from a sale. */
  /** Timing only (I7): `launch()` is permissionless; the coin may need several pokes before it is Live. */
  private async pokeLaunch(ticker: string) {
    const coin = `${ticker}.${this.cfg.factory}`;
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, this.pollMs));
      try {
        const st = await this.chain.view<unknown>(coin, "get_launch", {});
        if ((st && typeof st === "object" && "Live" in st) || st === "Failed") return;
        if (st && typeof st === "object" && "Creating" in st) await this.chain.call(coin, "launch", {}, 300);
      } catch {
        // the coin account may not exist yet; try again
      }
    }
  }

  private async prepare(action: Action, owner: string) {
    if ("CreateCoin" in action) {
      const { ticker, iconLen } = launchArgs(action.CreateCoin.args_json);
      const quote = await this.chain.view<string>(this.cfg.factory, "required_deposit", { icon_len: iconLen, first_buy_max_near: null });
      if (BigInt(quote) !== BigInt(action.CreateCoin.deposit)) throw new BadRequest(`the deposit must equal the factory's price, ${quote} yoctoNEAR (an excess stays at the gateway, a shortfall is refunded after settle_launch)`, 409);
      const taken = await this.chain.view<unknown>(this.cfg.gateway, "get_launch", { ticker });
      if (taken) throw new BadRequest("this ticker was already launched through the gateway", 409);
      return;
    }
    if ("CrownClaimFees" in action || "CrownClaimVested" in action || "CrownHeartbeat" in action || "CrownSend" in action) {
      const a = Object.values(action)[0] as { ticker: string };
      const holder = await this.chain.view<string | null>(this.cfg.gateway, "get_crown_owner", { ticker: a.ticker });
      if (holder !== owner) throw new BadRequest("the gateway holds no PumpBag of this ticker for this wallet", 404);
      if ("CrownClaimVested" in action) await this.ensureRegistered(`${a.ticker}.${this.cfg.factory}`);
      return;
    }
    if ("Buy" in action || "JoinWar" in action) {
      const key = "Buy" in action ? action.Buy.key : action.JoinWar.key;
      const coin = await this.chain.view<{ account: string; pool_id?: string | null; launched?: boolean } | null>(this.cfg.arena, "get_coin", { key });
      if (!coin) throw new BadRequest(`unknown coin ${key}`, 404);
      await this.ensureRegistered(coin.account);
    } else if ("Sell" in action) {
      const wrap = (await this.chain.view<{ wrap: string }>(this.cfg.gateway, "get_config", {})).wrap;
      await this.ensureRegistered(wrap);
    }
  }

  /** POST /claim: a deposit that credits a wallet (Phantom / MetaMask). */
  async claim(raw: unknown, ip: string): Promise<Reply> {
    const o = raw as Record<string, unknown>;
    const owner = parseOwner(o?.owner);
    const txId = typeof o?.tx_id === "string" ? o.tx_id : "";
    const chain = owner.startsWith("sol:") ? "solana" : "evm";
    if (chain === "solana" ? !SOL_SIG.test(txId) : !EVM_TX.test(txId)) throw new BadRequest(chain === "solana" ? "tx_id must be a Solana signature" : "tx_id must be 0x + 64 lowercase hex");
    await this.precheck(chain, txId);
    this.limit(owner, ip);
    const r = await this.chain.call(this.cfg.gateway, "claim_wallet_deposit", { tx_id: txId, owner }, 300);
    if (r.failure) throw new BadRequest(`the gateway refused: ${r.failure}`, 422);
    return { status: 200, body: { ok: true, tx: r.txHash, next: "the TEE verifies the deposit (about a minute); poll GET /wallet/<owner>" } };
  }

  /** POST /claim_deposit: the NEAR-beneficiary flow (replaces scripts/relay_claim.py): claim, then spend once verified. */
  async claimDeposit(raw: unknown, ip: string): Promise<Reply> {
    const o = raw as Record<string, unknown>;
    const txId = typeof o?.tx_id === "string" ? o.tx_id : "";
    const beneficiary = typeof o?.beneficiary === "string" ? o.beneficiary : "";
    if (!SOL_SIG.test(txId)) throw new BadRequest("tx_id must be a Solana signature");
    if (!/^[a-z0-9][a-z0-9._-]{1,63}$/.test(beneficiary)) throw new BadRequest("beneficiary must be a NEAR account");
    const intent = o.intent;
    if (typeof intent !== "object" || intent === null) throw new BadRequest("intent is required");
    await this.precheck("solana", txId);
    this.limit(beneficiary, ip);
    const r = await this.chain.call(this.cfg.gateway, "claim_deposit", { tx_id: txId, beneficiary, intent }, 300);
    if (r.failure) throw new BadRequest(`the gateway refused: ${r.failure}`, 422);
    void this.watch(txId);
    return { status: 200, body: { ok: true, tx: r.txHash, next: "the relayer executes the claim as soon as it is verified" } };
  }

  /** Polls a claim until it is Verified, then executes it (own transaction). Anyone could do this; the relayer just does it. */
  async watch(txId: string): Promise<void> {
    if (this.watching.has(txId)) return;
    this.watching.add(txId);
    try {
      for (let i = 0; i < 60; i++) {
        await new Promise((r) => setTimeout(r, this.pollMs));
        const c = await this.chain.view<unknown>(this.cfg.gateway, "get_claim", { tx_id: txId }).catch(() => undefined);
        if (c && typeof c === "object" && "Verified" in c) {
          await this.chain.call(this.cfg.gateway, "execute_claim", { tx_id: txId }, 300).catch(() => undefined);
          return;
        }
        if (c && (typeof c === "object" ? !("Pending" in c) : true) && c !== undefined) return; // Done, Rejected or unknown: nothing to execute
      }
    } finally {
      this.watching.delete(txId);
    }
  }

  /** POST /execute_claim: spend an already verified deposit (permissionless on the contract). */
  async executeClaim(raw: unknown, ip: string): Promise<Reply> {
    const txId = typeof (raw as any)?.tx_id === "string" ? (raw as any).tx_id : "";
    if (!SOL_SIG.test(txId)) throw new BadRequest("tx_id must be a Solana signature");
    const c = await this.chain.view<unknown>(this.cfg.gateway, "get_claim", { tx_id: txId });
    if (!c || typeof c !== "object" || !("Verified" in c)) throw new BadRequest("no verified deposit is waiting for this transaction", 409);
    this.limit(txId, ip);
    const r = await this.chain.call(this.cfg.gateway, "execute_claim", { tx_id: txId }, 300);
    if (r.failure) throw new BadRequest(`the gateway refused: ${r.failure}`, 422);
    return { status: 200, body: { ok: true, tx: r.txHash } };
  }

  /** POST /message: the exact text the wallet must sign (a view of the gateway; the UI shows it). */
  async message(raw: unknown): Promise<Reply> {
    const o = raw as Record<string, unknown>;
    const owner = parseOwner(o?.owner);
    const action = parseAction(o?.action);
    const nonce = o?.nonce;
    const exp = o?.expires_ms;
    if (typeof nonce !== "number" || typeof exp !== "number") throw new BadRequest("nonce and expires_ms are required numbers");
    const text = await this.chain.view<string>(this.cfg.gateway, "wallet_message", { owner, action, nonce, expires_ms: exp });
    return { status: 200, body: { text } };
  }

  /** GET /wallet/<owner>[?token=<account>]: balance, nonce, running token operation, optional token balance. */
  async wallet(ownerRaw: string, token?: string): Promise<Reply> {
    const owner = parseOwner(ownerRaw);
    const [w, lock] = await Promise.all([this.chain.view<object>(this.cfg.gateway, "get_wallet", { owner }), this.chain.view<unknown>(this.cfg.gateway, "get_lock", {})]);
    const body: Record<string, unknown> = { ...w, lock };
    if (token) {
      if (!/^[a-z0-9][a-z0-9._-]{1,63}$/.test(token)) throw new BadRequest("token must be a NEAR account");
      Object.assign(body, await this.chain.view<object>(this.cfg.gateway, "get_wallet_tokens", { owner, token }));
    }
    return { status: 200, body };
  }

  config(): Reply {
    return { status: 200, body: { gateway: this.cfg.gateway, arena: this.cfg.arena, limits: { per_wallet_per_min: this.cfg.ratePerMin, per_wallet_per_day: this.cfg.ratePerDay } } };
  }
}

/** The job id from the gateway's `wallet_hired` event for this owner, if the transaction's logs carry it. */
export function hiredJobId(logs: string[], owner: string): number | undefined {
  for (const l of logs) {
    if (!l.startsWith("EVENT_JSON:")) continue;
    try {
      const e = JSON.parse(l.slice(11));
      const d = e?.data?.[0];
      if (e?.standard === "nme" && e?.event === "wallet_hired" && String(d?.owner).toLowerCase() === owner.toLowerCase() && Number.isSafeInteger(d?.job_id)) return d.job_id;
    } catch {
      // not ours
    }
  }
  return undefined;
}
