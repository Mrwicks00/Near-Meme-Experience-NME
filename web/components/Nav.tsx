"use client";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";

const links = [
  { href: "/colosseum", label: "Colosseum" },
  { href: "/#memes", label: "Memes" },
  { href: "/#launch", label: "Launch" },
  { href: "/#times", label: "Live feed" },
  { href: "/me", label: "My coins" },
];

export function Nav({ right }: { right?: React.ReactNode }) {
  const path = usePathname();
  const [open, setOpen] = useState(false);
  // Close the phone menu whenever the page changes.
  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    window.addEventListener("hashchange", close);
    return () => window.removeEventListener("hashchange", close);
  }, [open]);

  return (
    <header className="relative z-30 px-4 py-3 sm:px-8">
      <div className="flex items-center justify-between gap-3">
        <Link href="/" className="flex items-center gap-1.5 font-display text-[2rem] leading-none tracking-tight" aria-label="NME home" onClick={() => setOpen(false)}>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src="/brand/nme-mascot-192.png" alt="" width={44} height={44} className="h-11 w-11 -my-1 drop-shadow" />
          nme
        </Link>
        <nav aria-label="Pages" className="hidden items-center gap-7 text-[0.95rem] font-semibold md:flex">
          {links.map((l) => (
            <Link key={l.href} href={l.href} aria-current={path === l.href ? "page" : undefined} className="underline-offset-4 hover:underline aria-[current=page]:underline">
              {l.label}
            </Link>
          ))}
        </nav>
        <div className="flex min-w-0 items-center gap-2">
          <span className="hidden sm:inline-flex">
            <span className="chip bg-paper" title="Everything here runs on NEAR testnet">
              <span className="h-2 w-2 rounded-full bg-[#3bbf3b]" /> Testnet
            </span>
          </span>
          {right}
          <span className="inline-flex md:hidden">
          <button
            className="chip bg-paper px-3 py-2"
            aria-expanded={open}
            aria-controls="phone-menu"
            aria-label={open ? "Close the menu" : "Open the menu"}
            onClick={() => setOpen((v) => !v)}
          >
            <span aria-hidden className="text-lg leading-none">{open ? "✕" : "☰"}</span>
          </button>
          </span>
        </div>
      </div>
      {open && (
        <nav id="phone-menu" aria-label="Pages" className="mt-3 grid gap-1 rounded-2xl border border-ink bg-paper p-2 text-base font-semibold md:hidden">
          {links.map((l) => (
            <Link key={l.href} href={l.href} onClick={() => setOpen(false)} aria-current={path === l.href ? "page" : undefined} className="rounded-xl px-3 py-2.5 hover:bg-mint aria-[current=page]:bg-mint">
              {l.label}
            </Link>
          ))}
        </nav>
      )}
    </header>
  );
}
