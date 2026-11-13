"use client";
// Fast punches. A wallet popup per punch is too slow for a fight, so:
// - Google sign-in accounts already sign on this device (one tap);
// - NEAR wallet users load NEAR once into this device's quick-punch account, then punch with one tap, and cash out
//   (coins and NEAR back to the wallet) whenever they like;
// - nobody connected: the fight offers Google sign-in right here.
import { useEffect, useState } from "react";
import { useCoins } from "@/lib/hooks";
import { compact, near } from "@/lib/format";
import { isQuick, playBalance, playTopup } from "@/lib/play";
import { PUNCH_EVENT, useActions } from "./Actions";
import { GoogleSignIn } from "./GoogleSignIn";

export function QuickPunch({ dark }: { dark?: boolean }) {
  const { play, nearWallet, x, quickLoad, cashOut, toast } = useActions();
  const coins = useCoins();
  const [bal, setBal] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const quick = isQuick(play);

  useEffect(() => {
    if (!play) return;
    let alive = true;
    let t: ReturnType<typeof setTimeout>;
    const run = () => playBalance(play).then((b) => { if (alive) { setBal(b ? near(b) : null); t = setTimeout(run, 10000); } });
    const onPunch = () => { clearTimeout(t); t = setTimeout(run, 3000); };
    run();
    window.addEventListener(PUNCH_EVENT, onPunch);
    return () => { alive = false; clearTimeout(t); window.removeEventListener(PUNCH_EVENT, onPunch); };
  }, [play]);

  const box = `rounded-2xl border p-3 text-sm ${dark ? "border-cream/25 bg-cream/5 text-cream" : "border-ink bg-paper"}`;
  const chip = `chip ${dark ? "bg-cream text-ink" : "bg-mint"} disabled:opacity-50`;
  const run = async (f: () => Promise<unknown>) => { setBusy(true); try { await f(); } finally { setBusy(false); } };

  if (play && !quick) {
    if (!dark) return null;
    const low = bal !== null && bal < 0.6; // a punch needs ~0.55 NEAR available (0.25 + the gas reserve)
    return (
      <div className="flex flex-wrap items-center justify-center gap-2 text-center text-sm text-cream/80">
        <span>⚡ One-tap punches on, signed on this device as {play}{bal !== null ? ` · ${compact(bal)} NEAR` : ""}</span>
        {low && <button className={chip} disabled={busy} onClick={() => run(async () => {
          const r = await playTopup();
          toast(r.error ? { tone: "err", text: r.error } : { tone: "ok", text: `Added ${compact(near(r.sent ?? "0"))} free test NEAR.` });
        })}>{busy ? "Sending…" : "Low on NEAR · get free test NEAR"}</button>}
      </div>
    );
  }

  if (quick) {
    const low = bal !== null && bal < 0.6;
    return (
      <div className={box}>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p><strong>⚡ Quick punches</strong> · {bal === null ? "…" : `${compact(bal)} NEAR left`}{low ? " · top up to keep punching" : ""}</p>
          <div className="flex flex-wrap gap-2">
            {nearWallet && <button className={chip} disabled={busy} onClick={() => run(() => quickLoad(1))}>+1 NEAR</button>}
            <button className={chip} disabled={busy} onClick={() => {
              if (confirm("Send every coin and all the NEAR in your quick-punch account back to your wallet, and close it?")) run(() => cashOut((coins.data ?? []).map((c) => c.account)));
            }}>{busy ? "Working…" : "Cash out to wallet"}</button>
          </div>
        </div>
        <p className={`mt-1 text-xs ${dark ? "text-cream/70" : "text-ink-2"}`}>Signed on this device, no popups. Coins you buy sit here until you cash out.</p>
      </div>
    );
  }

  if (nearWallet) {
    return (
      <div className={box}>
        <p><strong>Wallet popups slow your punches.</strong> Load NEAR once and every punch is one tap. Cash out to {nearWallet.length > 20 ? nearWallet.slice(0, 14) + "…" : nearWallet} anytime.</p>
        <div className="mt-2 flex flex-wrap gap-2">
          {[1, 2, 5].map((n) => <button key={n} className={chip} disabled={busy} onClick={() => run(() => quickLoad(n))}>Load {n} NEAR</button>)}
        </div>
      </div>
    );
  }

  if (dark && !x) {
    return (
      <div className={`${box} mx-auto w-full max-w-md`}>
        <p className="mb-2 text-center"><strong>Punch with one tap:</strong> sign in with Google, get free test NEAR.</p>
        <GoogleSignIn />
      </div>
    );
  }
  return null;
}
