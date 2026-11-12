// The launch draft and its validation. The rules mirror nme-factory::create_coin / nme-common (ticker.rs, params.rs): the model's
// output is never trusted, and the draft carries NO economic parameter (those are fixed in buildArgs, D-58).
export const MAX_NAME = 64;
export const MAX_DESCRIPTION = 280;
export const MAX_PERSONA = 280;
export const MAX_RIVALS = 8;
export const MAX_ICON_PROMPT = 300;
export const MAX_ICON_URL = 24 * 1024;
const RESERVED = ["arena", "crown", "gateway", "factory", "admin", "nme", "treasury", "burn"];

export interface LaunchDraft {
  ticker: string;
  name: string;
  description: string;
  persona: { vibe: string; rivals: string[] };
  icon_prompt: string;
}

export class DraftError extends Error {}

const bytes = (s: string) => Buffer.byteLength(s, "utf8");

export function normalizeTicker(raw: string): string {
  const t = raw.trim().toLowerCase();
  if (t.length < 2 || t.length > 32) throw new DraftError("the ticker must be 2 to 32 characters");
  if (!/^[a-z0-9_-]+$/.test(t)) throw new DraftError("the ticker may only use a-z, 0-9, '-' and '_'");
  if (/^[-_]|[-_]$|[-_]{2}/.test(t)) throw new DraftError("the ticker may not start or end with, or repeat, '-' or '_'");
  if (RESERVED.includes(t)) throw new DraftError("that ticker is reserved");
  return t;
}

const clean = (v: unknown, field: string): string => {
  if (typeof v !== "string") throw new DraftError(`${field} is missing`);
  const s = v.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  if (!s) throw new DraftError(`${field} is empty`);
  return s;
};

/** Picks exactly the draft fields from untrusted JSON and checks every limit. Anything else the model returned is dropped. */
export function validateDraft(raw: unknown, rootLen = "nme.testnet".length): LaunchDraft {
  if (!raw || typeof raw !== "object") throw new DraftError("not an object");
  const o = raw as Record<string, unknown>;
  const ticker = normalizeTicker(typeof o.ticker === "string" ? o.ticker : "");
  if (ticker.length + 1 + rootLen > 64) throw new DraftError("the ticker is too long for an account name");
  const name = clean(o.name, "the name");
  if (bytes(name) > MAX_NAME) throw new DraftError("the name is longer than 64 bytes");
  const description = clean(o.description, "the description");
  if (bytes(description) > MAX_DESCRIPTION) throw new DraftError("the description is longer than 280 bytes");
  const vibe = clean(o.vibe ?? (o.persona as any)?.vibe, "the persona vibe");
  const rivalsRaw = o.rivals ?? (o.persona as any)?.rivals ?? [];
  if (!Array.isArray(rivalsRaw) || rivalsRaw.length > MAX_RIVALS) throw new DraftError("at most 8 rivals");
  const rivals = rivalsRaw.map((r) => clean(r, "a rival"));
  if (bytes(vibe) + rivals.reduce((n, r) => n + bytes(r), 0) > MAX_PERSONA) throw new DraftError("the persona is longer than 280 bytes");
  const icon_prompt = clean(o.icon_prompt, "the icon prompt");
  if (icon_prompt.length > MAX_ICON_PROMPT) throw new DraftError("the icon prompt is too long");
  return { ticker, name, description, persona: { vibe, rivals }, icon_prompt };
}

export function validateIconUrl(u: unknown): string {
  if (typeof u !== "string" || !/^data:image\/(webp|png);base64,[A-Za-z0-9+/]+={0,2}$/.test(u)) throw new DraftError("the icon must be a data:image/webp or data:image/png base64 URL");
  if (u.length > MAX_ICON_URL) throw new DraftError("the icon is larger than 24 KB");
  return u;
}
