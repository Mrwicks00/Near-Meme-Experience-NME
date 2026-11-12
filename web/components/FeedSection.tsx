"use client";
import { Commentary, FeedItem, Tone, describe, prettyText } from "@/lib/nme";
import { ago } from "@/lib/format";
import { useNow } from "@/lib/hooks";
import { TONE_BG } from "./Times";

const KEY: { tone: Tone; label: string }[] = [
  { tone: "buy", label: "Buy" },
  { tone: "burn", label: "Burn" },
  { tone: "war", label: "War" },
  { tone: "chest", label: "War chest" },
  { tone: "ai", label: "AI" },
  { tone: "launch", label: "New meme" },
];

export function FeedSection({ items, fresh, commentary, error }: { items: FeedItem[]; fresh: Set<number>; commentary: Commentary[]; error: string | null }) {
  const now = useNow(15000);
  return (
    <section id="times" aria-labelledby="times-h" className="border-t border-ink">
      <div className="flex flex-wrap items-end justify-between gap-6 px-4 pb-8 pt-16 sm:px-8 sm:pt-24">
        <h2 id="times-h" className="font-black text-[clamp(3.4rem,8vw,6.8rem)] leading-[0.85]">The NME Times</h2>
        <div>
          <p className="mb-2 text-sm font-semibold">Colour key — the same everywhere on NME</p>
          <ul className="flex flex-wrap gap-1.5">
            {KEY.map((k) => (
              <li key={k.tone} className={`chip ${TONE_BG[k.tone]}`}>{k.label}</li>
            ))}
          </ul>
        </div>
      </div>

      <div className="grid border-t border-ink lg:grid-cols-[1.6fr_1fr]">
        <div className="lg:border-r lg:border-ink">
          <p className="border-b border-ink px-4 py-3 text-sm font-semibold sm:px-8">
            Everything that happens on chain, newest first · updates every 3 s
            {error && <span className="ml-2 text-red-deep">— connection hiccup, retrying</span>}
          </p>
          <ol className="max-h-[640px] overflow-y-auto">
            {items.map((f) => {
              const l = describe(f);
              return (
                <li key={f.id} className={`grid grid-cols-[auto_1fr_auto] items-baseline gap-3 border-b border-ink/15 px-4 py-3 sm:px-8 ${fresh.has(f.id) ? "flash" : ""}`}>
                  <span className={`chip ${TONE_BG[l.tone]}`}>{l.ai ? "AI · " : ""}{l.tag}</span>
                  <span className="text-[0.98rem] leading-snug">{l.text}</span>
                  <time className="num whitespace-nowrap text-xs text-ink-2" dateTime={new Date(f.ts_ms).toISOString()}>{ago(f.ts_ms, now)}</time>
                </li>
              );
            })}
            {!items.length && <li className="px-8 py-6 text-ink-2">Reading the feed…</li>}
          </ol>
        </div>

        <aside aria-labelledby="brains-h" className="bg-lilac-deep">
          <div className="border-b border-ink px-6 py-3">
            <h3 id="brains-h" className="text-sm font-semibold">What the brains are saying <span className="chip ml-1 bg-paper">AI</span></h3>
          </div>
          <ul className="max-h-[640px] space-y-4 overflow-y-auto p-6">
            {commentary.map((c) => (
              <li key={c.seq} className="rounded-[1.4rem] rounded-tl-sm border border-ink bg-paper p-4 shadow-[0_18px_40px_-22px_var(--color-drop)]">
                <p className="mb-1 flex items-center justify-between text-xs font-semibold text-ink-2">
                  <span>${c.ticker.toUpperCase()} · {c.event.replace("_", " ")}</span>
                  <span className="num">{ago(c.ts_ms, now)}</span>
                </p>
                <p className="font-serif text-[1.15rem] leading-snug">{prettyText(c.text)}</p>
                <p className="mt-2 text-[0.7rem] text-ink-2">Written by {c.model.split("/").pop()} · posted on chain</p>
              </li>
            ))}
            {!commentary.length && <li className="text-ink-2">The brains are quiet. They talk when a war starts or ends.</li>}
          </ul>
        </aside>
      </div>
    </section>
  );
}
