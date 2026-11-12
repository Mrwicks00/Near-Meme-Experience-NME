"use client";
import { useRef, useState } from "react";
import { NME } from "@/lib/config";
import { nearStr } from "@/lib/format";
import { Draft, Preview, iconFromFile, preview as buildPreview, problem } from "@/lib/launch";
import { useActions } from "./Actions";
import { walletName } from "@/lib/xwallet";

type Stage = { s: "idle" } | { s: "thinking"; what: string } | { s: "ask"; q: string } | { s: "error"; msg: string };

async function post<T>(path: string, body: unknown): Promise<T> {
  let r: Response;
  try {
    r = await fetch(`${NME.intentUrl}${path}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), signal: AbortSignal.timeout(60000) });
  } catch {
    throw new Error("Couldn't reach the AI helper (no answer within 60 s). Try again, or fill it in yourself.");
  }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(`${j.error ?? `The AI helper answered ${r.status}`}${r.status === 503 ? " You can still fill it in yourself." : ""}`);
  return j as T;
}

const EXAMPLES = ["a grumpy frog that hates dog coins", "a sleepy capybara that only pumps on Sundays", "a pigeon that steals bread from whales"];
const EMPTY: Draft = { ticker: "", name: "", description: "", persona: { vibe: "", rivals: [] } };
const field = "mt-0.5 block w-full rounded-lg border-2 border-ink/30 bg-paper-2 px-2 py-1.5 text-ink outline-none focus:border-blue";

export function LaunchBox() {
  const { account, x, xNear, connect, act, call } = useActions();
  const [mode, setMode] = useState<"ai" | "manual">("ai");
  const [text, setText] = useState("");
  const [answer, setAnswer] = useState("");
  const [stage, setStage] = useState<Stage>({ s: "idle" });
  const [draft, setDraft] = useState<Draft | null>(null);
  const [icon, setIcon] = useState<string | null>(null);
  const [pv, setPv] = useState<Preview | null>(null);
  const [signing, setSigning] = useState(false);
  const seq = useRef(0);

  // Debounced preview; only the newest request may write its result.
  async function refresh(d: Draft, ic: string | null) {
    const me = ++seq.current;
    setPv(null);
    if (problem(d, ic)) return;
    await new Promise((r) => setTimeout(r, 400));
    if (me !== seq.current) return;
    try {
      const p = await buildPreview(d, ic!);
      if (me === seq.current) setPv(p);
    } catch (e) {
      if (me === seq.current) setStage({ s: "error", msg: (e as Error).message });
    }
  }

  async function fromAi(prompt: string) {
    if (!prompt.trim()) return;
    setPv(null); setDraft(null); setIcon(null);
    try {
      setStage({ s: "thinking", what: "Reading your idea…" });
      const p = await post<{ draft?: Draft; ask?: string }>("/launch/parse", { text: prompt });
      if (p.ask || !p.draft) return setStage({ s: "ask", q: p.ask ?? "Tell me a bit more about your meme." });
      const d = { ...p.draft, persona: { ...p.draft.persona, rivals: p.draft.persona.rivals.slice(0, 8) } };
      setDraft(d);
      setStage({ s: "thinking", what: "Drawing the icon…" });
      const ic = await post<{ icon_data_url: string }>("/launch/icon", { prompt: (d.icon_prompt ?? d.name).slice(0, 300) });
      setIcon(ic.icon_data_url);
      setStage({ s: "thinking", what: "Pricing the launch…" });
      await refresh(d, ic.icon_data_url);
      setStage({ s: "idle" });
    } catch (e) {
      setStage({ s: "error", msg: (e as Error).message });
    }
  }

  function goManual(seed?: string) {
    setMode("manual");
    setStage({ s: "idle" });
    const d = draft ?? { ...EMPTY, description: (seed ?? text).slice(0, 280) };
    setDraft(d);
    refresh(d, icon);
  }

  function edit(patch: Partial<Draft>) {
    const d = { ...(draft ?? EMPTY), ...patch };
    setDraft(d);
    refresh(d, icon);
  }

  async function onIcon(f: File | undefined) {
    if (!f) return;
    try { const ic = await iconFromFile(f); setIcon(ic); if (draft) refresh(draft, ic); } catch (e) { setStage({ s: "error", msg: (e as Error).message }); }
  }

  async function launch() {
    if (!pv || !draft) return;
    const t = `$${draft.ticker.toUpperCase()}`;
    setSigning(true);
    try {
      if (account) {
        await call({ contractId: NME.factory, method: "create_coin", args: { args: pv.args }, tgas: 300, deposit: pv.required_deposit, ok: `${t} is being born. It shows up in the roster once its pool opens.` });
      } else if (x) {
        // Phantom / MetaMask launch (D-59): the gateway pays the factory from the wallet's NME balance and holds the PumpBag for it.
        await act({ x: pv.wallet_action, ok: `${t} is being born, paid from your NME balance. Its PumpBag is held for your wallet.` });
      } else {
        connect();
      }
    } finally {
      setSigning(false);
    }
  }

  const issue = draft ? problem(draft, icon) : null;
  const need = pv ? Number(pv.required_deposit) / 1e24 : 0;
  const short = !account && x && xNear !== null && xNear < need;

  return (
    <section id="launch" aria-labelledby="launch-h" className="border-t border-ink bg-lime-deep px-4 py-16 sm:px-8 sm:py-24">
      <div className="grid gap-12 lg:grid-cols-[1fr_1.1fr]">
        <div>
          <p className="font-hand text-[2.8rem] leading-none">launch a meme</p>
          <h2 id="launch-h" className="mt-3 font-serif text-[clamp(2.4rem,5vw,4.4rem)] font-light leading-[0.98] tracking-[-0.015em]">
            Say what it is. <em>We&apos;ll show you exactly what you sign.</em>
          </h2>

          <div className="mt-8 inline-flex rounded-full border border-ink bg-paper p-1" role="tablist" aria-label="How to launch">
            {(["ai", "manual"] as const).map((m) => (
              <button key={m} role="tab" aria-selected={mode === m} onClick={() => (m === "manual" ? goManual() : setMode("ai"))}
                className={`rounded-full px-4 py-2 text-sm font-bold ${mode === m ? "bg-night text-cream" : ""}`}>
                {m === "ai" ? "Describe it (AI)" : "Fill it in yourself"}
              </button>
            ))}
          </div>

          {mode === "ai" ? (
            <>
              <form className="mt-5" onSubmit={(e) => { e.preventDefault(); fromAi(text); }}>
                <label htmlFor="idea" className="text-sm font-semibold">Your meme in one line</label>
                <div className="mt-2 flex flex-col gap-3 sm:flex-row">
                  <input id="idea" value={text} maxLength={400} onChange={(e) => setText(e.target.value)} placeholder={EXAMPLES[0]}
                    className="min-w-0 flex-1 rounded-full border border-ink bg-paper px-5 py-3.5 text-lg outline-none focus:outline-3 focus:outline-blue" />
                  <button className="btn bg-night text-cream" disabled={stage.s === "thinking" || !text.trim()}>Make it</button>
                </div>
              </form>
              <div className="mt-3 flex flex-wrap gap-2">
                {EXAMPLES.map((v) => (
                  <button key={v} className="chip bg-paper/70 normal-case tracking-normal hover:bg-paper" onClick={() => { setText(v); fromAi(v); }}>{v}</button>
                ))}
              </div>
              <p className="mt-8 max-w-[52ch] text-sm leading-relaxed">An AI turns your line into a name, ticker, personality and icon. Nothing is sent until you sign in your own wallet, and every field stays editable.</p>
            </>
          ) : (
            <p className="mt-5 max-w-[52ch] text-sm leading-relaxed">No AI involved: type the details, upload a picture (we square it and shrink it under 24 KB), and the preview is built right here in your browser.</p>
          )}
        </div>

        <div aria-live="polite" className="space-y-4">
          {stage.s === "thinking" && <div className="grid min-h-[200px] place-items-center rounded-[2rem] border border-ink bg-paper p-8"><p className="font-hand text-4xl">{stage.what}</p></div>}
          {stage.s === "ask" && mode === "ai" && (
            <div className="rounded-[2rem] border border-ink bg-paper p-6">
              <p className="text-lg"><strong>The AI asks:</strong> {stage.q}</p>
              <form className="mt-4 flex flex-col gap-2 sm:flex-row" onSubmit={(e) => { e.preventDefault(); const t = `${text}. ${answer}`; setText(t); setAnswer(""); fromAi(t); }}>
                <input value={answer} onChange={(e) => setAnswer(e.target.value)} placeholder="e.g. call it breadpigeon, it hates whales" className="min-w-0 flex-1 rounded-full border border-ink px-4 py-2.5" />
                <button className="btn bg-night text-cream" disabled={!answer.trim()}>Answer</button>
              </form>
              <button className="mt-3 text-sm font-semibold underline" onClick={() => goManual(text)}>Or fill it in yourself</button>
            </div>
          )}
          {stage.s === "error" && (
            <div className="rounded-[2rem] border border-ink bg-red p-6 text-cream">
              <p>{stage.msg}</p>
              {mode === "ai" && <button className="mt-3 text-sm font-semibold underline" onClick={() => goManual(text)}>Fill it in yourself instead</button>}
            </div>
          )}

          {(mode === "manual" || draft) && stage.s !== "thinking" && (
            <div className="rounded-[2rem] border border-ink bg-paper p-6 shadow-[0_18px_40px_-22px_var(--color-drop)] sm:p-8">
              <div className="flex items-start gap-5">
                <label className="group relative grid h-28 w-28 shrink-0 cursor-pointer place-items-center overflow-hidden rounded-2xl border border-dashed border-ink bg-paper-2 text-center text-xs font-semibold">
                  {/* eslint-disable-next-line @next/next/no-img-element */}
                  {icon ? <img src={icon} alt="Coin icon" className="h-full w-full object-cover" /> : <span className="px-2">Upload icon</span>}
                  <span className="absolute inset-x-0 bottom-0 bg-night/85 py-0.5 text-[0.65rem] text-cream opacity-0 group-hover:opacity-100">{icon ? "Replace" : "PNG, JPG, WebP"}</span>
                  <input type="file" accept="image/*" className="sr-only" onChange={(e) => onIcon(e.target.files?.[0])} />
                </label>
                <div className="min-w-0 flex-1 space-y-2">
                  <label className="block text-xs font-semibold text-ink-2">Ticker
                    <input value={draft?.ticker ?? ""} placeholder="breadpigeon" onChange={(e) => edit({ ticker: e.target.value.toLowerCase().replace(/[^a-z0-9_-]/g, "").slice(0, 32) })} className={`${field} font-display text-2xl uppercase`} />
                  </label>
                  <label className="block text-xs font-semibold text-ink-2">Name
                    <input value={draft?.name ?? ""} placeholder="Bread Pigeon" onChange={(e) => edit({ name: e.target.value })} className={`${field} font-semibold`} />
                  </label>
                </div>
              </div>
              <label className="mt-4 block text-xs font-semibold text-ink-2">Description
                <textarea value={draft?.description ?? ""} rows={2} onChange={(e) => edit({ description: e.target.value })} className={`${field} text-sm`} />
              </label>
              <label className="mt-3 block text-xs font-semibold text-ink-2">Personality (its AI talks like this, fixed forever)
                <input value={draft?.persona.vibe ?? ""} placeholder="greedy little bread thief" onChange={(e) => edit({ persona: { ...(draft ?? EMPTY).persona, vibe: e.target.value } })} className={`${field} text-sm`} />
              </label>
              <label className="mt-3 block text-xs font-semibold text-ink-2">Rivals (comma separated, up to 8)
                <input value={draft?.persona.rivals.join(", ") ?? ""} placeholder="doge, whales" onChange={(e) => edit({ persona: { ...(draft ?? EMPTY).persona, rivals: e.target.value.split(",").map((r) => r.trim()).filter(Boolean).slice(0, 8) } })} className={`${field} text-sm`} />
              </label>

              {issue && <p className="mt-4 rounded-xl bg-yolk px-3 py-2 text-sm font-semibold">{issue}</p>}
              {pv && !pv.available && <p className="mt-4 rounded-xl bg-red px-3 py-2 text-sm font-semibold text-cream">${draft?.ticker.toUpperCase()} is already taken. Pick another ticker.</p>}

              <dl className="mt-5 grid grid-cols-2 gap-3 rounded-2xl bg-paper-2 p-4 text-sm">
                <div><dt className="text-ink-2">You sign</dt><dd className="font-semibold">{NME.factory} · create_coin</dd></div>
                <div><dt className="text-ink-2">You pay</dt><dd className="num font-semibold">{pv ? nearStr(pv.required_deposit) : "–"} <span className="font-normal text-ink-2">(one-time launch cost)</span></dd></div>
                <div><dt className="text-ink-2">Supply</dt><dd className="font-semibold">1,000,000,000</dd></div>
                <div><dt className="text-ink-2">Admin key</dt><dd className="font-semibold">None, ever</dd></div>
                {!account && x && <div className="col-span-2"><dt className="text-ink-2">Your NME balance</dt><dd className="num font-semibold">{xNear === null ? "…" : `${xNear.toFixed(3)} NEAR`}{short ? " — top up first" : ""}</dd></div>}
              </dl>
              <div className="mt-5 flex flex-wrap gap-3">
                <button className="btn flex-1 bg-mint" onClick={launch} disabled={signing || !pv || !pv.available || !!short}>
                  {signing ? "Waiting for wallet…" : account || x ? `Launch $${(draft?.ticker || "…").toUpperCase()}${!account && x ? ` from ${walletName(x)}` : ""}` : "Connect a wallet to launch"}
                </button>
                {mode === "ai" && <button className="btn bg-paper" onClick={() => fromAi(text)}>Try again</button>}
              </div>
            </div>
          )}

          {mode === "ai" && !draft && stage.s === "idle" && (
            <div className="grid min-h-[320px] place-items-center rounded-[2rem] border border-dashed border-ink/50 p-8 text-center">
              <p className="font-serif text-2xl text-ink/70">Your preview card appears here.</p>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
