import { COIN_DECIMALS, NEAR_DECIMALS } from "./config";

/** Scale a raw integer string by 10^decimals into a JS number (fine for display). */
export function scale(raw: string | undefined | null, decimals: number): number {
  if (!raw) return 0;
  const s = raw.replace(/^0+/, "") || "0";
  if (s.length <= decimals) return Number("0." + s.padStart(decimals, "0"));
  return Number(s.slice(0, s.length - decimals) + "." + s.slice(s.length - decimals, s.length - decimals + 8));
}

export const near = (yocto?: string | null) => scale(yocto, NEAR_DECIMALS);
export const tokens = (raw?: string | null) => scale(raw, COIN_DECIMALS);

/** Compact number: 1.2K, 3.4M, 0.0021, 4.1e-8 shown as 0.0₇41 style is overkill, keep sig figs. */
export function compact(n: number, digits = 2): string {
  if (!isFinite(n)) return "–";
  const a = Math.abs(n);
  if (a === 0) return "0";
  if (a >= 1e9) return (n / 1e9).toFixed(digits).replace(/\.?0+$/, "") + "B";
  if (a >= 1e6) return (n / 1e6).toFixed(digits).replace(/\.?0+$/, "") + "M";
  if (a >= 1e4) return (n / 1e3).toFixed(1).replace(/\.0$/, "") + "K";
  if (a >= 1) return n.toFixed(a >= 100 ? 0 : digits).replace(/\.?0+$/, "");
  if (a >= 0.001) return n.toPrecision(2);
  // Tiny prices the way DEX screeners write them: 0.0₈25 = 0.0000000025 (subscript = zeros after the point)
  const zeros = Math.floor(-Math.log10(a));
  const sig = Math.min(99, Math.round(a * Math.pow(10, zeros + 2))).toString().replace(/0$/, "");
  const sub = String(zeros).split("").map((d) => "₀₁₂₃₄₅₆₇₈₉"[+d]).join("");
  return `${n < 0 ? "-" : ""}0.0${sub}${sig}`;
}

export const nearStr = (yocto?: string | null) => `${compact(near(yocto), 2)} NEAR`;

/** DCL: raw Y/X = 1.0001^point. Returns NEAR per whole coin. */
export function priceFromPoint(point: number, coinIsX: boolean): number {
  const rawYperX = Math.pow(1.0001, point);
  const rawCoinPriceInWrap = coinIsX ? rawYperX : 1 / rawYperX;
  return rawCoinPriceInWrap * Math.pow(10, COIN_DECIMALS - NEAR_DECIMALS);
}

export function ago(ms: number, now = Date.now()): string {
  if (!now) return "";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

export function clock(msLeft: number): string {
  const s = Math.max(0, Math.floor(msLeft / 1000));
  const m = Math.floor(s / 60);
  const r = s % 60;
  if (m >= 60) return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
  return `${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
}

export const short = (acct: string, n = 18) => (acct.length > n ? acct.slice(0, n - 6) + "…" + acct.slice(-5) : acct);
