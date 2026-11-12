"use client";
import { FeedItem, Tone, describe } from "@/lib/nme";

export const TONE_BG: Record<Tone, string> = {
  buy: "bg-mint",
  sell: "bg-paper-2",
  burn: "bg-yolk",
  war: "bg-red text-cream",
  ai: "bg-lilac",
  launch: "bg-lime",
  chest: "bg-blue text-cream",
  plain: "bg-paper",
};

function Strip({ row, hidden }: { row: FeedItem[]; hidden?: boolean }) {
  return (
    <ul className="flex shrink-0 items-center gap-8 pr-8" aria-hidden={hidden}>
      <li className="font-black text-[1.9rem] leading-none text-cream">The NME Times</li>
      {row.map((f) => {
        const l = describe(f);
        return (
          <li key={f.id} className="flex items-center gap-2.5 whitespace-nowrap text-[0.95rem] text-cream">
            <span className={`chip border-cream/60 ${TONE_BG[l.tone]} ${l.tone === "war" || l.tone === "chest" ? "" : "text-ink"}`}>{l.ai ? "AI · " : ""}{l.tag}</span>
            <span className="max-w-[46ch] truncate">{l.text}</span>
          </li>
        );
      })}
    </ul>
  )
}

/** The NME Times: a ticker strip of the latest on-chain events, in plain words. */
export function Times({ items }: { items: FeedItem[] }) {
  if (!items.length) return null;
  const row = items.slice(0, 16);
  return (
    <div className="marquee-wrap overflow-hidden border-b border-ink bg-night py-3 motion-reduce:overflow-x-auto" role="region" aria-label="Latest arena events">
      <div className="marquee flex w-max" style={{ ["--marquee-s" as string]: `${row.length * 6}s` }}>
        <Strip row={row} />
        <Strip row={row} hidden />
      </div>
    </div>
  );
}
