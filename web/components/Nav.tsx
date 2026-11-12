"use client";
import Link from "next/link";
import { usePathname } from "next/navigation";

const links = [
  { href: "/colosseum", label: "Colosseum" },
  { href: "/#memes", label: "Memes" },
  { href: "/#launch", label: "Launch" },
  { href: "/#times", label: "Live feed" },
];

export function Nav({ right }: { right?: React.ReactNode }) {
  const path = usePathname();
  return (
    <header className="relative z-20 flex items-center justify-between gap-4 px-4 py-3 sm:px-8">
      <Link href="/" className="font-display text-[2rem] leading-none tracking-tight" aria-label="NME home">
        nme
      </Link>
      <nav aria-label="Pages" className="hidden items-center gap-7 text-[0.95rem] font-semibold md:flex">
        {links.map((l) => (
          <Link key={l.href} href={l.href} aria-current={path === l.href ? "page" : undefined} className="underline-offset-4 hover:underline aria-[current=page]:underline">
            {l.label}
          </Link>
        ))}
      </nav>
      <div className="flex items-center gap-2">
        <span className="chip hidden bg-paper sm:inline-flex" title="Everything here runs on NEAR testnet">
          <span className="h-2 w-2 rounded-full bg-[#3bbf3b]" /> Testnet
        </span>
        {right}
      </div>
    </header>
  );
}
