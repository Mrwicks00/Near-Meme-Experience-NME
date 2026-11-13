import { NME } from "./config";
import { view } from "./rpc";
import type { Coin } from "./nme";

export const yoctoOf = (n: number) => {
  const [i, f = ""] = n.toFixed(6).split(".");
  return (BigInt(i) * 10n ** 24n + BigInt((f + "0".repeat(24)).slice(0, 24))).toString();
};

let feeBps: Promise<number> | null = null;
/** The arena router's fee on buys (fixed at deploy). */
export const routerFeeBps = () => (feeBps ??= view<{ cfg: { router_fee_bps: number } }>(NME.arena, "get_config").then((c) => c.cfg.router_fee_bps).catch(() => { feeBps = null; return 0; }));

/** Ref DCL quote: raw tokens out for spending `amountNear` NEAR on `coin` through the arena (after its router fee). */
export async function quoteBuy(coin: Coin, amountNear: number, afterFee = true): Promise<string | null> {
  try {
    const entry = await view<{ pool_id?: string }>(NME.arena, "get_coin", { key: coin.key });
    if (!entry?.pool_id) return null;
    const spend = afterFee ? amountNear * (1 - (await routerFeeBps()) / 10000) : amountNear;
    const q = await view<{ amount: string }>(NME.dcl, "quote", { pool_ids: [entry.pool_id], input_token: NME.wrap, output_token: coin.account, input_amount: yoctoOf(spend), tag: null });
    return q?.amount && q.amount !== "0" ? q.amount : null;
  } catch {
    return null;
  }
}

/** 3% slippage floor; null when there is no quote (we never send min_out 0). */
export const minOutOf = (raw: string | null) => (raw ? ((BigInt(raw) * 97n) / 100n).toString() : null);

/** The largest buy (NEAR, after the router fee) the coin's pool can fill right now: DCL quotes 0 when a swap can't be
 *  filled in full, so search between 0.001 and 5 NEAR. Cached for a minute per coin. */
const maxCache = new Map<string, { at: number; v: Promise<number> }>();
export function maxBuyNear(coin: Coin): Promise<number> {
  const c = maxCache.get(coin.key);
  if (c && Date.now() - c.at < 60_000) return c.v;
  const v = (async () => {
    if (await quoteBuy(coin, 5, false)) return 5;
    let lo = 0, hi = 5;
    for (let i = 0; i < 9; i++) {
      const m = (lo + hi) / 2;
      if (await quoteBuy(coin, Math.max(m, 0.001), false)) lo = m; else hi = m;
    }
    return lo;
  })();
  maxCache.set(coin.key, { at: Date.now(), v });
  return v;
}
