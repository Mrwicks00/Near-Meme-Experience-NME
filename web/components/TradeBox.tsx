"use client";
import { useEffect, useState } from "react";
import { NME } from "@/lib/config";
import { compact, near, scale } from "@/lib/format";
import { Coin, loadBalance, loadCoin, loadJobPrice, quoteSell } from "@/lib/nme";
import { minOutOf, quoteBuy, yoctoOf } from "@/lib/quote";
import { claimDeposit, depositAddress, sendEth, sendSol, walletName, walletState, walletTokens } from "@/lib/xwallet";
import { CoinIcon } from "./CoinIcon";
import { errText, useActions } from "./Actions";

type Asset = "NEAR" | "SOL" | "ETH";
type Intent =
  | { action: "buy" | "join_war"; ticker: string; amount: string; asset: Asset }
  | { action: "sell"; ticker: string; amount: string }
  | { action: "fund_chest"; ticker: string; amount: string; asset: Asset }
  | { action: "hire"; ticker: string; kind: string; target: string; prompt: string };

const EXAMPLES = ["ape $PEPE with 1 NEAR", "ape $PEPE with 0.1 SOL", "fund $PEPE chest with 0.5 NEAR", "hire $AXOLOTL to roast FROGGY for sleeping", "sell all my $PEPE"];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Inside a coin's widget, short commands name no coin ("ape with 1 NEAR", "roast DOGE"): put that coin in. */
export function withTicker(t: string, tk?: string): string {
  const s = t.trim();
  if (!tk || /\$/.test(s) || new RegExp(`\\b${tk.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "i").test(s)) return s;
  if (/^(roast|diss|shill|promote|lore|draw|meme)\b/i.test(s)) return `hire $${tk} to ${s}`;
  if (/^(ape|aping|buy|get|grab|long|snipe|scoop|bag|yolo)\s+(with|for|using)\b/i.test(s)) return s.replace(/^(\S+)\s+/, `$1 $${tk} `);
  if (/^(sell|dump)\s+(all|my)\b/i.test(s)) return `sell all my $${tk}`;
  if (/^(sell|dump)\s+\d/i.test(s)) return `${s} $${tk}`;
  if (/^fund\b/i.test(s)) return s.replace(/^fund\s+(the\s+)?((war\s+)?chest\s+)?/i, `fund $${tk} chest `);
  if (/^(join|fight)\b/i.test(s)) return s.replace(/^(join(\s+the)?(\s+war)?|fight)\s+(for\s+)?/i, `join the war for $${tk} `);
  return `${s} $${tk}`;
}

async function parse(text: string): Promise<{ intent?: Intent; ask?: string; source?: string }> {
  let r: Response;
  try {
    r = await fetch(`${NME.intentUrl}/trade/parse`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text }), signal: AbortSignal.timeout(45000) });
  } catch {
    throw new Error("The parser didn't answer. Try again in a moment.");
  }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error ?? `The parser answered ${r.status}`);
  return j;
}

const TITLE: Record<Intent["action"], (t: string) => string> = {
  buy: (t) => `Buy $${t}`, join_war: (t) => `Join the war for $${t}`, sell: (t) => `Sell $${t}`, fund_chest: (t) => `Fund $${t}'s war chest`, hire: (t) => `Hire $${t}'s brain`,
};

/** Type what you want; see exactly what will be signed; sign it. Parsing never executes anything. */
export function TradeBox({ defaultTicker, compactLayout = false, known }: { defaultTicker?: string; compactLayout?: boolean; known?: Coin[] }) {
  const { account, x, xNear, connect, connectX, act, buy, toast, refreshX } = useActions();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [ask, setAsk] = useState<string | null>(null);
  const [intent, setIntent] = useState<Intent | null>(null);
  const [coin, setCoin] = useState<Coin | null>(null);
  const [extra, setExtra] = useState<{ out?: string | null; price?: string; balance?: string | null }>({});

  async function go(t: string) {
    if (!t.trim()) return;
    setAsk(null); setIntent(null); setCoin(null); setExtra({});
    setBusy("Reading that…");
    try {
      const p = await parse(withTicker(t, defaultTicker));
      if (!p.intent) { setAsk(p.ask ?? "Say it another way?"); return; }
      // Use the roster this page already loaded; only fetch when the coin isn't in it.
      const c = known?.find((k) => k.key === p.intent!.ticker || k.ticker.toLowerCase() === p.intent!.ticker) ?? (await loadCoin(p.intent.ticker).catch(() => null));
      if (!c) { setAsk(`There's no $${p.intent.ticker.toUpperCase()} in the arena. Check the roster for the exact ticker.`); return; }
      setIntent(p.intent); setCoin(c);
    } catch (e) {
      setAsk((e as Error).message);
    } finally {
      setBusy(null);
    }
  }

  // Live numbers for the preview card.
  useEffect(() => {
    if (!intent || !coin) return;
    let alive = true;
    (async () => {
      if ((intent.action === "buy" || intent.action === "join_war") && intent.asset === "NEAR") {
        const out = await quoteBuy(coin, Number(intent.amount));
        if (alive) setExtra({ out });
      } else if (intent.action === "hire") {
        const price = await loadJobPrice(coin.key).catch(() => undefined);
        if (alive) setExtra({ price });
      } else if (intent.action === "sell") {
        const bal = account ? await loadBalance(coin.account, account).catch(() => null) : x ? await walletTokens(x.owner, coin.account).then((t) => t.tokens).catch(() => null) : null;
        const raw = intent.amount === "all" ? bal : (BigInt(Math.round(Number(intent.amount) * 1e6)) * 10n ** 12n).toString();
        const out = raw && raw !== "0" ? await quoteSell(coin, raw) : null;
        if (alive) setExtra({ balance: bal, out });
      }
    })();
    return () => { alive = false; };
  }, [intent, coin, account, x]);

  const needsX: "sol" | "evm" | null = intent && "asset" in intent && intent.asset !== "NEAR" ? (intent.asset === "SOL" ? "sol" : "evm") : null;
  const signer = needsX ? (x?.kind === needsX ? walletName(x) : null) : account || (x ? walletName(x) : null);

  /** SOL / ETH: send to the wallet's own deposit address, wait for the TEE to credit NEAR, and return the credited amount. */
  async function depositThenCredit(amount: number): Promise<number> {
    if (!x) throw new Error("Connect the wallet first.");
    const before = near((await walletState(x.owner)).near);
    const to = await depositAddress(x);
    const tx = x.kind === "sol" ? await sendSol(x, to, amount, setBusy) : await sendEth(x, to, amount, setBusy);
    setBusy("Verifying the deposit in a TEE and pricing it (about a minute)…");
    await claimDeposit(x, tx);
    for (let i = 0; i < 40; i++) {
      await sleep(6000);
      const now = near((await walletState(x.owner)).near);
      if (now > before + 1e-6) { refreshX(); return now - before; }
    }
    throw new Error("The deposit is still being verified. Your NME balance updates when it's done; then try again with NEAR.");
  }

  async function sign() {
    if (!intent || !coin) return;
    if (!signer) return connect();
    const t = coin.ticker;
    try {
      if (intent.action === "buy" || intent.action === "join_war") {
        if (intent.asset === "NEAR" && account) return buy(coin, Number(intent.amount)); // the buy sheet handles registration and slippage
        const n = intent.asset === "NEAR" ? Number(intent.amount) : await depositThenCredit(Number(intent.amount));
        const spend = Math.floor(n * 0.995 * 1e6) / 1e6; // keep a hair for rounding
        setBusy("Pricing the buy…");
        const minOut = minOutOf(await quoteBuy(coin, spend));
        if (!minOut) throw new Error("The pool gave no quote. Try again.");
        const body = { key: coin.key, amount: yoctoOf(spend), min_out: minOut };
        setBusy(`Sign in ${walletName(x!)}…`);
        await act({ x: intent.action === "join_war" ? { JoinWar: body } : { Buy: body }, ok: `Bought $${t} with ${compact(spend)} NEAR.` });
      } else if (intent.action === "fund_chest") {
        const n = intent.asset === "NEAR" ? Number(intent.amount) : await depositThenCredit(Number(intent.amount));
        const amt = yoctoOf(Math.floor(n * 0.995 * 1e6) / 1e6);
        setBusy("Sign in your wallet…");
        await act({ near: intent.asset === "NEAR" ? { contractId: NME.arena, method: "fund_chest", args: { key: coin.key }, tgas: 30, deposit: amt } : undefined, x: { FundChest: { ticker: coin.key, amount: amt } }, ok: `Added NEAR to $${t}'s war chest.` });
      } else if (intent.action === "sell") {
        const raw = intent.amount === "all" ? extra.balance : (BigInt(Math.round(Number(intent.amount) * 1e6)) * 10n ** 12n).toString();
        if (!raw || raw === "0") throw new Error(`You don't hold any $${t}.`);
        const minOut = minOutOf(extra.out ?? (await quoteSell(coin, raw)));
        if (!minOut) throw new Error("The pool gave no quote. Try again.");
        setBusy("Sign in your wallet…");
        await act({
          near: { contractId: coin.account, method: "ft_transfer_call", tgas: 300, deposit: "1", args: { receiver_id: NME.arena, amount: raw, msg: JSON.stringify({ sell: { min_out: minOut } }) } },
          x: { Sell: { token: coin.account, amount: raw, min_out: minOut } },
          ok: `Sold $${t}.`,
        });
      } else if (intent.action === "hire") {
        const price = extra.price ?? (await loadJobPrice(coin.key));
        setBusy("Sign in your wallet…");
        await act({
          near: { contractId: NME.arena, method: "hire", args: { ticker: coin.key, kind: intent.kind, target: intent.target, prompt: intent.prompt }, tgas: 300, deposit: price },
          x: { Hire: { ticker: coin.key, kind: intent.kind, target: intent.target, prompt: intent.prompt, amount: price } },
          ok: `Hired $${t} for a ${intent.kind}. The answer shows up on its page.`,
        });
      }
    } catch (e) {
      toast({ tone: "err", text: errText(e) });
    } finally {
      setBusy(null);
    }
  }

  const rows: [string, string][] = [];
  if (intent && coin) {
    if (intent.action === "buy" || intent.action === "join_war" || intent.action === "fund_chest") {
      rows.push(["You pay", `${intent.amount} ${intent.asset}${intent.asset === "SOL" ? " (Solana devnet)" : intent.asset === "ETH" ? " (Sepolia)" : ""}`]);
      if (intent.asset !== "NEAR") rows.push(["How", `Sent to your own deposit address, checked in a TEE, credited as NEAR (two price sources, 2% spread), then ${intent.action === "fund_chest" ? "added to the chest" : "spent on the buy"}${intent.asset === "ETH" ? ". Sepolia takes about 15 minutes to be final" : ""}.`]);
    }
    if ((intent.action === "buy" || intent.action === "join_war") && intent.asset === "NEAR") rows.push(["You get about", extra.out === undefined ? "quoting…" : extra.out === null ? "no quote" : `${compact(scale(extra.out, 18))} $${coin.ticker}`]);
    if (intent.action === "join_war") rows.push(["War hit", Number(intent.amount) >= 0.25 || intent.asset !== "NEAR" ? "yes, if $" + coin.ticker + " is fighting now" : "no — hits start at 0.25 NEAR"]);
    if (intent.action === "fund_chest") rows.push(["Note", "Chest money never comes back. It only buys and burns when the coin fights."]);
    if (intent.action === "sell") {
      rows.push(["You sell", intent.amount === "all" ? `all your $${coin.ticker} (${extra.balance ? compact(scale(extra.balance, 18)) : "0"})` : `${intent.amount} $${coin.ticker}`]);
      rows.push(["You get about", extra.out ? `${compact(near(extra.out))} NEAR` : "–"]);
    }
    if (intent.action === "hire") {
      rows.push(["Job", `${intent.kind}${intent.target ? ` · target ${intent.target}` : ""}`]);
      rows.push(["Prompt", intent.prompt]);
      rows.push(["You pay", extra.price ? `${compact(near(extra.price))} NEAR · about 90% buys and burns $${coin.ticker}` : "…"]);
    }
    rows.push(["Signed by", signer ?? (needsX ? (needsX === "sol" ? "Phantom (Solana devnet)" : "an EVM wallet on Sepolia") + (x ? ` — you're on ${walletName(x)} now` : "") : "connect a wallet")]);
    if (!account && x && (!("asset" in intent) || intent.asset === "NEAR")) rows.push(["Paid from", `your NME balance (${xNear === null ? "…" : compact(xNear)} NEAR)`]);
  }

  return (
    <div className={compactLayout ? "" : "grid gap-6 lg:grid-cols-[1fr_1fr]"}>
      <div>
        <form onSubmit={(e) => { e.preventDefault(); go(text); }}>
          <label htmlFor={`say-${defaultTicker ?? "home"}`} className="text-sm font-semibold">{defaultTicker ? `Tell $${defaultTicker.toUpperCase()} what to do` : "Say what you want"}</label>
          <div className="mt-2 flex gap-2">
            <input id={`say-${defaultTicker ?? "home"}`} value={text} onChange={(e) => setText(e.target.value)} maxLength={300} placeholder={defaultTicker ? "ape with 1 NEAR · roast DOGE" : EXAMPLES[0]}
              className="min-w-0 flex-1 rounded-full border border-ink bg-paper-2 px-5 py-3 text-base outline-none focus:border-blue" />
            <button className="btn bg-mint" disabled={!!busy || !text.trim()}>Go</button>
          </div>
        </form>
        {!compactLayout && (
          <div className="mt-3 flex flex-wrap gap-2">
            {EXAMPLES.map((v) => <button key={v} className="chip bg-paper-2 normal-case tracking-normal hover:bg-mint" onClick={() => { setText(v); go(v); }}>{v}</button>)}
          </div>
        )}
        <p className="mt-3 text-xs text-ink-2">Reading your sentence never moves anything. You see exactly what will happen, then you sign it yourself.</p>
      </div>

      <div aria-live="polite">
        {busy && <p className="rounded-2xl border border-ink bg-paper-2 p-4 font-hand text-2xl">{busy}</p>}
        {!busy && ask && <p className="rounded-2xl border border-ink bg-paper-2 p-4"><strong>Quick question:</strong> {ask}</p>}
        {!busy && intent && coin && (
          <div className="rounded-[1.6rem] border border-ink bg-paper-2 p-5 shadow-[0_18px_40px_-22px_var(--color-drop)]">
            <div className="flex items-center gap-3">
              <CoinIcon src={coin.icon} ticker={coin.ticker} size={44} className="rounded-xl" />
              <p className="font-display text-2xl leading-none">{TITLE[intent.action](coin.ticker)}</p>
            </div>
            <dl className="mt-4 space-y-2 text-sm">
              {rows.map(([k, v]) => (
                <div key={k} className="grid grid-cols-[7.5rem_1fr] gap-3"><dt className="text-ink-2">{k}</dt><dd className="font-semibold">{v}</dd></div>
              ))}
            </dl>
            <div className="mt-4 flex gap-2">
              {signer ? (
                <button className="btn flex-1 bg-mint" onClick={sign}>Sign it</button>
              ) : needsX === "sol" ? (
                // Paying in SOL needs Phantom: switch with one click (the other wallet's NME balance stays on chain, reconnect it anytime).
                <button className="btn flex-1 bg-mint" onClick={() => connectX("sol")}>{x ? `Switch to Phantom` : "Connect Phantom"}</button>
              ) : (
                <button className="btn flex-1 bg-mint" onClick={connect}>{needsX === "evm" ? (x ? "Switch to an EVM wallet" : "Connect an EVM wallet") : "Connect a wallet"}</button>
              )}
              <button className="btn bg-paper" onClick={() => { setIntent(null); setCoin(null); }}>Cancel</button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
