"use client";
// The coin's market at a glance: lifetime pool numbers (every trade on its Rhea pool), supply and burns, its ledgers,
// its record in the arena and the latest trades. Phone first: two columns, four on wide screens.
import { compact } from "@/lib/format";
import { useNow } from "@/lib/hooks";
import { Coin, Delivery, FeedItem, describe } from "@/lib/nme";
import { ago } from "@/lib/format";

function Stat({ k, v, sub }: { k: string; v: string; sub?: string }) {
  return (
    <div className="min-w-0 rounded-2xl border border-ink bg-paper p-3 sm:p-4">
      <dt className="text-[0.68rem] font-semibold uppercase tracking-wide text-ink-2">{k}</dt>
      <dd className="num mt-1 truncate text-xl font-bold sm:text-2xl">{v}</dd>
      {sub && <p className="mt-0.5 truncate text-xs text-ink-2">{sub}</p>}
    </div>
  );
}

function Bar({ left, right, leftLabel, rightLabel, leftTone, rightTone }: { left: number; right: number; leftLabel: string; rightLabel: string; leftTone: string; rightTone: string }) {
  const total = left + right;
  const pct = total > 0 ? Math.round((left / total) * 100) : 50;
  return (
    <div>
      <div className="flex h-3 overflow-hidden rounded-full border border-ink" role="img" aria-label={`${leftLabel} ${pct}%, ${rightLabel} ${100 - pct}%`}>
        <div className={leftTone} style={{ width: `${pct}%` }} />
        <div className={`flex-1 ${rightTone}`} />
      </div>
      <div className="mt-1 flex justify-between text-xs font-semibold">
        <span>{leftLabel} {pct}%</span>
        <span>{rightLabel} {100 - pct}%</span>
      </div>
    </div>
  );
}

export function CoinMarket({ coin, feed, deliveries }: { coin: Coin; feed: FeedItem[]; deliveries: Delivery[] }) {
  const now = useNow(30000);
  const p = coin.pool;
  const price = coin.priceNear ?? 0;
  const liquidity = p ? p.nearInPool + p.tokensInPool * price : 0;
  const minted = coin.supply + coin.burned;
  const burnPct = minted > 0 ? (coin.burned / minted) * 100 : 0;
  const trades = feed.filter((f) => f.kind === "trade" && f.ticker === coin.key);
  const buys = trades.filter((f) => !f.extra.startsWith("sell")).length;
  const sells = trades.length - buys;
  const won = feed.filter((f) => f.kind === "war_settled" && f.ticker === coin.key).length;
  const lost = feed.filter((f) => f.kind === "war_settled" && f.actor === coin.key).length;
  const raids = feed.filter((f) => f.kind === "raid_claimed" && f.ticker === coin.key).length;
  const jobs = deliveries.filter((d) => d.ticker === coin.key).length;

  return (
    <section className="border-b border-ink bg-paper-2 px-4 py-8 sm:px-8">
      <div className="mx-auto max-w-6xl space-y-6">
        <div className="flex flex-wrap items-end justify-between gap-2">
          <h2 className="font-serif text-[clamp(1.8rem,4vw,2.6rem)] font-light leading-tight">Market</h2>
          <p className="text-xs text-ink-2">Pool numbers count every trade on ${coin.ticker}&apos;s Rhea pool since launch.</p>
        </div>

        <dl className="grid grid-cols-2 gap-3 lg:grid-cols-3">
          <Stat k="Liquidity" v={p ? `${compact(liquidity)} NEAR` : "–"} sub={p ? `${compact(p.nearInPool)} NEAR + ${compact(p.tokensInPool)} $${coin.ticker}` : undefined} />
          <Stat k="Pool fees earned" v={p ? `${compact(p.feesNear)} NEAR` : "–"} sub="split creator / treasury / protocol" />
          <Stat k="Bought" v={p ? `${compact(p.nearBought)} NEAR` : "–"} sub="NEAR paid in, all time" />
          <Stat k="Sold" v={p ? `${compact(p.nearSold)} NEAR` : "–"} sub="NEAR paid out, all time" />
          <Stat k="Treasury" v={`${compact(coin.treasuryNear)} NEAR`} sub="only buys back and burns" />
          <Stat k="War chest" v={`${compact(coin.chestNear)} NEAR`} sub="only buys rivals in wars" />
        </dl>

        <div className="grid gap-4 md:grid-cols-2">
          <div className="rounded-2xl border border-ink bg-paper p-4">
            <p className="text-sm font-semibold">Buy vs sell pressure</p>
            <p className="mt-1 text-xs text-ink-2">Net flow {p ? `${p.nearBought - p.nearSold >= 0 ? "+" : ""}${compact(p.nearBought - p.nearSold)} NEAR` : "–"} into the pool</p>
            <div className="mt-3">{p ? <Bar left={p.nearBought} right={p.nearSold} leftLabel="Buys" rightLabel="Sells" leftTone="bg-mint" rightTone="bg-red" /> : <p className="text-sm">No pool yet.</p>}</div>
          </div>
          <div className="rounded-2xl border border-ink bg-paper p-4">
            <p className="text-sm font-semibold">Supply and burns</p>
            <p className="mt-1 text-xs text-ink-2">{compact(coin.supply)} left of {compact(minted)} ever minted</p>
            <div className="mt-3"><Bar left={coin.burned} right={coin.supply} leftLabel="Burned" rightLabel="Circulating" leftTone="bg-yolk" rightTone="bg-paper-2" /></div>
            <p className="mt-2 text-xs">{burnPct >= 0.01 ? `${burnPct.toFixed(2)}% burned forever` : "Burns so far are a sliver of supply"}</p>
          </div>
        </div>

        <div className="grid gap-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]">
          <div className="rounded-2xl border border-ink bg-paper p-4">
            <p className="text-sm font-semibold">Record <span className="font-normal text-ink-2">(recent activity)</span></p>
            <dl className="num mt-3 grid grid-cols-2 gap-x-4 gap-y-2 text-sm">
              <div className="flex justify-between gap-2"><dt>Wars won</dt><dd className="font-bold">{won}</dd></div>
              <div className="flex justify-between gap-2"><dt>Wars lost</dt><dd className="font-bold">{lost}</dd></div>
              <div className="flex justify-between gap-2"><dt>Buys</dt><dd className="font-bold">{buys}</dd></div>
              <div className="flex justify-between gap-2"><dt>Sells</dt><dd className="font-bold">{sells}</dd></div>
              <div className="flex justify-between gap-2"><dt>AI jobs</dt><dd className="font-bold">{jobs}</dd></div>
              <div className="flex justify-between gap-2"><dt>Raid posts</dt><dd className="font-bold">{raids}</dd></div>
            </dl>
          </div>
          <div className="min-w-0 rounded-2xl border border-ink bg-paper p-4">
            <p className="text-sm font-semibold">Latest trades</p>
            {trades.length === 0 && <p className="mt-2 text-sm text-ink-2">No trades through NME in the recent feed yet.</p>}
            <ul className="mt-2 divide-y divide-ink/15">
              {trades.slice(0, 8).map((f) => {
                const l = describe(f);
                const sell = f.extra.startsWith("sell");
                return (
                  <li key={f.id} className="flex items-start gap-3 py-2 text-sm">
                    <span className={`chip shrink-0 ${sell ? "bg-red text-cream" : "bg-mint text-ink"}`}>{sell ? "Sell" : "Buy"}</span>
                    <span className="min-w-0 flex-1 break-words leading-snug">{l.text}</span>
                    <span className="shrink-0 text-xs text-ink-2">{now ? ago(f.ts_ms, now) : ""}</span>
                  </li>
                );
              })}
            </ul>
          </div>
        </div>
      </div>
    </section>
  );
}
