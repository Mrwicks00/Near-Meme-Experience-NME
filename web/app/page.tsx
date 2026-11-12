"use client";
import { useMemo } from "react";
import { FightPoster } from "@/components/FightPoster";
import { Times } from "@/components/Times";
import { Verbs } from "@/components/Verbs";
import { MemeGrid } from "@/components/MemeGrid";
import { LaunchBox } from "@/components/LaunchBox";
import { FeedSection } from "@/components/FeedSection";
import { TradeBox } from "@/components/TradeBox";
import { useArena, useCoins, useCommentary, useFeed, useNow } from "@/lib/hooks";

function Home() {
  const coins = useCoins();
  const arena = useArena();
  const feed = useFeed(40);
  const commentary = useCommentary();
  const now = useNow(5000);

  const fighting = useMemo(() => {
    const r = arena.data?.round;
    const s = new Set<string>();
    if (r && now < r.end_ms) r.pairs.filter((p) => p.state === "Pending").forEach((p) => { s.add(p.a); s.add(p.b); });
    return s;
  }, [arena.data, now]);

  // Cards flash mint on a fresh buy and yolk on a fresh burn.
  const flashes = useMemo(() => {
    const m = new Map<string, "buy" | "burn">();
    for (const f of feed.items) {
      if (!feed.fresh.has(f.id)) continue;
      if (f.kind === "trade" && f.extra === "buy") m.set(f.ticker, "buy");
      if (f.kind === "treasury_buyback" || f.kind === "war_settled" || f.kind === "job_delivered") m.set(f.ticker, "burn");
    }
    return m;
  }, [feed.items, feed.fresh]);

  return (
    <>
      <main>
        <Times items={feed.items} />
        <FightPoster arena={arena.data} coins={coins.data ?? []} loading={coins.loading || arena.loading} error={arena.error ?? coins.error} />
        <section aria-label="What NME is" className="px-4 py-14 sm:px-8 sm:py-20">
          <p className="max-w-[34ch] text-[clamp(1.6rem,3.2vw,2.7rem)] font-medium leading-[1.12] tracking-[-0.015em]">
            NME is a memecoin arena on NEAR. Memes fight, think, work and defend their own price. Buying and selling are always free: no tax, no pause, no admin key.
          </p>
        </section>
        <section id="say" aria-labelledby="say-h" className="border-y border-ink bg-mint-deep px-4 py-12 sm:px-8 sm:py-16">
          <p className="font-hand text-[2.4rem] leading-none">just say it</p>
          <h2 id="say-h" className="mb-6 mt-2 font-serif text-[clamp(1.9rem,3.6vw,3rem)] font-light leading-tight">Type a trade like you&apos;d text it. <em>Sign what you see.</em></h2>
          <TradeBox known={coins.data ?? undefined} />
        </section>
        <Verbs />
        <MemeGrid coins={coins.data ?? []} fighting={fighting} flashes={flashes} loading={coins.loading} error={coins.error} />
        <LaunchBox />
        <FeedSection items={feed.items} fresh={feed.fresh} commentary={commentary.data ?? []} error={feed.error} />
      </main>
    </>
  );
}

export default Home;
