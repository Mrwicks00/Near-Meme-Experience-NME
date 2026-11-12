"use client";
import { Suspense } from "react";
import { useParams } from "next/navigation";
import { Widget } from "@/components/widget/Widget";

function ByKey() {
  const { key } = useParams<{ key: string }>();
  return <Widget coinKey={decodeURIComponent(key)} />;
}

export default function Page() {
  return (
    <Suspense fallback={null}>
      {/* Transparent so the card sits on the host site's own background. */}
      <style>{`html, body { background: transparent !important; }`}</style>
      <ByKey />
    </Suspense>
  );
}
