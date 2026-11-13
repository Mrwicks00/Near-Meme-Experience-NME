"use client";
import { NME } from "@/lib/config";
import { GoogleSignIn } from "./GoogleSignIn";
import { QuickPunch } from "./QuickPunch";
import { useEffect, useState } from "react";
import { compact, near } from "@/lib/format";
import { Claim, EvmChoice, XWallet, claimDeposit, depositAddress, evmWallets, gatewayLimits, getClaim, sendEth, sendSol, solDeposits, waitEthFinal } from "@/lib/xwallet";
import { walletName } from "@/lib/xwallet";
import { errText, useActions } from "./Actions";

const short = (s: string) => (s.length > 16 ? `${s.slice(0, 8)}…${s.slice(-6)}` : s);

const statusOf = (c: Claim): { label: string; tone: string; retry: boolean } => {
  if (!c) return { label: "Not claimed yet", tone: "bg-paper", retry: true };
  if (c.Credited || c.Done) return { label: `Credited ${compact(near((c.Credited ?? c.Done)!.near_paid))} NEAR`, tone: "bg-mint", retry: false };
  if (c.Pending) return { label: "Being verified in the TEE…", tone: "bg-yolk", retry: false };
  if (c.Rejected) return { label: `Rejected: ${c.Rejected.reason}`, tone: "bg-red", retry: true };
  return { label: "Verified, being paid…", tone: "bg-yolk", retry: false };
};

/** Top up a Phantom / EVM wallet's NME balance: send to its own deposit address, then the TEE verifies and credits it. */
function TopUp({ x }: { x: XWallet }) {
  const { toast, refreshX } = useActions();
  const sol = x.kind === "sol";
  const [addr, setAddr] = useState<string | null>(null);
  const [amt, setAmt] = useState(sol ? "0.1" : "0.002");
  const [step, setStep] = useState<string | null>(null);
  const [manual, setManual] = useState("");
  const [copied, setCopied] = useState(false);
  const [limits, setLimits] = useState<{ reserve: number; max: number } | null>(null);
  const [deposits, setDeposits] = useState<{ sig: string; sol: number | null; claim: Claim }[] | null>(null);
  const [tick, setTick] = useState(0);
  const [claiming, setClaiming] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    depositAddress(x).then((a) => alive && setAddr(a)).catch((e) => alive && setStep(`Couldn't derive your deposit address: ${errText(e)}`));
    gatewayLimits().then((l) => alive && setLimits({ reserve: near(l.reserve), max: near(l.maxClaim) })).catch(() => {});
    return () => { alive = false; };
  }, [x, tick]);

  // Solana: list what actually arrived at the deposit address and what the gateway did with each transfer.
  useEffect(() => {
    if (!sol || !addr) return;
    let alive = true;
    const load = async () => {
      const list = await solDeposits(addr).catch(() => []);
      const rows = await Promise.all(list.filter((d) => d.ok && (d.sol ?? 0) > 0).map(async (d) => ({ ...d, claim: await getClaim(d.sig).catch(() => null) })));
      if (alive) setDeposits(rows);
    };
    load();
    const t = setInterval(load, 15000);
    return () => { alive = false; clearInterval(t); };
  }, [sol, addr, tick]);

  async function claim(txId: string) {
    setStep("Asking the gateway to verify it in a TEE (about a minute)…");
    await claimDeposit(x, txId);
    // Follow the claim to its result instead of guessing: credited, or rejected with the reason.
    for (let i = 0; i < 30; i++) {
      await new Promise((r) => setTimeout(r, 5000));
      const c = sol ? await getClaim(txId).catch(() => null) : null;
      if (c?.Credited || c?.Done) { setStep(`Credited ${compact(near((c.Credited ?? c.Done)!.near_paid))} NEAR to your NME balance.`); refreshX(); setTick((v) => v + 1); return; }
      if (c?.Rejected) { setStep(`The gateway refused it: ${c.Rejected.reason}. Your coins are still at your deposit address; you can retry later.`); setTick((v) => v + 1); return; }
      if (!sol && i === 12) { refreshX(); }
    }
    setStep("Still being verified. Check the list below in a minute.");
    refreshX();
  }

  async function send() {
    if (!addr) return;
    try {
      const n = Number(amt);
      const tx = sol ? await sendSol(x, addr, n, setStep) : await sendEth(x, addr, n, setStep);
      await claim(tx);
    } catch (e) {
      setStep(null);
      toast({ tone: "err", text: errText(e) });
    }
  }

  async function retry(txId: string) {
    if (claiming) return;
    setClaiming(txId);
    try { await claim(txId); } catch (e) { setStep(null); toast({ tone: "err", text: errText(e) }); } finally { setClaiming(null); }
  }

  async function claimManual() {
    const id = manual.trim();
    if (!id) return;
    try {
      if (!sol) await waitEthFinal(id.toLowerCase(), setStep);
      await claim(sol ? id : id.toLowerCase());
      setManual("");
    } catch (e) {
      setStep(null);
      toast({ tone: "err", text: errText(e) });
    }
  }

  const low = limits && limits.reserve < limits.max;

  return (
    <div className="mt-4 rounded-2xl border border-ink bg-paper-2 p-4">
      <p className="text-sm font-semibold">Top up from {sol ? "Solana devnet" : "Sepolia"}</p>
      <p className="mt-1 text-xs text-ink-2">
        Your own deposit address, made by NEAR Chain Signatures for your wallet. Whatever lands here can only credit you. The NEAR value is set by two price sources, minus a 2% spread.
      </p>
      {limits && (
        <p className={`mt-2 rounded-xl p-2 text-xs ${low ? "bg-yolk" : "bg-paper"}`}>
          Each deposit credits at most <strong>{compact(limits.max)} NEAR</strong> (testnet safety cap; value above it is not credited), so send small amounts.
          {low && <> The gateway reserve is low right now (<strong>{compact(limits.reserve)} NEAR</strong>): a deposit worth more than that is refused until the reserve is topped up. Your coins stay at your address and can be claimed then.</>}
        </p>
      )}
      <button className="mt-3 flex w-full items-center justify-between gap-2 rounded-xl bg-paper px-3 py-2 text-left font-mono text-xs" onClick={() => { if (addr) { navigator.clipboard?.writeText(addr).catch(() => {}); setCopied(true); setTimeout(() => setCopied(false), 1500); } }}>
        <span className="break-all">{addr ?? "deriving…"}</span>
        <span className="shrink-0 font-sans font-semibold">{copied ? "Copied" : "Copy"}</span>
      </button>
      <div className="mt-3 flex gap-2">
        <input inputMode="decimal" value={amt} onChange={(e) => setAmt(e.target.value.replace(",", "."))} aria-label={`Amount in ${sol ? "SOL" : "ETH"}`}
          className="w-28 rounded-xl border border-ink bg-paper px-3 py-2 font-semibold" />
        <button className="btn flex-1 bg-mint py-2 text-sm" disabled={!addr || !!step?.startsWith("Waiting") || !(Number(amt) > 0)} onClick={send}>
          Send {amt} {sol ? "SOL" : "ETH"} from {walletName(x)}
        </button>
      </div>
      {step && <p className="mt-2 text-sm font-medium">{step}</p>}

      {sol && deposits && deposits.length > 0 && (
        <div className="mt-4">
          <p className="text-xs font-semibold">Your deposits</p>
          <ul className="mt-1 space-y-1.5">
            {deposits.map((d) => {
              const st = statusOf(d.claim);
              return (
                <li key={d.sig} className="grid grid-cols-[1fr_auto] items-center gap-x-3 gap-y-1 rounded-xl bg-paper px-3 py-2.5 text-xs">
                  <p className="flex min-w-0 items-center gap-2">
                    <span className={`h-2 w-2 shrink-0 rounded-full ${st.tone}`} aria-hidden />
                    <span className="font-semibold">{d.sol !== null ? `${d.sol} SOL` : "SOL"}</span>
                    <a className="truncate text-ink-2 underline" href={`https://explorer.solana.com/tx/${d.sig}?cluster=devnet`} target="_blank" rel="noreferrer">{d.sig.slice(0, 8)}…</a>
                  </p>
                  {st.retry ? (
                    <button className="chip bg-mint disabled:opacity-50" disabled={!!claiming} onClick={() => retry(d.sig)}>
                      {claiming === d.sig ? "Claiming…" : d.claim ? "Retry claim" : "Claim"}
                    </button>
                  ) : <span />}
                  <p className="col-span-2 break-words leading-snug text-ink-2">{claiming === d.sig ? "Verifying in the TEE and pricing it. This takes about a minute." : st.label}</p>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      <details className="mt-3 text-xs">
        <summary className="cursor-pointer font-semibold">Already sent? Claim it by transaction id</summary>
        <div className="mt-2 flex gap-2">
          <input value={manual} onChange={(e) => setManual(e.target.value)} placeholder={sol ? "Solana signature" : "0x… transaction hash"} className="min-w-0 flex-1 rounded-xl border border-ink bg-paper px-2 py-1.5 font-mono" />
          <button className="btn bg-paper px-3 py-1.5 text-xs" onClick={claimManual}>Claim</button>
        </div>
      </details>
    </div>
  );
}

export function WalletSheet({ onClose }: { onClose: () => void }) {
  const { nearWallet: account, x, xNear, connectNear, connectX, disconnect } = useActions();
  const [evm, setEvm] = useState<EvmChoice[] | null>(null);
  const listEvm = () => { evmWallets().then(setEvm); };
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);

  return (
    <div className="nme-overlay fixed inset-0 z-50 grid place-items-end bg-night/75 sm:place-items-center sm:p-6" onClick={onClose}>
      <div role="dialog" aria-modal aria-label="Wallets" onClick={(e) => e.stopPropagation()} className="max-h-[85dvh] w-full max-w-lg overflow-y-auto overscroll-contain rounded-t-[2rem] border border-ink bg-paper px-6 pb-[max(1.5rem,env(safe-area-inset-bottom))] shadow-[0_18px_40px_-22px_var(--color-drop)] sm:max-h-[92dvh] sm:rounded-[2rem]">
        {/* Sticky header: on phones the sheet scrolls, and Close must stay in reach. */}
        <div className="sticky top-0 z-10 -mx-6 flex items-center justify-between gap-4 border-b border-ink/15 bg-paper px-6 pb-3 pt-5">
          <h2 className="font-display text-4xl leading-none">wallets</h2>
          <button aria-label="Close" className="grid h-11 w-11 shrink-0 place-items-center rounded-full border border-ink bg-paper text-2xl leading-none hover:bg-mint" onClick={onClose}>✕</button>
        </div>

        {NME.googleClientId && (
          <section className="mt-6 rounded-[1.4rem] border-2 border-ink bg-yolk p-5 text-ink">
            <p className="text-xs font-semibold uppercase tracking-wide">Easiest · no wallet needed</p>
            <p className="mt-1 text-sm">Sign in with Google and get your own NEAR account with free test NEAR. Every action is one tap: no wallet app, no popups. Works on any phone.</p>
            <div className="mt-3">
              <GoogleSignIn onDone={onClose} />
            </div>
          </section>
        )}

        <section className="mt-4 rounded-[1.4rem] border border-ink bg-mint-deep p-5">
          <p className="text-xs font-semibold uppercase tracking-wide">NEAR wallet</p>
          {account ? (
            <div className="mt-2 flex items-center justify-between gap-3">
              <p className="truncate font-semibold">{account}</p>
              <button className="chip bg-paper" onClick={() => disconnect("near")}>Disconnect</button>
            </div>
          ) : null}
          {account && <div className="mt-3"><QuickPunch /></div>}
          {account ? null : (
            <>
              <p className="mt-1 text-sm">Meteor, Intear, Nightly, Ledger and other wallets that support NEAR testnet.</p>
              <button className="btn mt-3 w-full bg-paper" onClick={connectNear}>Connect a NEAR wallet</button>
            </>
          )}
        </section>

        <section className="mt-4 rounded-[1.4rem] border border-ink bg-lilac-deep p-5">
          <p className="text-xs font-semibold uppercase tracking-wide">No NEAR account? Use the wallet you have</p>
          <p className="mt-1 text-sm">Your wallet owns a balance in the NME gateway. You top it up with SOL or ETH, then sign plain-text messages to buy, sell, fund chests or hire. Nothing moves without your signature.</p>
          {x && (
            <div className="mt-3">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <p className="font-semibold">{walletName(x)} · {x.kind === "sol" ? "Solana devnet" : "Sepolia"}</p>
                  <p className="truncate font-mono text-xs">{short(x.address)}</p>
                </div>
                <div className="text-right">
                  <p className="text-xs">NME balance</p>
                  <p className="text-2xl font-bold">{xNear === null ? "…" : compact(xNear)} NEAR</p>
                </div>
              </div>
              <TopUp x={x} />
              <button className="mt-3 text-sm underline" onClick={() => disconnect("x")}>Disconnect {walletName(x)}</button>
            </div>
          )}
          {/* Always offered, so switching (say MetaMask -> Phantom to pay in SOL) is one click. Each wallet's NME balance stays on chain. */}
          <>
              {x && <p className="mt-5 text-xs font-semibold uppercase tracking-wide">Use a different wallet (your {walletName(x)} balance stays safe)</p>}
              <div className="mt-3 grid gap-2 sm:grid-cols-2">
                <button className="btn bg-paper" onClick={() => connectX("sol")}>Phantom</button>
                <button className="btn bg-paper" aria-expanded={evm !== null} onClick={listEvm}>EVM wallet</button>
              </div>
              {evm !== null && (
                <div className="mt-3 rounded-2xl border border-ink bg-paper p-2">
                  <p className="px-2 pb-1 pt-1 text-xs font-semibold">Pick a wallet. It will ask to switch to Sepolia.</p>
                  {evm.length === 0 && <p className="px-2 py-2 text-sm">No EVM wallet found in this browser. Install MetaMask, Rabby or another one, then reopen this.</p>}
                  <ul>
                    {evm.map((c) => (
                      <li key={c.uuid}>
                        <button className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left font-semibold hover:bg-mint" onClick={() => connectX("evm", c)}>
                          {/* eslint-disable-next-line @next/next/no-img-element -- wallet icons are data URLs from the extension */}
                          {c.icon ? <img src={c.icon} alt="" className="h-7 w-7 rounded-lg" /> : <span className="grid h-7 w-7 place-items-center rounded-lg bg-paper-2">?</span>}
                          {c.name}
                        </button>
                      </li>
                    ))}
                  </ul>
                </div>
              )}
          </>
        </section>
        <p className="mt-4 text-xs text-ink-2">Everything here is testnet: NEAR testnet, Solana devnet, Ethereum Sepolia. Signed actions are sent by the NME relayer, which pays the NEAR gas.</p>
      </div>
    </div>
  );
}
