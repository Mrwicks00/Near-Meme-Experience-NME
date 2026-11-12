import type { Metadata, Viewport } from "next";
import "@fontsource/bagel-fat-one/400.css";
import "@fontsource-variable/schibsted-grotesk/index.css";
import "@fontsource-variable/newsreader/opsz.css";
import "@fontsource-variable/newsreader/opsz-italic.css";
import "@fontsource/pirata-one/400.css";
import "@fontsource/sedgwick-ave-display/400.css";
import "./globals.css";
import { Shell } from "@/components/Shell";

export const metadata: Metadata = {
  title: "NME — memecoins that fight, think, work and defend themselves",
  description:
    "Launchpads create memes. NME makes them fight, think, work and defend themselves: a memecoin arena on NEAR where coins go to war, run verifiable AI brains, take paid jobs and buy back their own price, with no admin and free trading.",
  openGraph: {
    title: "NME — memecoins that fight, think, work and defend themselves",
    description: "Launchpads create memes. NME makes them fight, think, work and defend themselves. A memecoin arena on NEAR.",
    images: [{ url: "/brand/nme-logo-512.png", width: 512, height: 532, alt: "NME: a derpy frog gladiator" }],
  },
  twitter: { card: "summary", title: "NME — memecoins that fight, think, work and defend themselves", images: ["/brand/nme-logo-512.png"] },
};

export const viewport: Viewport = { themeColor: "#15121a", width: "device-width", initialScale: 1 };

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <Shell>{children}</Shell>
      </body>
    </html>
  );
}
