// Finds the coin someone meant: "$PEPE", "pepe", "Pepe Legion", "the legion", "peep", "pepee" all reach $PEPE.
// Order of trust: exact ticker / key / name, then prefixes, then a word of the name, then small typos.
// Two equally good answers come back as `choices`, so the app asks instead of guessing.
import type { Coin } from "./nme";

const norm = (s: string) => s.toLowerCase().replace(/^\$/, "").replace(/[^a-z0-9]/g, "");

/** Damerau-Levenshtein (optimal string alignment): swapped letters count as one typo. */
function typos(a: string, b: string): number {
  const d: number[][] = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array(b.length).fill(0)]);
  for (let j = 1; j <= b.length; j++) d[0][j] = j;
  for (let i = 1; i <= a.length; i++) {
    for (let j = 1; j <= b.length; j++) {
      const c = a[i - 1] === b[j - 1] ? 0 : 1;
      d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + c);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) d[i][j] = Math.min(d[i][j], d[i - 2][j - 2] + 1);
    }
  }
  return d[a.length][b.length];
}

/** Lower is better; null = not a match. */
function score(q: string, c: Coin): number | null {
  const ticker = norm(c.ticker), key = norm(c.key.split(".")[0]), name = norm(c.name);
  const words = c.name.toLowerCase().split(/[^a-z0-9]+/).filter((w) => w.length >= 3);
  if (q === ticker || q === key || q === norm(c.key)) return 0;
  if (q === name) return 1;
  if (q.length >= 2 && (ticker.startsWith(q) || key.startsWith(q))) return 2 + (ticker.length - q.length) / 100;
  if (q.length >= 3 && name.startsWith(q)) return 3 + (name.length - q.length) / 100;
  if (q.length >= 3 && words.includes(q)) return 4;
  if (q.length >= 4 && (ticker.includes(q) || name.includes(q))) return 5;
  // Typos: 1 for short words, 2 from 7 letters. Never on 1-2 letter queries.
  if (q.length >= 3) {
    const allow = q.length >= 7 ? 2 : 1;
    const best = Math.min(typos(q, ticker), typos(q, key), typos(q, name), ...words.map((w) => typos(q, w)));
    if (best <= allow) return 6 + best;
  }
  return null;
}

export type Match = { coin: Coin } | { choices: Coin[] } | null;

export function matchCoin(query: string, coins: Coin[]): Match {
  const q = norm(query);
  if (!q) return null;
  const ranked = coins
    .map((c) => ({ c, s: score(q, c) }))
    .filter((r): r is { c: Coin; s: number } => r.s !== null)
    .sort((a, b) => a.s - b.s);
  if (!ranked.length) return null;
  const top = ranked.filter((r) => Math.floor(r.s) === Math.floor(ranked[0].s));
  if (top.length === 1 || ranked[0].s <= 1) return { coin: ranked[0].c };
  return { choices: top.slice(0, 4).map((r) => r.c) };
}
