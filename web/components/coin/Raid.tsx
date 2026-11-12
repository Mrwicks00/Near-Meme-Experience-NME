"use client";
// Raids on NEAR Social: start a raid, post with its hashtag (optionally with one of the coin's AI memes), claim the post.
// The arena reads the post straight from SocialDB, so everything here is a plain NEAR transaction the user signs.
import { useState } from "react";
import { NME } from "@/lib/config";
import { compact, near } from "@/lib/format";
import { useNow, usePolled } from "@/lib/hooks";
import { Coin, Delivery, Raid as RaidT, loadLatestRaid, loadRaidClaim, loadRaidRules, loadSocialPosts } from "@/lib/nme";
import { useActions } from "../Actions";

const START = "20000000000000000000000"; // 0.02 NEAR, the raid record's storage
const CLAIM = "2000000000000000000000"; // 0.002 NEAR, the claim record's storage
const SOCIAL_STORAGE = "20000000000000000000000"; // 0.02 NEAR into the poster's own SocialDB storage balance

function left(ms: number) {
  if (ms <= 0) return "now";
  const m = Math.floor(ms / 60000);
  return m >= 1 ? `${m} min` : `${Math.ceil(ms / 1000)} s`;
}

// Pictures shown only from our own FastFS uploads (the coin's AI memes); any other link is listed, not displayed.
const SAFE_IMG = /^https:\/\/test\.fastfs\.io\//;

/** Counted raid posts (from the feed) plus the visitor's own post, read from NEAR Social. */
function RaidWall({ raid, raiders, me }: { raid: RaidT; raiders: string[]; me: string }) {
  const who = Array.from(new Set([...raiders, ...(me ? [me] : [])])).slice(0, 30);
  const posts = usePolled(who.length ? () => loadSocialPosts(who) : null, 15000, [who.join(","), raid.id]).data ?? {};
  const tag = `#${raid.tag}`.toLowerCase();
  const rows = who
    .map((a) => ({ a, p: posts[a], counted: raiders.includes(a) }))
    .filter((r) => r.p && (r.counted || r.p.text.toLowerCase().includes(tag)));
  if (!rows.length) return null;
  return (
    <div className="rounded-2xl border border-ink bg-paper p-4">
      <p className="text-sm font-semibold">Raid wall · #{raid.tag}</p>
      <p className="mt-1 text-xs">Read live from NEAR Social (SocialDB). The old near.social site no longer shows feeds, so the posts are shown here.</p>
      <ul className="mt-3 space-y-3">
        {rows.map(({ a, p, counted }) => (
          <li key={a} className="rounded-xl border border-ink/40 p-3">
            <p className="flex flex-wrap items-center justify-between gap-2 text-xs font-semibold">
              <span className="break-all">@{a}</span>
              <span className={`chip ${counted ? "bg-lime text-ink" : "bg-yolk text-ink"}`}>{counted ? "counted" : "not claimed yet"}</span>
            </p>
            {p!.image && SAFE_IMG.test(p!.image) && (
              // eslint-disable-next-line @next/next/no-img-element
              <img src={p!.image} alt={`Raid picture posted by ${a}`} className="mt-2 w-full rounded-lg border border-ink/40" />
            )}
            <p className="mt-2 whitespace-pre-wrap break-words text-sm">{p!.text}</p>
            {p!.image && !SAFE_IMG.test(p!.image) && <p className="mt-1 text-xs break-all">picture: {p!.image}</p>}
          </li>
        ))}
      </ul>
    </div>
  );
}

export function Raid({ coin, memes, raiders }: { coin: Coin; memes: Delivery[]; raiders: { raid: number; account: string }[] }) {
  const { account, x, call, connect } = useActions();
  const now = useNow(5000);
  const raidQ = usePolled(() => loadLatestRaid(coin.key), 10000, [coin.key]);
  const raid: RaidT | null = raidQ.data ?? null;
  const rules = usePolled(loadRaidRules, 0, []).data;
  const mine = usePolled(raid && account ? () => loadRaidClaim(raid.id, account) : null, 10000, [raid?.id, account]).data;
  const pics = memes.filter((d) => d.media_url).slice(0, 6);
  const [pic, setPic] = useState<string>("");
  // The draft belongs to one raid: a new raid starts from the default text again (no effect needed).
  const [draft, setDraft] = useState<{ tag: string; text: string } | null>(null);

  const endMs = raid ? raid.start_ms + (raid.end_block - raid.start_block) * NME.blockMs : 0;
  const claimEndMs = raid && rules ? endMs + rules.grace_blocks * NME.blockMs : endMs;
  const open = !!raid && raid.state === "Open" && now < claimEndMs;
  const posting = !!raid && raid.state === "Open" && now < endMs;
  const cooldownLeft = raid && rules ? raid.start_ms + rules.cooldown_ms - now : 0;

  const text = raid && draft?.tag === raid.tag ? draft.text : raid ? `$${coin.ticker} is coming for you. Buy, fight, burn. #${raid.tag}` : "";
  const setText = (t: string) => raid && setDraft({ tag: raid.tag, text: t });

  const post = () => {
    const body: Record<string, unknown> = { type: "md", text };
    if (pic) body.image = { url: pic };
    const data = {
      [account]: {
        post: { main: JSON.stringify(body) },
        index: {
          post: JSON.stringify({ key: "main", value: { type: "md" } }),
          hashtag: JSON.stringify([{ key: raid!.tag, value: { type: "social", path: `${account}/post/main` } }]),
        },
      },
    };
    return call({ contractId: NME.social, method: "set", args: { data }, tgas: 100, deposit: SOCIAL_STORAGE, ok: "Posted on NEAR Social. Now claim it below." });
  };

  return (
    <section className="rounded-[1.8rem] border border-ink bg-red-deep text-cream p-6 shadow-[0_18px_40px_-22px_var(--color-drop)] sm:p-8">
      <p className="font-hand text-[2.2rem] leading-none">raid</p>
      <h2 className="mt-1 font-serif text-[clamp(1.8rem,3vw,2.5rem)] font-light leading-tight">Raid NEAR Social for ${coin.ticker}</h2>
      <p className="mt-2 text-[1.02rem] leading-relaxed">
        Post about ${coin.ticker} on NEAR Social with the raid hashtag, then claim. Each counted post burns 0.1% of the treasury when the raid ends (up to 1%)
        and, while ${coin.ticker} is in a war, adds 2% of the stake to its score (up to 20%). The arena checks your post on chain: right hashtag, written during the raid, and you hold ${coin.ticker}.
      </p>

      {!raid || !open ? (
        <div className="mt-5 rounded-2xl border border-ink bg-paper p-4 text-sm">
          {raid && <p className="mb-2">Last raid <strong>#{raid.tag}</strong>: {raid.claims} post{raid.claims === 1 ? "" : "s"}{BigInt(raid.burned_near || "0") > 0n ? `, ${compact(near(raid.burned_near))} NEAR burned` : ""}.</p>}
          {cooldownLeft > 0 ? (
            <p>Next raid possible in {left(cooldownLeft)}.</p>
          ) : (
            <>
              <button className="btn bg-night text-cream" onClick={() => (account ? call({ contractId: NME.arena, method: "start_raid", args: { ticker: coin.key }, tgas: 50, deposit: START, ok: "Raid started. Post with the hashtag." }) : connect())}>
                {account ? "Start a raid (0.02 NEAR)" : "Connect a NEAR wallet to raid"}
              </button>
              {!account && <p className="mt-2 text-xs">Raids run on NEAR Social, where posts belong to NEAR accounts, so Phantom and MetaMask can&apos;t raid.</p>}
            </>
          )}
        </div>
      ) : (
        <div className="mt-5 space-y-4">
          <div className="rounded-2xl border border-ink bg-paper p-4">
            <p className="text-sm">Hashtag</p>
            <p className="font-display text-2xl break-all">#{raid.tag}</p>
            <p className="mt-1 text-sm">{posting ? `Posting closes in about ${left(endMs - now)}` : "Posting is closed; claims are open a little longer"} · {raid.claims}/{rules?.max_claims ?? 25} posts counted</p>
          </div>

          {x && !account && <p className="rounded-2xl border border-ink bg-paper p-4 text-sm">Raids need a NEAR account: NEAR Social posts belong to NEAR accounts. Connect a NEAR wallet to raid.</p>}

          {posting && account && (
            <div className="rounded-2xl border border-ink bg-paper p-4">
              <p className="text-sm font-semibold">1. Post</p>
              <textarea value={text} onChange={(e) => setText(e.target.value)} rows={3} className="mt-2 w-full rounded-xl border border-ink bg-transparent p-3 text-base" />
              {!text.toLowerCase().includes(`#${raid.tag}`) && <p className="mt-1 text-xs">Keep #{raid.tag} in the text, or the post will not count.</p>}
              {pics.length > 0 && (
                <>
                  <p className="mt-3 text-sm">Add one of ${coin.ticker}&apos;s AI memes (optional)</p>
                  <div className="mt-2 grid grid-cols-3 gap-2">
                    {pics.map((d) => (
                      <button key={d.seq} onClick={() => setPic(pic === d.media_url ? "" : d.media_url!)} className={`overflow-hidden rounded-xl border-2 ${pic === d.media_url ? "border-night" : "border-transparent"}`} aria-pressed={pic === d.media_url}>
                        {/* eslint-disable-next-line @next/next/no-img-element */}
                        <img src={d.media_url!} alt={d.text} className="aspect-square w-full object-cover" />
                      </button>
                    ))}
                  </div>
                </>
              )}
              {pics.length === 0 && <p className="mt-3 text-xs">No AI memes yet: hire ${coin.ticker}&apos;s brain for a <strong>meme</strong> to get a picture for raids.</p>}
              <button className="btn mt-3 w-full bg-night text-cream" disabled={!text.toLowerCase().includes(`#${raid.tag}`)} onClick={post}>
                Post on NEAR Social
              </button>
              <p className="mt-2 text-xs">Your wallet adds 0.02 NEAR to your own NEAR Social storage (yours to keep). You can also post on <a className="underline" href={NME.socialUi} target="_blank" rel="noreferrer">test.near.social</a>.</p>
            </div>
          )}

          {account && (
            <div className="rounded-2xl border border-ink bg-paper p-4">
              <p className="text-sm font-semibold">2. Claim</p>
              {mine === true ? (
                <p className="mt-1">Your post counted. 🐸</p>
              ) : (
                <>
                  <p className="mt-1 text-sm">The arena reads your latest NEAR Social post. Claim before you post anything else.</p>
                  <button className="btn mt-3 w-full bg-yolk text-ink" disabled={mine === false} onClick={() => call({ contractId: NME.arena, method: "claim_raid", args: { raid_id: raid.id }, tgas: 100, deposit: CLAIM, ok: "Claim sent. If the post matches, it shows in the feed in a few seconds." })}>
                    {mine === false ? "Checking your post…" : "Claim my raid post (0.002 NEAR)"}
                  </button>
                </>
              )}
            </div>
          )}
          {!account && !x && <button className="btn w-full bg-night text-cream" onClick={connect}>Connect a NEAR wallet to raid</button>}
        </div>
      )}
      {raid && (
        <div className="mt-4">
          <RaidWall raid={raid} raiders={raiders.filter((r) => r.raid === raid.id).map((r) => r.account)} me={account} />
        </div>
      )}
    </section>
  );
}
