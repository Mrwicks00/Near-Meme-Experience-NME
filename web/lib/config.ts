// Testnet accounts and endpoints. Every value can be overridden with NEXT_PUBLIC_* env vars.
const env = (k: string, d: string) => (process.env[k] && process.env[k]!.length > 0 ? process.env[k]! : d);

export const NME = {
  network: "testnet" as const,
  factory: env("NEXT_PUBLIC_NME_ROOT", "nme.testnet"),
  arena: env("NEXT_PUBLIC_NME_ARENA", "arena.nme.testnet"),
  crown: env("NEXT_PUBLIC_NME_CROWN", "crown.nme.testnet"),
  gateway: env("NEXT_PUBLIC_NME_GATEWAY", "gateway.nme.testnet"),
  wrap: "wrap.testnet",
  dcl: env("NEXT_PUBLIC_NME_DCL", "dclv2.ref-dev.testnet"),
  // Tried in order; the first one that answers wins until it fails.
  rpcs: env("NEXT_PUBLIC_NEAR_RPCS", "https://test.rpc.fastnear.com,https://rpc.testnet.fastnear.com,https://rpc.testnet.near.org")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean),
  // Both services are reached through this site's own /api/svc pass-through (server env INTENT_URL / RELAYER_URL).
  intentUrl: env("NEXT_PUBLIC_INTENT_URL", "/api/svc/intent"),
  relayerUrl: env("NEXT_PUBLIC_RELAYER_URL", "/api/svc/relayer"),
  explorer: "https://testnet.nearblocks.io",
  // NEAR Social's data contract (SocialDB) and a testnet front end for reading posts.
  social: env("NEXT_PUBLIC_SOCIAL", "v1.social08.testnet"),
  socialUi: "https://test.near.social",
  // Average testnet block time, for showing a raid's time left (the contract counts blocks).
  blockMs: 573,
  feedPollMs: 3000,
};

export const COIN_DECIMALS = 18;
export const NEAR_DECIMALS = 24;
