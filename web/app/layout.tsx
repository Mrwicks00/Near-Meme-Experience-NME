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
  title: "NME — memes that fight",
  description:
    "A memecoin arena on NEAR testnet. Two memes fight each round; the loser's war chest buys the winner and burns it. Buying and selling are always free.",
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
