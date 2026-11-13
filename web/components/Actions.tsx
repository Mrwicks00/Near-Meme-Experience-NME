"use client";
import { isQuick, playAccount, playCall, playSignOut, quickAccount, quickCashOut, setPlayAccount } from "@/lib/play";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { NearProvider, useNearWallet } from "near-connect-hooks";
import { NME } from "@/lib/config";
import { view } from "@/lib/rpc";
import { compact, near, scale } from "@/lib/format";
import type { Coin, Round } from "@/lib/nme";
import { EvmChoice, XKind, XWallet, WalletAction, connectEvm, connectPhantom, signedAction, walletState } from "@/lib/xwallet";
import { walletName } from "@/lib/xwallet";
import { CoinIcon } from "./CoinIcon";
import { minOutOf, quoteBuy } from "@/lib/quote";
import { WalletSheet } from "./WalletSheet";
import { announceChange } from "@/lib/hooks";

export const TGAS = (n: number) => (BigInt(n) * 10n ** 12n).toString();
export const yocto = (n: number) => {
  const [i, f = ""] = n.toFixed(6).split(".");
  return (BigInt(i) * 10n ** 24n + BigInt((f + "0".repeat(24)).slice(0, 24))).toString();
};
const STORAGE_DEPOSIT = "1250000000000000000000"; // 0.00125 NEAR, NEP-145 registration on the coin
const MIN_HIT_NEAR = 0.25; // arena MIN_SCORED_BUY
const X_KEY = "nme.xwallet";

type Toast = { tone: "ok" | "err" | "info"; text: string; href?: string };
type NearCall = { contractId: string; method: string; args: Record<string, unknown>; tgas: number; deposit?: string };
type Ctx = {
  account: string;            // the NEAR account that signs (a NEAR wallet, else a Google sign-in account), "" if none
  nearWallet: string;         // the connected NEAR wallet only (near-connect), "" if none
  x: XWallet | null;          // connected Phantom / MetaMask wallet
  xNear: number | null;       // that wallet's NEAR balance inside the gateway
  refreshX: () => void;
  connect: () => void;        // opens the wallet chooser
  connectNear: () => void;
  connectX: (k: XKind, evm?: EvmChoice) => Promise<void>;
  disconnect: (which: "near" | "x" | "play") => void;
  /** A Google sign-in account (`<name>.nmeplay.testnet`) whose key lives in this browser, or null. */
  play: string | null;
  setPlay: (account: string | null) => void;
  buy: (coin: Coin, amountNear?: number) => void;
  /** Fight button: a device account (Google or quick punch) buys the minimum hit at once, no sheet; others get the buy sheet. */
  punch: (coin: Coin) => void;
  /** NEAR wallet: send `amountNear` once to this device's quick-punch account. */
  quickLoad: (amountNear: number) => Promise<boolean>;
  /** Quick-punch account: send its coins and NEAR back to the NEAR wallet and close it. */
  cashOut: (tokens: string[]) => Promise<boolean>;
  poke: () => void;
  toast: (t: Toast) => void;
  /** NEAR-account-only call. */
  call: (c: NearCall & { ok: string }) => Promise<boolean>;
  /** Do an action with whatever is connected: a NEAR wallet signs `near`; Phantom/MetaMask sign `x` through the gateway. */
  act: (a: { near?: NearCall; x?: WalletAction; ok: string }) => Promise<boolean>;
};
const ActionsCtx = createContext<Ctx | null>(null);

/** Fired the moment a buy is sent (device accounts) or confirmed (wallets), so the arena can throw the punch at once. */
export const PUNCH_EVENT = "nme:punch";
export type PunchDetail = { key: string; amountNear: number };
function announcePunch(key: string, amountNear: number) {
  if (amountNear >= MIN_HIT_NEAR) window.dispatchEvent(new CustomEvent<PunchDetail>(PUNCH_EVENT, { detail: { key, amountNear } }));
}

/** A buy signed on this device: register on the coin if needed, then buy. Returns the transaction hash. */
async function deviceBuy(account: string, coin: Coin, amountNear: number, minOut: string): Promise<string> {
  const reg = await view<unknown>(coin.account, "storage_balance_of", { account_id: account }).catch(() => null);
  if (!reg) await playCall({ contractId: coin.account, method: "storage_deposit", args: { account_id: account, registration_only: true }, tgas: 30, deposit: STORAGE_DEPOSIT });
  return playCall({ contractId: NME.arena, method: "buy", args: { key: coin.key, min_out: minOut }, tgas: 300, deposit: yocto(amountNear) });
}

export function useActions() {
  const c = useContext(ActionsCtx);
  if (!c) throw new Error("useActions outside <Actions>");
  return c;
}

function txHash(outcome: unknown): string | undefined {
  const o = outcome as { transaction?: { hash?: string }; transaction_outcome?: { id?: string } } | undefined;
  return o?.transaction?.hash ?? o?.transaction_outcome?.id;
}

export function errText(e: unknown) {
  const m = (e as Error)?.message ?? String(e);
  if (/reject|cancel|denied|closed|user rejected/i.test(m)) return "You closed the wallet. Nothing was sent.";
  return m.length > 240 ? m.slice(0, 240) + "…" : m;
}

const quote = (coin: Coin, amountNear: number) => quoteBuy(coin, amountNear, false);

async function inLiveWar(key: string) {
  const r = await view<Round | null>(NME.arena, "current_round").catch(() => null);
  return !!r && Date.now() < r.end_ms && r.pairs.some((p) => p.state === "Pending" && (p.a === key || p.b === key));
}

function loadX(): XWallet | null {
  try { const s = localStorage.getItem(X_KEY); return s ? (JSON.parse(s) as XWallet) : null; } catch { return null; }
}
function saveX(x: XWallet | null) {
  try { if (x) localStorage.setItem(X_KEY, JSON.stringify(x)); else localStorage.removeItem(X_KEY); } catch { /* private mode */ }
}

function Inner({ children }: { children: React.ReactNode }) {
  const { signedAccountId, signIn, signOut, callFunction, signAndSendTransaction, signAndSendTransactions } = useNearWallet();
  const [sheet, setSheet] = useState<Coin | null>(null);
  const [sheetAmount, setSheetAmount] = useState<number | undefined>(undefined);
  const [chooser, setChooser] = useState(false);
  const [toasts, setToasts] = useState<(Toast & { id: number })[]>([]);
  const [x, setX] = useState<XWallet | null>(null);
  const [xNear, setXNear] = useState<number | null>(null);
  const [xTick, setXTick] = useState(0);
  const [play, setPlayState] = useState<string | null>(null);
  useEffect(() => { const t = setTimeout(() => setPlayState(playAccount()), 0); return () => clearTimeout(t); }, []);
  const setPlay = useCallback((a: string | null) => { if (!a) playSignOut(); setPlayState(a); }, []);
  // A NEAR wallet wins when both are present; otherwise the Google account signs.
  const account = signedAccountId || play || "";

  useEffect(() => { const t = setTimeout(() => setX(loadX()), 0); return () => clearTimeout(t); }, []);
  useEffect(() => {
    if (!x) return;
    let alive = true;
    let t: ReturnType<typeof setTimeout>;
    const run = async () => {
      try { const w = await walletState(x.owner); if (alive) setXNear(near(w.near)); } catch { /* keep last */ }
      if (alive) t = setTimeout(run, 15000);
    };
    run();
    return () => { alive = false; clearTimeout(t); };
  }, [x, xTick]);

  const toast = useCallback((t: Toast) => {
    const id = Date.now() + Math.random();
    setToasts((v) => [...v, { ...t, id }]);
    setTimeout(() => setToasts((v) => v.filter((y) => y.id !== id)), t.tone === "err" ? 9000 : 6500);
  }, []);

  const connectNear = useCallback(() => { setChooser(false); signIn().catch((e) => toast({ tone: "err", text: errText(e) })); }, [signIn, toast]);
  const connectX = useCallback(async (k: XKind, evm?: EvmChoice) => {
    try {
      if (k === "evm" && !evm) throw new Error("Pick an EVM wallet from the list.");
      const w = k === "sol" ? await connectPhantom() : await connectEvm(evm!);
      setX(w); saveX(w); setXNear(null);
      toast({ tone: "ok", text: `${w.kind === "sol" ? "Phantom" : w.name ?? "Wallet"} connected. Top up your NME balance to start.` });
    } catch (e) {
      toast({ tone: "err", text: errText(e) });
    }
  }, [toast]);
  const disconnect = useCallback((which: "near" | "x" | "play") => {
    if (which === "near") signOut().catch(() => {});
    else if (which === "play") {
      // A quick-punch account holds money only this device's key controls: it closes through "Cash out", never by forgetting the key.
      if (isQuick(play)) { toast({ tone: "err", text: "Cash out your quick punches to your wallet first." }); return; }
      setPlay(null);
    }
    else { setX(null); saveX(null); setXNear(null); }
  }, [signOut, setPlay, play, toast]);

  const call = useCallback<Ctx["call"]>(async ({ contractId, method, args, tgas, deposit = "0", ok }) => {
    if (!signedAccountId && play) {
      // Google sign-in account: signed right here with this device's key. One tap, no popup.
      try {
        const h = await playCall({ contractId, method, args, tgas, deposit });
        toast({ tone: "ok", text: ok, href: h ? `${NME.explorer}/txns/${h}` : undefined });
        announceChange();
        return true;
      } catch (e) {
        toast({ tone: "err", text: errText(e) });
        return false;
      }
    }
    if (!signedAccountId) { setChooser(true); return false; }
    try {
      const out = await callFunction({ contractId, method, args, gas: TGAS(tgas), deposit });
      const h = txHash(out);
      toast({ tone: "ok", text: ok, href: h ? `${NME.explorer}/txns/${h}` : undefined });
      announceChange();
      return true;
    } catch (e) {
      toast({ tone: "err", text: errText(e) });
      return false;
    }
  }, [signedAccountId, play, callFunction, toast]);

  const xBusy = useRef(false);
  const act = useCallback<Ctx["act"]>(async ({ near: n, x: xa, ok }) => {
    if ((signedAccountId || play) && n) return call({ ...n, ok });
    if (x && xa) {
      // One signed action at a time: a second click would sign the same nonce again and be refused on chain.
      if (xBusy.current) { toast({ tone: "info", text: "Your last action is still going through. Wait for it to finish." }); return false; }
      xBusy.current = true;
      try {
        toast({ tone: "info", text: `Check ${walletName(x)}: it shows the exact text you're signing.` });
        const r = await signedAction(x, xa);
        toast({ tone: "ok", text: ok, href: r.tx ? `${NME.explorer}/txns/${r.tx}` : undefined });
        setXTick((v) => v + 1);
        announceChange();
        return true;
      } catch (e) {
        toast({ tone: "err", text: errText(e) });
        return false;
      } finally {
        xBusy.current = false;
      }
    }
    if (x && !xa) { toast({ tone: "info", text: "This one needs a NEAR wallet. Phantom and EVM wallets can't sign it yet." }); return false; }
    setChooser(true);
    return false;
  }, [signedAccountId, play, x, call, toast]);

  const poke = useCallback(() => {
    act({ near: { contractId: NME.arena, method: "tick", args: { max_tasks: 8 }, tgas: 300 }, ok: "Poked. The arena ran its due tasks." });
  }, [act]);

  // One device-signed punch at a time (parallel sends would reuse a nonce and fail); at most a few queued.
  const punchQ = useRef<{ tail: Promise<void>; n: number }>({ tail: Promise.resolve(), n: 0 });
  const punch = useCallback((coin: Coin) => {
    if (!play) { setSheetAmount(MIN_HIT_NEAR); setSheet(coin); return; }
    const q = punchQ.current;
    if (q.n >= 4) { toast({ tone: "info", text: "Easy, champ: your last punches are still landing." }); return; }
    announcePunch(coin.key, MIN_HIT_NEAR); // the blow lands on screen now; the chain confirms in a second or two
    q.n++;
    q.tail = q.tail.then(async () => {
      try {
        const minOut = minOutOf(await quoteBuy(coin, MIN_HIT_NEAR)); // after the router fee, 3% slippage floor
        if (!minOut) throw new Error("no price for this coin right now, so nothing was sent.");
        await deviceBuy(play, coin, MIN_HIT_NEAR, minOut);
        announceChange();
      } catch (e) {
        toast({ tone: "err", text: `That punch missed: ${errText(e)}` });
      } finally {
        q.n--;
      }
    });
  }, [play, toast]);

  const quickLoad = useCallback(async (amountNear: number) => {
    if (!signedAccountId) { setChooser(true); return false; }
    try {
      const id = quickAccount();
      await signAndSendTransaction({ receiverId: id, actions: [{ type: "Transfer", params: { deposit: yocto(amountNear) } }] as never });
      if (!play) { setPlayAccount(id); setPlayState(id); }
      toast({ tone: "ok", text: `Loaded ${amountNear} NEAR. Every punch is one tap now, no wallet popups.` });
      return true;
    } catch (e) {
      toast({ tone: "err", text: errText(e) });
      return false;
    }
  }, [signedAccountId, signAndSendTransaction, play, toast]);

  const cashOut = useCallback(async (tokens: string[]) => {
    if (!signedAccountId) { toast({ tone: "info", text: "Connect the NEAR wallet that loaded it, so the money has somewhere to go." }); setChooser(true); return false; }
    try {
      await quickCashOut(signedAccountId, tokens, (t) => toast({ tone: "info", text: t }));
      if (isQuick(play)) setPlay(null);
      toast({ tone: "ok", text: `Cashed out. Your coins and NEAR are back in ${signedAccountId}.` });
      announceChange();
      return true;
    } catch (e) {
      toast({ tone: "err", text: errText(e) });
      return false;
    }
  }, [signedAccountId, play, setPlay, toast]);

  const value = useMemo<Ctx>(() => ({
    account, nearWallet: signedAccountId ?? "", play, setPlay, x, xNear, refreshX: () => setXTick((v) => v + 1),
    connect: () => setChooser(true), connectNear, connectX, disconnect,
    buy: (coin, amountNear) => { setSheetAmount(amountNear); setSheet(coin); }, punch, quickLoad, cashOut, poke, toast, call, act,
  }), [account, signedAccountId, play, setPlay, x, xNear, connectNear, connectX, disconnect, punch, quickLoad, cashOut, poke, toast, call, act]);

  return (
    <ActionsCtx.Provider value={value}>
      {children}
      {chooser && <WalletSheet onClose={() => setChooser(false)} />}
      {sheet && (
        <BuySheet
          coin={sheet}
          initial={sheetAmount}
          payer={play ? { kind: "near", label: play, device: true } : account ? { kind: "near", label: account } : x ? { kind: "x", label: walletName(x), balance: xNear } : null}
          onClose={() => setSheet(null)}
          onConnect={() => setChooser(true)}
          send={async (amountNear, minOut) => {
            if (play) {
              // Device account (Google sign-in or quick punch): signed right here, no popup.
              announcePunch(sheet.key, amountNear);
              const h = await deviceBuy(play, sheet, amountNear, minOut);
              toast({ tone: "ok", text: `Bought $${sheet.ticker}. If a war is on, that was a hit.`, href: h ? `${NME.explorer}/txns/${h}` : undefined });
              announceChange();
              setSheet(null);
              return;
            }
            if (signedAccountId) {
              const reg = await view<unknown>(sheet.account, "storage_balance_of", { account_id: signedAccountId }).catch(() => null);
              const txs = [];
              if (!reg) txs.push({ receiverId: sheet.account, actions: [{ type: "FunctionCall", params: { methodName: "storage_deposit", args: { account_id: signedAccountId, registration_only: true }, gas: TGAS(30), deposit: STORAGE_DEPOSIT } }] });
              txs.push({ receiverId: NME.arena, actions: [{ type: "FunctionCall", params: { methodName: "buy", args: { key: sheet.key, min_out: minOut }, gas: TGAS(300), deposit: yocto(amountNear) } }] });
              // eslint-disable-next-line @typescript-eslint/no-explicit-any
              const outs = await signAndSendTransactions({ transactions: txs as any });
              const h = txHash(Array.isArray(outs) ? outs[outs.length - 1] : outs);
              announcePunch(sheet.key, amountNear);
              toast({ tone: "ok", text: `Bought $${sheet.ticker}. If a war is on, that was a hit.`, href: h ? `${NME.explorer}/txns/${h}` : undefined });
              announceChange();
              setSheet(null);
              return;
            }
            // Phantom / MetaMask: a war buy scores for the wallet itself (JoinWar); otherwise a plain gateway buy.
            const war = amountNear >= MIN_HIT_NEAR && (await inLiveWar(sheet.key));
            const body = { key: sheet.key, amount: yocto(amountNear), min_out: minOut };
            const ok = await act({ x: war ? { JoinWar: body } : { Buy: body }, ok: `Bought $${sheet.ticker}${war ? " and landed a hit" : ""}. The tokens sit in your NME balance.` });
            if (ok) { announcePunch(sheet.key, amountNear); setSheet(null); }
          }}
          onError={(e) => toast({ tone: "err", text: errText(e) })}
        />
      )}
      <div aria-live="polite" className="nme-toasts pointer-events-none fixed bottom-4 right-4 z-[60] flex w-[min(92vw,380px)] flex-col gap-2">
        {toasts.map((t) => (
          <div key={t.id} className={`pointer-events-auto rounded-2xl border border-ink p-3 text-sm font-medium shadow-[0_18px_40px_-22px_var(--color-drop)] ${t.tone === "ok" ? "bg-mint" : t.tone === "err" ? "bg-red text-cream" : "bg-paper"}`}>
            {t.text} {t.href && <a className="underline" href={t.href} target="_blank" rel="noreferrer">See the transaction</a>}
          </div>
        ))}
      </div>
    </ActionsCtx.Provider>
  );
}

type Payer = { kind: "near"; label: string; device?: boolean } | { kind: "x"; label: string; balance: number | null } | null;

function BuySheet({ coin, initial, payer, onClose, onConnect, send, onError }: {
  coin: Coin; initial?: number; payer: Payer; onClose: () => void; onConnect: () => void;
  send: (amountNear: number, minOut: string) => Promise<void>; onError: (e: unknown) => void;
}) {
  const [amount, setAmount] = useState(initial ? String(initial) : "0.25");
  // Quote keyed by the amount it was made for; a stale quote shows as "quoting…".
  const [q, setQ] = useState<{ for: number; out: string | null } | null>(null);
  const [busy, setBusy] = useState(false);
  const [feeBps, setFeeBps] = useState<number | null>(null);
  const n = Number(amount);
  const valid = isFinite(n) && n > 0 && n <= 1000;
  const short = payer?.kind === "x" && payer.balance !== null && n > payer.balance;

  useEffect(() => {
    view<{ cfg: { router_fee_bps: number } }>(NME.arena, "get_config").then((c) => setFeeBps(c.cfg.router_fee_bps)).catch(() => setFeeBps(null));
  }, []);
  useEffect(() => {
    if (!valid) return;
    const t = setTimeout(() => {
      const spend = feeBps !== null ? n * (1 - feeBps / 10000) : n;
      quote(coin, spend).then((o) => setQ({ for: n, out: o }));
    }, 350);
    return () => clearTimeout(t);
  }, [n, valid, coin, feeBps]);
  const out = valid && q && q.for === n ? q.out : undefined;
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);

  // 3% slippage floor on the quoted amount. Without a quote we refuse rather than send min_out 0.
  const minOut = out ? ((BigInt(out) * 97n) / 100n).toString() : null;

  return (
    <div className="nme-overlay fixed inset-0 z-40 grid place-items-end bg-night/75 p-0 sm:place-items-center sm:p-6" onClick={onClose}>
      <div role="dialog" aria-modal aria-label={`Buy $${coin.ticker}`} onClick={(e) => e.stopPropagation()} className="w-full max-w-md rounded-t-[2rem] border border-ink bg-paper p-6 shadow-[0_18px_40px_-22px_var(--color-drop)] sm:rounded-[2rem]">
        <div className="flex items-center gap-4">
          <CoinIcon src={coin.icon} ticker={coin.ticker} size={56} className="rounded-xl" />
          <div>
            <p className="font-display text-3xl leading-none">Buy ${coin.ticker}</p>
            <p className="text-sm text-ink-2">{coin.name} · through the NME arena router</p>
          </div>
        </div>
        <label className="mt-6 block text-sm font-semibold" htmlFor="amt">You pay</label>
        <div className="mt-1 flex items-center rounded-2xl border border-ink bg-paper-2 px-4 py-3 focus-within:outline focus-within:outline-3 focus-within:outline-blue">
          <input id="amt" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value.replace(",", "."))} className="num w-full bg-transparent text-3xl font-bold outline-none" />
          <span className="text-lg font-bold">NEAR</span>
        </div>
        <div className="mt-2 flex gap-2">
          {[0.25, 1, 5].map((v) => (
            <button key={v} className="chip bg-paper hover:bg-mint" onClick={() => setAmount(String(v))}>{v} NEAR</button>
          ))}
        </div>
        <dl className="mt-5 space-y-2 text-sm">
          <div className="flex justify-between"><dt>You get about</dt><dd className="num font-bold">{out === undefined ? "quoting…" : out === null ? "no quote" : `${compact(scale(out, 18))} $${coin.ticker}`}</dd></div>
          <div className="flex justify-between"><dt>Arena router fee</dt><dd className="num">{feeBps === null ? "–" : `${feeBps / 100}%`}</dd></div>
          <div className="flex justify-between"><dt>Counts as a war hit</dt><dd>{n >= MIN_HIT_NEAR ? "yes, if your coin is fighting" : `no — hits start at ${MIN_HIT_NEAR} NEAR`}</dd></div>
          {payer && <div className="flex justify-between"><dt>Paid from</dt><dd className="num font-semibold">{payer.kind === "near" ? payer.label : `${payer.label} · NME balance ${payer.balance === null ? "…" : compact(payer.balance)} NEAR`}</dd></div>}
        </dl>
        <p className="mt-4 rounded-xl bg-paper-2 p-3 text-xs leading-relaxed text-ink-2">
          The coin itself never charges a tax and can never block you from selling. {payer?.kind === "near" && payer.device ? "Signed instantly on this device, no wallet needed" : "You sign this in your own wallet"}; the price can move up to 3% before it fails.
        </p>
        <div className="mt-5 flex gap-3">
          <button className="btn flex-1 bg-paper" onClick={onClose}>Cancel</button>
          {payer ? (
            <button className="btn flex-[2] bg-mint" disabled={!valid || !minOut || busy || short} onClick={async () => {
              setBusy(true);
              try { await send(n, minOut!); } catch (e) { onError(e); } finally { setBusy(false); }
            }}>
              {busy ? (payer?.kind === "near" && payer.device ? "Buying…" : "Waiting for wallet…") : short ? "Top up your NME balance first" : `Buy for ${valid ? n : "–"} NEAR`}
            </button>
          ) : (
            <button className="btn flex-[2] bg-mint" onClick={onConnect}>Connect a wallet</button>
          )}
        </div>
      </div>
    </div>
  );
}

export function Actions({ children }: { children: React.ReactNode }) {
  return (
    <NearProvider config={{ network: "testnet" }}>
      <Inner>{children}</Inner>
    </NearProvider>
  );
}

const noop = () => () => {};

export function WalletButton() {
  const { account, x, xNear, connect } = useActions();
  // Wallets come from this browser's storage: render the label only after hydration (the server can't know it).
  const mounted = useSyncExternalStore(noop, () => true, () => false);
  const label = !mounted ? null : account
    ? account.length > 20 ? account.slice(0, 14) + "…" : account
    : x ? `${walletName(x)} · ${xNear === null ? "…" : compact(xNear)} Ⓝ` : null;
  return (
    <button className={`btn px-4 py-2.5 text-sm ${label ? "bg-paper" : "bg-mint"}`} onClick={connect}>
      {label ?? "Connect wallet"}
    </button>
  );
}
