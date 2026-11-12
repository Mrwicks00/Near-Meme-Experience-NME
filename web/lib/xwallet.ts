// Phantom (Solana devnet) and any EVM browser wallet on Sepolia (MetaMask, Rabby, …) without a NEAR account (D-57).
// The wallet owns a balance inside gateway.nme.testnet. Deposits go to an address derived by the Chain Signatures MPC
// for "wallet:<owner>", and every action is a plain-text message the wallet signs. The relayer pays the NEAR gas.
import { keccak_256 } from "@noble/hashes/sha3";
import bs58 from "bs58";
import { NME } from "./config";
import { view } from "./rpc";

export type XKind = "sol" | "evm";
/** `name` / `rdns` identify which EVM wallet extension was picked (EIP-6963), so we keep talking to that one. */
export type XWallet = { kind: XKind; address: string; owner: string; name?: string; rdns?: string };
export const walletName = (x: XWallet) => (x.kind === "sol" ? "Phantom" : x.name ?? "EVM wallet");
export type WalletAction = Record<string, Record<string, unknown>>;

export const MPC = "v1.signer-prod.testnet";
export const SOL_RPC = process.env.NEXT_PUBLIC_SOLANA_RPC || "https://api.devnet.solana.com";
const SEPOLIA = "0xaa36a7";

/* eslint-disable @typescript-eslint/no-explicit-any */
type Phantom = {
  isPhantom?: boolean;
  publicKey?: { toBase58(): string };
  connect(): Promise<{ publicKey: { toBase58(): string } }>;
  disconnect(): Promise<void>;
  signMessage(msg: Uint8Array, enc?: string): Promise<{ signature: Uint8Array }>;
  signAndSendTransaction(tx: any): Promise<{ signature: string }>;
};
type Eth = { isMetaMask?: boolean; request(a: { method: string; params?: unknown[] }): Promise<any> };
const w = () => (typeof window === "undefined" ? undefined : (window as any));
export const phantom = (): Phantom | undefined => w()?.phantom?.solana ?? (w()?.solana?.isPhantom ? w()!.solana : undefined);
/* eslint-enable @typescript-eslint/no-explicit-any */

// ---- EIP-6963: every installed EVM wallet announces itself; the user picks one instead of whoever grabbed window.ethereum.
export type EvmChoice = { uuid: string; name: string; icon: string; rdns: string; provider: Eth };
const announced = new Map<string, EvmChoice>();
let picked: Eth | null = null;
if (typeof window !== "undefined") {
  window.addEventListener("eip6963:announceProvider", ((ev: CustomEvent) => {
    const { info, provider } = ev.detail ?? {};
    if (info?.uuid && provider?.request) announced.set(info.uuid, { uuid: info.uuid, name: info.name, icon: info.icon, rdns: info.rdns, provider });
  }) as EventListener);
}

/** Every EVM wallet extension in this browser. Falls back to window.ethereum for wallets that predate EIP-6963. */
export async function evmWallets(): Promise<EvmChoice[]> {
  if (typeof window === "undefined") return [];
  window.dispatchEvent(new Event("eip6963:requestProvider"));
  await new Promise((r) => setTimeout(r, 250));
  const list = [...announced.values()];
  const legacy = w()?.ethereum as Eth | undefined;
  if (!list.length && legacy) list.push({ uuid: "legacy", name: "Browser wallet", icon: "", rdns: "legacy", provider: legacy });
  return list.sort((a, b) => a.name.localeCompare(b.name));
}

/** The EVM wallet currently in use: the one the user picked, else the same rdns after a reload, else window.ethereum. */
export const ethereum = (rdns?: string): Eth | undefined => {
  if (picked) return picked;
  if (rdns) for (const c of announced.values()) if (c.rdns === rdns) return (picked = c.provider);
  return w()?.ethereum;
};

const toB64 = (b: Uint8Array) => btoa(String.fromCharCode(...b));
const hexToBytes = (h: string) => Uint8Array.from((h.replace(/^0x/, "").match(/.{2}/g) ?? []).map((x) => parseInt(x, 16)));

export async function connectPhantom(): Promise<XWallet> {
  const p = phantom();
  if (!p) throw new Error("Phantom isn't installed in this browser. Get it at phantom.com, then switch it to Devnet.");
  const r = await p.connect();
  const pk = r.publicKey.toBase58();
  return { kind: "sol", address: pk, owner: `sol:${pk}` };
}

export async function connectEvm(choice: EvmChoice): Promise<XWallet> {
  picked = choice.provider;
  const [addr] = (await choice.provider.request({ method: "eth_requestAccounts" })) as string[];
  if (!addr) throw new Error(`${choice.name} returned no account.`);
  await ensureSepolia();
  const a = addr.toLowerCase();
  return { kind: "evm", address: a, owner: `evm:${a}`, name: choice.name, rdns: choice.rdns };
}

/** After a reload: find the same extension again before signing with it. */
export async function restoreEvm(x: XWallet) {
  if (x.kind !== "evm" || !x.rdns || picked) return;
  await evmWallets();
  ethereum(x.rdns);
}

async function ensureSepolia() {
  const e = ethereum()!;
  const id = await e.request({ method: "eth_chainId" });
  if (id === SEPOLIA) return;
  try {
    await e.request({ method: "wallet_switchEthereumChain", params: [{ chainId: SEPOLIA }] });
  } catch (err) {
    // 4902 (or this message on some MetaMask builds): the wallet doesn't know Sepolia yet, so add it, which also switches.
    const code = (err as { code?: number; data?: { originalError?: { code?: number } } })?.code ?? (err as { data?: { originalError?: { code?: number } } })?.data?.originalError?.code;
    if (code !== 4902 && !/unrecognized chain/i.test(String((err as Error)?.message))) throw err;
    await e.request({
      method: "wallet_addEthereumChain",
      params: [{
        chainId: SEPOLIA,
        chainName: "Sepolia",
        nativeCurrency: { name: "Sepolia Ether", symbol: "ETH", decimals: 18 },
        rpcUrls: ["https://ethereum-sepolia-rpc.publicnode.com", "https://rpc.sepolia.org"],
        blockExplorerUrls: ["https://sepolia.etherscan.io"],
      }],
    });
  }
  if ((await e.request({ method: "eth_chainId" })) !== SEPOLIA) throw new Error("Switch your wallet to the Sepolia test network to continue.");
}

/** The wallet's own deposit address, derived by the MPC for the gateway. Anything sent here can only credit this wallet. */
export async function depositAddress(x: XWallet): Promise<string> {
  const path = await view<string>(NME.gateway, "wallet_deposit_path", { owner: x.owner });
  const key = await view<string>(MPC, "derived_public_key", { path, predecessor: NME.gateway, domain_id: x.kind === "sol" ? 1 : 0 });
  if (x.kind === "sol") return key.replace(/^ed25519:/, "");
  const pub = bs58.decode(key.replace(/^secp256k1:/, "")); // 64 bytes: x || y
  return "0x" + Array.from(keccak_256(pub).slice(12), (b) => b.toString(16).padStart(2, "0")).join("");
}

// ---- relayer ----
async function relay<T>(path: string, body?: unknown, timeoutMs = 60000): Promise<T> {
  let r: Response;
  try {
    r = await fetch(`${NME.relayerUrl}${path}`, {
      method: body === undefined ? "GET" : "POST",
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch {
    throw new Error("Couldn't reach the NME relayer. Try again in a moment.");
  }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error ?? `Relayer answered ${r.status}`);
  return j as T;
}

export type GatewayWallet = { near: string; nonce: number };
/** Read straight from the gateway contract (no relayer needed to see a balance). */
export const walletState = (owner: string) => view<GatewayWallet>(NME.gateway, "get_wallet", { owner });
export const walletTokens = (owner: string, token: string) => view<{ tokens: string; wnear_due: string }>(NME.gateway, "get_wallet_tokens", { owner, token });
export const crownOwnerOf = (ticker: string) => view<string | null>(NME.gateway, "get_crown_owner", { ticker });

/** Sign with the connected wallet. Phantom: ed25519 over the UTF-8 text. MetaMask: EIP-191 personal_sign. Both as base64. */
async function signText(x: XWallet, text: string): Promise<string> {
  await restoreEvm(x);
  if (x.kind === "sol") {
    const p = phantom();
    if (!p) throw new Error("Phantom is not available.");
    const { signature } = await p.signMessage(new TextEncoder().encode(text), "utf8");
    return toB64(signature);
  }
  const hex = "0x" + Array.from(new TextEncoder().encode(text), (b) => b.toString(16).padStart(2, "0")).join("");
  const sig = (await ethereum()!.request({ method: "personal_sign", params: [hex, x.address] })) as string;
  return toB64(hexToBytes(sig));
}

/** Show the exact text in the wallet, sign it, and hand it to the relayer, which submits it to the gateway. */
export async function signedAction(x: XWallet, action: WalletAction): Promise<{ tx?: string }> {
  const { nonce } = await walletState(x.owner);
  const expires_ms = Date.now() + 120_000;
  const m = await relay<{ text?: string; message?: string }>("/message", { owner: x.owner, action, nonce, expires_ms });
  const text = m.text ?? m.message;
  if (!text) throw new Error("The relayer returned no message to sign.");
  const signature = await signText(x, text);
  return relay<{ tx?: string }>("/action", { owner: x.owner, action, nonce, expires_ms, signature }, 300000);
}

// ---- deposits ----
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Phantom: send `sol` devnet SOL to the deposit address and wait until it is finalized. Returns the signature. */
export async function sendSol(x: XWallet, to: string, sol: number, onStep: (s: string) => void): Promise<string> {
  const { Connection, PublicKey, SystemProgram, Transaction, LAMPORTS_PER_SOL } = await import("@solana/web3.js");
  const conn = new Connection(SOL_RPC, "confirmed");
  const from = new PublicKey(x.address);
  const tx = new Transaction().add(SystemProgram.transfer({ fromPubkey: from, toPubkey: new PublicKey(to), lamports: Math.round(sol * LAMPORTS_PER_SOL) }));
  tx.feePayer = from;
  tx.recentBlockhash = (await conn.getLatestBlockhash("finalized")).blockhash;
  onStep("Approve the transfer in Phantom…");
  const { signature } = await phantom()!.signAndSendTransaction(tx);
  onStep("Waiting for Solana to finalize it (about 15 s)…");
  for (let i = 0; i < 60; i++) {
    const st = await conn.getSignatureStatus(signature, { searchTransactionHistory: true }).catch(() => null);
    if (st?.value?.err) throw new Error("The Solana transfer failed.");
    if (st?.value?.confirmationStatus === "finalized") return signature;
    await sleep(2000);
  }
  throw new Error(`Solana hasn't finalized ${signature.slice(0, 10)}… yet. Claim it later from this panel.`);
}

/** MetaMask: send `eth` Sepolia ETH to the deposit address and wait until its block is finalized (~15 min on Sepolia). */
export async function sendEth(x: XWallet, to: string, eth: number, onStep: (s: string) => void): Promise<string> {
  await restoreEvm(x);
  const e = ethereum()!;
  await ensureSepolia();
  const wei = BigInt(Math.round(eth * 1e6)) * 10n ** 12n;
  onStep(`Approve the transfer in ${x.name ?? "your wallet"}…`);
  const hash = ((await e.request({ method: "eth_sendTransaction", params: [{ from: x.address, to, value: "0x" + wei.toString(16) }] })) as string).toLowerCase();
  await waitEthFinal(hash, onStep);
  return hash;
}

export async function waitEthFinal(hash: string, onStep: (s: string) => void) {
  const e = ethereum()!;
  for (let i = 0; i < 240; i++) {
    const rc = await e.request({ method: "eth_getTransactionReceipt", params: [hash] }).catch(() => null);
    if (rc) {
      if (rc.status !== "0x1") throw new Error("The Sepolia transfer failed.");
      const fin = await e.request({ method: "eth_getBlockByNumber", params: ["finalized", false] }).catch(() => null);
      const left = fin ? Number(BigInt(rc.blockNumber) - BigInt(fin.number)) : NaN;
      if (fin && left <= 0) return;
      onStep(isFinite(left) ? `Waiting for Sepolia to finalize (${left} blocks to go, about ${Math.max(1, Math.ceil(left * 12 / 60))} min). Keep this tab open.` : "Waiting for Sepolia to finalize…");
    } else {
      onStep("Waiting for the transfer to be mined…");
    }
    await sleep(15000);
  }
  throw new Error("Sepolia hasn't finalized the transfer yet. Claim it later from this panel.");
}

/** Ask the gateway (through the relayer) to have the TEE verify the deposit and credit this wallet. */
export const claimDeposit = (x: XWallet, txId: string) => relay<{ tx?: string }>("/claim", { owner: x.owner, tx_id: txId }, 300000);

// ---- deposit status ----
export type Claim = { Pending?: { since_ms: number }; Credited?: { near_paid: string }; Rejected?: { reason: string }; Done?: { near_paid: string }; Verified?: unknown } | null;
export const getClaim = (txId: string) => view<Claim>(NME.gateway, "get_claim", { tx_id: txId });

/** What the gateway can pay right now: its reserve, and the most any single deposit is ever credited. */
export async function gatewayLimits(): Promise<{ reserve: string; maxClaim: string; minClaim: string; spreadBps: number }> {
  const [reserve, cfg] = await Promise.all([
    view<string>(NME.gateway, "get_reserve"),
    view<{ cfg: { max_claim_near: string; min_claim_near: string; spread_bps: number } }>(NME.gateway, "get_config"),
  ]);
  return { reserve, maxClaim: cfg.cfg.max_claim_near, minClaim: cfg.cfg.min_claim_near, spreadBps: cfg.cfg.spread_bps };
}

/** Recent transfers into a Solana deposit address (devnet RPC), newest first. EVM has no free index for this. */
export async function solDeposits(address: string, limit = 8): Promise<{ sig: string; sol: number | null; ok: boolean }[]> {
  const rpc = (method: string, params: unknown[]) =>
    fetch(SOL_RPC, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }), signal: AbortSignal.timeout(15000) }).then((r) => r.json());
  const sigs = ((await rpc("getSignaturesForAddress", [address, { limit }])).result ?? []) as { signature: string; err: unknown }[];
  return Promise.all(sigs.map(async (s) => {
    try {
      const t = (await rpc("getTransaction", [s.signature, { maxSupportedTransactionVersion: 0, commitment: "finalized" }])).result;
      const keys: string[] = t.transaction.message.accountKeys.map((k: string | { pubkey: string }) => (typeof k === "string" ? k : k.pubkey));
      const i = keys.indexOf(address);
      return { sig: s.signature, sol: i >= 0 ? (t.meta.postBalances[i] - t.meta.preBalances[i]) / 1e9 : null, ok: !s.err };
    } catch {
      return { sig: s.signature, sol: null, ok: !s.err };
    }
  }));
}
