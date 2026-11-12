"use client";
// Spoils of war: a won war burns half of what the loser's stake bought and leaves the other half to the winning side's
// fighters, each claiming a share by the war score they earned. NEAR accounts claim here; shares are paid in the winner's coin.
import { NME } from "@/lib/config";
import { compact, scale } from "@/lib/format";
import { useNow, usePolled } from "@/lib/hooks";
import { FeedItem } from "@/lib/nme";
import { view } from "@/lib/rpc";
import { useActions } from "../Actions";

type SpoilsRec = { round_id: number; coin: string; token: string; tokens: string; claimed: string; total_score: string; settled_ms: number; closed: boolean };
type Row = { round: number; coin: string; s: SpoilsRec; mine: string };

const CLAIM_MS = 7 * 86_400_000;

async function loadSpoils(wars: { round: number; coin: string }[], account: string): Promise<Row[]> {
  const rows = await Promise.all(
    wars.map(async ({ round, coin }) => {
      const s = await view<SpoilsRec | null>(NME.arena, "get_spoils", { round_id: round, coin }).catch(() => null);
      if (!s) return null;
      const mine = account ? await view<string>(NME.arena, "spoils_claimable", { round_id: round, coin, account }).catch(() => "0") : "0";
      return { round, coin, s, mine };
    }),
  );
  return rows.filter((r): r is Row => !!r);
}

export function Spoils({ feed }: { feed: FeedItem[] }) {
  const { account, call, connect } = useActions();
  const now = useNow(30000);
  const wars = feed
    .filter((f) => f.kind === "war_settled")
    .map((f) => ({ round: Number(/round=(\d+)/.exec(f.extra)?.[1] ?? 0), coin: f.ticker }))
    .filter((w) => w.round > 0)
    .slice(0, 8);
  const key = wars.map((w) => `${w.round}:${w.coin}`).join(",");
  const rows = usePolled(wars.length ? () => loadSpoils(wars, account) : null, 20000, [key, account]).data ?? [];
  const claimable = rows.filter((r) => BigInt(r.mine || "0") > 0n);

  return (
    <section className="overflow-hidden rounded-[1.8rem] border border-ink bg-paper shadow-[0_18px_40px_-22px_var(--color-drop)]">
      <h2 className="border-b border-ink bg-yolk px-6 py-4 font-black text-4xl text-ink">Spoils of war</h2>
      <div className="space-y-4 px-6 py-5">
        <p className="text-[0.98rem] leading-relaxed">
          When a coin wins, the loser&apos;s stake buys it: <strong>half is burned, half goes to the winning side&apos;s fighters</strong>,
          shared by the war score each wallet earned (buys during the round, minus sells, capped per wallet). Claim within 7 days; after that, anyone can burn what is left.
        </p>
        {!account && <button className="btn bg-night text-cream" onClick={connect}>Connect a NEAR wallet to see your spoils</button>}
        {account && claimable.length === 0 && <p className="text-sm">No spoils waiting for {account}. Buy the coin you back during a war to earn a share if it wins.</p>}
        {claimable.map((r) => (
          <div key={`${r.round}:${r.coin}`} className="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-ink bg-lime-deep p-4">
            <p className="font-semibold">Round {r.round}: {compact(scale(r.mine, 18))} ${r.coin.toUpperCase()} for you</p>
            <button className="btn bg-night text-cream" onClick={() => call({ contractId: NME.arena, method: "claim_spoils", args: { round_id: r.round, coin: r.coin }, tgas: 60, deposit: "1", ok: `Spoils of round ${r.round} sent to your wallet.` })}>
              Claim
            </button>
          </div>
        ))}
        {rows.length > 0 && (
          <ul className="divide-y divide-ink/15 text-sm">
            {rows.map((r) => {
              const left = r.s.settled_ms + CLAIM_MS - now;
              return (
                <li key={`${r.round}:${r.coin}`} className="flex flex-wrap justify-between gap-2 py-2">
                  <span>Round {r.round} · <strong>${r.coin.toUpperCase()}</strong> won: {compact(scale(r.s.tokens, 18))} to fighters, {compact(scale(r.s.claimed, 18))} claimed</span>
                  <span className="text-ink-2">{r.s.closed ? "closed (rest burned)" : left > 0 ? `${Math.ceil(left / 86_400_000)} d left` : "window over: rest can be burned"}</span>
                </li>
              );
            })}
          </ul>
        )}
        <p className="text-xs text-ink-2">Shares are paid in the winning coin, so your wallet must be registered on it (any earlier buy does that). Phantom / MetaMask fighters can&apos;t claim spoils yet; their share is burned after 7 days.</p>
      </div>
    </section>
  );
}
