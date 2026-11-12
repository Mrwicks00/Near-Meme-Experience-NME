import { Account, JsonRpcProvider, KeyPairSigner, teraToGas, type KeyPairString } from "near-api-js";

/** The public RPC rate-limits busy IPs with HTTP 429 / JSON-RPC -429. That is "try again shortly", not a refusal. */
export const RATE_LIMITED = /\b-?429\b|rate limit|too many requests/i;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** What the relayer needs from NEAR. Injected so the logic is testable without a network. */
export interface Chain {
  view<T>(contract: string, method: string, args: object): Promise<T>;
  /** Signs with the relayer account and waits for the transaction; the deposit is paid by the relayer. */
  call(contract: string, method: string, args: object, gasTgas: number, depositYocto?: bigint): Promise<{ txHash: string; failure?: string; logs?: string[] }>;
}

export class NearChain implements Chain {
  private provider: JsonRpcProvider;
  private account: Account;
  private queue: Promise<unknown> = Promise.resolve();

  constructor(rpc: string, accountId: string, key: string) {
    this.provider = new JsonRpcProvider({ url: rpc });
    this.account = new Account(accountId, this.provider, KeyPairSigner.fromSecretKey(key as KeyPairString));
  }

  async view<T>(contract: string, method: string, args: object): Promise<T> {
    let last: unknown;
    for (let i = 0; i < 7; i++) {
      try {
        return (await this.provider.callFunction({ contractId: contract, method, args: args as Record<string, unknown> })) as T;
      } catch (e) {
        last = e;
        const msg = String(e) + String((e as any)?.cause);
        const limited = RATE_LIMITED.test(msg);
        if (!limited && !/fetch failed|ETIMEDOUT|ECONNRESET|timed? ?out|50[234]/i.test(msg)) throw e; // contract errors are not retried
        if (!limited && i >= 3) break;
        await sleep(limited ? 1500 * 2 ** Math.min(i, 4) : 400 * (i + 1)); // rate limit: 1.5 s, 3 s, 6 s, 12 s, 24 s…
      }
    }
    throw last;
  }

  /** One transaction at a time: a single access key must not race its own nonce. */
  call(contract: string, method: string, args: object, gasTgas: number, depositYocto = 0n) {
    const run = async () => {
      // callFunction returns the contract's return value (null for most methods); the outcome (hash, status) is in callFunctionRaw.
      let out: any;
      // A network error before the node answered is retried: a repeated signed action is safe (the contract refuses a used nonce).
      for (let i = 0; ; i++) {
        try {
          out = await this.account.callFunctionRaw({ contractId: contract, methodName: method, args: args as Record<string, unknown>, gas: teraToGas(gasTgas), deposit: depositYocto });
          break;
        } catch (e) {
          const msg = String((e as any)?.message ?? e) + String((e as any)?.cause);
          if (i < 3 && /fetch failed|ETIMEDOUT|ECONNRESET/i.test(msg)) {
            await sleep(800 * (i + 1));
            continue;
          }
          // Rate limited before the node took the transaction: wait and send it again (a repeat is safe, see above).
          if (i < 6 && RATE_LIMITED.test(msg)) {
            await sleep(1500 * 2 ** Math.min(i, 4));
            continue;
          }
          return { txHash: "", failure: String((e as any)?.message ?? e).slice(0, 300) };
        }
      }
      const txHash: string = out?.transaction?.hash ?? out?.transaction_outcome?.id ?? "";
      if (!txHash) return { txHash, failure: "the transaction outcome had no hash" };
      const st = out?.status;
      const failure = st && typeof st === "object" && "Failure" in st ? JSON.stringify(st.Failure).slice(0, 300) : undefined;
      const logs: string[] = (out?.receipts_outcome ?? []).flatMap((r: any) => r?.outcome?.logs ?? []);
      return { txHash, failure, logs };
    };
    const p = this.queue.then(run, run);
    this.queue = p.catch(() => undefined);
    return p;
  }
}
