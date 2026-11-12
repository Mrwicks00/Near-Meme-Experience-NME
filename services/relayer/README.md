# NME relayer

A courier for wallet users who have no NEAR account (D-57). It submits what the wallet signed and pays the gas from `nme-relayer.testnet`.
It can choose **timing only**: the gateway contract rebuilds the signed text from the call arguments, verifies the signature itself and
decides the outcome, so a relayer can neither forge, alter nor replay an action (I7). Anyone can run one.

    cd services/relayer && npm install && npm test      # 15 tests, no network
    RELAYER_KEY=ed25519:... npm start                    # http://localhost:8787

| Endpoint | What it does |
|---|---|
| `POST /message` `{owner, action, nonce, expires_ms}` | the exact text the wallet must sign (a gateway view) |
| `POST /action` `{owner, action, nonce, expires_ms, signature}` | submits `gateway.wallet_action` (FundChest, TransferNear, Hire, Buy, JoinWar, Sell, TransferTokens) |
| `POST /claim` `{owner, tx_id}` | a Phantom / MetaMask deposit credits the wallet (`claim_wallet_deposit`) |
| `POST /claim_deposit` `{tx_id, beneficiary, intent}` | NEAR-beneficiary flow (replaces `scripts/relay_claim.py`): claim, then `execute_claim` once verified |
| `POST /execute_claim` `{tx_id}` | spends an already verified deposit |
| `GET /wallet/<owner>?token=<account>` | NEAR balance, nonce, running token operation, token balance |
| `GET /config`, `GET /health` | |

Before it spends gas the relayer checks: request shape, expiry window, nonce, the signature (ed25519 / EIP-191, locally), enough NEAR or
tokens, no running token operation (the gateway runs one at a time), the coin exists; for a Buy it registers the gateway on the token
(0.00125 NEAR, once per token), for a Sell on wNEAR. Deposit claims are checked on the source chain first (Solana devnet finalized and
successful, Sepolia receipt status 1) so a made-up transaction id costs nothing. Requests that are certain to fail are refused for free and
do not count against the rate limits (per wallet per minute and per day, per IP, global; see `.env.example`).

**Limits (honest):** one process, in-memory rate limits (a restart resets them); one signing key, transactions are sent one at a time;
the relayer pays gas for buys, sells and token transfers because the relay fee is never charged on those (D-57); the source-chain
pre-check protects the relayer path only, the contract can still be called directly by anyone.
