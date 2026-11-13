"use client";
// "Continue with Google": Google's own button gives an ID token; the relayer checks it and creates (or reopens) the
// player's NEAR account for this device's key. New players pick a name once.
import { useEffect, useRef, useState } from "react";
import { NME } from "@/lib/config";
import { isQuick, playBalance, playSecret, playSession } from "@/lib/play";
import { compact, near } from "@/lib/format";
import { useActions } from "./Actions";

type GoogleId = {
  accounts: { id: { initialize: (o: object) => void; renderButton: (el: HTMLElement, o: object) => void; disableAutoSelect: () => void } };
};
declare global {
  interface Window { google?: GoogleId }
}

let gsiLoading: Promise<void> | null = null;
function loadGsi(): Promise<void> {
  if (window.google?.accounts?.id) return Promise.resolve();
  if (!gsiLoading) {
    gsiLoading = new Promise((resolve, reject) => {
      const s = document.createElement("script");
      s.src = "https://accounts.google.com/gsi/client";
      s.async = true;
      s.onload = () => resolve();
      s.onerror = () => { gsiLoading = null; reject(new Error("Couldn't load Google sign-in. Check your connection.")); };
      document.head.appendChild(s);
    });
  }
  return gsiLoading;
}

export function GoogleSignIn({ onDone }: { onDone?: () => void }) {
  const { play, setPlay, toast, disconnect } = useActions();
  const btn = useRef<HTMLDivElement>(null);
  const [token, setToken] = useState<string | null>(null);
  const [needName, setNeedName] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [bal, setBal] = useState<string | null>(null);
  const [showKey, setShowKey] = useState(false);

  useEffect(() => {
    if (play || !NME.googleClientId || !btn.current) return;
    let alive = true;
    loadGsi()
      .then(() => {
        if (!alive || !btn.current || !window.google) return;
        window.google.accounts.id.initialize({
          client_id: NME.googleClientId,
          callback: (r: { credential?: string }) => r.credential && setToken(r.credential),
          ux_mode: "popup",
          auto_select: false,
        });
        window.google.accounts.id.renderButton(btn.current, { type: "standard", theme: "filled_black", size: "large", text: "continue_with", shape: "pill", width: Math.min(360, btn.current.clientWidth || 320) });
      })
      .catch((e) => setErr((e as Error).message));
    return () => { alive = false; };
  }, [play, needName]);

  // A fresh Google token: reopen the account on this device, or ask for a name.
  useEffect(() => {
    if (!token || needName) return;
    let alive = true;
    (async () => {
      setBusy(true); setErr("");
      try {
        const r = await playSession(token);
        if (!alive) return;
        if (r.needs_name) setNeedName(true);
        else if (r.account) { setPlay(r.account); toast({ tone: "ok", text: `Welcome back, ${r.account}.` }); onDone?.(); }
      } catch (e) {
        if (alive) setErr((e as Error).message);
      } finally {
        if (alive) setBusy(false);
      }
    })();
    return () => { alive = false; };
  }, [token, needName, setPlay, toast, onDone]);

  useEffect(() => {
    if (!play) return;
    let alive = true;
    playBalance(play).then((b) => alive && setBal(b));
    return () => { alive = false; };
  }, [play]);

  const create = async () => {
    if (!token) return;
    setBusy(true); setErr("");
    try {
      const r = await playSession(token, name);
      if (r.account) { setPlay(r.account); toast({ tone: "ok", text: `Your account ${r.account} is ready, with free test NEAR to play.` }); onDone?.(); }
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  if (!NME.googleClientId) return null;

  if (isQuick(play)) return <p className="text-sm">Your quick-punch account is open on this device. Cash it out to your wallet first, then sign in with Google.</p>;

  if (play) {
    return (
      <div className="space-y-3">
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0">
            <p className="truncate font-semibold">{play}</p>
            <p className="text-sm">{bal ? `${compact(near(bal))} NEAR` : "…"} · signs on this device, no popups</p>
          </div>
          <button className="chip bg-paper" onClick={() => { if (confirm("Sign out on this device? Back up your key first if you want to use it elsewhere.")) disconnect("play"); }}>Sign out</button>
        </div>
        <button className="text-sm underline" onClick={() => setShowKey((v) => !v)}>{showKey ? "Hide my key" : "Back up my key"}</button>
        {showKey && (
          <div className="rounded-xl border border-ink bg-paper p-3 text-xs">
            <p>Anyone with this key controls the account. Keep it private. Import it in any NEAR wallet (for example Meteor) to use {play} there.</p>
            <p className="mt-2 break-all font-mono">{playSecret()}</p>
            <button className="chip mt-2 bg-mint" onClick={() => navigator.clipboard?.writeText(playSecret() ?? "").then(() => toast({ tone: "ok", text: "Key copied." })).catch(() => {})}>Copy</button>
          </div>
        )}
      </div>
    );
  }

  return (
    <div>
      {!needName && <div ref={btn} className="min-h-[44px] w-full" />}
      {needName && (
        <div className="space-y-2">
          <p className="text-sm">Pick your name. Your account will be <strong>{(name || "yourname").toLowerCase()}.nmeplay.testnet</strong>.</p>
          <div className="flex gap-2">
            <input autoFocus value={name} onChange={(e) => setName(e.target.value.toLowerCase().replace(/[^a-z0-9_-]/g, "").slice(0, 20))} placeholder="degenfrog" className="min-w-0 flex-1 rounded-full border border-ink bg-paper px-4 py-2.5 font-semibold" />
            <button className="btn bg-night text-cream" disabled={busy || name.length < 3} onClick={create}>{busy ? "Creating…" : "Create"}</button>
          </div>
        </div>
      )}
      {busy && !needName && <p className="mt-2 text-sm">Opening your account…</p>}
      {err && <p className="mt-2 text-sm text-red">{err}</p>}
    </div>
  );
}
