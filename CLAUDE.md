# NME — Near-Meme Experience

A programmable memecoin arena on NEAR where any meme coin — ours or anyone's on Rhea — fights in the Colosseum, with AI brains that talk trash, take paid jobs and defend the price with capped buybacks, a PumpBag that holds real money, intents that let you ape from other chains, and a machine that runs itself with no admin — so supply and treasuries move with the community, and buying and selling stay completely free. *Launchpads create memes. NME makes them fight, think, work and defend themselves.* Memes are agents: they think (NEAR AI), earn from jobs, defend their price with capped TEE-attested buybacks, and can be aped from other chains by typing an intent. (Core redefined by docs/11-CORE-PIVOT.md and upgraded by docs/12-AGENT-MEMES.md.)

Built for NEAR HACKATHON (nearhackathon.com). Primary track: **Memecoins With Mechanics**. Also fits: **NFTs That Do Something**, AI Security & Defense, Social Money, and Privacy once Sealed War Bets ship. Network: **testnet-first**.

---

## Prime directive

**The README may only claim what works end-to-end.** Hackathon screening is done first by Claude Code / Codex agents that clone the repo, follow the README, and check every claim. One flawless loop beats ten half-built features. Build the core loop first (docs/06-BUILD-PLAN.md, Phase 1), make it bulletproof, then extend.

## Working principles (from the project owner)

- Treat every run as if it will succeed: consult the best available information (skills, docs MCP, source code of the dependency) **before** acting, rather than launching an attempt you expect to throw away.
- Learn from failures; don't budget for them. This governs effort posture, not architecture — systems still get full error handling, retries, rollbacks and failure paths.
- Keep explanations brief and mechanism-level. No filler.
- When a fact is unverified, say so and verify it (Phase 0 spikes exist for this). Never invent contract method names, account IDs or API shapes.

## Invariants — every coin, every contract, no exceptions

| ID | Invariant | Enforced where |
|---|---|---|
| I1 | `ft_transfer`, buying and selling can never be blocked, taxed by the token, or paused | nme-coin has no pause/blacklist/tax code paths; fees exist only in the optional arena router |
| I2 | No admin key. All parameters fixed at creation and publicly readable | No owner/guardian methods; coin accounts are created with **no access keys**; `get_config()` views |
| I3 | Treasuries can only **buy and burn** their own coin, or pay **one mercenary job per war** (≤ 0.5% of the treasury, decided by a fixed contract rule, never by AI; the payment buys and burns the hired coin; a failed job refunds the treasury; owner decision 2026-11-12, mercenary.rs). War chests can only **buy the winner**: half of what a won war buys is burned, half is shared by that war's winning fighters by their war score (Spoils of war, owner decision 2026-11-11; unclaimed after 7 days is burned). Never sell, never withdraw. The graveyard account `burn.NME_ROOT` has no keys and no way out | nme-arena ledgers (treasury, chest) have no withdraw path; only buyback→burn, Colosseum chest→buy→50% burn / 50% `claim_spoils` (spoils.rs), and native `burn` or transfer to the graveyard |
| I4 | The dev allocation lives in the PumpBag vault under vesting, never in a free wallet | nme-coin mints dev share to `crown.<coin>` vault at launch |
| I5 | External inputs (cross-chain deposits, real-world events, X counts) are accepted only if produced by an attested TEE (OutLayer) | Only via OutLayer `request_execution` yield/resume callbacks; never from an EOA |
| I6 | Every mint, burn, trade, war and crown action emits an NEP-297 event (`standard: "nme"`) and is appended to an on-chain feed ring buffer | `nme-common::events` |
| I7 | Every due transition is executable by **anyone**, gives the same result whoever calls it, and never needs a privileged caller. Pokers choose timing only, never outcomes | `tick()` in nme-arena; no transition method checks the caller except the trust-graph calls (docs/10-AUTONOMY.md) |
| I8 | AI (brains, Operator) can only (a) append to the commentary buffer and deliver job results, and (b) trigger **buy & burn of its own coin** from that coin's treasury/chest, within immutable per-coin caps, and only via a **TEE-attested decision** (OutLayer callback, I5). AI can never withdraw, transfer to arbitrary accounts, sell, change parameters or touch another coin's funds. Users always sign their own trades (Intent Memes never auto-execute) | `post_commentary` is append-only; `deliver`/defense decisions are accepted only from the OutLayer yield/resume callback of a request the arena itself made; caps are immutable per coin and checked in the callback; no value path exists for the brain account (docs/12-AGENT-MEMES.md §2) |

If a feature request conflicts with an invariant, stop and flag it.

## Read order

1. `docs/01-PRODUCT.md` — what we're building and why people use it
2. `docs/02-ARCHITECTURE.md` — system map, accounts, flows, decisions log
3. `docs/03-CONTRACTS.md` — contract-by-contract spec (state, methods, events, security)
4. `docs/04-OFFCHAIN.md` — OutLayer workers, relayer, keeper, feed, images
5. `docs/05-FRONTEND.md` — UI spec + wallet integration (NEAR, Solana, EVM)
6. `docs/06-BUILD-PLAN.md` — Phase 0 spikes → Phase 1 core → Phase 2+, with acceptance tests
7. `docs/07-TOOLING.md` — skills, MCP servers, llms.txt, toolchain
8. `docs/08-RESEARCH-NOTES.md` — verified facts with sources; deprecated things; open unknowns
9. `docs/09-SUBMISSION.md` — hackathon rules, README template, demo script
10. `docs/10-AUTONOMY.md` — autonomy layer: permissionless `tick`, inline ticks, bounties, the NME Operator agent (add-on spec, owner-approved)
11. `docs/11-CORE-PIVOT.md` — **changes the Phase 1 core**: war-first Colosseum open to every Rhea meme, minimal single-sided launch, brains, widget; moves buy-from-any-chain to Phase 2 (add-on spec, owner-approved; where it conflicts with 01–09, it wins)
12. `docs/12-AGENT-MEMES.md` — **upgrades the core again**: Working Memes (hire the brain, payment buys & burns), Self-Defending Memes (AI timing, contract caps), Intent Memes ("ape $PEPE with 1 SOL"; Solana devnet in core); replaces I8 (add-on spec, owner-approved; where it conflicts with 01–11, it wins)
13. `HACKATHON.md` (official rules, if present in repo root) is the source of truth for submission rules.

## Network constants (testnet)

| Thing | ID | Status |
|---|---|---|
| NME root account | `nme.testnet` (fallback `nearmeme.testnet`) — read from env `NME_ROOT` | created (S1) |
| Wrapped NEAR | `wrap.testnet` | verified (Ref SDK testnet config) |
| Rhea / Ref exchange | `ref-finance-101.testnet` | verified (Ref SDK testnet config; meme.cooking testnet env) |
| Ref testnet indexer | `https://testnet-indexer.ref-finance.com` | verified (Ref SDK config) |
| OutLayer contract | `outlayer.testnet` | verified (OutLayer README / llms.txt) |
| OutLayer HTTPS API | `https://testnet-api.outlayer.ai` | verified |
| Chain Signatures MPC | `v1.signer-prod.testnet` | verified (near-smart-contracts skill, chain-signatures rule) |
| RPC | `https://rpc.testnet.fastnear.com` (alt: `https://test.rpc.fastnear.com`) | verified |
| meme.cooking (testnet) | factory `factory.v10.meme-cooking.testnet`, API `https://api-testnet.meme.cooking` | verified (their public frontend env) |
| Solana | devnet RPC (choose provider in S6) | core (C5 Intent Memes, Solana-only gateway) |
| Rhea DCL | mainnet `dclv2.ref-labs.near`; testnet contract unknown | to find (S12) |

Mainnet equivalents (later): `wrap.near`, `v2.ref-finance.near`, `outlayer.near`, `v1.signer`, `intents.near` (Intents has **no** testnet).

## Repo layout (target)

```
nme/
  CLAUDE.md  PROMPT.md  README.md  LICENSE  HACKATHON.md
  docs/                      # this spec
  contracts/                 # cargo workspace (near-sdk 5.x)
    nme-common/              # shared types, events, feed ring buffer
    nme-factory/             # at NME_ROOT: creates coins
    nme-coin/                # per-coin NEP-141 + minimal single-sided launch (global contract code)
    nme-crown/               # PumpBag NFT collection (NEP-171/177/178)
    nme-crown-vault/         # per-coin vault account code (global contract code)
    nme-arena/               # router + treasuries/chests + Colosseum + jobs (Working Memes) + defense (Self-Defending) + feed/commentary
    nme-gateway/             # cross-chain deposits (Chain Signatures + OutLayer): Solana-only minimal in core (C5); EVM/Hyperliquid next
  workers/                   # OutLayer WASI projects (Rust, wasm32-wasip2)
    deposit-verifier/        # core (C5, M7b): Solana + EVM (Sepolia) adapters; Hyperliquid later
    event-oracle/            # Phase 2
    brain/                   # core (C4): NEAR AI Cloud — commentary + Working Memes jobs (TEE path for deliveries)
    defense/                 # core (C3): TEE defense decision worker (buy back or hold, capped by the contract)
  services/
    relayer/                 # TS: relays claim-deposit and wallet-signed actions (Solana/EVM, D-57); pays gas; prompt store; keeper fallback
    intent/                  # TS: NEAR AI intent parser (strict JSON schema) — parse never executes
  web/                       # Next.js (App Router) + React + TS (+ embeddable widget + intent chat box, C8)
  scripts/                   # deploy, seed, demo, simulate
  tests/                     # near-workspaces integration tests
```

## Coding rules (contracts)

- Rust, **near-sdk 5.x**, scaffold with `cargo near new` (never hand-write Cargo.toml for a new contract). Build with `cargo near build` (reproducible for final deploy).
- Use the `near-smart-contracts` skill for patterns and `near-contract-audit` skill to review every contract before deploy.
- Cross-contract calls: mutate state **before** the promise only if you also write the rollback in the callback; every callback is `#[private]` and handles `PromiseError`.
- Sensitive methods that move value require `assert_one_yocto()` (forces a full-access key, blocks function-call keys).
- Checked math everywhere (`checked_*` or `u128` with explicit overflow handling). Token amounts are `U128` in JSON.
- Storage: NEP-145 for token accounts; measure `env::storage_usage()` deltas and charge the caller; refund excess.
- Yield/resume timeout is ~200 blocks (~4 min). Every OutLayer call must have a timeout branch that leaves state consistent.
- Events: NEP-297 `EVENT_JSON:` logs with `standard: "nme"`, `version: "1.0.0"`; also append to the feed ring buffer (see 03-CONTRACTS).
- No `#[private]` misuse, no `unwrap()` on external input, no admin/owner fields.

## Coding rules (frontend/services)

- Wallets: `@hot-labs/near-connect` + `near-connect-hooks` (wallet-selector is legacy). View calls via `near-api-js` `JsonRpcProvider`.
- Solana: `@solana/wallet-adapter-*` on devnet (Phantom first).
- Never ship private keys to the browser except function-call keys created by `function-call-key-plugin` (Phase 2).
- TypeScript strict mode. Every network call has timeout + retry + user-visible error state.

## Do NOT

- Copy code from meme.cooking's frontend repo (no license = all rights reserved). Reading their public API is fine.
- Use Shade Agent Framework (deprecated, unmaintained after 2026-04-19).
- Use NEAR Lake indexer (deprecated).
- Assume NEAR Intents exists on testnet (it doesn't). Testnet cross-chain = Chain Signatures + OutLayer.
- Add any feature that can block, tax or pause transfers.
- Claim a feature in README/demo that is not reproducible from a clean clone.

## Definition of done (per milestone)

1. Contracts: unit tests + near-workspaces integration test for the happy path **and** every failure branch listed in 03-CONTRACTS.
2. Deployed to testnet by a script in `scripts/` (idempotent).
3. A "do X → see Y" block added to README, executed from a clean clone.
4. `near-contract-audit` skill pass with findings fixed or documented.
5. Feed shows the milestone's events in the UI.
