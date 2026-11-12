import sharp from "sharp";
import { MAX_ICON_URL, DraftError } from "./draft.js";
import type { Chain, Llm } from "./launch.js";

const BASE = "https://cloud-api.near.ai/v1";
export const DEFAULT_MODEL = "Qwen/Qwen3.8-27B"; // the brain worker's default (workers/brain/src/logic.rs)
export const IMAGE_MODEL = "black-forest-labs/FLUX.2-klein-4B";

async function post(url: string, key: string, body: object, ms: number): Promise<any> {
  let last: unknown;
  for (let i = 0; i < 3; i++) {
    try {
      const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${key}` }, body: JSON.stringify(body), signal: AbortSignal.timeout(ms) });
      if (r.status >= 400 && r.status < 500) throw new DraftError(`NEAR AI refused the request (HTTP ${r.status})`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      return await r.json();
    } catch (e) {
      if (e instanceof DraftError) throw e;
      last = e;
      await new Promise((r) => setTimeout(r, 500 * (i + 1)));
    }
  }
  throw new Error(`NEAR AI is not reachable: ${String(last).slice(0, 80)}`);
}

export class NearAiLlm implements Llm {
  constructor(private key: string, private model = DEFAULT_MODEL) {}
  async complete(system: string, user: string): Promise<string> {
    const j = await post(`${BASE}/chat/completions`, this.key, { model: this.model, temperature: 0.4, max_tokens: 700, messages: [{ role: "system", content: system }, { role: "user", content: user }] }, 60_000);
    return String(j?.choices?.[0]?.message?.content ?? "");
  }
}

export type IconMaker = (prompt: string) => Promise<string>;

/** FLUX draws 512x512; the factory takes at most 24 KB of data URL, so the picture is shrunk until it fits. */
export function nearAiIcon(key: string): IconMaker {
  return async (prompt) => {
    const j = await post(`${BASE}/images/generations`, key, { model: IMAGE_MODEL, prompt: `${prompt}. Flat meme coin mascot icon, centered, plain background, no text, no letters.`, n: 1, size: "512x512", response_format: "b64_json" }, 120_000);
    const b64 = j?.data?.[0]?.b64_json;
    if (typeof b64 !== "string" || !b64) throw new Error("the image model returned no picture");
    return shrinkIcon(Buffer.from(b64, "base64"));
  };
}

export async function shrinkIcon(png: Buffer): Promise<string> {
  for (const size of [160, 128, 96, 64]) {
    for (const q of [80, 60, 40, 25]) {
      const webp = await sharp(png).resize(size, size, { fit: "cover" }).webp({ quality: q }).toBuffer();
      const url = `data:image/webp;base64,${webp.toString("base64")}`;
      if (url.length <= MAX_ICON_URL) return url;
    }
  }
  throw new Error("could not shrink the icon under 24 KB");
}

/** Read-only NEAR RPC view for required_deposit. */
export class RpcChain implements Chain {
  constructor(private rpc: string) {}
  async view<T>(contract: string, method: string, args: object): Promise<T> {
    let last: unknown;
    for (let i = 0; i < 4; i++) {
      try {
        const r = await fetch(this.rpc, { method: "POST", headers: { "content-type": "application/json" }, signal: AbortSignal.timeout(10_000), body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "query", params: { request_type: "call_function", finality: "final", account_id: contract, method_name: method, args_base64: Buffer.from(JSON.stringify(args)).toString("base64") } }) });
        const j: any = await r.json();
        if (j?.result?.result) return JSON.parse(Buffer.from(j.result.result).toString()) as T;
        throw new DraftError(`the contract view failed: ${JSON.stringify(j?.error ?? j?.result?.error ?? "").slice(0, 120)}`);
      } catch (e) {
        if (e instanceof DraftError) throw e;
        last = e;
        await new Promise((r) => setTimeout(r, 400 * (i + 1)));
      }
    }
    throw new Error(`NEAR RPC is not reachable: ${String(last).slice(0, 80)}`);
  }
}
