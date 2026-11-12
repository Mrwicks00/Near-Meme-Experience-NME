import { NME } from "@/lib/config";

export function Footer() {
  return (
    <footer className="m-3 overflow-hidden rounded-[2rem] border border-ink/15 bg-night text-cream sm:m-5">
      <div className="grid gap-8 p-6 text-sm sm:grid-cols-3 sm:p-10">
        <div>
          <p className="font-semibold uppercase tracking-wide">NME · Near-Meme Experience</p>
          <p className="mt-3 max-w-[34ch] text-cream/80">A memecoin arena on NEAR testnet. No admin key. Treasuries and war chests can only buy and burn.</p>
        </div>
        <div>
          <p className="font-semibold uppercase tracking-wide">Contracts</p>
          <ul className="mt-3 space-y-1 text-cream/80">
            {[NME.factory, NME.arena, NME.crown, NME.gateway].map((a) => (
              <li key={a}><a className="underline-offset-4 hover:underline" href={`${NME.explorer}/address/${a}`} target="_blank" rel="noreferrer">{a}</a></li>
            ))}
          </ul>
        </div>
        <div>
          <p className="font-semibold uppercase tracking-wide">Credits</p>
          <p className="mt-3 text-cream/80">Photos: Wikimedia Commons (public domain, CC0, CC BY, CC BY-SA). Full list in <code>web/public/img/CREDITS.md</code>.</p>
        </div>
      </div>
      <p aria-hidden className="select-none px-2 pb-2 text-center font-display text-[31vw] leading-[0.72] tracking-[-0.04em] text-cream">nme</p>
    </footer>
  );
}
