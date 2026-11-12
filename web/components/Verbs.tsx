"use client";
import Image from "next/image";
import { useRef } from "react";
import gsap from "gsap";
import { ScrollTrigger } from "gsap/ScrollTrigger";
import { useGSAP } from "@gsap/react";

gsap.registerPlugin(useGSAP, ScrollTrigger);

// Four things a meme does here. Not a sequence, so no numbers: each block wears the colour of what it touches.
const VERBS = [
  {
    id: "launch", hand: "make one", title: "Type an idea. Get a coin.", bg: "bg-lime-deep", img: "/img/axolotl.jpg", alt: "An axolotl looking at the camera",
    body: "Describe a meme in one line. NME draws the icon, writes its personality and shows you exactly what you will sign. 1 billion supply, no presale, and the creator's share vests inside its PumpBag instead of a wallet.",
  },
  {
    id: "fight", hand: "fight", title: "Buys are punches.", bg: "bg-red-deep", img: "/img/mosaic.jpg", alt: "Roman mosaic of gladiators fighting, Galleria Borghese",
    body: "Each round pairs two memes. Every buy of 0.25 NEAR or more lands a hit for your side. When the bell rings, the loser's stake from its war chest buys the winner on its pool and burns it.",
  },
  {
    id: "work", hand: "work", title: "Hire its brain.", bg: "bg-lilac-deep", img: "/img/frog.jpg", alt: "A small frog on a leaf",
    body: "Pay a meme to roast, shill, write lore or draw. Its AI does the job, and about 90% of what you paid buys that coin and burns it. If it misses the deadline, you get refunded.",
  },
  {
    id: "defend", hand: "defend", title: "It guards its own price.", bg: "bg-yolk-deep", img: "/img/boxer.jpg", alt: "The Boxer at Rest, Hellenistic bronze sculpture",
    body: "When the chart dips, the coin's AI may buy back from its own treasury and burn it, inside limits fixed at launch. It can never sell, withdraw, or touch another coin's money.",
  },
];

export function Verbs() {
  const root = useRef<HTMLElement>(null);
  useGSAP(() => {
    const mm = gsap.matchMedia();
    mm.add("(prefers-reduced-motion: no-preference)", () => {
      // Only blocks still below the fold get hidden and revealed; anything already on screen stays put.
      gsap.utils.toArray<HTMLElement>(".verb").forEach((el) => {
        if (el.getBoundingClientRect().top < window.innerHeight * 0.9) return;
        const img = el.querySelector(".verb-img");
        const txt = el.querySelectorAll(".verb-txt > *");
        gsap.set(img, { clipPath: "inset(100% 0 0 0)" });
        gsap.set(txt, { y: 28, opacity: 0 });
        ScrollTrigger.create({
          trigger: el, start: "top 80%", once: true,
          onEnter: () => {
            gsap.to(img, { clipPath: "inset(0% 0 0 0)", duration: 1.1, ease: "power3.out" });
            gsap.to(txt, { y: 0, opacity: 1, stagger: 0.08, duration: 0.7, ease: "power3.out" });
          },
        });
      });
    });
  }, { scope: root });

  return (
    <section ref={root} aria-labelledby="verbs-h" className="border-b border-ink">
      <h2 id="verbs-h" className="sr-only">What a meme does on NME</h2>
      <div className="grid md:grid-cols-2">
        {VERBS.map((v, i) => (
          <article key={v.id} id={v.id === "launch" ? undefined : v.id === "fight" ? "how" : undefined}
            className={`verb ${v.bg} grid grid-rows-[auto_1fr] border-ink ${i % 2 === 0 ? "md:border-r" : ""} ${i < 2 ? "border-b" : i === 2 ? "max-md:border-b" : ""}`}>
            <div className="verb-img relative aspect-[16/9] overflow-hidden border-b border-ink">
              <Image src={v.img} alt={v.alt} fill sizes="(min-width: 768px) 50vw, 100vw" className="object-cover saturate-[1.15]" />
            </div>
            <div className="verb-txt flex flex-col gap-3 p-6 sm:p-10">
              <p className="font-hand text-[2.6rem] leading-none">{v.hand}</p>
              <h3 className="font-serif text-[clamp(2rem,3.6vw,3.2rem)] font-light leading-[1.02] tracking-[-0.01em]">{v.title}</h3>
              <p className="max-w-[52ch] text-[1.05rem] leading-relaxed">{v.body}</p>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
