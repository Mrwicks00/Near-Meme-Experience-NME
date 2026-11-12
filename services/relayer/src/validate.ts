// Shape checks for what the relayer accepts. The gateway contract is the authority (it re-checks everything and verifies the
// signature); these checks only save gas on requests that are certain to fail.
export type Action =
  | { FundChest: { ticker: string; amount: string } }
  | { TransferNear: { to: string; amount: string } }
  | { Hire: { ticker: string; kind: string; target: string; prompt: string; amount: string } }
  | { Buy: { key: string; amount: string; min_out: string } }
  | { JoinWar: { key: string; amount: string; min_out: string } }
  | { Sell: { token: string; amount: string; min_out: string } }
  | { TransferTokens: { token: string; to: string; amount: string } }
  | { CreateCoin: { args_json: string; deposit: string } }
  | { CrownClaimFees: { ticker: string } }
  | { CrownClaimVested: { ticker: string } }
  | { CrownHeartbeat: { ticker: string } }
  | { CrownSend: { ticker: string; to: string } };

export interface SignedAction {
  owner: string;
  action: Action;
  nonce: number;
  expires_ms: number;
  signature: string; // base64
}

export class BadRequest extends Error {
  constructor(msg: string, public status = 400) {
    super(msg);
  }
}

const U128 = /^\d{1,39}$/;
const ACCOUNT = /^[a-z0-9][a-z0-9._-]{1,63}$/;
const PLAIN = /^[^:\u0000-\u001f\u007f]{1,64}$/; // ends up in a ':'-separated signed text line
export const OWNER_SOL = /^sol:[1-9A-HJ-NP-Za-km-z]{32,44}$/;
export const OWNER_EVM = /^evm:0x[0-9a-f]{40}$/;

function obj(v: unknown, what: string): Record<string, unknown> {
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new BadRequest(`${what} must be an object`);
  return v as Record<string, unknown>;
}
function str(o: Record<string, unknown>, k: string, re?: RegExp): string {
  const v = o[k];
  if (typeof v !== "string") throw new BadRequest(`${k} must be a string`);
  if (re && !re.test(v)) throw new BadRequest(`${k} has a bad format`);
  return v;
}
function exact(o: Record<string, unknown>, keys: string[], what: string) {
  const extra = Object.keys(o).filter((k) => !keys.includes(k));
  if (extra.length) throw new BadRequest(`${what}: unexpected field ${extra[0]}`);
}
function positive(v: string, k: string) {
  if (BigInt(v) <= 0n) throw new BadRequest(`${k} must be above zero`);
}

export function parseOwner(v: unknown): string {
  if (typeof v !== "string" || !(OWNER_SOL.test(v) || OWNER_EVM.test(v))) throw new BadRequest("owner must be sol:<pubkey> or evm:0x<lowercase address>");
  return v;
}

export function parseAction(v: unknown): Action {
  const top = obj(v, "action");
  const names = Object.keys(top);
  if (names.length !== 1) throw new BadRequest("action must have exactly one variant");
  const name = names[0]!;
  const a = obj(top[name], name);
  switch (name) {
    case "FundChest": {
      exact(a, ["ticker", "amount"], name);
      const amount = str(a, "amount", U128);
      positive(amount, "amount");
      return { FundChest: { ticker: str(a, "ticker", PLAIN), amount } };
    }
    case "TransferNear": {
      exact(a, ["to", "amount"], name);
      const amount = str(a, "amount", U128);
      positive(amount, "amount");
      return { TransferNear: { to: str(a, "to", ACCOUNT), amount } };
    }
    case "Hire": {
      exact(a, ["ticker", "kind", "target", "prompt", "amount"], name);
      const amount = str(a, "amount", U128);
      positive(amount, "amount");
      const prompt = str(a, "prompt");
      if (prompt.length > 4000) throw new BadRequest("prompt is longer than 4000 characters");
      return { Hire: { ticker: str(a, "ticker", PLAIN), kind: str(a, "kind", PLAIN), target: str(a, "target", /^[^:\u0000-\u001f\u007f]{0,64}$/), prompt, amount } };
    }
    case "Buy":
    case "JoinWar": {
      exact(a, ["key", "amount", "min_out"], name);
      const amount = str(a, "amount", U128);
      positive(amount, "amount");
      const body = { key: str(a, "key", PLAIN), amount, min_out: str(a, "min_out", U128) };
      return name === "Buy" ? { Buy: body } : { JoinWar: body };
    }
    case "Sell": {
      exact(a, ["token", "amount", "min_out"], name);
      const amount = str(a, "amount", U128);
      positive(amount, "amount");
      return { Sell: { token: str(a, "token", ACCOUNT), amount, min_out: str(a, "min_out", U128) } };
    }
    case "TransferTokens": {
      exact(a, ["token", "to", "amount"], name);
      const amount = str(a, "amount", U128);
      positive(amount, "amount");
      return { TransferTokens: { token: str(a, "token", ACCOUNT), to: str(a, "to", ACCOUNT), amount } };
    }
    case "CreateCoin": {
      exact(a, ["args_json", "deposit"], name);
      const deposit = str(a, "deposit", U128);
      positive(deposit, "deposit");
      const args_json = str(a, "args_json");
      if (args_json.length > 40_000) throw new BadRequest("the launch args are too large");
      launchTicker(args_json);
      return { CreateCoin: { args_json, deposit } };
    }
    case "CrownClaimFees":
    case "CrownClaimVested":
    case "CrownHeartbeat": {
      exact(a, ["ticker"], name);
      return { [name]: { ticker: str(a, "ticker", PLAIN) } } as Action;
    }
    case "CrownSend": {
      exact(a, ["ticker", "to"], name);
      return { CrownSend: { ticker: str(a, "ticker", PLAIN), to: str(a, "to", ACCOUNT) } };
    }
    default:
      throw new BadRequest(`unknown action ${name}`);
  }
}

/** The ticker and icon length of a create_coin args object (the gateway and the factory re-check everything). */
export function launchArgs(argsJson: string): { ticker: string; iconLen: number } {
  let v: any;
  try {
    v = JSON.parse(argsJson);
  } catch {
    throw new BadRequest("args_json is not JSON");
  }
  const ticker = v?.ticker;
  if (typeof ticker !== "string" || !/^[a-z0-9_-]{2,32}$/.test(ticker)) throw new BadRequest("the ticker must already be normalized (a-z, 0-9, '-', '_', 2 to 32 characters)");
  if (typeof v?.icon_data_url !== "string") throw new BadRequest("icon_data_url is missing");
  if (v?.launch?.first_buy != null) throw new BadRequest("a wallet launch cannot include a first buy");
  return { ticker, iconLen: v.icon_data_url.length };
}
const launchTicker = (j: string) => launchArgs(j).ticker;

export function parseSigned(v: unknown): SignedAction {
  const o = obj(v, "body");
  exact(o, ["owner", "action", "nonce", "expires_ms", "signature"], "body");
  const nonce = o.nonce;
  const exp = o.expires_ms;
  if (typeof nonce !== "number" || !Number.isSafeInteger(nonce) || nonce < 0) throw new BadRequest("nonce must be a non-negative integer");
  if (typeof exp !== "number" || !Number.isSafeInteger(exp) || exp <= 0) throw new BadRequest("expires_ms must be a positive integer");
  const signature = str(o, "signature", /^[A-Za-z0-9+/]{80,100}={0,2}$/);
  return { owner: parseOwner(o.owner), action: parseAction(o.action), nonce, expires_ms: exp, signature };
}

/** Tokens or NEAR the action spends: [near spent from the NEAR balance, relay fee applies, token spent]. */
export function costOf(a: Action): { near: bigint; fee: boolean; token?: { account: string; amount: bigint } } {
  if ("FundChest" in a) return { near: BigInt(a.FundChest.amount), fee: true };
  if ("TransferNear" in a) return { near: BigInt(a.TransferNear.amount), fee: true };
  if ("Hire" in a) return { near: BigInt(a.Hire.amount), fee: true };
  if ("Buy" in a) return { near: BigInt(a.Buy.amount), fee: false };
  if ("JoinWar" in a) return { near: BigInt(a.JoinWar.amount), fee: false };
  if ("Sell" in a) return { near: 0n, fee: false, token: { account: a.Sell.token, amount: BigInt(a.Sell.amount) } };
  if ("CreateCoin" in a) return { near: BigInt(a.CreateCoin.deposit), fee: true };
  if ("CrownHeartbeat" in a || "CrownSend" in a) return { near: 0n, fee: true };
  if ("CrownClaimFees" in a || "CrownClaimVested" in a) return { near: 0n, fee: false };
  return { near: 0n, fee: false, token: { account: a.TransferTokens.token, amount: BigInt(a.TransferTokens.amount) } };
}

export function isTokenOp(a: Action): boolean {
  return "Buy" in a || "JoinWar" in a || "Sell" in a || "TransferTokens" in a;
}
