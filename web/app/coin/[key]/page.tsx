"use client";
import { Suspense } from "react";
import { useParams } from "next/navigation";
import { CoinPage } from "@/components/coin/CoinPage";

function ByKey() {
  const { key } = useParams<{ key: string }>();
  return <CoinPage coinKey={decodeURIComponent(key)} />;
}

export default function Page() {
  return (
    <Suspense fallback={<main className="grid min-h-[60vh] place-items-center font-serif text-2xl">Loading the coin…</main>}>
      <ByKey />
    </Suspense>
  );
}
