// Google sign-in accounts: this browser holds the account's own ed25519 key (made here, never sent anywhere) and signs
// transactions directly, so every action is one tap: no wallet app, no extension, no popup.
// The relayer created `<name>.nmeplay.testnet` with this key after checking the Google sign-in.
import { Account, JsonRpcProvider, KeyPair, KeyPairSigner, teraToGas, type KeyPairString } from "near-api-js";
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
