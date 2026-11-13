"use client";
import Link from "next/link";
import { Coin, holderName } from "@/lib/nme";
import { compact } from "@/lib/format";
import { CoinIcon, useIconTint } from "./CoinIcon";
import { useActions } from "./Actions";

function Card({ coin, fighting, flash }: { coin: Coin; fighting: boolean; flash?: "buy" | "burn" }) {
  const tint = useIconTint(coin.icon);
  const { buy } = useActions();
  return (
    <article className="group relative flex min-w-0 flex-col overflow-hidden rounded-[1.2rem] border border-ink bg-paper shadow-[0_18px_40px_-22px_var(--color-drop)] transition-transform duration-200 hover:-translate-y-1">
      <div className={`relative grid aspect-[4/3] place-items-center border-b border-ink ${flash ? "flash" : ""}`} style={{ background: tint, ["--flash" as string]: flash === "burn" ? "var(--color-yolk)" : "var(--color-mint)" }}>
        <CoinIcon src={coin.icon} ticker={coin.ticker} size={84} className="rounded-[1rem] transition-transform duration-300 group-hover:scale-105 group-hover:-rotate-2 sm:!h-24 sm:!w-24" />
        <div className="absolute left-2 top-2 flex flex-wrap gap-1 [&>.chip]:px-2 [&>.chip]:py-0.5 [&>.chip]:text-[0.62rem]">
          {fighting && <span className="chip bg-red text-cream">At war</span>}
          {!coin.native && <span className="chip bg-blue text-cream">Outside fighter</span>}
          {coin.native && !fighting && <span className="chip bg-paper">Live</span>}
        </div>
      </div>
      <div className="flex flex-1 flex-col gap-2 p-3">
        <div>
          <h3 className="truncate font-display text-xl leading-none sm:text-2xl">
            <Link href={`/coin/${encodeURIComponent(coin.key)}`} className="after:absolute after:inset-0 hover:underline">${coin.ticker}</Link>
          </h3>
          <p className="mt-1 truncate text-xs text-ink-2">{coin.name}{coin.vibe ? ` — ${coin.vibe}` : ""}</p>
          <p className="mt-0.5 truncate text-[0.7rem] text-ink-2">{coin.native ? `by ${holderName(coin.creatorWallet ?? coin.creator)}` : "outside fighter"}</p>
        </div>
        <dl className="num grid grid-cols-3 gap-1.5 text-[0.68rem] leading-tight">
          <div><dt className="text-ink-2">Price</dt><dd className="font-bold">{coin.priceNear !== null ? `${compact(coin.priceNear)} Ⓝ` : "–"}</dd></div>
          <div><dt className="text-ink-2">Chest</dt><dd className="font-bold">{compact(coin.chestNear)} Ⓝ</dd></div>
          <div><dt className="text-ink-2">Burned</dt><dd className="font-bold">{compact(coin.burned)}</dd></div>
        </dl>
        <button className="btn relative z-10 mt-auto w-full bg-mint px-2 py-1.5 text-xs sm:text-sm" onClick={() => buy(coin)}>Buy ${coin.ticker}</button>
      </div>
    </article>
  );
}

export function MemeGrid({ coins, fighting, flashes, loading, error }: { coins: Coin[]; fighting: Set<string>; flashes: Map<string, "buy" | "burn">; loading: boolean; error: string | null }) {
  return (
    <section id="memes" aria-labelledby="memes-h" className="px-4 py-16 sm:px-8 sm:py-24">
      <div className="mb-10 flex flex-wrap items-end justify-between gap-4">
        <h2 id="memes-h" className="font-hand text-[clamp(3.2rem,7vw,5.6rem)] leading-[0.9]">the roster</h2>
        <p className="max-w-[40ch] text-[1.05rem] text-ink-2">Every meme that can fight right now, newest first. Coins from other launchpads can enlist too.</p>
      </div>
      {error && !coins.length && <p className="rounded-2xl border border-ink bg-red p-4 text-cream">Couldn&apos;t load the roster: {error}</p>}
      {loading && !coins.length && <p className="text-ink-2">Loading memes from testnet…</p>}
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 sm:gap-4 lg:grid-cols-4 xl:grid-cols-5">
        {coins.map((c) => (
          <Card key={c.key} coin={c} fighting={fighting.has(c.key)} flash={flashes.get(c.key)} />
        ))}
      </div>
    </section>
  );
}
