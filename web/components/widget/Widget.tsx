"use client";
import { useEffect, useRef, useState } from "react";
import { NME } from "@/lib/config";
import { clock, compact, near } from "@/lib/format";
import { useArena, useNow, usePolled } from "@/lib/hooks";
import { displayTicker, loadCoin, loadDeliveries } from "@/lib/nme";
import { CoinIcon, useIconTint } from "../CoinIcon";
import { useActions, WalletButton } from "../Actions";
import { Hire } from "../coin/CoinPage";
import { TradeBox } from "../TradeBox";

/** The embeddable coin card: what the coin is, whether it is fighting right now, and Buy / Hire. */
export function Widget({ coinKey }: { coinKey: string }) {
  const coin = usePolled(() => loadCoin(coinKey), 15000, [coinKey]);
  const deliveries = usePolled(coin.data?.native ? () => loadDeliveries(32) : null, 30000, [coin.data?.native]).data;
  const arena = useArena();
  const now = useNow(1000);
  const { buy } = useActions();
  const tint = useIconTint(coin.data?.icon);
  const [tab, setTab] = useState<"buy" | "hire" | "chat">("buy");
  const root = useRef<HTMLDivElement>(null);

  // Embedded: sheets (buy, wallets) open inside the card's frame instead of floating over a page we don't own,
  // and we tell the host page our full height so the iframe grows with them and never scrolls.
  useEffect(() => {
    document.documentElement.classList.add("embed");
    if (window.parent === window) return;
    const report = () => window.parent.postMessage({ type: "nme:height", ticker: coinKey, h: Math.ceil(document.body.getBoundingClientRect().height) }, "*");
    const ro = new ResizeObserver(report);
    ro.observe(document.body);
    const mo = new MutationObserver(report);
    mo.observe(document.body, { childList: true, subtree: true });
    return () => { ro.disconnect(); mo.disconnect(); };
  }, [coinKey]);

  const c = coin.data;
  const r = arena.data?.round;
  const pair = r?.pairs.find((p) => p.a === coinKey || p.b === coinKey);
  const otherKey = pair ? (pair.a === coinKey ? pair.b : pair.a) : null;
  const rivalCoin = usePolled(otherKey ? () => loadCoin(otherKey) : null, 0, [otherKey]).data;
  const live = !!pair && !!now && now < r!.end_ms && pair.state === "Pending";
  const bell = !!pair && !!now && now >= r!.end_ms && pair.state === "Pending";
  const mineA = pair?.a === coinKey;
  const rivalKey = pair ? (mineA ? pair.b : pair.a) : null;
  const rival = rivalCoin?.ticker ?? (rivalKey ? displayTicker(rivalKey) : "");
  const sm = pair ? near(mineA ? pair.score_a : pair.score_b) : 0;
  const so = pair ? near(mineA ? pair.score_b : pair.score_a) : 0;
  const share = sm + so > 0 ? sm / (sm + so) : 0.5;

  return (
    <div ref={root} className="nme-card min-h-0 bg-transparent p-2">
      <div className="overflow-hidden rounded-[1.6rem] border border-ink bg-paper shadow-[0_18px_40px_-22px_var(--color-drop)]">
        {!c ? (
          <p className="p-6 font-serif text-xl">{coin.error ? `Couldn't load $${coinKey.toUpperCase()}: ${coin.error}` : "Loading…"}</p>
        ) : (
          <>
            <div className="flex items-center gap-3 border-b border-ink p-4" style={{ background: tint }}>
              <CoinIcon src={c.icon} ticker={c.ticker} size={60} className="rounded-xl border-2 border-ink bg-paper" />
              <div className="min-w-0 flex-1">
                <p className="font-display text-3xl leading-none">${c.ticker}</p>
                <p className="truncate text-sm">{c.name}</p>
              </div>
              <WalletButton />
            </div>

            <dl className="num grid grid-cols-3 border-b border-ink text-sm">
              {[["Price", c.priceNear !== null ? `${compact(c.priceNear)} Ⓝ` : "–"], ["War chest", `${compact(c.chestNear)} Ⓝ`], ["Burned", compact(c.burned)]].map(([k, v], i) => (
                <div key={k} className={`px-3 py-2.5 ${i < 2 ? "border-r border-ink" : ""}`}><dt className="text-xs text-ink-2">{k}</dt><dd className="font-bold">{v}</dd></div>
              ))}
            </dl>

            <div className={`border-b border-ink p-4 ${live ? "bg-red-deep" : "bg-paper-2"}`}>
              {live ? (
                <>
                  <p className="flex items-center justify-between font-semibold">
                    <span><span className="mr-1 inline-block h-2 w-2 animate-pulse rounded-full bg-paper" /> At war with ${rival}</span>
                    <span className="font-display text-xl tnum">{clock(r!.end_ms - now)}</span>
                  </p>
                  <div className="mt-2 h-4 overflow-hidden rounded-full border border-ink bg-night/50" role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(share * 100)} aria-label={`$${c.ticker} share of hits`}>
                    <div className="h-full bg-red" style={{ width: `${Math.max(4, share * 100)}%` }} />
                  </div>
                  <p className="mt-1.5 text-xs">Every buy of 0.25 NEAR or more is a hit. The loser&apos;s chest buys the winner and burns it.</p>
                </>
              ) : bell ? (
                <p className="text-sm"><strong>Fight with ${rival} is over</strong>, waiting for anyone to ring the bell and settle it.</p>
              ) : (
                <p className="text-sm"><strong>Not fighting right now.</strong> A bigger war chest gets ${c.ticker} into the next round.</p>
              )}
            </div>

            <div className="flex border-b border-ink" role="tablist">
              {(["buy", "hire", "chat"] as const).filter((t) => t !== "hire" || c.native).map((t) => (
                <button key={t} role="tab" aria-selected={tab === t} onClick={() => setTab(t)} className={`flex-1 py-2.5 text-sm font-bold ${tab === t ? "bg-night text-cream" : ""}`}>
                  {t === "buy" ? (live ? "Join the war" : "Buy") : t === "hire" ? "Hire its brain" : "Chat"}
                </button>
              ))}
            </div>

            <div className="p-4">
              {tab === "chat" ? (
                <TradeBox defaultTicker={coinKey} compactLayout />
              ) : tab === "buy" ? (
                <>
                  <button className="btn w-full bg-mint text-lg" onClick={() => buy(c)}>{live ? `Buy $${c.ticker} and hit $${rival}` : `Buy $${c.ticker}`}</button>
                  <p className="mt-2 text-center text-xs text-ink-2">No tax, no pause: buying and selling are always free. Pay with a NEAR wallet, Phantom or an EVM wallet.</p>
                </>
              ) : (
                <div className="-m-4 [&>section]:rounded-none [&>section]:border-0 [&>section]:shadow-none">
                  <Hire coin={c} deliveries={(deliveries ?? []).filter((d) => d.ticker === coinKey).slice(0, 2)} />
                </div>
              )}
            </div>

            <a href={`/coin/${encodeURIComponent(coinKey)}`} target="_blank" rel="noreferrer" className="flex items-center justify-between border-t border-ink bg-night px-4 py-2 text-xs font-semibold text-cream">
              <span><span className="font-display text-base">nme</span> · memes that fight on NEAR {NME.network}</span>
              <span className="underline">Open full page</span>
            </a>
          </>
        )}
      </div>
    </div>
  );
}
