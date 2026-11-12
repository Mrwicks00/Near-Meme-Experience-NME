"use client";
// "My coins": every coin whose PumpBag the connected wallet holds (as creator or buyer of the bag), with its deadline
// and the owner's three actions. NEAR accounts own bags directly; Phantom / MetaMask wallets own them through the gateway.
import Link from "next/link";
import { NME } from "@/lib/config";
import { useCoins, useNow, usePolled } from "@/lib/hooks";
import { BAG_TONE, Coin, CrownStatus, bagState, loadCrown } from "@/lib/nme";
import { crownOwnerOf } from "@/lib/xwallet";
import { useActions } from "./Actions";
import { CoinIcon } from "./CoinIcon";
import { PumpBag } from "./coin/CoinPage";

type Mine = { coin: Coin; crown: CrownStatus; xOwner: string | null };

async function findMine(coins: Coin[], account: string, xOwner: string | null): Promise<Mine[]> {
  const native = coins.filter((c) => c.native);
  const rows = await Promise.all(
    native.map(async (coin) => {
      const crown = await loadCrown(coin.key).catch(() => null);
      if (!crown) return null;
      if (account && crown.owner === account) return { coin, crown, xOwner: null };
      if (xOwner && crown.owner === NME.gateway) {
        const holder = await crownOwnerOf(coin.key).catch(() => null);
        if (holder === xOwner) return { coin, crown, xOwner: holder };
      }
      return null;
    }),
  );
  return rows.filter((r): r is Mine => !!r).sort((a, b) => bagState(a.crown, Date.now()).left - bagState(b.crown, Date.now()).left);
}

export function MyCoins() {
  const { account, x, connect } = useActions();
  const coins = useCoins().data;
  const now = useNow(30000);
  const who = account || x?.owner || "";
  const mine = usePolled(coins && who ? () => findMine(coins, account, x?.owner ?? null) : null, 30000, [coins?.length, who]);

  return (
    <main className="mx-auto max-w-5xl px-4 py-12 sm:px-8">
      <p className="font-hand text-[2.4rem] leading-none">creator desk</p>
      <h1 className="mt-1 font-serif text-[clamp(2.2rem,5vw,3.6rem)] font-light leading-tight">My coins</h1>
      <p className="mt-3 max-w-[60ch] text-[1.02rem] leading-relaxed">
        Every coin whose PumpBag you hold. Your 5% vests in its vault and its pool fees pay you, as long as you check in before the deadline.
        Only <strong>I&apos;m still here</strong> restarts the clock. Miss it and anyone can forfeit the bag: the unvested part is burned.
        You can&apos;t change a coin after launch: no admin, by design.
      </p>

      {!who && (
        <button className="btn mt-8 bg-night text-cream" onClick={connect}>Connect the wallet you launched with</button>
      )}
      {who && !mine.data && <p className="mt-8">Looking for your PumpBags…</p>}
      {who && mine.data && mine.data.length === 0 && (
        <p className="mt-8 rounded-2xl border border-ink bg-paper p-5">
          This wallet holds no PumpBag. Launch a coin from the <Link className="underline" href="/#launch">home page</Link>, or connect the wallet you launched with.
        </p>
      )}

      {now > 0 && mine.data && mine.data.length > 0 && (
        <>
          <ul className="mt-8 flex flex-wrap gap-2">
            {mine.data.map(({ coin, crown }) => {
              const st = bagState(crown, now);
              return (
                <li key={coin.key}>
                  <a href={`#bag-${coin.key}`} className={`chip ${BAG_TONE[st.tone]}`}>${coin.ticker} · {st.tone === "gone" ? "forfeited" : st.tone === "due" ? "overdue" : st.label.replace("Active, ", "")}</a>
                </li>
              );
            })}
          </ul>
          <div className="mt-8 grid grid-cols-[minmax(0,1fr)] gap-8 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
            {mine.data.map(({ coin, crown, xOwner }) => (
              <div key={coin.key} id={`bag-${coin.key}`} className="min-w-0 space-y-3">
                <Link href={`/coin/${encodeURIComponent(coin.key)}`} className="flex items-center gap-3 font-display text-3xl hover:underline">
                  <CoinIcon src={coin.icon} ticker={coin.ticker} size={44} className="rounded-xl" />${coin.ticker}
                </Link>
                <PumpBag coin={coin} crown={crown} xOwner={xOwner} />
              </div>
            ))}
          </div>
        </>
      )}
    </main>
  );
}
