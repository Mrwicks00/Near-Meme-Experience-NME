"use client";
import { useEffect, useState } from "react";

const cache = new Map<string, string>();

/**
 * Memes bring their own colour: the tile behind an icon is a soft tint of the icon's
 * average colour, so the system palette stays reserved for meaning (buy, burn, war, AI).
 */
export function useIconTint(src: string | null | undefined, fallback = "#221d29") {
  const [tint, setTint] = useState(fallback);
  useEffect(() => {
    if (!src || cache.has(src)) return;
    const img = new Image();
    img.onload = () => {
      try {
        const c = document.createElement("canvas");
        c.width = c.height = 16;
        const g = c.getContext("2d", { willReadFrequently: true })!;
        g.drawImage(img, 0, 0, 16, 16);
        const d = g.getImageData(0, 0, 16, 16).data;
        let r = 0, gg = 0, b = 0, n = 0;
        for (let i = 0; i < d.length; i += 4) {
          const a = d[i + 3];
          const lum = (d[i] + d[i + 1] + d[i + 2]) / 3;
          if (a < 128 || lum < 30 || lum > 235) continue; // skip outlines, background, transparency
          r += d[i]; gg += d[i + 1]; b += d[i + 2]; n++;
        }
        if (!n) return;
        // Mix 40% icon colour into the dark surface: a deep tint that never fights the palette's meaning colours.
        const mix = (v: number, p: number) => Math.round(v / n * 0.4 + p * 0.6);
        const t = `rgb(${mix(r, 21)} ${mix(gg, 18)} ${mix(b, 26)})`;
        cache.set(src, t);
        setTint(t);
      } catch { /* tainted canvas: keep fallback */ }
    };
    img.src = src;
  }, [src]);
  return (src && cache.get(src)) || tint;
}

export function CoinIcon({ src, ticker, size = 64, className = "" }: { src: string | null | undefined; ticker: string; size?: number; className?: string }) {
  if (!src) {
    return (
      <span
        aria-hidden
        className={`grid place-items-center rounded-full bg-paper-2 font-display text-ink ${className}`}
        style={{ width: size, height: size, fontSize: size * 0.42 }}
      >
        {ticker.slice(0, 1)}
      </span>
    );
  }
  // eslint-disable-next-line @next/next/no-img-element -- on-chain data URL, nothing to optimise
  return <img src={src} alt={`$${ticker} icon`} width={size} height={size} className={`object-contain ${className}`} style={{ width: size, height: size }} />;
}
