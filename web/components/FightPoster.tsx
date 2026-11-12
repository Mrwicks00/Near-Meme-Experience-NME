"use client";
import { useRef } from "react";
import gsap from "gsap";
import { useGSAP } from "@gsap/react";
import { Coin, Pair, displayTicker } from "@/lib/nme";
import { Arena, useNow } from "@/lib/hooks";
import { ago, clock, compact, near } from "@/lib/format";
import { CoinIcon } from "./CoinIcon";
import { useActions } from "./Actions";

gsap.registerPlugin(useGSAP);

export type Phase =
  | { kind: "live"; pair: Pair; endsMs: number; round: number }
  | { kind: "bell"; pair: Pair; endedMs: number; round: number }
  | { kind: "next"; a: Coin | undefined; b: Coin | undefined; startsMs: number };

export function phaseOf(arena: Arena | null, coins: Coin[], now: number): Phase | null {
  const r = arena?.round;
  if (r && r.pairs.length) {
    const open = r.pairs.find((p) => p.state === "Pending") ?? r.pairs[0];
    if (now < r.end_ms) return { kind: "live", pair: open, endsMs: r.end_ms, round: r.id };
    if (r.settled_pairs < r.pairs.length) return { kind: "bell", pair: open, endedMs: r.end_ms, round: r.id };
  }
  if (!coins.length) return null;
  // Pairing is by chest size among coins that clear the minimum: show the two biggest chests.
  const byChest = [...coins].sort((x, y) => y.chestNear - x.chestNear);
  return { kind: "next", a: byChest[0], b: byChest[1], startsMs: arena?.nextRoundMs ?? now };
}

function Corner({ side, coin, keyName, score, share, unique, minUnique, rival }: {
  side: "a" | "b"; coin?: Coin; keyName: string; score: number; share: number; unique: number; minUnique: number; rival: string;
}) {
  const { buy } = useActions();
  const red = side === "a";
  const ticker = coin?.ticker ?? displayTicker(keyName);
  return (
    <div className={`corner-${side} relative isolate flex flex-col ${red ? "items-start text-left max-md:bg-red-deep" : "items-end text-right max-md:bg-blue-deep max-md:pt-14"} gap-4 px-5 pb-10 pt-6 sm:px-10 lg:px-14`}>
      <div className={`absolute inset-0 -z-10 md:hidden ${red ? "halftone" : "halftone-light"}`} />
      <span className="chip border-cream/80 text-cream">{red ? "Red corner" : "Blue corner"}{coin && !coin.native ? " · outside fighter" : ""}</span>
      <div className={`fighter-icon relative ${red ? "-rotate-3" : "rotate-3"} rounded-[2rem] border border-ink bg-paper p-3 shadow-[0_18px_40px_-22px_var(--color-drop)]`}>
        <CoinIcon src={coin?.icon} ticker={ticker} size={136} className="rounded-[1.4rem] max-md:!h-24 max-md:!w-24" />
      </div>
      <h2 className={`fighter-name font-display text-[clamp(2.8rem,8.5vw,7.5rem)] leading-[0.82] tracking-[-0.02em] ${red ? "text-red" : "text-blue"}`}>
        {ticker}
      </h2>
      {coin?.vibe && <p className="max-w-[26ch] font-serif text-lg italic leading-snug text-cream/95">“{coin.vibe}”</p>}

      <div className="w-full max-w-md">
        <div className={`mb-1.5 flex justify-between text-sm font-semibold text-cream ${red ? "" : "flex-row-reverse"}`}>
          <span className="num">{score > 0 ? `${compact(score)} NEAR of hits` : "No hits yet"}</span>
          <span className="num">{unique}/{minUnique} wallets</span>
        </div>
        <div className={`health h-6 overflow-hidden rounded-full border border-ink bg-night/50 ${red ? "" : "rotate-180"}`} role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(share * 100)} aria-label={`${ticker} share of hits`}>
          <div className={`health-fill h-full rounded-full ${red ? "bg-red" : "bg-blue"} transition-[width] duration-700`} style={{ width: `${Math.max(4, share * 100)}%` }} />
        </div>
      </div>

      <div className={`flex flex-wrap items-center gap-3 ${red ? "" : "flex-row-reverse"}`}>
        <button className="btn bg-mint text-ink" onClick={() => coin && buy(coin)} disabled={!coin}>
          Buy ${ticker}
        </button>
        <span className="text-sm font-semibold text-cream">lands a hit on ${rival}</span>
      </div>
      {coin && <p className="num text-sm text-cream/90">War chest {compact(coin.chestNear)} NEAR</p>}
    </div>
  );
}

export function FightPoster({ arena, coins, loading, error }: { arena: Arena | null; coins: Coin[]; loading: boolean; error: string | null }) {
  const now = useNow(1000);
  const root = useRef<HTMLElement>(null);
  const { poke } = useActions();
  // Wait for the roster too, so the poster never flashes raw account ids.
  const phase = now && coins.length ? phaseOf(arena, coins, now) : null;
  const find = (k: string) => coins.find((c) => c.key === k || c.account === k);

  useGSAP(
    () => {
      if (!root.current) return;
      const mm = gsap.matchMedia();
      mm.add("(prefers-reduced-motion: no-preference)", () => {
        const tl = gsap.timeline({ defaults: { ease: "power4.out" } });
        tl.from(".corner-a", { xPercent: -18, opacity: 0, duration: 0.9 })
          .from(".corner-b", { xPercent: 18, opacity: 0, duration: 0.9 }, "<")
          .from(".fighter-name", { scale: 1.35, opacity: 0, duration: 0.6, ease: "back.out(2.2)", stagger: 0.12 }, "-=0.5")
          .from(".vs-stamp", { scale: 3, rotate: -30, opacity: 0, duration: 0.5, ease: "back.out(1.8)" }, "-=0.3")
          .from(".bell", { yPercent: 100, duration: 0.5 }, "-=0.2");
        gsap.to(".fighter-icon", { y: -8, duration: 1.6, ease: "sine.inOut", yoyo: true, repeat: -1, stagger: 0.8 });
      });
    },
    { scope: root, dependencies: [!!phase], revertOnUpdate: true },
  );

  if (!phase) {
    return (
      <section id="fight" className="grid min-h-[60vh] place-items-center bg-paper-2 px-6 text-center">
        <p className="font-serif text-2xl">{error ? `The Colosseum can't be reached right now: ${error}` : loading ? "Opening the Colosseum…" : "No memes have entered the Colosseum yet."}</p>
      </section>
    );
  }

  const keyA = phase.kind === "next" ? phase.a?.key ?? "?" : phase.pair.a;
  const keyB = phase.kind === "next" ? phase.b?.key ?? "?" : phase.pair.b;
  const A = find(keyA);
  const B = find(keyB);
  const sa = phase.kind === "next" ? 0 : near(phase.pair.score_a);
  const sb = phase.kind === "next" ? 0 : near(phase.pair.score_b);
  const total = sa + sb;
  const shareA = total > 0 ? sa / total : 0.5;
  const minUnique = arena?.war.min_unique ?? 2;
  const tA = A?.ticker ?? displayTicker(keyA);
  const tB = B?.ticker ?? displayTicker(keyB);

  return (
    <section id="fight" ref={root} aria-label={`${tA} versus ${tB}`} className="relative isolate overflow-hidden border-y border-ink bg-red-deep">
      <div className="halftone absolute inset-0 -z-10" />
      <div aria-hidden className="absolute inset-0 -z-10 bg-[radial-gradient(60%_80%_at_18%_45%,rgb(242_115_95/0.22),transparent_70%),radial-gradient(60%_80%_at_82%_45%,rgb(130_158_254/0.22),transparent_70%)]" />
      <div className="absolute inset-y-0 right-0 -z-10 hidden w-[56%] bg-blue-deep [clip-path:polygon(14%_0,100%_0,100%_100%,0_100%)] md:block">
        <div className="halftone-light h-full w-full" />
      </div>

      <div className="flex items-center justify-center gap-3 pt-5 text-cream">
        <span className="chip border-cream bg-night text-cream">
          {phase.kind === "live" && <span className="h-2 w-2 animate-pulse rounded-full bg-red" />}
          {phase.kind === "next" ? "Up next" : `Round ${phase.round}`}
        </span>
      </div>

      <div className="relative grid md:grid-cols-2">
        <Corner side="a" coin={A} keyName={keyA} score={sa} share={shareA} unique={phase.kind === "next" ? 0 : phase.pair.unique_a} minUnique={minUnique} rival={tB} />
        <div aria-hidden className="vs-stamp pointer-events-none z-10 grid h-24 w-24 place-items-center max-md:relative max-md:mx-auto max-md:-my-12 md:absolute md:left-1/2 md:top-1/2 md:-translate-x-1/2 md:-translate-y-1/2 rounded-full border border-ink bg-yolk pb-2 font-serif text-[3.2rem] font-light italic leading-none shadow-[0_18px_40px_-22px_var(--color-drop)] sm:h-32 sm:w-32 sm:text-[4.4rem]">
          vs
        </div>
        <Corner side="b" coin={B} keyName={keyB} score={sb} share={1 - shareA} unique={phase.kind === "next" ? 0 : phase.pair.unique_b} minUnique={minUnique} rival={tA} />
      </div>

      <div className="bell flex flex-col items-center justify-center gap-3 border-t border-ink bg-night px-4 py-4 text-center text-cream sm:flex-row sm:gap-6">
        {phase.kind === "live" && (
          <>
            <p className="font-display text-3xl tnum">{clock(phase.endsMs - now)} left in the fight</p>
            <p className="text-sm text-cream/80">Every buy of 0.25 NEAR or more is a punch. When time runs out, the loser&apos;s war chest buys the winner and burns it.</p>
          </>
        )}
        {phase.kind === "bell" && (
          <>
            <p className="text-base"><strong>The bell rang {ago(phase.endedMs, now)}.</strong> Anyone can settle the round and earn the poke bounty.</p>
            <button className="btn bg-yolk text-ink" onClick={() => poke()}>Settle the round</button>
          </>
        )}
        {phase.kind === "next" && (
          <>
            <p className="text-base">
              <strong>No fight right now.</strong> The two biggest war chests meet next
              {phase.startsMs > now ? <> — doors open in <span className="num">{clock(phase.startsMs - now)}</span></> : ". Anyone can open the next round."}
            </p>
            <button className="btn bg-yolk text-ink" onClick={() => poke()}>Open the next round</button>
          </>
        )}
      </div>
    </section>
  );
}
