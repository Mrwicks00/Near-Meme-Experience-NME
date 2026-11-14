"use client";
// Fast punches. A wallet popup per punch is too slow for a fight, so:
// - Google sign-in accounts already sign on this device (one tap);
// - NEAR wallet users load NEAR once into this device's quick-punch account, then punch with one tap, and cash out
//   (coins and NEAR back to the wallet) whenever they like;
// - nobody connected: the fight offers Google sign-in right here.
import { useEffect, useState } from "react";
import { useCoins } from "@/lib/hooks";
import { compact, near } from "@/lib/format";
import { isQuick, playBalance, playSecret, playTopup } from "@/lib/play";
import { walletName } from "@/lib/xwallet";
import { PUNCH_EVENT, useActions } from "./Actions";
import { GoogleSignIn } from "./GoogleSignIn";

export function QuickPunch({ dark }: { dark?: boolean }) {
  const { play, nearWallet, x, xNear, quickLoad, cashOut, toast } = useActions();
  const coins = useCoins();
  const [bal, setBal] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [dest, setDest] = useState<string | null>(null); // cash-out target typed by a Phantom / MetaMask user
  const [showKey, setShowKey] = useState(false);
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
    const low = bal !== null && bal < 0.6; // a punch needs ~0.56 NEAR available (0.26 + the gas reserve)
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
    const tokens = (coins.data ?? []).map((c) => c.account);
    const canLoad = !!nearWallet || !!x;
    return (
      <div className={box}>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <p><strong>⚡ Quick punches</strong> · {bal === null ? "…" : `${compact(bal)} NEAR left`}{low ? " · top up to keep punching" : ""}</p>
          <div className="flex flex-wrap gap-2">
            {canLoad && <button className={chip} disabled={busy} onClick={() => run(() => quickLoad(1))}>+1 NEAR</button>}
            <button className={chip} disabled={busy} onClick={() => {
              if (nearWallet) { if (confirm("Send every coin and all the NEAR in your quick-punch account back to your wallet, and close it?")) run(() => cashOut(tokens)); }
              else setDest((v) => (v === null ? "" : null));
            }}>{busy ? "Working…" : nearWallet ? "Cash out to wallet" : "Cash out"}</button>
          </div>
        </div>
        {dest !== null && !nearWallet && (
          <div className="mt-2 space-y-2">
            <p className="text-xs">Phantom and MetaMask balances can only be filled by deposits, so cash out goes to a NEAR account. Type one (for example yourname.testnet), or keep playing and back up the key below.</p>
            <div className="flex gap-2">
              <input value={dest} onChange={(e) => setDest(e.target.value.trim().toLowerCase())} placeholder="yourname.testnet" className="min-w-0 flex-1 rounded-full border border-ink bg-paper px-3 py-2 text-ink" />
              <button className={chip} disabled={busy || !/^([a-z0-9_-]+\.)*[a-z0-9_-]+$/.test(dest) || dest.length < 2} onClick={() => {
                if (confirm(`Send every coin and all the NEAR in your quick-punch account to ${dest}, and close it?`)) run(() => cashOut(tokens, dest));
              }}>Send</button>
            </div>
          </div>
        )}
        <p className={`mt-1 text-xs ${dark ? "text-cream/70" : "text-ink-2"}`}>
          Signed on this device, no popups. Coins you buy sit here until you cash out.{" "}
          <button className="underline" onClick={() => setShowKey((v) => !v)}>{showKey ? "Hide key" : "Back up key"}</button>
        </p>
        {showKey && (
          <p className="mt-2 break-all rounded-xl border border-ink bg-paper p-2 font-mono text-[0.7rem] text-ink">
            {play}<br />{playSecret()}<br /><span className="font-sans">Anyone with this key controls the account. Import it in any NEAR wallet to use it there.</span>
          </p>
        )}
      </div>
    );
  }

  if (nearWallet || x) {
    return (
      <div className={box}>
        {nearWallet
          ? <p><strong>Wallet popups slow your punches.</strong> Load NEAR once and every punch is one tap. Cash out to {nearWallet.length > 20 ? nearWallet.slice(0, 14) + "…" : nearWallet} anytime.</p>
          : <p><strong>{walletName(x!)} popups slow your punches.</strong> Sign once to move NEAR from your NME balance ({xNear === null ? "…" : `${compact(xNear)} NEAR`}) to a quick-punch account on this device, then every punch is one tap.</p>}
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
