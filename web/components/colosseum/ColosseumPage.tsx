"use client";
import { Spoils } from "./Spoils";
import dynamic from "next/dynamic";
import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { useArena, useCoins, useCommentary, useFeed, useNow } from "@/lib/hooks";
import { ago, clock, compact, near } from "@/lib/format";
import { describe, displayTicker, prettyText } from "@/lib/nme";
import { FightPoster, phaseOf } from "../FightPoster";
import { PUNCH_EVENT, type PunchDetail, useActions } from "../Actions";
import { QuickPunch } from "../QuickPunch";
import { TONE_BG } from "../Times";
import type { Bubble, Hit, Outcome } from "./Arena3D";

const Arena3D = dynamic(() => import("./Arena3D"), {
  ssr: false,
  loading: () => <div className="grid h-full place-items-center font-hand text-4xl">Building the Colosseum…</div>,
});

function canRender3D() {
  if (typeof window === "undefined") return false;
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return false;
  try {
    const c = document.createElement("canvas");
    return !!(c.getContext("webgl2") || c.getContext("webgl"));
  } catch {
    return false;
  }
}

export function ColosseumPage() {
  const coins = useCoins();
  const arena = useArena();
  const feed = useFeed(100);
  const commentary = useCommentary();
  const now = useNow(1000);
  const { punch, poke, play } = useActions();
  const [mode, setMode] = useState<"3d" | "2d" | null>(null);
  const [hits, setHits] = useState<(Hit & { at: number })[]>([]);

  useEffect(() => {
    const t = setTimeout(() => setMode(canRender3D() ? "3d" : "2d"), 0);
    return () => clearTimeout(t);
  }, []);

  const [outcome, setOutcome] = useState<Outcome>(null);
  const [replay, setReplay] = useState<{ a: string; b: string; until: number; round: string } | null>(null);

  const list = coins.data ?? [];
  const phase = now && list.length ? phaseOf(arena.data, list, now) : null;
  const replaying = !!replay && now < replay.until;
  const keyA = replaying ? replay!.a : phase ? (phase.kind === "next" ? phase.a?.key : phase.pair.a) : undefined;
  const keyB = replaying ? replay!.b : phase ? (phase.kind === "next" ? phase.b?.key : phase.pair.b) : undefined;
  const practice = !phase || phase.kind !== "live";
  // Punches only while the clock runs: once the bell rings, the fight buttons lock until the next round opens.
  const fighting = !!phase && phase.kind === "live" && phase.endsMs > (now ?? 0) && !replaying;
  const A = list.find((c) => c.key === keyA);
  const B = list.find((c) => c.key === keyB);
  const tA = A?.ticker ?? (keyA ? displayTicker(keyA) : "?");
  const tB = B?.ticker ?? (keyB ? displayTicker(keyB) : "?");

  // Your own punch lands the moment you tap (or your wallet confirms); its feed entry a few seconds later is then skipped.
  const mine = useRef<{ side: "a" | "b"; at: number }[]>([]);
  useEffect(() => {
    const on = (e: Event) => {
      const d = (e as CustomEvent<PunchDetail>).detail;
      if (!fighting || (d.key !== keyA && d.key !== keyB)) return;
      const side: "a" | "b" = d.key === keyA ? "a" : "b";
      const at = Date.now();
      mine.current = [...mine.current.filter((m) => at - m.at < 60000), { side, at }];
      setHits((h) => [...h, { id: -at - Math.random(), side, label: `+${compact(d.amountNear)} Ⓝ`, at }]);
      setTimeout(() => setHits((h) => h.filter((x) => Date.now() - x.at < 1600)), 1700);
    };
    window.addEventListener(PUNCH_EVENT, on);
    return () => window.removeEventListener(PUNCH_EVENT, on);
  }, [fighting, keyA, keyB]);

  // Every fresh buy of a fighter is a hit for its side. Pops fade after 1.6 s.
  const seen = useRef(new Set<number>());
  useEffect(() => {
    const fresh = feed.items.filter((f) => feed.fresh.has(f.id) && !seen.current.has(f.id) && f.kind === "trade" && f.extra === "buy" && (f.ticker === keyA || f.ticker === keyB));
    if (!fresh.length) return;
    fresh.forEach((f) => seen.current.add(f.id));
    const others = fresh.filter((f) => {
      const side = f.ticker === keyA ? "a" : "b";
      const i = mine.current.findIndex((m) => m.side === side && Date.now() - m.at < 60000);
      if (i < 0) return true;
      mine.current = mine.current.filter((_, j) => j !== i); // already thrown when you tapped
      return false;
    });
    if (!others.length) return;
    const t0 = setTimeout(() => setHits((h) => [...h, ...others.map((f) => ({ id: f.id, side: (f.ticker === keyA ? "a" : "b") as "a" | "b", label: `+${compact(near(f.amount))} Ⓝ`, at: Date.now() }))]), 0);
    const t1 = setTimeout(() => setHits((h) => h.filter((x) => Date.now() - x.at < 1600)), 1700);
    return () => { clearTimeout(t0); clearTimeout(t1); };
  }, [feed.items, feed.fresh, keyA, keyB]);

  // A real result arriving while you watch: the loser is knocked out (war_settled: ticker = winner, actor = loser).
  const seenResult = useRef(new Set<number>());
  useEffect(() => {
    const r = feed.items.find((f) => feed.fresh.has(f.id) && !seenResult.current.has(f.id) && (f.kind === "war_settled" || f.kind === "war_void") && [keyA, keyB].includes(f.ticker) && [keyA, keyB].includes(f.actor));
    if (!r) return;
    seenResult.current.add(r.id);
    const o: Outcome = r.kind === "war_settled" ? { id: `f${r.id}`, kind: "ko", loser: r.actor === keyA ? "a" : "b" } : { id: `f${r.id}`, kind: "void" };
    const t = setTimeout(() => setOutcome(o), 0);
    return () => clearTimeout(t);
  }, [feed.items, feed.fresh, keyA, keyB]);

  // Replay of the last real result in the feed. The punches are a dramatisation and are labelled "replay".
  const last = feed.items.find((f) => f.kind === "war_settled" || f.kind === "war_void");
  function startReplay() {
    if (!last) return;
    const t0 = Date.now();
    setOutcome(null);
    setReplay({ a: last.ticker, b: last.actor, until: t0 + 9500, round: last.extra.match(/round=(\d+)/)?.[1] ?? "" });
    if (last.kind === "war_settled") {
      [300, 900, 1500].forEach((d, i) => setTimeout(() => setHits((h) => [...h, { id: -t0 - i, side: "a", label: "replay", at: Date.now() }]), d));
      setTimeout(() => setHits((h) => h.filter((x) => Date.now() - x.at < 1600)), 3300);
      setTimeout(() => setOutcome({ id: `r${t0}`, kind: "ko", loser: "b" }), 2100);
    } else {
      setTimeout(() => setOutcome({ id: `r${t0}`, kind: "void" }), 600);
    }
  }

  // Latest AI line from either fighter becomes a speech bubble.
  const said = (commentary.data ?? []).find((x) => x.ticker === keyA || x.ticker === keyB);
  const bubble: Bubble = said ? { side: said.ticker === keyA ? "a" : "b", text: prettyText(said.text) } : null;

  const wars = feed.items.filter((f) => f.kind.startsWith("war_"));
  const war = arena.data?.war;
  const sa = phase && phase.kind !== "next" ? near(phase.pair.score_a) : 0;
  const sb = phase && phase.kind !== "next" ? near(phase.pair.score_b) : 0;
  const share = sa + sb > 0 ? sa / (sa + sb) : 0.5;

  return (
    <main>
      <section className="flex flex-wrap items-end justify-between gap-4 border-t border-ink px-4 pb-6 pt-10 sm:px-8">
        <h1 className="font-black text-[clamp(3.4rem,9vw,7.5rem)] leading-[0.82]">The Colosseum</h1>
        <div className="flex flex-wrap items-center gap-2" role="group" aria-label="View">
          {last && (
            <button onClick={startReplay} disabled={replaying} className="chip bg-yolk px-3 py-1.5 text-sm">
              {replaying ? `Replaying round ${replay!.round}…` : last.kind === "war_settled" ? "Replay the last knockout" : "Replay the last fight"}
            </button>
          )}
          {(["3d", "2d"] as const).map((m) => (
            <button key={m} aria-pressed={mode === m} onClick={() => setMode(m)} className={`chip px-3 py-1.5 text-sm ${mode === m ? "bg-night text-cream" : "bg-paper"}`}>{m === "3d" ? "3D arena" : "Poster view"}</button>
          ))}
        </div>
      </section>

      {mode === "2d" && <FightPoster arena={arena.data} coins={list} loading={coins.loading || arena.loading} error={arena.error ?? coins.error} />}

      {mode === "3d" && (
        <section aria-label={`${tA} versus ${tB}, 3D view`} className="relative border-y border-ink">
          <div className="h-[min(72vh,680px)] min-h-[440px]">
            {A && B ? <Arena3D a={{ ticker: tA, icon: A.icon }} b={{ ticker: tB, icon: B.icon }} hits={hits} outcome={outcome} practice={practice && !replaying} /> : <div className="grid h-full place-items-center font-serif text-2xl">{arena.error ?? coins.error ?? "Finding the fighters…"}</div>}
          </div>

          {/* Health bars and status sit on top of the scene as real HTML, so they stay readable and accessible. */}
          <div className="pointer-events-none absolute inset-x-0 top-0 grid grid-cols-2 gap-4 p-4 sm:p-6">
            {(["a", "b"] as const).map((s) => {
              const red = s === "a";
              const sc = red ? sa : sb;
              const sh = red ? share : 1 - share;
              const uq = phase && phase.kind !== "next" ? (red ? phase.pair.unique_a : phase.pair.unique_b) : 0;
              return (
                <div key={s} className={`rounded-2xl border border-ink p-3 shadow-[0_18px_40px_-22px_var(--color-drop)] ${red ? "bg-red-deep" : "bg-blue-deep text-right"}`}>
                  <p className="font-display text-2xl leading-none sm:text-3xl">${red ? tA : tB}</p>
                  <div className={`mt-2 h-4 overflow-hidden rounded-full border border-ink bg-night/50 ${red ? "" : "rotate-180"}`} role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(sh * 100)} aria-label={`$${red ? tA : tB} share of hits`}>
                    <div className={`h-full ${red ? "bg-red" : "bg-blue"} transition-[width] duration-700`} style={{ width: `${Math.max(4, sh * 100)}%` }} />
                  </div>
                  <p className="mt-1.5 text-xs font-semibold">{sc > 0 ? `${compact(sc)} NEAR of hits` : "No hits yet"} · {uq}/{war?.min_unique ?? 2} wallets</p>
                </div>
              );
            })}
          </div>

          {bubble && (
            <div className={`pointer-events-none absolute bottom-24 max-w-[min(360px,80vw)] rounded-[1.3rem] border border-ink bg-lilac-deep p-4 shadow-[0_18px_40px_-22px_var(--color-drop)] ${bubble.side === "a" ? "left-4 rounded-bl-sm sm:left-6" : "right-4 rounded-br-sm sm:right-6"}`}>
              <p className="mb-1 text-xs font-semibold"><span className="chip mr-1 bg-paper">AI</span> ${bubble.side === "a" ? tA : tB} says</p>
              <p className="font-serif text-[1.05rem] leading-snug">{bubble.text.length > 160 ? bubble.text.slice(0, 160) + "…" : bubble.text}</p>
            </div>
          )}

          <div className="flex flex-col items-center justify-between gap-3 border-t border-ink bg-night px-4 py-4 text-cream sm:flex-row sm:px-8">
            <button className="btn bg-mint text-ink disabled:opacity-40" disabled={!A || !fighting} onClick={() => A && fighting && punch(A)}>{fighting ? `${play ? "Punch" : "Buy"} $${tA}` : "Round over"}</button>
            <p className="text-center">
              {!phase ? "…" : phase.kind === "live" ? <span className="flex flex-col items-center leading-tight"><span className="font-display text-3xl tnum">{clock(phase.endsMs - now)} left in the fight</span><span className="text-sm text-cream/80">Buy 0.25 NEAR or more of either side to land a punch</span></span>
                : phase.kind === "bell" ? <>The bell rang {ago(phase.endedMs, now)}. <button className="pointer-events-auto underline" onClick={poke}>Settle the round</button></>
                : <>No fight on. <button className="underline" onClick={poke}>Open the next round</button></>}
            </p>
            <button className="btn bg-mint text-ink disabled:opacity-40" disabled={!B || !fighting} onClick={() => B && fighting && punch(B)}>{fighting ? `${play ? "Punch" : "Buy"} $${tB}` : "Round over"}</button>
          </div>
          {fighting && <div className="border-t border-cream/15 bg-night px-4 pb-4 text-cream sm:px-8"><QuickPunch dark /></div>}
        </section>
      )}

      <div className="grid grid-cols-[minmax(0,1fr)] gap-8 px-4 py-14 sm:px-8 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)] [&>*]:min-w-0">
        <section className="rounded-[1.8rem] border border-ink bg-paper p-6 shadow-[0_18px_40px_-22px_var(--color-drop)] sm:p-8">
          <p className="font-hand text-[2.2rem] leading-none">rules of war</p>
          <p className="mt-1 text-sm text-ink-2">Read live from the arena contract. Nobody can change them.</p>
          {war && (
            <ul className="mt-5 space-y-3 text-[1.02rem] leading-relaxed">
              <li><strong>A round lasts {Math.round(war.round_duration_ms / 60000)} minutes.</strong> Up to {war.max_pairs} fights run at once.</li>
              <li><strong>Every buy of 0.25 NEAR or more through NME is a hit</strong> for that coin. Buying elsewhere doesn&apos;t count.</li>
              <li><strong>The biggest war chests get paired.</strong> The stake is {war.chest_bps / 100}% of the smaller chest, and it must be at least {compact(near(war.min_stake))} NEAR.</li>
              <li><strong>One wallet&apos;s hits count up to {war.wallet_cap_bps / 100}% of the stake</strong>, and each side needs {war.min_unique} different wallets or the fight is called off.</li>
              <li><strong>The loser&apos;s stake buys the winner.</strong> Half is burned (supply goes down), half is shared by the winning side&apos;s fighters as spoils.</li>
              <li><strong>Anyone can ring the bell.</strong> Settling and opening rounds is permissionless and pays a small bounty.</li>
            </ul>
          )}
        </section>

        <section className="rounded-[1.8rem] border border-ink bg-blue-deep p-6 shadow-[0_18px_40px_-22px_var(--color-drop)] sm:p-8 lg:col-span-2">
          <p className="font-hand text-[2.2rem] leading-none">how a fight starts</p>
          <div className="mt-4 grid grid-cols-[minmax(0,1fr)] gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)] [&>*]:min-w-0">
            <ol className="space-y-3 text-[1.02rem] leading-relaxed">
              <li><strong>1. Get in line.</strong> Anyone can add NEAR to a coin&apos;s war chest on its coin page. You can&apos;t pick an opponent: the arena pairs the biggest chests, strongest against next strongest.</li>
              <li><strong>2. Ring the bell.</strong> Anyone with a NEAR wallet can poke the arena. It runs whatever is due, in order (settling the last round, opening the next one, price checks, fee claims), and the poker earns a small bounty. The Operator keeper does this on its own when it&apos;s running.</li>
              <li><strong>3. Fight.</strong> For {war ? Math.round(war.round_duration_ms / 60000) : "a few"} minutes, buys of 0.25 NEAR or more are hits. Then the bell rings again.</li>
            </ol>
            <div>
              <p className="text-sm font-semibold">Next in line, by war chest (live)</p>
              <ul className="mt-2 divide-y divide-ink/20 rounded-2xl border border-ink bg-paper text-ink">
                {[...list].sort((x, y) => y.chestNear - x.chestNear).slice(0, 6).map((c, i, arr) => {
                  const partner = i % 2 === 0 ? arr[i + 1] : arr[i - 1];
                  const stake = partner && war ? Math.min(c.chestNear, partner.chestNear) * war.chest_bps / 10000 : 0;
                  const ok = !!war && stake >= near(war.min_stake) && i < (war.max_pairs ?? 2) * 2;
                  return (
                    <li key={c.key} className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 px-4 py-2.5">
                      <Link href={`/coin/${encodeURIComponent(c.key)}`} className="min-w-0 break-all font-display text-xl hover:underline">${c.ticker}</Link>
                      <span className="num whitespace-nowrap text-sm">{compact(c.chestNear)} NEAR</span>
                      <span className={`chip ${ok ? "bg-mint" : "bg-paper-2"}`}>{ok ? `fights $${partner!.ticker}` : "needs a bigger chest"}</span>
                    </li>
                  );
                })}
              </ul>
              <div className="mt-4 flex flex-wrap items-center gap-3">
                <button className="btn bg-yolk text-ink" onClick={poke}>Ring the bell</button>
                <span className="text-sm">Needs a NEAR wallet. It only does something when a round is due.</span>
              </div>
            </div>
          </div>
        </section>

        <Spoils feed={feed.items} />

        <section className="overflow-hidden rounded-[1.8rem] border border-ink bg-paper shadow-[0_18px_40px_-22px_var(--color-drop)]">
          <h2 className="border-b border-ink bg-red-deep px-6 py-4 font-black text-4xl text-red">War record</h2>
          <ol className="max-h-[520px] overflow-y-auto">
            {wars.map((f) => {
              const l = describe(f);
              return (
                <li key={f.id} className="grid grid-cols-[auto_minmax(0,1fr)] items-baseline gap-3 border-b border-ink/15 px-6 py-3">
                  <span className={`chip ${TONE_BG[l.tone]}`}>{l.tag}</span>
                  <span className="text-[0.95rem] leading-snug">{l.text} <span className="text-xs text-ink-2">· {ago(f.ts_ms, now)}</span></span>
                </li>
              );
            })}
            {!wars.length && <li className="px-6 py-5 text-ink-2">No wars in the recent feed yet.</li>}
          </ol>
          <p className="border-t border-ink px-6 py-3 text-sm">Want your meme in here? Fund its war chest on its <Link className="underline" href="/#memes">coin page</Link>.</p>
        </section>
      </div>
    </main>
  );
}
