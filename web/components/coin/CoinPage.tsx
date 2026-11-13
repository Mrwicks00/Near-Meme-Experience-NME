"use client";
import { MemeImage } from "../MemeImage";
import Link from "next/link";
import { useEffect, useMemo, useState } from "react";
import { NME } from "@/lib/config";
import { accessKeyCount, view } from "@/lib/rpc";
import { ago, compact, near, scale } from "@/lib/format";
import { useCoins, useFeed, useNow, usePolled } from "@/lib/hooks";
import {
  Coin, CrownStatus, DefenseConfig, DefenseState, Delivery, JOB_KINDS, describe, bagState, BAG_TONE, holderName, timeLeft, walletLabel,
  prettyText, loadBalance, loadCoin, loadCrown, loadDefenseConfig, loadDefenseState, loadDeliveries, loadJobPrice, pendingBurns, quoteSell,
} from "@/lib/nme";
import { CoinIcon, useIconTint } from "../CoinIcon";
import { TONE_BG } from "../Times";
import { HIRE_TGAS, useActions, yocto } from "../Actions";
import { crownOwnerOf, walletTokens } from "@/lib/xwallet";
import { TradeBox } from "../TradeBox";
import { Raid } from "./Raid";
import { CoinMarket } from "./CoinMarket";
import { ProofBadge } from "../ProofBadge";
import { walletName } from "@/lib/xwallet";

const ONE_YOCTO = "1";
const STORAGE = "1250000000000000000000";
const bytes = (s: string) => new TextEncoder().encode(s).length;

function Panel({ tone, title, kicker, ai, children, id }: { tone: string; title: string; kicker?: string; ai?: boolean; children: React.ReactNode; id?: string }) {
  return (
    <section id={id} className={`rounded-[1.8rem] border border-ink p-6 shadow-[0_18px_40px_-22px_var(--color-drop)] sm:p-8 ${tone}`}>
      {kicker && <p className="font-hand text-[2.2rem] leading-none">{kicker}</p>}
      <h2 className="mt-1 flex items-center gap-2 font-serif text-[clamp(1.8rem,3vw,2.5rem)] font-light leading-tight">
        {title} {ai && <span className="chip bg-paper font-sans">AI</span>}
      </h2>
      <div className="mt-5">{children}</div>
    </section>
  );
}

function Field({ label, children, hint }: { label: string; children: React.ReactNode; hint?: string }) {
  return (
    <label className="block">
      <span className="text-sm font-semibold">{label}</span>
      <div className="mt-1">{children}</div>
      {hint && <span className="mt-1 block text-xs opacity-80">{hint}</span>}
    </label>
  );
}

const input = "w-full rounded-2xl border border-ink bg-paper-2 px-4 py-3 text-lg font-semibold text-ink outline-none focus:outline-3 focus:outline-blue";

function Trade({ coin, balanceRaw }: { coin: Coin; balanceRaw: string | null }) {
  const { buy, act, account, x, connect } = useActions();
  const signer = account || x;
  const [amount, setAmount] = useState("");
  const [q, setQ] = useState<{ for: string; out: string | null } | null>(null);
  const raw = useMemo(() => {
    const n = Number(amount);
    if (!isFinite(n) || n <= 0) return null;
    const [i, f = ""] = amount.split(".");
    try { return (BigInt(i || "0") * 10n ** 18n + BigInt((f + "0".repeat(18)).slice(0, 18))).toString(); } catch { return null; }
  }, [amount]);
  useEffect(() => {
    if (!raw) return;
    const t = setTimeout(() => quoteSell(coin, raw).then((out) => setQ({ for: raw, out })), 350);
    return () => clearTimeout(t);
  }, [raw, coin]);
  const out = raw && q?.for === raw ? q.out : undefined;
  const tooMuch = raw && balanceRaw ? BigInt(raw) > BigInt(balanceRaw) : false;
  const minOut = out ? ((BigInt(out) * 97n) / 100n).toString() : null;

  return (
    <Panel tone="bg-mint-deep" kicker="trade" title={`Buy or sell $${coin.ticker}`}>
      <p className="text-[1.02rem] leading-relaxed">Through the NME router: buys can land war hits, sells are never blocked. The coin has no tax and no pause switch.</p>
      <button className="btn mt-5 w-full bg-paper text-lg" onClick={() => buy(coin)}>Buy ${coin.ticker}</button>
      <div className="my-6 h-px bg-ink/15" />
      <Field label={`Sell $${coin.ticker}`} hint={signer ? `You hold ${balanceRaw ? compact(scale(balanceRaw, 18)) : "0"} $${coin.ticker}${!account && x ? " in your NME balance" : ""}` : "Connect a wallet to see your balance"}>
        <div className="flex gap-2">
          <input inputMode="decimal" placeholder="0" value={amount} onChange={(e) => setAmount(e.target.value.replace(",", "."))} className={input} />
          {balanceRaw && balanceRaw !== "0" && <button className="chip bg-paper" onClick={() => setAmount(String(scale(balanceRaw, 18)))}>Max</button>}
        </div>
      </Field>
      <p className="num mt-2 text-sm">{!raw ? "" : out === undefined ? "Quoting…" : out === null ? "No quote from the pool" : `You get about ${compact(near(out))} NEAR (fails if the price moves over 3%)`}</p>
      <button className="btn mt-3 w-full bg-night text-cream" disabled={!!signer && (!minOut || tooMuch)}
        onClick={() => {
          if (!signer) return connect();
          act({
            near: { contractId: coin.account, method: "ft_transfer_call", tgas: 300, deposit: ONE_YOCTO, args: { receiver_id: NME.arena, amount: raw, msg: JSON.stringify({ sell: { min_out: minOut } }) } },
            x: { Sell: { token: coin.account, amount: raw, min_out: minOut } },
            ok: `Sold ${amount} $${coin.ticker}.`,
          }).then((ok) => ok && setAmount(""));
        }}>
        {!signer ? "Connect a wallet to sell" : tooMuch ? "More than you hold" : `Sell ${amount || "0"} $${coin.ticker}`}
      </button>
    </Panel>
  );
}

function Chest({ coin }: { coin: Coin }) {
  const { act } = useActions();
  const [amt, setAmt] = useState("0.5");
  const n = Number(amt);
  return (
    <Panel tone="bg-blue-deep" kicker="war chest" title={`${compact(coin.chestNear)} NEAR in the chest`}>
      <p className="text-[1.02rem] leading-relaxed">The biggest chests get paired for the next fight. If ${coin.ticker} loses, its stake from this chest buys the winner and burns it. Chest money can never be withdrawn, by anyone. It only buys and burns.</p>
      <div className="mt-5 flex flex-col gap-3 sm:flex-row">
        <input inputMode="decimal" value={amt} onChange={(e) => setAmt(e.target.value.replace(",", "."))} className={input} aria-label="NEAR to add to the war chest" />
        <button className="btn bg-yolk text-ink" disabled={!(n > 0)} onClick={() => act({ near: { contractId: NME.arena, method: "fund_chest", args: { key: coin.key }, tgas: 30, deposit: yocto(n) }, x: { FundChest: { ticker: coin.key, amount: yocto(n) } }, ok: `Added ${n} NEAR to $${coin.ticker}'s war chest.` })}>
          Add {n > 0 ? n : ""} NEAR
        </button>
      </div>
      <p className="mt-3 text-sm opacity-90">This is a one-way trip. You get nothing back except a meaner meme.</p>
    </Panel>
  );
}

export function Hire({ coin, deliveries }: { coin: Coin; deliveries: Delivery[] }) {
  const { act, poke } = useActions();
  const ids = deliveries.map((d) => d.job_id).join(",");
  const pending = usePolled(ids ? () => pendingBurns(coin, deliveries.map((d) => d.job_id)) : null, 20000, [ids, coin.key]).data;
  const [kind, setKind] = useState<(typeof JOB_KINDS)[number]>("roast");
  const [target, setTarget] = useState("");
  const [prompt, setPrompt] = useState("");
  const price = usePolled(() => loadJobPrice(coin.key), 0, [coin.key]).data;
  // Only roast and meme aim at someone else; shill and lore are always about this coin, so they send no target.
  const hasTarget = kind === "roast" || kind === "meme";
  const sentTarget = hasTarget ? target : "";
  const pb = bytes(prompt);
  const tb = bytes(sentTarget);
  const p = near(price);
  const ok = pb > 0 && pb <= 500 && tb <= 40 && !!price;
  const T = `$${coin.ticker}`;
  const what: Record<string, { about: string; ask: string; eg: string }> = {
    roast: { about: `${T} roasts another coin or account.`, ask: "What should the roast hit?", eg: "dog coins are slow and their holders are tired" },
    shill: { about: `${T} hypes itself. A shill is always about ${T}; to hype another coin, hire that coin's brain.`, ask: "What should the shill push?", eg: "the war chest is huge and we never sell" },
    lore: { about: `${T} writes a short backstory about itself: where it came from, its enemies, its legend.`, ask: "What should the story be about?", eg: "how it escaped the swamp and swore revenge on whales" },
    meme: { about: `${T} draws a meme picture with a short caption. Takes about a minute.`, ask: "What should the picture show?", eg: "a frog in sunglasses laughing at a crying dog" },
  };
  return (
    <Panel tone="bg-lilac-deep" kicker="work" title={`Hire $${coin.ticker}'s brain`} ai>
      <div className="flex flex-wrap gap-2" role="radiogroup" aria-label="Job">
        {JOB_KINDS.map((k) => (
          <button key={k} role="radio" aria-checked={kind === k} onClick={() => setKind(k)} className={`chip px-3 py-1.5 text-[0.8rem] ${kind === k ? "bg-night text-cream" : "bg-paper"}`}>{k}</button>
        ))}
      </div>
      <p className="mt-2 text-sm">{what[kind].about}</p>
      <div className="mt-4 grid gap-4">
        {hasTarget && (
          <Field label="Target (optional)" hint={`${tb}/40 bytes — e.g. DOGE`}>
            <input value={target} onChange={(e) => setTarget(e.target.value)} className={input} placeholder="DOGE" />
          </Field>
        )}
        <Field label={what[kind].ask} hint={`${pb}/500 bytes`}>
          <textarea value={prompt} rows={3} onChange={(e) => setPrompt(e.target.value)} className={`${input} text-base font-normal`} placeholder={what[kind].eg} />
        </Field>
      </div>
      <div className="mt-4 rounded-2xl border border-ink bg-paper p-4 text-sm leading-relaxed">
        <strong className="num">You pay {price ? `${compact(p)} NEAR` : "…"}.</strong> When the job is delivered, about {compact(p * 0.9)} NEAR buys ${coin.ticker} and burns it. If it isn&apos;t delivered before the deadline, you can take your money back.
      </div>
      <button className="btn mt-4 w-full bg-night text-cream" disabled={!ok}
        onClick={() => act({ near: { contractId: NME.arena, method: "hire", args: { ticker: coin.key, kind, target: sentTarget, prompt }, tgas: HIRE_TGAS, deposit: price! }, x: { Hire: { ticker: coin.key, kind, target: sentTarget, prompt, amount: price! } }, ok: `Hired $${coin.ticker} for a ${kind}. The answer lands in the deliveries below.` }).then((d) => d && setPrompt(""))}>
        Hire for {price ? `${compact(p)} NEAR` : "…"}
      </button>

      {pending && pending.jobs > 0 && (
        <div className="mt-6 rounded-2xl border border-ink bg-yolk-deep p-4 text-sm">
          <p className="font-semibold">Waiting to burn: about {pending.tokens !== null ? compact(pending.tokens) : "…"} ${coin.ticker}</p>
          <p className="mt-1">{compact(pending.near)} NEAR from {pending.jobs} delivered job{pending.jobs > 1 ? "s" : ""} is set aside in the war chest. The buy-and-burn runs the next time anyone pokes the arena, then the Burned count goes up by the exact amount.</p>
          <button className="btn mt-3 bg-paper py-2 text-sm" onClick={poke}>Poke the arena now</button>
        </div>
      )}

      <h3 className="mt-8 text-sm font-semibold">Deliveries</h3>
      <ul className="mt-2 space-y-3">
        {deliveries.map((d) => (
          <li key={d.seq} className="rounded-2xl border border-ink bg-paper p-4">
            <p className="flex justify-between text-xs font-semibold"><span>{d.kind} · job {d.job_id}</span><span>{ago(d.ts_ms)}</span></p>
            {d.media_url && <MemeImage src={d.media_url} alt={`Meme drawn by $${coin.ticker}`} className="mt-2 w-full rounded-xl border-2 border-ink" />}
            <p className="mt-2 font-serif text-lg leading-snug">{prettyText(d.text)}</p>
            <p className="mt-1 text-[0.7rem] opacity-70">AI · {d.model.split("/").pop()}</p>
            <ProofBadge chatId={d.chat_id} model={d.model} requestHash={d.request_hash} responseHash={d.response_hash} />
          </li>
        ))}
        {!deliveries.length && <li className="text-sm">No jobs delivered yet. Be the first to hire it.</li>}
      </ul>
    </Panel>
  );
}

function Defense({ coin, cfg, st }: { coin: Coin; cfg: DefenseConfig | null; st: DefenseState | null }) {
  const now = useNow(30000);
  if (!cfg) return null;
  const dayFresh = st && now && now - st.day_start_ms < 86_400_000;
  const spent = dayFresh ? st!.day_spent_bps : 0;
  const left = Math.max(0, cfg.daily_bps - spent);
  const pct = (bps: number) => `${(bps / 100).toFixed(bps % 100 ? 2 : 0)}%`;
  return (
    <Panel tone="bg-yolk-deep" kicker="defend" title="Price defense" ai>
      <p className="text-[1.02rem] leading-relaxed">When the price drops, the AI decides to buy back or hold. Buybacks come from ${coin.ticker}&apos;s own treasury and are burned. These limits are fixed in the contract; the AI can&apos;t change them.</p>
      <dl className="num mt-5 grid grid-cols-2 gap-3 text-sm">
        <div className="rounded-2xl border border-ink bg-paper p-3"><dt>Per buyback, at most</dt><dd className="text-xl font-bold">{pct(cfg.per_action_bps)} of treasury</dd></div>
        <div className="rounded-2xl border border-ink bg-paper p-3"><dt>Per day, at most</dt><dd className="text-xl font-bold">{pct(cfg.daily_bps)} of treasury</dd></div>
        <div className="rounded-2xl border border-ink bg-paper p-3"><dt>Left today</dt><dd className="text-xl font-bold">{pct(left)} ≈ {compact(coin.treasuryNear * left / 10000)} NEAR</dd></div>
        <div className="rounded-2xl border border-ink bg-paper p-3"><dt>Last buyback</dt><dd className="text-xl font-bold">{st?.last_action_ms ? ago(st.last_action_ms, now) : "never"}</dd></div>
      </dl>
      <p className="num mt-3 text-sm">Treasury: {compact(coin.treasuryNear)} NEAR · at least {Math.round(cfg.cooldown_ms / 60000)} min between buybacks</p>
    </Panel>
  );
}

/** Who made the coin, who holds its PumpBag now (it can be sold) and whether they are keeping it alive. */
function CreatorLine({ coin, crown }: { coin: Coin; crown: CrownStatus | null }) {
  const now = useNow(30000);
  const st = crown && now ? bagState(crown, now) : null;
  // Bags of wallet-launched coins sit at the gateway for a Phantom / MetaMask key: name that key, not the gateway.
  const viaGw = crown?.owner === NME.gateway;
  const gwHolder = usePolled(viaGw ? () => crownOwnerOf(coin.key) : null, 0, [viaGw, coin.key]).data;
  const creator = walletLabel(coin.creatorWallet ?? coin.creator);
  const holderAcct = viaGw ? gwHolder : crown?.owner;
  const holder = walletLabel(holderAcct);
  const sold = !!holderAcct && holderAcct !== (coin.creatorWallet ?? coin.creator);
  return (
    <p className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
      {creator && (
        <span>
          Created by {creator.kind !== "NEAR" && <span>{creator.kind} </span>}
          <a className="font-semibold underline" href={creator.href} target="_blank" rel="noreferrer" title={creator.full}>{creator.short}</a>
        </span>
      )}
      {sold && holder && (
        <span>· PumpBag now held by {holder.kind !== "NEAR" && <span>{holder.kind} </span>}<a className="font-semibold underline" href={holder.href} target="_blank" rel="noreferrer" title={holder.full}>{holder.short}</a></span>
      )}
      {st && <span className={`chip ${BAG_TONE[st.tone]}`}>{st.tone === "gone" ? "creator bag forfeited" : st.tone === "due" ? "creator inactive" : "creator active"}</span>}
    </p>
  );
}

export function PumpBag({ coin, crown, xOwner }: { coin: Coin; crown: CrownStatus | null; xOwner: string | null }) {
  const { account, x, call, act } = useActions();
  const now = useNow(30000);
  const gwOwner = usePolled(crown?.owner === NME.gateway ? () => crownOwnerOf(coin.key) : null, 0, [crown?.owner, coin.key]).data;
  if (!crown) return null;
  // A wallet-launched coin's PumpBag is held by the gateway for the wallet key that launched it (D-59).
  const viaGateway = !!x && crown.owner === NME.gateway && xOwner === x.owner;
  const mine = account === crown.owner || viaGateway;
  const canForfeit = !crown.forfeited && now > crown.forfeitable_after_ms;
  const st = bagState(crown, now);
  const ownerName = crown.owner === NME.gateway ? holderName(gwOwner ?? undefined) : crown.owner;
  return (
    <Panel tone="bg-paper" kicker="pumpbag" title="The creator's bag">
      <p className="text-[1.02rem] leading-relaxed">The creator&apos;s 5% sits in a vault and vests over time, never in a free wallet. If the owner goes quiet too long, anyone can forfeit the bag: the unvested part is burned.</p>
      <dl className="mt-5 space-y-2 text-sm">
        <div className="flex justify-between gap-4"><dt>Owner</dt><dd className="truncate font-semibold">{viaGateway ? `held for your ${walletName(x!)} wallet` : ownerName}{mine && " (you)"}</dd></div>
        <div className="flex justify-between gap-4"><dt>Vault</dt><dd className="truncate"><a className="underline" href={`${NME.explorer}/address/${crown.vault}`} target="_blank" rel="noreferrer">{crown.vault}</a></dd></div>
        <div className="flex justify-between gap-4"><dt>Last heartbeat</dt><dd>{crown.last_heartbeat_ms ? ago(crown.last_heartbeat_ms, now) : "–"}</dd></div>
        <div className="flex justify-between gap-4"><dt>Must check in every</dt><dd>{timeLeft(crown.inactivity_ms)}</dd></div>
        <div className="flex items-center justify-between gap-4"><dt>Status</dt><dd><span className={`chip ${BAG_TONE[st.tone]}`}>{crown.forfeited ? "Forfeited: fees go to the treasury" : st.label}</span></dd></div>
      </dl>
      {mine && !crown.forfeited && (
        <p className={`mt-4 rounded-2xl border border-ink p-3 text-sm ${st.tone === "ok" ? "bg-lime-deep" : "bg-yolk"}`}>
          You hold this PumpBag. Press <strong>I&apos;m still here</strong> before {st.left > 0 ? `the deadline (in ${timeLeft(st.left)})` : "anyone forfeits it"}; only that button restarts the clock. Claiming does not.
        </p>
      )}
      {mine && !crown.forfeited && (
        <div className="mt-5 grid gap-2 sm:grid-cols-3">
          <button className="btn bg-mint text-sm" onClick={() => viaGateway
            ? act({ x: { CrownClaimFees: { ticker: coin.key } }, ok: "Fees claimed into your NME balance." })
            : call({ contractId: NME.crown, method: "crown_claim_fees", args: { token_id: coin.key }, tgas: 300, deposit: ONE_YOCTO, ok: "Fees claimed to your wallet." })}>Claim fees</button>
          <button className="btn bg-mint text-sm" onClick={async () => {
            if (viaGateway) return act({ x: { CrownClaimVested: { ticker: coin.key } }, ok: "Vested tokens added to your NME balance." });
            const reg = await view(coin.account, "storage_balance_of", { account_id: account }).catch(() => null);
            call({ contractId: NME.crown, method: "crown_claim_vested", args: { token_id: coin.key }, tgas: 300, deposit: reg ? ONE_YOCTO : STORAGE, ok: "Vested tokens sent to your wallet." });
          }}>Claim vested</button>
          <button className="btn bg-paper text-sm" onClick={() => viaGateway
            ? act({ x: { CrownHeartbeat: { ticker: coin.key } }, ok: "Heartbeat sent. The inactivity clock restarted." })
            : call({ contractId: NME.crown, method: "crown_heartbeat", args: { token_id: coin.key }, tgas: 30, ok: "Heartbeat sent. The inactivity clock restarted." })}>I&apos;m still here</button>
        </div>
      )}
      {canForfeit && (
        <button className="btn mt-5 w-full bg-red text-cream" onClick={() => act({ near: { contractId: NME.crown, method: "crown_forfeit", args: { token_id: coin.key }, tgas: 150 }, ok: "Forfeited. The unvested bag was burned." })}>Forfeit this PumpBag</button>
      )}
    </Panel>
  );
}

function Rules({ coin }: { coin: Coin }) {
  const keys = usePolled(() => accessKeyCount(coin.account), 0, [coin.account]);
  return (
    <section className="rounded-[1.8rem] bg-night p-6 text-cream sm:p-8">
      <p className="font-hand text-[2.2rem] leading-none">the rules</p>
      <ul className="mt-4 space-y-3 text-[1.02rem]">
        <li className="flex gap-3"><span aria-hidden>●</span><span>
          <strong>No admin key.</strong> Access keys on <code className="text-sm">{coin.account}</code>: {keys.data === null ? "checking…" : <strong className="text-mint">{keys.data}</strong>}
          {keys.data === 0 && " — nobody can sign as this coin, so nobody can change it."}
        </span></li>
        <li className="flex gap-3"><span aria-hidden>●</span><span><strong>No tax, no pause, no blacklist.</strong> Transfers, buys and sells can never be blocked.</span></li>
        <li className="flex gap-3"><span aria-hidden>●</span><span><strong>Treasury and war chest only buy and burn.</strong> There is no withdraw method.</span></li>
        <li className="flex gap-3"><span aria-hidden>●</span><span><strong>AI can only talk and burn.</strong> It can&apos;t sell, move money, or touch other coins.</span></li>
      </ul>
      <a className="mt-5 inline-block text-sm underline" href={`${NME.explorer}/address/${coin.account}`} target="_blank" rel="noreferrer">Check the contract on NearBlocks</a>
    </section>
  );
}

export function CoinPage({ coinKey }: { coinKey: string }) {
  const { account, x } = useActions();
  const coinQ = usePolled(() => loadCoin(coinKey), 15000, [coinKey]);
  const coin = coinQ.data;
  const balance = usePolled(
    coin && account ? () => loadBalance(coin.account, account)
      : coin && x ? () => walletTokens(x.owner, coin.account).then((t) => t.tokens)
      : null, 15000, [account, x?.owner, coin?.account]).data;
  const xOwner = usePolled(coin?.native && x ? () => crownOwnerOf(coinKey) : null, 0, [coin?.native, x?.owner, coinKey]).data;
  const crown = usePolled(coin?.native ? () => loadCrown(coinKey) : null, 30000, [coin?.native, coinKey]).data;
  const dcfg = usePolled(coin?.native ? loadDefenseConfig : null, 0, [coin?.native]).data;
  const dst = usePolled(coin?.native ? () => loadDefenseState(coinKey) : null, 20000, [coin?.native, coinKey]).data;
  const deliveries = usePolled(coin?.native ? () => loadDeliveries(32) : null, 20000, [coin?.native]).data;
  const feed = useFeed(100);
  useCoins(); // loads every fighter's symbol, so AI lines name rivals by ticker
  const tint = useIconTint(coin?.icon);

  const mine = useMemo(() => feed.items.filter((f) => f.ticker === coinKey || ((f.kind.startsWith("war_")) && f.actor === coinKey)), [feed.items, coinKey]);

  if (!coin) {
    return (
      <main className="grid min-h-[60vh] place-items-center px-6 text-center">
        <p className="font-serif text-2xl">{coinQ.error ? `Couldn't load this coin: ${coinQ.error}` : "Loading the coin from testnet…"}</p>
      </main>
    );
  }

  const stats = [
    { k: "Price", v: coin.priceNear !== null ? `${compact(coin.priceNear)} NEAR` : "–" },
    { k: "Supply", v: coin.supply ? compact(coin.supply) : "–" },
    { k: "Burned", v: compact(coin.burned) },
    { k: "Market cap", v: coin.priceNear !== null && coin.supply ? `${compact(coin.priceNear * coin.supply)} NEAR` : "–" },
    ...(account || x ? [{ k: "You hold", v: balance ? compact(scale(balance, 18)) : "0" }] : []),
  ];

  return (
    <main>
      <section className="border-y border-ink" style={{ background: tint }}>
        <div className="flex flex-col gap-8 px-4 py-10 sm:px-8 md:flex-row md:items-center md:py-14">
          <div className="-rotate-2 self-start rounded-[2rem] border border-ink bg-paper p-3 shadow-[0_18px_40px_-22px_var(--color-drop)]">
            <CoinIcon src={coin.icon} ticker={coin.ticker} size={200} className="rounded-[1.5rem] max-md:!h-36 max-md:!w-36" />
          </div>
          <div className="min-w-0">
            <div className="flex flex-wrap gap-2">
              <span className={`chip ${coin.native ? "bg-lime" : "bg-blue text-cream"}`}>{coin.native ? "Made on NME" : "Outside fighter"}</span>
              <a className="chip bg-paper normal-case tracking-normal" href={`${NME.explorer}/address/${coin.account}`} target="_blank" rel="noreferrer">{coin.account}</a>
            </div>
            <h1 className="mt-4 font-display text-[clamp(3.4rem,11vw,9rem)] leading-[0.85] tracking-[-0.02em]">${coin.ticker}</h1>
            <p className="mt-2 text-lg font-semibold">{coin.name}</p>
            {coin.native && <CreatorLine coin={coin} crown={crown} />}
            {coin.vibe && <p className="mt-3 max-w-[40ch] font-serif text-2xl italic leading-snug">“{coin.vibe}”</p>}
            {!!coin.rivals?.length && (
              <p className="mt-3 flex flex-wrap items-center gap-1.5 text-sm font-semibold">Rivals: {coin.rivals.map((r) => <span key={r} className="chip bg-red text-cream">${r.toUpperCase()}</span>)}</p>
            )}
          </div>
        </div>
        <dl className={`num grid grid-cols-2 border-t border-ink bg-paper sm:grid-cols-3 ${stats.length > 4 ? "lg:grid-cols-5" : "lg:grid-cols-4"}`}>
          {stats.map((s) => (
            <div key={s.k} className="border-b border-r border-ink px-4 py-4 sm:px-6 lg:border-b-0">
              <dt className="text-xs font-semibold uppercase tracking-wide text-ink-2">{s.k}</dt>
              <dd className="mt-1 text-2xl font-bold">{s.v}</dd>
            </div>
          ))}
        </dl>
      </section>

      <CoinMarket coin={coin} feed={feed.items} deliveries={deliveries ?? []} />

      <section className="border-b border-ink bg-mint-deep px-4 py-8 sm:px-8">
        <TradeBox defaultTicker={coin.key} />
      </section>

      <div className="grid grid-cols-[minmax(0,1fr)] gap-8 px-4 py-12 sm:px-8 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
        <div className="min-w-0 space-y-8">
          <Trade coin={coin} balanceRaw={balance} />
          <Chest coin={coin} />
          {coin.native && <Hire coin={coin} deliveries={(deliveries ?? []).filter((d) => d.ticker === coinKey)} />}
          {coin.priceNear !== null && (
            <Raid
              coin={coin}
              memes={(deliveries ?? []).filter((d) => d.ticker === coinKey)}
              raiders={mine.filter((f) => f.kind === "raid_claimed").map((f) => ({ raid: Number(/raid=(\d+)/.exec(f.extra)?.[1] ?? 0), account: f.actor }))}
            />
          )}
        </div>
        <div className="min-w-0 space-y-8">
          {coin.native && <Defense coin={coin} cfg={dcfg} st={dst} />}
          {coin.native && <PumpBag coin={coin} crown={crown} xOwner={xOwner} />}
          <Rules coin={coin} />
          <section className="rounded-[1.8rem] border border-ink bg-lime-deep p-6 sm:p-8">
            <p className="font-hand text-[2.2rem] leading-none">put it on your site</p>
            <p className="mt-2 text-[1.02rem]">One line drops a live ${coin.ticker} card into any web page: war status, Buy and Hire, with wallets inside the card.</p>
            <pre className="mt-4 overflow-x-auto rounded-2xl bg-night p-4 text-xs text-cream">{`<script src="${typeof window === "undefined" ? "" : window.location.origin}/nme-widget.js" data-ticker="${coin.key}"></script>`}</pre>
            <a className="mt-3 inline-block text-sm font-semibold underline" href={`/widget-demo.html?ticker=${encodeURIComponent(coin.key)}`} target="_blank" rel="noreferrer">See it on a demo page</a>
          </section>
          <section className="rounded-[1.8rem] border border-ink bg-paper">
            <h2 className="border-b border-ink px-6 py-4 font-black text-4xl">${coin.ticker} Times</h2>
            <ol className="max-h-[520px] overflow-y-auto">
              {mine.map((f) => {
                const l = describe(f);
                return (
                  <li key={f.id} className={`grid grid-cols-[auto_1fr] items-baseline gap-3 border-b border-ink/15 px-6 py-3 ${feed.fresh.has(f.id) ? "flash" : ""}`}>
                    <span className={`chip ${TONE_BG[l.tone]}`}>{l.ai ? "AI · " : ""}{l.tag}</span>
                    <span className="text-[0.95rem] leading-snug">{l.text} <span className="text-xs text-ink-2">· {ago(f.ts_ms)}</span></span>
                  </li>
                );
              })}
              {!mine.length && <li className="px-6 py-5 text-ink-2">Nothing recent for ${coin.ticker}. Buy, fund or hire to make the news.</li>}
            </ol>
          </section>
        </div>
      </div>
      <p className="px-4 pb-10 sm:px-8"><Link href="/#memes" className="font-semibold underline">← All memes</Link></p>
    </main>
  );
}
