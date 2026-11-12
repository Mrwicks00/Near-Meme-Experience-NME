// Builds the exact `create_coin` arguments in the browser. Mirrors services/intent/src/{draft,launch}.ts so a manual
// launch needs no AI and no service; the factory checks everything again on chain.
import { NME } from "./config";
import { view } from "./rpc";

export type Draft = { ticker: string; name: string; description: string; persona: { vibe: string; rivals: string[] }; icon_prompt?: string };

const RESERVED = ["arena", "crown", "gateway", "factory", "admin", "nme", "treasury", "burn"];
const MS_DAY = 86_400_000;
const DEFAULTS = { range_points: 40_000, start_point: 92_000, crown: { cliff_ms: MS_DAY, vest_ms: 30 * MS_DAY, inactivity_ms: 7 * MS_DAY } };
const bytes = (s: string) => new TextEncoder().encode(s).length;

/** First problem with the draft, in plain words, or null if it is launchable. */
export function problem(d: Draft, icon: string | null): string | null {
  const t = d.ticker;
  if (t.length < 2 || t.length > 32) return "The ticker needs 2 to 32 characters.";
  if (!/^[a-z0-9_-]+$/.test(t)) return "The ticker can only use a-z, 0-9, - and _.";
  if (/^[-_]|[-_]$|[-_]{2}/.test(t)) return "The ticker can't start or end with - or _, or repeat them.";
  if (RESERVED.includes(t)) return "That ticker is reserved.";
  if (t.length + 1 + NME.factory.length > 64) return "The ticker is too long for a NEAR account name.";
  if (!d.name.trim()) return "Give it a name.";
  if (bytes(d.name) > 64) return "The name is longer than 64 bytes.";
  if (!d.description.trim()) return "Add a one-line description.";
  if (bytes(d.description) > 280) return "The description is longer than 280 bytes.";
  if (!d.persona.vibe.trim()) return "Describe its personality: its AI talks like this.";
  if (d.persona.rivals.length > 8) return "At most 8 rivals.";
  if (bytes(d.persona.vibe) + d.persona.rivals.reduce((n, r) => n + bytes(r), 0) > 280) return "Personality plus rivals must fit in 280 bytes.";
  if (!icon) return "Add an icon.";
  if (icon.length > 24 * 1024) return "The icon is larger than 24 KB.";
  return null;
}

export function buildArgs(d: Draft, icon: string) {
  const coinIsX = `${d.ticker}.${NME.factory}` < NME.wrap;
  return {
    ticker: d.ticker,
    name: d.name.trim(),
    icon_data_url: icon,
    description: d.description.trim(),
    socials: "",
    persona: { vibe: d.persona.vibe.trim(), rivals: d.persona.rivals },
    launch: { start_point: coinIsX ? DEFAULTS.start_point : -DEFAULTS.start_point, range_points: DEFAULTS.range_points, first_buy: null },
    crown: DEFAULTS.crown,
    colosseum_opt_in: true,
  };
}

export type Preview = {
  args: ReturnType<typeof buildArgs>;
  required_deposit: string;
  available: boolean;
  wallet_action: { CreateCoin: { args_json: string; deposit: string } };
};

export async function preview(d: Draft, icon: string): Promise<Preview> {
  const args = buildArgs(d, icon);
  const [required, available] = await Promise.all([
    view<string>(NME.factory, "required_deposit", { icon_len: icon.length, first_buy_max_near: null }),
    view<boolean>(NME.factory, "ticker_available", { ticker: d.ticker }),
  ]);
  return { args, required_deposit: required, available, wallet_action: { CreateCoin: { args_json: JSON.stringify(args), deposit: required } } };
}

/** Square-crop an uploaded image and encode it as WebP under 24 KB (tries smaller sizes and qualities). */
export async function iconFromFile(file: File): Promise<string> {
  const url = URL.createObjectURL(file);
  try {
    const img = await new Promise<HTMLImageElement>((res, rej) => { const i = new Image(); i.onload = () => res(i); i.onerror = () => rej(new Error("That file isn't an image the browser can read.")); i.src = url; });
    const side = Math.min(img.naturalWidth, img.naturalHeight);
    const sx = (img.naturalWidth - side) / 2, sy = (img.naturalHeight - side) / 2;
    for (const size of [256, 192, 160, 128]) {
      const c = document.createElement("canvas");
      c.width = c.height = size;
      c.getContext("2d")!.drawImage(img, sx, sy, side, side, 0, 0, size, size);
      for (const q of [0.85, 0.7, 0.55, 0.4]) {
        const d = c.toDataURL("image/webp", q);
        // Safari can't encode WebP and returns PNG instead; the factory accepts both.
        if (/^data:image\/(webp|png);base64,/.test(d) && d.length <= 24 * 1024) return d;
      }
    }
    throw new Error("Couldn't get the icon under 24 KB. Try a simpler picture.");
  } finally {
    URL.revokeObjectURL(url);
  }
}
