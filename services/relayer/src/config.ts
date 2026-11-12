import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export interface Config {
  port: number;
  rpc: string;
  gateway: string;
  arena: string;
  factory: string;
  account: string;
  key: string;
  ratePerMin: number;
  ratePerDay: number;
  ipPerMin: number;
  globalPerMin: number;
  solanaRpc: string;
  evmRpc: string;
  corsOrigin: string;
  trustProxy: boolean;
  registrationYocto: bigint;
}

function int(name: string, dflt: number): number {
  const v = process.env[name];
  if (v === undefined || v === "") return dflt;
  const n = Number(v);
  if (!Number.isInteger(n) || n <= 0) throw new Error(`${name} must be a positive integer`);
  return n;
}

/** The relayer's key: RELAYER_KEY (ed25519:...) or the near-cli credentials file of the account. */
function loadKey(account: string): string {
  if (process.env.RELAYER_KEY) return process.env.RELAYER_KEY;
  const file = join(homedir(), ".near-credentials", "testnet", `${account}.json`);
  try {
    const j = JSON.parse(readFileSync(file, "utf8")) as { private_key?: string };
    if (j.private_key) return j.private_key;
  } catch {
    /* fall through */
  }
  throw new Error(`no key for ${account}: set RELAYER_KEY or provide ${file}`);
}

export function loadConfig(): Config {
  const account = process.env.RELAYER_ACCOUNT ?? "nme-relayer.testnet";
  return {
    port: int("PORT", 8787),
    rpc: process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com",
    gateway: process.env.GATEWAY ?? "gateway.nme.testnet",
    arena: process.env.ARENA ?? "arena.nme.testnet",
    factory: process.env.NME_ROOT ?? "nme.testnet",
    account,
    key: loadKey(account),
    ratePerMin: int("RATE_PER_MIN", 6),
    ratePerDay: int("RATE_PER_DAY", 100),
    ipPerMin: int("IP_PER_MIN", 30),
    globalPerMin: int("GLOBAL_PER_MIN", 120),
    solanaRpc: process.env.SOLANA_RPC ?? "https://api.devnet.solana.com",
    evmRpc: process.env.EVM_RPC ?? "https://1rpc.io/sepolia",
    corsOrigin: process.env.CORS_ORIGIN ?? "*",
    trustProxy: process.env.TRUST_PROXY === "1",
    registrationYocto: 1_250_000_000_000_000_000_000n, // 0.00125 NEAR: NEP-145 minimum on NME coins and wNEAR
  };
}
