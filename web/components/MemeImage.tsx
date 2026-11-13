"use client";
// A meme picture on FastFS. The brain finishes as soon as the upload is in a block; FastFS starts serving the file a
// little later (seconds to a couple of minutes). Until then, show a placeholder and retry, then show the picture.
import { useState } from "react";

export function MemeImage({ src, alt, className }: { src: string; alt: string; className?: string }) {
  const [tries, setTries] = useState(0);
  const [failed, setFailed] = useState(false);
  const url = tries === 0 ? src : `${src}?r=${tries}`;
  return (
    <>
      {failed && <div className={`grid place-items-center bg-paper-2 p-6 text-center text-sm text-ink-2 ${className ?? ""}`}>{tries >= 40 ? "The picture isn't on FastFS yet. Check back later." : "Picture arriving from FastFS…"}</div>}
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        key={url}
        src={url}
        alt={alt}
        className={`${className ?? ""} ${failed ? "hidden" : ""}`}
        onLoad={() => setFailed(false)}
        onError={() => { setFailed(true); if (tries < 40) setTimeout(() => setTries((t) => t + 1), 6000); }}
      />
    </>
  );
}
