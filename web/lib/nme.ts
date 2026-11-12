import { NME } from "./config";
import { view } from "./rpc";
import { compact, near, nearStr, priceFromPoint, short, tokens } from "./format";

// ---- Shapes (quoted from contracts; see tasks/web-api-notes in the M9 handoff) ----
export type CoinRecord = { account: string; creator: string; created_ms: number; status: "Reserved" | "Created" | "Live"; inactivity_ms: number; colosseum_opt_in: boolean };
export type CoinEntry = { kind: "Native" | "Outside"; account: string; crown_vault?: string | null; colosseum_opt_in: boolean; pool_id?: string | null; lpt_id?: string | null; treasury_near: string; chest_near: string; launched: boolean };
export type Summary = {
  ticker: string; account: string; vault: string; creator: string;
  pool: { pool_id?: string | null; fee_tier?: number | null; lpt_id?: string | null; coin_is_x: boolean };
  supply: string; burned: string; persona: { vibe: string; rivals: string[] };
  fees: { pending_near: string; claimed_near: string; burned_tokens: string };
};
export type FtMeta = { name: string; symbol: string; decimals: number; icon?: string | null };
export type Pair = { a: string; b: string; stake: string; score_a: string; score_b: string; unique_a: number; unique_b: number; state: "Pending" | "Settled"; winner: string; note: string };
export type Round = { id: number; start_ms: number; end_ms: number; pairs: Pair[]; settled_pairs: number };
export type WarConfig = { round_duration_ms: number; round_interval_ms: number; chest_bps: number; wallet_cap_bps: number; min_unique: number; min_stake: string; max_pairs: number };
export type FeedItem = { id: number; ts_ms: number; kind: string; ticker: string; actor: string; amount: string; extra: string };
export type Commentary = { seq: number; ticker: string; event: string; text: string; model: string; chat_id: string; ts_ms: number };

export type Coin = {
  key: string;            // arena key: ticker for NME coins, token account for outside coins
  ticker: string;         // display ticker (upper case)
  name: string;
  account: string;
  icon: string | null;
  native: boolean;
  creator?: string;
  /** For coins launched from Phantom / MetaMask (creator = the gateway): the wallet that launched it, `sol:<pk>` / `evm:<0x…>`. */
  creatorWallet?: string;
  createdMs?: number;
  vibe?: string;
  rivals?: string[];
  priceNear: number | null;
  treasuryNear: number;
  chestNear: number;
  burned: number;
  supply: number;
};

const safe = async <T,>(p: Promise<T>): Promise<T | null> => {
  try { return await p; } catch { return null; }
};

// ft_metadata (and its ≤24 KB icon) never changes: fetch once per session.
const metaCache = new Map<string, Promise<FtMeta | null>>();
const lastGood = new Map<string, Coin>();

/**
 * One coin, read fresh. A failed read THROWS (it never degrades to "not an NME coin" or a missing price): pollers keep the
 * last good copy instead of flickering cards away while the public RPC rate-limits. `null` only when the coin really is unknown.
 */
export async function loadCoin(keyIn: string, rec?: CoinRecord, lenient = false): Promise<Coin | null> {
  // `/coin/toad.nme.testnet` (the account) means the NME coin `toad`.
  const key = keyIn.endsWith(`.${NME.factory}`) ? keyIn.slice(0, -(NME.factory.length + 1)) : keyIn;
  // `lenient`: a failed read gives a partial card instead of an error (the home grid's first load must show every coin).
  const v = <T,>(c: string, m: string, a: object = {}) => (lenient ? safe(view<T>(c, m, a)) : view<T>(c, m, a)) as Promise<T>;
  const entry = await v<CoinEntry | null>(NME.arena, "get_coin", { key });
  const account = entry?.account ?? rec?.account ?? (key.includes(".") ? key : `${key}.${NME.factory}`);
  if (!entry && !rec && !key.includes(".")) return null; // not registered anywhere
  const native = entry ? entry.kind === "Native" : !!rec;
  const [meta, summary] = await Promise.all([
    metaCache.get(account) ?? v<FtMeta | null>(account, "ft_metadata").then((m) => { if (m) metaCache.set(account, Promise.resolve(m)); return m; }),
    native ? v<Summary | null>(account, "get_summary") : Promise.resolve(null),
  ]);
  const poolId = entry?.pool_id ?? summary?.pool.pool_id ?? null;
  let priceNear: number | null = null;
  if (poolId) {
    const pool = await v<{ token_x: string; current_point: number } | null>(NME.dcl, "get_pool", { pool_id: poolId });
    if (pool) priceNear = priceFromPoint(pool.current_point, pool.token_x === account);
  }
  const dec = meta?.decimals ?? 18;
  return {
    key,
    ticker: (meta?.symbol ?? key).toUpperCase(),
    name: meta?.name ?? key,
    account,
    icon: meta?.icon ?? null,
    native,
    creator: summary?.creator ?? rec?.creator,
    creatorWallet: (summary?.creator ?? rec?.creator) === NME.gateway ? (await launchWallet(key)) ?? undefined : undefined,
    createdMs: rec?.created_ms,
    vibe: summary?.persona.vibe,
    rivals: summary?.persona.rivals,
    priceNear: priceNear !== null ? priceNear * Math.pow(10, 18 - dec) : null,
    treasuryNear: near(entry?.treasury_near),
    chestNear: near(entry?.chest_near),
    burned: tokens(summary?.burned),
    supply: tokens(summary?.supply),
  };
}

/** Every coin the arena knows (NME coins + enlisted outside fighters). */
export async function loadCoins(): Promise<Coin[]> {
  const [list, roster] = await Promise.all([
    view<[string, CoinRecord][]>(NME.factory, "list_coins", { from_index: 0, limit: 50 }),
    safe(view<string[]>(NME.arena, "roster", { from: 0, limit: 100 })),
  ]);
  const recs = new Map(list.filter(([, r]) => r.status === "Live").map(([t, r]) => [t, r]));
  const keys = Array.from(new Set([...recs.keys(), ...(roster ?? [])]));
  // A coin whose read failed this round keeps its last good copy rather than vanishing from the grid.
  const coins = await Promise.all(keys.map(async (k) => {
    const c = await safe(loadCoin(k, recs.get(k)));
    if (c) lastGood.set(k, c);
    // Never drop a coin from the grid for one failed read: last good copy, else a partial card.
    return c ?? lastGood.get(k) ?? (await safe(loadCoin(k, recs.get(k), true)));
  }));
  const out = coins.filter((c): c is Coin => !!c).sort((a, b) => (b.createdMs ?? 0) - (a.createdMs ?? 0));
  rememberSymbols(out);
  return out;
}

export const loadRound = () => view<Round | null>(NME.arena, "current_round");
export const loadWarConfig = () => view<WarConfig>(NME.arena, "get_war_config");
export const loadNextRoundMs = () => view<number>(NME.arena, "next_round_ms");
export const loadNextDueMs = () => view<number | null>(NME.arena, "next_due_ms");
export const loadFeedLastId = () => view<number>(NME.arena, "feed_last_id");
/** The arena logs a refund without a ticker; recover it from the matching `job_hired` (oldest first, `jobs` persists across polls). */
export function fillJobTickers(batch: FeedItem[], jobs: Map<string, string>): FeedItem[] {
  return batch.map((f) => {
    const job = /^job=(\d+)/.exec(f.extra)?.[1];
    if (!job) return f;
    if (f.kind === "job_hired" && f.ticker) jobs.set(job, f.ticker);
    if (f.kind === "job_refunded" && !f.ticker && jobs.has(job)) return { ...f, ticker: jobs.get(job)! };
    return f;
  });
}

export const loadFeedSince = (id: number, limit = 100) => view<FeedItem[]>(NME.arena, "feed_since", { id, limit });
export const loadCommentary = (limit = 20) => view<Commentary[]>(NME.arena, "get_commentary", { limit });

// ---- Plain-English feed lines ----
// Tone = which palette colour the line wears. Each colour means one thing across the whole site.
export type Tone = "buy" | "sell" | "burn" | "war" | "ai" | "launch" | "chest" | "plain";
export type Line = { tone: Tone; tag: string; text: string; ai?: boolean };

// Outside coins are keyed by account; show their on-chain symbol once the roster has loaded it.
const symbols = new Map<string, string>();
export const rememberSymbols = (coins: Coin[]) => coins.forEach((c) => symbols.set(c.key, c.ticker));
/** AI text names outside coins by their token account (that's what the arena tells the brain); show their symbol instead. */
export function prettyText(text: string): string {
  let out = text;
  for (const [key, sym] of symbols) if (key.includes(".")) out = out.split(key).join(`$${sym}`);
  return out;
}
export const displayTicker = (k: string) => symbols.get(k) ?? (k.includes(".") ? k.split(".")[0].replace(/^nme-/, "") : k).toUpperCase();
const who = (a: string) => short(a, 20);
const kv = (extra: string, k: string) => extra.match(new RegExp(`${k}=([^\\s]+)`))?.[1];

export function describe(f: FeedItem): Line {
  const T = displayTicker;
  const t = `$${T(f.ticker)}`;
  switch (f.kind) {
    case "trade":
      if (f.extra.startsWith("sell")) return { tone: "sell", tag: "Sell", text: `${who(f.actor)} sold ${compact(tokens(kv(f.extra, "tokens")))} ${t}` };
      return { tone: "buy", tag: "Buy", text: `${who(f.actor)} bought ${t} for ${nearStr(f.amount)}` };
    case "chest_funded":
      return { tone: "chest", tag: "Chest", text: `${who(f.actor)} added ${nearStr(f.amount)} to ${t}'s war chest` };
    case "war_started":
      return { tone: "war", tag: "War", text: `${t} vs $${T(f.actor)} — the fight is on. ${nearStr(f.amount)} at stake` };
    case "war_settled":
      return { tone: "war", tag: "Winner", text: `${t} beat $${T(f.actor)}. ${nearStr(f.amount)} of the loser's chest buys ${t}: half burned, half to ${t}'s fighters` };
    case "spoils_reserved":
      return { tone: "chest", tag: "Spoils", text: `${compact(tokens(f.amount))} ${t} reserved for the fighters who won round ${kv(f.extra, "round") ?? "?"}: claim on the Colosseum page` };
    case "spoils_claimed":
      return { tone: "chest", tag: "Spoils", text: `${who(f.actor)} claimed ${compact(tokens(f.amount))} ${t} of spoils (round ${kv(f.extra, "round") ?? "?"})` };
    case "war_void":
      return { tone: "plain", tag: "No contest", text: `${t} vs $${T(f.actor)} called off: ${f.extra}` };
    case "treasury_buyback":
      return { tone: "burn", tag: "Burn", text: `${compact(tokens(kv(f.extra, "tokens")))} ${t} burned forever (bought with ${nearStr(f.amount)})` };
    case "defense": {
      const reason = f.extra.replace(/^(hold|buyback \d+ bps):\s*/, "");
      if (f.extra.startsWith("hold")) return { tone: "ai", tag: "Held", ai: true, text: `${t} decided not to buy back: “${reason}”` };
      return { tone: "burn", tag: "Defense", ai: f.actor === "ai", text: `${t} defended its price with ${(Number(f.amount) / 100).toFixed(2)}% of its treasury: “${reason}”` };
    }
    case "commentary":
      return { tone: "ai", tag: "Trash talk", ai: true, text: `${t}: ${prettyText(f.extra.replace(/^[a-z_]+:\s*/, ""))}` };
    case "job_hired":
      return { tone: "ai", tag: "Hired", text: `${who(f.actor)} hired ${t}'s brain for a ${kv(f.extra, "kind") ?? "job"} (${nearStr(f.amount)})` };
    case "job_delivered":
      return { tone: "ai", tag: "Delivered", ai: true, text: `${t} finished a job. ${nearStr(f.amount)} is set aside to buy and burn ${t} at the next poke. “${prettyText(f.extra.replace(/^job=\d+\s*/, ""))}”` };
    case "job_refunded":
    {
      const why = f.extra.replace(/^job=\d+\s*/, "");
      const brain = f.ticker ? `${t}'s brain` : "The brain";
      return { tone: "plain", tag: "Refund", text: `${brain} couldn't finish the job${why ? ` (${why})` : ""} — ${nearStr(f.amount)} refunded to ${who(f.actor)}` };
    }
    case "raid_started":
      return { tone: "launch", tag: "Raid", text: `${t} called a raid on NEAR Social: post with ${f.extra.replace(/^raid=\d+\s*/, "")}` };
    case "raid_claimed":
      return { tone: "chest", tag: "Raider", text: `${who(f.actor)} raided for ${t} (post ${kv(f.extra, "posts") ?? "?"})${BigInt(f.amount || "0") > 0n ? `, +${nearStr(f.amount)} of war score` : ""}` };
    case "raid_settled":
      return { tone: "burn", tag: "Raid over", text: `${t}'s raid closed with ${kv(f.extra, "posts") ?? "0"} posts${BigInt(f.amount || "0") > 0n ? `: ${nearStr(f.amount)} of the treasury buys and burns ${t}` : ""}` };
    case "fees_claimed":
      return { tone: "chest", tag: "Fees", text: `${t} collected ${nearStr(f.amount)} in pool fees` };
    case "fees_forfeited":
      return { tone: "plain", tag: "PumpBag", text: `${t}'s PumpBag went quiet — its fees moved to the treasury` };
    case "coin_registered":
      return { tone: "launch", tag: "New", text: `${t} was born` };
    case "launch_finalized":
      return { tone: "launch", tag: "Live", text: `${t} is live — trading is open` };
    case "coin_enlisted":
      return { tone: "launch", tag: "Enlisted", text: `${t} joined the Colosseum as an outside fighter` };
    default:
      return { tone: "plain", tag: f.kind, text: `${t} ${f.extra}` };
  }
}

// ---- Coin page views ----
export type CrownStatus = { owner: string; vault: string; inactivity_ms: number; last_heartbeat_ms: number; forfeitable_after_ms: number; forfeited: boolean };
export type DefenseConfig = { per_action_bps: number; daily_bps: number; cooldown_ms: number; interval_ms: number; look_points: number; trigger_points: number; fallback_after: number; min_treasury: string; outlayer_fee: string; fee_cap_per_day: string };
export type DefenseState = { high_point: number; high_ms: number; last_action_ms: number; day_start_ms: number; day_spent_bps: number; misses: number; in_flight_until_ms: number };
export type Delivery = { seq: number; job_id: number; ticker: string; kind: string; text: string; model: string; chat_id: string; ts_ms: number; media_url?: string | null; media_hash?: string | null; request_hash?: string; response_hash?: string };
export const JOB_KINDS = ["roast", "shill", "lore", "meme"] as const;

/** "6 d 4 h", "3 h 12 min", "8 min". */
export function timeLeft(ms: number): string {
  if (ms <= 0) return "now";
  const m = Math.floor(ms / 60000), h = Math.floor(m / 60), d = Math.floor(h / 24);
  if (d >= 1) return `${d} d ${h % 24} h`;
  if (h >= 1) return `${h} h ${m % 60} min`;
  return `${Math.max(1, m)} min`;
}

export type BagState = { tone: "ok" | "soon" | "due" | "gone"; label: string; left: number };
/** Where a PumpBag stands: active, deadline within a day, past it (anyone can forfeit), or forfeited. */
export function bagState(c: CrownStatus, now: number): BagState {
  const left = c.forfeitable_after_ms - now;
  if (c.forfeited) return { tone: "gone", label: "Forfeited", left };
  if (left <= 0) return { tone: "due", label: "Inactive: can be forfeited", left };
  if (left < 86_400_000 || left < c.inactivity_ms / 7) return { tone: "soon", label: `Active, heartbeat due in ${timeLeft(left)}`, left };
  return { tone: "ok", label: `Active, safe for ${timeLeft(left)}`, left };
}
export const BAG_TONE: Record<BagState["tone"], string> = { ok: "bg-lime text-ink", soon: "bg-yolk text-ink", due: "bg-red text-cream", gone: "bg-paper text-ink" };

// The wallet that launched a coin through the gateway never changes: read it once per session.
const launchCache = new Map<string, Promise<string | null>>();
function launchWallet(key: string): Promise<string | null> {
  if (!launchCache.has(key)) {
    const p = view<{ owner: string } | null>(NME.gateway, "get_launch", { ticker: key }).then((l) => l?.owner ?? null).catch(() => null);
    launchCache.set(key, p);
    p.then((v) => { if (v === null) launchCache.delete(key); });
  }
  return launchCache.get(key)!;
}

export type WalletLabel = { kind: string; short: string; full: string; href: string };
/** Names an account exactly: a NEAR account, a Phantom (Solana) key or a MetaMask (Ethereum) address. */
export function walletLabel(acct?: string | null): WalletLabel | null {
  if (!acct) return null;
  if (acct.startsWith("sol:")) {
    const k = acct.slice(4);
    return { kind: "Phantom (Solana)", short: `${k.slice(0, 4)}…${k.slice(-4)}`, full: k, href: `https://explorer.solana.com/address/${k}?cluster=devnet` };
  }
  if (acct.startsWith("evm:")) {
    const a = acct.slice(4);
    return { kind: "MetaMask (Ethereum)", short: `${a.slice(0, 6)}…${a.slice(-4)}`, full: a, href: `https://sepolia.etherscan.io/address/${a}` };
  }
  return { kind: "NEAR", short: acct, full: acct, href: `${NME.explorer}/address/${acct}` };
}
/** Plain-text version for tight spots (cards). */
export const holderName = (acct?: string | null) => {
  const w = walletLabel(acct);
  return !w ? "–" : w.kind === "NEAR" ? w.short : `${w.kind} ${w.short}`;
};

export const loadCrown = (ticker: string) => view<CrownStatus | null>(NME.crown, "crown_status", { token_id: ticker });
export const loadDefenseConfig = () => view<DefenseConfig>(NME.arena, "get_defense_config");
export const loadDefenseState = (ticker: string) => view<DefenseState | null>(NME.arena, "get_defense_state", { ticker });
export const loadJobPrice = (ticker: string) => view<string>(NME.arena, "get_job_price", { ticker });
export type Raid = {
  id: number; ticker: string; tag: string; starter: string; start_block: number; end_block: number; start_ms: number;
  claims: number; points: string; points_round: number; state: "Open" | "Settled"; burned_near: string;
};
export type RaidRules = { blocks: number; grace_blocks: number; cooldown_ms: number; max_claims: number; burn_bps_per_claim: number; burn_bps_max: number; war_points_bps: number; war_points_cap_bps: number; start_deposit: string; claim_deposit: string; social: string };
export const loadLatestRaid = (ticker: string) => view<Raid | null>(NME.arena, "latest_raid", { ticker });
export const loadRaidRules = () => view<RaidRules>(NME.arena, "raid_rules");
export const loadRaidClaim = (raidId: number, account: string) => view<boolean | null>(NME.arena, "raid_claim_of", { raid_id: raidId, account });
export type SocialPost = { text: string; image?: string; block: number };
/** Latest NEAR Social post of each account, straight from SocialDB (one `get`). */
export async function loadSocialPosts(accounts: string[]): Promise<Record<string, SocialPost>> {
  if (!accounts.length) return {};
  const raw = await view<Record<string, { post?: { main?: { ":block"?: number; ""?: string } } }>>(NME.social, "get", { keys: accounts.map((a) => `${a}/post/main`), options: { with_block_height: true } });
  const out: Record<string, SocialPost> = {};
  for (const a of accounts) {
    const m = raw?.[a]?.post?.main;
    if (!m || typeof m[""] !== "string") continue;
    try {
      const p = JSON.parse(m[""]) as { text?: unknown; image?: { url?: unknown } };
      if (typeof p.text !== "string") continue;
      out[a] = { text: p.text.slice(0, 1000), image: typeof p.image?.url === "string" ? p.image.url : undefined, block: m[":block"] ?? 0 };
    } catch {
      // not a post we can read
    }
  }
  return out;
}

export const loadSocialStorage = (account: string) => view<{ total: string; available: string } | null>(NME.social, "storage_balance_of", { account_id: account });

export const loadDeliveries = (limit = 32) => view<Delivery[]>(NME.arena, "get_deliveries", { limit });
export const loadBalance = (token: string, account: string) => view<string>(token, "ft_balance_of", { account_id: account });

/** Ref DCL quote for selling raw coin amount into wNEAR (yocto). */
export async function quoteSell(coin: Coin, rawAmount: string): Promise<string | null> {
  const entry = await view<CoinEntry | null>(NME.arena, "get_coin", { key: coin.key }).catch(() => null);
  if (!entry?.pool_id) return null;
  const q = await view<{ amount: string }>(NME.dcl, "quote", { pool_ids: [entry.pool_id], input_token: coin.account, output_token: NME.wrap, input_amount: rawAmount, tag: null }).catch(() => null);
  return q?.amount && q.amount !== "0" ? q.amount : null;
}

export type Job = { id: number; ticker: string; kind: string; payer: string; amount: string; status: "Requested" | "Delivered" | "Burned" | "Refunded"; deadline_ms: number };
export const loadJob = (job_id: number) => view<Job | null>(NME.arena, "get_job", { job_id });
export const loadBrainConfig = () => view<{ cut_bps: number }>(NME.arena, "get_brain_config");

/** Delivered jobs whose buy-and-burn hasn't run yet (it runs at the next arena poke), and roughly how many tokens it will burn. */
export async function pendingBurns(coin: Coin, jobIds: number[]): Promise<{ near: number; jobs: number; tokens: number | null }> {
  const [jobs, cfg] = await Promise.all([Promise.all(jobIds.map((id) => loadJob(id).catch(() => null))), loadBrainConfig()]);
  const waiting = jobs.filter((j): j is Job => !!j && j.status === "Delivered");
  const net = waiting.reduce((n, j) => n + BigInt(j.amount) - (BigInt(j.amount) * BigInt(cfg.cut_bps)) / 10000n, 0n);
  if (net === 0n) return { near: 0, jobs: 0, tokens: 0 };
  const entry = await view<CoinEntry | null>(NME.arena, "get_coin", { key: coin.key }).catch(() => null);
  const q = entry?.pool_id
    ? await view<{ amount: string }>(NME.dcl, "quote", { pool_ids: [entry.pool_id], input_token: NME.wrap, output_token: coin.account, input_amount: net.toString(), tag: null }).catch(() => null)
    : null;
  return { near: near(net.toString()), jobs: waiting.length, tokens: q?.amount ? tokens(q.amount) : null };
}
