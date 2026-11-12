import { createPrivateKey, createPublicKey, sign as edSign } from "node:crypto";
import { secp256k1 } from "@noble/curves/secp256k1";
import { keccak_256 } from "@noble/hashes/sha3";
import bs58 from "bs58";
import type { Chain } from "../src/chain.js";

const PKCS8 = Buffer.from("302e020100300506032b657004220420", "hex");
const solSeed = Buffer.alloc(32, 7);
const solPriv = createPrivateKey({ key: Buffer.concat([PKCS8, solSeed]), format: "der", type: "pkcs8" });
const solPubRaw = createPublicKey(solPriv).export({ format: "der", type: "spki" }).subarray(-32);
export const SOL_OWNER = `sol:${bs58.encode(solPubRaw)}`;

const evmPriv = new Uint8Array(32).fill(9);
export const EVM_OWNER = (() => {
  const pub = secp256k1.getPublicKey(evmPriv, false);
  return `evm:0x${Buffer.from(keccak_256(pub.slice(1)).slice(12)).toString("hex")}`;
})();

export function signText(owner: string, text: string): string {
  if (owner.startsWith("sol:")) return edSign(null, Buffer.from(text), solPriv).toString("base64");
  const body = Buffer.from(text);
  const digest = keccak_256(Buffer.concat([Buffer.from(`\x19Ethereum Signed Message:\n${body.length}`), body]));
  const s = secp256k1.sign(digest, evmPriv);
  return Buffer.concat([Buffer.from(s.toCompactRawBytes()), Buffer.from([27 + s.recovery])]).toString("base64");
}

export interface Call {
  contract: string;
  method: string;
  args: any;
  gas: number;
  deposit: bigint;
}

/** A scripted gateway/arena/token: views answer from `state`, calls are recorded. */
export class FakeChain implements Chain {
  calls: Call[] = [];
  views: string[] = [];
  state = {
    wallet: { near: "10000000000000000000000000", nonce: 0 } as { near: string; nonce: number },
    tokens: "1000",
    lock: null as unknown,
    relayFee: "1000000000000000000000",
    coin: { account: "pepe.nme.testnet" } as unknown,
    registered: new Set<string>(),
    claim: undefined as unknown,
    claimSeq: [] as unknown[],
    failWith: undefined as string | undefined,
    quote: "900000000000000000000000",
    crownOwner: null as string | null,
    launch: null as unknown,
    coinLaunch: [{ Live: {} }] as unknown[],
    jobSeq: [] as unknown[],
    hire: null as unknown,
    logs: [] as string[],
    feed: [] as { id: number; kind: string; actor: string; extra: string }[],
    hires: {} as Record<number, { credited: boolean }>,
    walletAfter: null as null | { near: string; nonce: number },
  };
  async view<T>(contract: string, method: string, args: any): Promise<T> {
    this.views.push(`${contract}.${method}`);
    const s = this.state;
    switch (method) {
      case "get_wallet": return ((this.calls.some((c) => c.method === "wallet_action") && s.walletAfter) || s.wallet) as T;
      case "feed_last_id": return (s.feed.length ? s.feed[s.feed.length - 1]!.id : 0) as T;
      case "feed_since": return s.feed.filter((f) => f.id > args.id).slice(0, args.limit) as T;
      case "get_wallet_tokens": return { tokens: s.tokens, wnear_due: "0" } as T;
      case "get_lock": return s.lock as T;
      case "get_config": return { relay_fee: s.relayFee, wrap: "wrap.testnet" } as T;
      case "get_coin": return s.coin as T;
      case "storage_balance_of": return (s.registered.has(contract) ? { total: "1" } : null) as T;
      case "wallet_message": return `TEXT|${args.owner}|${JSON.stringify(args.action)}|${args.nonce}|${args.expires_ms}` as T;
      case "required_deposit": return s.quote as T;
      case "get_crown_owner": return s.crownOwner as T;
      case "get_launch": return (contract === "gateway.nme.testnet" ? s.launch : (s.coinLaunch.length > 1 ? s.coinLaunch.shift() : s.coinLaunch[0])) as T;
      case "get_claim": return (s.claimSeq.length ? s.claimSeq.shift() : s.claim) as T;
      case "get_job": return (s.jobSeq.length > 1 ? s.jobSeq.shift() : s.jobSeq[0] ?? null) as T;
      case "get_hire": return (s.hires[args.job_id] ?? s.hire) as T;
    }
    throw new Error(`unscripted view ${method}`);
  }
  async call(contract: string, method: string, args: any, gas: number, deposit = 0n) {
    this.calls.push({ contract, method, args, gas, deposit });
    if (method === "register_token") this.state.registered.add(args.token);
    return this.state.failWith ? { txHash: "", failure: this.state.failWith } : { txHash: `tx-${this.calls.length}`, logs: method === "wallet_action" ? this.state.logs : [] };
  }
}
