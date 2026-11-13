"use client";
// Sticky top bar on every page. Wide screens: every destination in one row. Phones and tablets: a menu button that opens
// a full-width list with a one-line hint per destination, so a first-time visitor knows where each link goes.
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState, useSyncExternalStore } from "react";

export const DESTINATIONS = [
  { href: "/colosseum", label: "Colosseum", icon: "⚔️", hint: "Watch the live war. Buys are punches." },
  { href: "/#memes", label: "Memes", icon: "🐸", hint: "Every coin in the arena, live." },
  { href: "/#say", label: "Trade", icon: "💬", hint: "Type a trade like a text: “ape $PEPE with 1 NEAR”." },
  { href: "/#launch", label: "Launch", icon: "🚀", hint: "Type an idea, get a coin. No presale." },
  { href: "/#times", label: "Live feed", icon: "📰", hint: "Every buy, burn, war and AI job as it happens." },
  { href: "/me", label: "My coins", icon: "👜", hint: "Your PumpBags, fees and vesting." },
] as const;

const subscribeHash = (cb: () => void) => {
  window.addEventListener("hashchange", cb);
  return () => window.removeEventListener("hashchange", cb);
};

/** Which destination the visitor is on: the page, plus the #section on the home page. */
function useCurrent(): string {
  const path = usePathname() ?? "/";
  const hash = useSyncExternalStore(subscribeHash, () => window.location.hash, () => "");
  return path === "/" ? `/${hash}` : path;
}

export function Nav({ right }: { right?: React.ReactNode }) {
  const current = useCurrent();
  const [open, setOpen] = useState(false);

  // The open menu closes on Escape and on any navigation (hash or page).
  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    const key = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("hashchange", close);
    window.addEventListener("popstate", close);
    window.addEventListener("keydown", key);
    return () => { window.removeEventListener("hashchange", close); window.removeEventListener("popstate", close); window.removeEventListener("keydown", key); };
  }, [open]);

  const isCurrent = (href: string) => current === href || (href.startsWith("/") && !href.includes("#") && current.startsWith(href) && href !== "/");

  return (
    <header className="sticky top-0 z-40 border-b border-ink/40 bg-paper/85 px-4 py-2.5 backdrop-blur-md sm:px-8">
      <div className="flex items-center justify-between gap-3">
        <Link href="/" className="flex shrink-0 items-center gap-1.5 font-display text-[1.8rem] leading-none tracking-tight" aria-label="NME home" onClick={() => setOpen(false)}>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src="/brand/nme-mascot-192.png" alt="" width={40} height={40} className="-my-1 h-10 w-10 drop-shadow" />
          nme
        </Link>
        <nav aria-label="Pages" className="hidden items-center gap-1 text-[0.95rem] font-semibold lg:flex">
          {DESTINATIONS.map((l) => (
            <Link key={l.href} href={l.href} aria-current={isCurrent(l.href) ? "page" : undefined}
              className="rounded-full px-3 py-1.5 hover:bg-ink/10 aria-[current=page]:bg-mint aria-[current=page]:text-ink">
              {l.label}
            </Link>
          ))}
        </nav>
        <div className="flex min-w-0 items-center gap-2">
          <span className="hidden xl:inline-flex">
            <span className="chip bg-paper" title="Everything here runs on NEAR testnet">
              <span className="h-2 w-2 rounded-full bg-[#3bbf3b]" /> Testnet
            </span>
          </span>
          {right}
          <span className="inline-flex lg:hidden">
            <button
              className="grid h-11 w-11 place-items-center rounded-full border border-ink bg-paper"
              aria-expanded={open}
              aria-controls="site-menu"
              aria-label={open ? "Close the menu" : "Open the menu"}
              onClick={() => setOpen((v) => !v)}
            >
              <span aria-hidden className="text-xl leading-none">{open ? "✕" : "☰"}</span>
            </button>
          </span>
        </div>
      </div>
      {open && (
        <nav id="site-menu" aria-label="Pages" className="absolute inset-x-0 top-full max-h-[calc(100dvh-64px)] overflow-y-auto border-b border-ink bg-paper px-4 pb-5 pt-2 shadow-[0_24px_40px_-20px_rgba(0,0,0,0.7)] lg:hidden">
          <ul className="grid gap-1.5 sm:grid-cols-2">
            <li className="sm:col-span-2">
              <Link href="/" onClick={() => setOpen(false)} aria-current={current === "/" ? "page" : undefined}
                className="flex items-center gap-3 rounded-2xl px-3 py-3 hover:bg-ink/10 aria-[current=page]:bg-mint aria-[current=page]:text-ink">
                <span aria-hidden className="text-2xl">🏠</span>
                <span><span className="block font-semibold">Home</span><span className="block text-sm opacity-75">What NME is, today&apos;s fight and the latest news.</span></span>
              </Link>
            </li>
            {DESTINATIONS.map((l) => (
              <li key={l.href}>
                <Link href={l.href} onClick={() => setOpen(false)} aria-current={isCurrent(l.href) ? "page" : undefined}
                  className="flex items-center gap-3 rounded-2xl px-3 py-3 hover:bg-ink/10 aria-[current=page]:bg-mint aria-[current=page]:text-ink">
                  <span aria-hidden className="text-2xl">{l.icon}</span>
                  <span><span className="block font-semibold">{l.label}</span><span className="block text-sm opacity-75">{l.hint}</span></span>
                </Link>
              </li>
            ))}
          </ul>
          <p className="mt-3 flex items-center gap-2 px-3 text-sm opacity-75"><span className="h-2 w-2 rounded-full bg-[#3bbf3b]" /> Everything here runs on NEAR testnet: play money only.</p>
        </nav>
      )}
    </header>
  );
}
