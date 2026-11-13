// Google sign-in accounts: this browser holds the account's own ed25519 key (made here, never sent anywhere) and signs
// transactions directly, so every action is one tap: no wallet app, no extension, no popup.
// The relayer created `<name>.nmeplay.testnet` with this key after checking the Google sign-in.
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner, actions, teraToGas, type KeyPairString } from "near-api-js";
import { NME } from "./config";

const STORE = "nme.play.v1";
type Saved = { account: string | null; sk: string };

function read(): Saved | null {
  try {
    const v = JSON.parse(localStorage.getItem(STORE) ?? "null");
    return v && typeof v.sk === "string" ? v : null;
  } catch {
    return null;
  }
}
function write(v: Saved | null) {
  try {
    if (v) localStorage.setItem(STORE, JSON.stringify(v));
    else localStorage.removeItem(STORE);
  } catch {
    /* private mode: the session lasts as long as the tab */
  }
}

/** This device's key (made on first use). */
export function deviceKey(): { publicKey: string; sk: string } {
  let s = read();
  if (!s) {
    s = { account: null, sk: KeyPair.fromRandom("ed25519").toString() };
    write(s);
  }
  return { publicKey: KeyPair.fromString(s.sk as KeyPairString).getPublicKey().toString(), sk: s.sk };
}

export const playAccount = () => read()?.account ?? null;
export function setPlayAccount(account: string) {
  const s = read() ?? { account: null, sk: deviceKey().sk };
  write({ ...s, account });
}
/** Forget the account on this device (the key too: back it up first). */
export const playSignOut = () => write(null);
/** The secret key, for "Back up my key" (import it in any NEAR wallet to use the account elsewhere). */
export const playSecret = () => read()?.sk ?? null;
/** Import a backed-up key for an account on this device. */
export function playImport(account: string, sk: string) {
  KeyPair.fromString(sk as KeyPairString); // throws on a malformed key
  write({ account, sk });
}

function provider() {
  return new JsonRpcProvider({ url: `${window.location.origin}/api/rpc` });
}

export type SessionReply = { account: string | null; needs_name?: boolean; created?: boolean; error?: string };
/** Hands the Google ID token and this device's public key to the relayer: signs up (with a name) or adds this device. */
export async function playSession(idToken: string, name?: string): Promise<SessionReply> {
  const { publicKey } = deviceKey();
  const r = await fetch(`${NME.relayerUrl}/play/session`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ id_token: idToken, device_pk: publicKey, name }),
    signal: AbortSignal.timeout(90_000),
  });
  const j = (await r.json().catch(() => ({}))) as SessionReply;
  if (!r.ok) throw new Error(j.error ?? `HTTP ${r.status}`);
  if (j.account) setPlayAccount(j.account);
  return j;
}

/** Signs and sends a function call with this device's key. Returns the transaction hash. */
export async function playCall(c: { contractId: string; method: string; args: Record<string, unknown>; tgas: number; deposit?: string }): Promise<string> {
  const s = read();
  if (!s?.account) throw new Error("not signed in");
  const acc = new Account(s.account, provider(), KeyPairSigner.fromSecretKey(s.sk as KeyPairString));
  const out = (await acc.callFunctionRaw({ contractId: c.contractId, methodName: c.method, args: c.args, gas: teraToGas(c.tgas), deposit: BigInt(c.deposit ?? "0") })) as { status?: unknown; transaction?: { hash?: string }; transaction_outcome?: { id?: string } };
  const st = out?.status;
  if (st && typeof st === "object" && "Failure" in st) throw new Error(`the transaction failed: ${JSON.stringify(st.Failure).slice(0, 200)}`);
  return out?.transaction?.hash ?? out?.transaction_outcome?.id ?? "";
}

/** The account's NEAR balance (yoctoNEAR string), or null. */
export async function playBalance(account: string): Promise<string | null> {
  try {
    const a = (await provider().viewAccount({ accountId: account })) as unknown as { amount: string | bigint };
    return String(a.amount);
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------- quick punch (NEAR wallet users)
// A wallet popup per punch is too slow for a fight. The wallet sends NEAR once to this device's *implicit* account
// (the 64-hex account NEAR derives from the device key: it exists as soon as it receives NEAR, no relayer needed).
// From then on every punch is signed here instantly. "Cash out" sends the coins and all NEAR back to the wallet and
// deletes the quick account.

/** True for a 64-hex implicit account (the quick-punch account), false for `<name>.nmeplay.testnet`. */
export const isQuick = (a: string | null | undefined): boolean => !!a && /^[0-9a-f]{64}$/.test(a);

/** This device's quick-punch account id (made from the device key). */
export function quickAccount(): string {
  const pk = KeyPair.fromString(deviceKey().sk as KeyPairString).getPublicKey();
  return Array.from(pk.data, (b) => b.toString(16).padStart(2, "0")).join("");
}

const GAS30 = teraToGas(30);
const YOCTO1 = 1n;
const REG = 1_250_000_000_000_000_000_000n; // 0.00125 NEAR, NEP-145 registration

async function sendFrom(account: string, receiverId: string, acts: ReturnType<typeof actions.transfer>[]): Promise<void> {
  const s = read();
  if (!s) throw new Error("no key on this device");
  const acc = new Account(account, provider(), KeyPairSigner.fromSecretKey(s.sk as KeyPairString));
  const out = (await acc.signAndSendTransaction({ receiverId, actions: acts })) as { status?: unknown };
  const st = out?.status;
  if (st && typeof st === "object" && "Failure" in st) throw new Error(`the transaction failed: ${JSON.stringify(st.Failure).slice(0, 200)}`);
}

async function viewOf<T>(contractId: string, method: string, args: Record<string, unknown>): Promise<T | null> {
  try {
    const r = (await provider().callFunction({ contractId, method, args })) as T;
    return r ?? null;
  } catch {
    return null;
  }
}

/** Sends every coin the quick account holds, then all its NEAR, back to `wallet`, and deletes the quick account. */
export async function quickCashOut(wallet: string, tokens: string[], onStep?: (t: string) => void): Promise<void> {
  const me = quickAccount();
  for (const token of tokens) {
    const bal = await viewOf<string>(token, "ft_balance_of", { account_id: me });
    if (!bal || BigInt(bal) === 0n) continue;
    onStep?.(`Sending your ${token.split(".")[0]} back…`);
    const reg = await viewOf<unknown>(token, "storage_balance_of", { account_id: wallet });
    const acts = [];
    if (!reg) acts.push(actions.functionCall("storage_deposit", { account_id: wallet, registration_only: true }, GAS30, REG));
    acts.push(actions.functionCall("ft_transfer", { receiver_id: wallet, amount: bal }, GAS30, YOCTO1));
    await sendFrom(me, token, acts);
  }
  onStep?.("Sending your NEAR back…");
  await sendFrom(me, me, [actions.deleteAccount(wallet)]);
  if (read()?.account === me) write({ account: null, sk: deviceKey().sk });
}

/** Free test NEAR for a low Google sign-in account: the phone signs a dated request with its own key (no popup). */
export async function playTopup(): Promise<{ sent?: string; error?: string }> {
  const s = read();
  if (!s?.account) return { error: "not signed in" };
  const kp = KeyPair.fromString(s.sk as KeyPairString);
  const ts = Date.now();
  const sig = kp.sign(new TextEncoder().encode(`nme-topup:${s.account}:${ts}`)).signature;
  const r = await fetch(`${NME.relayerUrl}/play/topup`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ account: s.account, public_key: kp.getPublicKey().toString(), ts, signature: btoa(String.fromCharCode(...sig)) }),
    signal: AbortSignal.timeout(60_000),
  });
  const j = (await r.json().catch(() => ({}))) as { sent?: string; error?: string };
  if (!r.ok) return { error: j.error ?? `HTTP ${r.status}` };
  return j;
}
