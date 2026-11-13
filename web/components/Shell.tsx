"use client";
import { Suspense } from "react";
import { usePathname } from "next/navigation";
import { Actions, WalletButton } from "./Actions";
import { Nav } from "./Nav";
import { Footer } from "./Footer";
import { WhereNext } from "./WhereNext";

/** Nav and footer everywhere except the embeddable widget (/widget/*), which is just the card. */
function Chrome({ children }: { children: React.ReactNode }) {
  const embed = usePathname()?.startsWith("/widget");
  if (embed) return <>{children}</>;
  return (
    <>
      <Nav right={<WalletButton />} />
      {children}
      <WhereNext />
      <Footer />
    </>
  );
}

/** Wallet provider, nav and footer shared by every page. */
export function Shell({ children }: { children: React.ReactNode }) {
  return (
    <Actions>
      <Suspense fallback={<div className="h-[62px]" />}>
        <Chrome>{children}</Chrome>
      </Suspense>
    </Actions>
  );
}
