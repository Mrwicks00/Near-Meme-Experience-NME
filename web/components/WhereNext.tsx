"use client";
// "Where next?" at the end of every page: the other destinations as big cards, so nobody reaches a dead end.
import Link from "next/link";
import { usePathname } from "next/navigation";
import { DESTINATIONS } from "./Nav";

export function WhereNext() {
  const path = usePathname() ?? "/";
  // On the home page every #section is already above, so offer the other pages first.
  const list = path === "/"
    ? DESTINATIONS.filter((d) => !d.href.startsWith("/#")).concat(DESTINATIONS.filter((d) => d.href === "/#launch" || d.href === "/#say"))
    : DESTINATIONS.filter((d) => d.href.includes("#") || !path.startsWith(d.href));
  return (
    <nav aria-labelledby="where-next-h" className="border-t border-ink px-4 py-12 sm:px-8">
      <h2 id="where-next-h" className="font-hand text-[2.2rem] leading-none">where next?</h2>
      <ul className="mt-5 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {list.slice(0, 4).map((d) => (
          <li key={d.href}>
            <Link href={d.href} className="group flex h-full items-start gap-3 rounded-[1.4rem] border border-ink bg-paper-2 p-4 transition-colors hover:bg-mint hover:text-ink">
              <span aria-hidden className="text-3xl">{d.icon}</span>
              <span className="min-w-0">
                <span className="block font-display text-xl leading-tight">{d.label} <span aria-hidden className="inline-block transition-transform group-hover:translate-x-1">→</span></span>
                <span className="mt-1 block text-sm opacity-80">{d.hint}</span>
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </nav>
  );
}
