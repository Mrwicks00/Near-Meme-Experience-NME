//! nme-gateway (M7 core, Solana devnet only): turns SOL that someone sent to a Chain-Signatures deposit address into a
//! NEAR action for a NEAR beneficiary: buy a coin through the arena, or fund its war chest. docs/03, docs/12 §5.
//!
//! Flow: `claim_deposit(tx_id, beneficiary, intent)` (anyone) -> derive the deposit address from the MPC contract
//! (path commits to the beneficiary AND the intent) -> OutLayer runs the pinned deposit-verifier worker in a TEE
//! (finalized Solana transfer to exactly that address + two agreeing price sources) -> `on_verified` pays the NEAR
//! value (minus a spread) out of the reserve into the arena for the beneficiary. Nothing here is a privileged key:
//! no admin, no withdraw; the reserve only ever leaves into `arena.buy_for` / `arena.fund_chest` (I3/I5/I8).
mod walletsig;

use near_sdk::json_types::{Base64VecU8, U128};
use near_sdk::serde::Deserialize;
use near_sdk::serde_json::{self, json};
use near_sdk::{bs58, env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};
use walletsig::Owner;

const DERIVE_GAS: Gas = Gas::from_tgas(15);
const REQUEST_GAS: Gas = Gas::from_tgas(100);
const INTENT_GAS: Gas = Gas::from_tgas(250);
const BUY_GAS: Gas = Gas::from_tgas(250);
const RETRY_AFTER_MS: u64 = 15 * 60 * 1000;

/// What the deposit is for. The hash of this (with the beneficiary) is part of the deposit address, so a deposit made
/// for one intent can never be spent on another.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum DepositIntent {
    /// Buy `ticker` through the arena; the tokens go to the beneficiary. `min_out` is enforced by the pool.
    Buy { ticker: String, min_out: U128 },
    /// Put the value into `ticker`'s war chest (it can only ever buy and burn).
    FundChest { ticker: String },
}

impl DepositIntent {
    fn canonical(&self) -> String {
        match self {
            DepositIntent::Buy { ticker, min_out } => format!("buy:{ticker}:{}", min_out.0),
            DepositIntent::FundChest { ticker } => format!("chest:{ticker}"),
        }
    }
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum ClaimStatus {
    /// The verifier is running (a stuck claim can be retried after 15 minutes).
    Pending { since_ms: u64 },
    /// The deposit is verified and the NEAR value is set aside from the reserve; `execute_claim` spends it (own transaction).
    Verified { near_pay: U128, lamports: u64, beneficiary: AccountId, intent: DepositIntent },
    Done { near_paid: U128, lamports: u64 },
    /// Nothing was paid; the SOL stays at the deposit address (core has no refund path).
    Rejected { reason: String },
    /// A wallet deposit (D-57): the NEAR value was credited to the wallet owner's gateway balance.
    Credited { owner: String, near_paid: U128, raw_amount: U128 },
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// Kept from the NEAR value of every deposit (covers price error and the reserve's float).
    pub spread_bps: u16,
    /// One claim never pays more than this.
    pub max_claim_near: U128,
    /// Deposits worth less than this are rejected (dust).
    pub min_claim_near: U128,
}

/// Where the verifier worker is: pinned by hash (I5).
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct WorkerSource {
    pub url: String,
    pub hash: String,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct VerifierOut {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: String,
    #[serde(default)]
    tx_id: String,
    #[serde(default)]
    expected_to: String,
    #[serde(default)]
    lamports: u64,
    /// The deposit in the chain's smallest unit (lamports, wei) as a decimal string; `lamports` is kept for the Solana path.
    #[serde(default)]
    raw_amount: String,
    #[serde(default)]
    near_yocto: String,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    outlayer: AccountId,
    mpc: AccountId,
    arena: AccountId,
    worker: WorkerSource,
    cfg: GatewayConfig,
    reserve_near: u128,
    claims: near_sdk::store::LookupMap<String, ClaimStatus>,
    /// Wallet owners' NEAR balances (D-57), keyed `sol:<pubkey>` / `evm:0x<address>`. Inflow: verified deposits. Outflow: only a
    /// message signed by the owner (fund a chest, send to a NEAR account) and the fixed relay fee that signature pays.
    near_bal: near_sdk::store::LookupMap<String, u128>,
    /// Next valid nonce per owner (sequential, so a signed message can be used exactly once).
    nonces: near_sdk::store::LookupMap<String, u64>,
    /// Paid from the signer's balance to whoever submits a signed action (anyone may; no privileged relayer).
    relay_fee: u128,
    /// job id -> who hired through the gateway, and whether a refund was already credited (D-57 Hire).
    hires: near_sdk::store::LookupMap<u64, HireRec>,
    /// wNEAR (`wrap.testnet`): wallet sales are paid to the gateway in wNEAR so the proceeds can be measured exactly.
    wrap: AccountId,
    /// Wallet owners' token balances, keyed `<owner>|<token account>`: filled by wallet buys (reported by the arena), emptied by a
    /// signed sale or token transfer.
    tok_bal: near_sdk::store::LookupMap<String, u128>,
    /// wNEAR received from a sale that is not yet unwrapped into the owner's NEAR balance (normally zero; `retry_unwrap` finishes it).
    wnear_due: near_sdk::store::LookupMap<String, u128>,
    /// One token operation at a time (buy, sale, token transfer): the tokens and the wNEAR a sale brings are measured as balance
    /// differences of this account, which is exact only while nothing else touches those balances. It times out, so a stuck chain
    /// can block wallet token actions for at most `TOKEN_LOCK_MS`.
    lock: Option<TokenLock>,
    /// Coins launched through the gateway for a wallet (D-59), by ticker: who asked and what was paid to the factory.
    launches: near_sdk::store::LookupMap<String, LaunchRec>,
    /// PumpBags (crown NFTs) the gateway holds for a wallet owner, by ticker (= crown token id). The owner's signature is the only
    /// way anything leaves: fees and vested tokens are credited to the owner's ledger, the NFT can be sent to a NEAR account.
    crowns: near_sdk::store::LookupMap<String, String>,
    /// The NME factory (the root account: coins are `<ticker>.<factory>`) and the PumpBag collection, fixed at deployment.
    factory: AccountId,
    crown: AccountId,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct LaunchRec {
    owner: String,
    /// What the factory was paid (0 until `create_coin` has answered).
    paid: u128,
    /// The ledger already got back what the factory returned (a rolled-back creation, or a failed launch).
    settled: bool,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct TokenLock {
    pub owner: String,
    pub until_ms: u64,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct HireRec {
    owner: String,
    credited: bool,
}

/// What a signed message can do (D-57). The canonical text of each is in `WalletAction::canonical`.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum WalletAction {
    /// Put `amount` of the owner's NEAR into `ticker`'s war chest (chests only ever buy and burn, I3).
    FundChest { ticker: String, amount: U128 },
    /// Send `amount` of the owner's NEAR to a NEAR account.
    TransferNear { to: AccountId, amount: U128 },
    /// Hire `ticker`'s brain (Working Memes). `amount` is the payment (at least the coin's job price). The prompt is bound to the
    /// signature by its hash. A refused or failed job refunds the escrow to the gateway; `claim_hire_refund` credits it to the owner.
    Hire { ticker: String, kind: String, target: String, prompt: String, amount: U128 },
    /// Buy `key` (a ticker, or a token account for an outside coin) with `amount` of the owner's NEAR. The tokens are credited to the
    /// owner's token balance here. Needs the gateway to be registered on the token (`register_token`). No relay fee (D-57: never on buys).
    Buy { key: String, amount: U128, min_out: U128 },
    /// The same buy, but only if a war is open for the coin: the owner scores as their own fighter.
    JoinWar { key: String, amount: U128, min_out: U128 },
    /// Sell `amount` tokens of the owner's balance through the arena; the NEAR proceeds are credited to the owner's NEAR balance.
    /// No relay fee (D-57: never on sells).
    Sell { token: AccountId, amount: U128, min_out: U128 },
    /// Send `amount` tokens of the owner's balance to a NEAR account (which must be registered on the token). No relay fee.
    TransferTokens { token: AccountId, to: AccountId, amount: U128 },
    /// Launch a coin (D-59). `args_json` is the exact `create_coin` args object; the signed text carries its sha256. `deposit` is what the
    /// owner pays from the NEAR balance: at least the factory's `required_deposit`, and the gateway attaches exactly that (the rest stays).
    CreateCoin { args_json: String, deposit: U128 },
    /// Claim the PumpBag's accrued pool fees into the owner's NEAR balance (not after a forfeit: those go to the coin's treasury).
    CrownClaimFees { ticker: String },
    /// Claim the PumpBag's vested dev tokens into the owner's token balance.
    CrownClaimVested { ticker: String },
    /// "I am still here": restarts the PumpBag's inactivity clock.
    CrownHeartbeat { ticker: String },
    /// Send the PumpBag NFT to a NEAR account. After this the gateway no longer holds anything for the owner.
    CrownSend { ticker: String, to: AccountId },
}

impl WalletAction {
    fn canonical(&self) -> String {
        match self {
            WalletAction::FundChest { ticker, amount } => format!("fund_chest:{ticker}:{}", amount.0),
            WalletAction::TransferNear { to, amount } => format!("transfer_near:{to}:{}", amount.0),
            WalletAction::Hire { ticker, kind, target, prompt, amount } => {
                let h: String = env::sha256(prompt.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
                format!("hire:{ticker}:{kind}:{target}:{h}:{}", amount.0)
            }
            WalletAction::Buy { key, amount, min_out } => format!("buy:{key}:{}:{}", amount.0, min_out.0),
            WalletAction::JoinWar { key, amount, min_out } => format!("join_war:{key}:{}:{}", amount.0, min_out.0),
            WalletAction::Sell { token, amount, min_out } => format!("sell:{token}:{}:{}", amount.0, min_out.0),
            WalletAction::TransferTokens { token, to, amount } => format!("transfer_tokens:{token}:{to}:{}", amount.0),
            WalletAction::CreateCoin { args_json, deposit } => {
                let h: String = env::sha256(args_json.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
                format!("create_coin:{h}:{}", deposit.0)
            }
            WalletAction::CrownClaimFees { ticker } => format!("crown_claim_fees:{ticker}"),
            WalletAction::CrownClaimVested { ticker } => format!("crown_claim_vested:{ticker}"),
            WalletAction::CrownHeartbeat { ticker } => format!("crown_heartbeat:{ticker}"),
            WalletAction::CrownSend { ticker, to } => format!("crown_send:{ticker}:{to}"),
        }
    }
    /// PumpBag actions move no NEAR or tokens out of the owner's balances, so they carry no amount.
    fn zero_amount_ok(&self) -> bool {
        matches!(self, WalletAction::CrownClaimFees { .. } | WalletAction::CrownClaimVested { .. } | WalletAction::CrownHeartbeat { .. } | WalletAction::CrownSend { .. })
    }
    /// The action's main amount (NEAR or tokens): must be above zero.
    fn amount(&self) -> u128 {
        match self {
            WalletAction::FundChest { amount, .. }
            | WalletAction::TransferNear { amount, .. }
            | WalletAction::Hire { amount, .. }
            | WalletAction::Buy { amount, .. }
            | WalletAction::JoinWar { amount, .. }
            | WalletAction::Sell { amount, .. }
            | WalletAction::TransferTokens { amount, .. } => amount.0,
            WalletAction::CreateCoin { deposit, .. } => deposit.0,
            _ => 0,
        }
    }
    /// NEAR taken from the owner's NEAR balance (token actions spend tokens, not NEAR).
    fn near_spend(&self) -> u128 {
        match self {
            WalletAction::Sell { .. } | WalletAction::TransferTokens { .. } => 0,
            _ => self.amount(),
        }
    }
    /// The relay fee is charged on NEAR-balance actions only; never on buys, sells or token transfers (D-57).
    fn takes_relay_fee(&self) -> bool {
        matches!(self, WalletAction::FundChest { .. } | WalletAction::TransferNear { .. } | WalletAction::Hire { .. } | WalletAction::CreateCoin { .. } | WalletAction::CrownHeartbeat { .. } | WalletAction::CrownSend { .. })
    }
}

/// The layout deployed on 2026-11-09 (wallet balances and token ledger), read once by `migrate_v3`.
mod v2 {
    use super::*;

    #[near(serializers = [borsh])]
    pub struct Contract {
        pub outlayer: AccountId,
        pub mpc: AccountId,
        pub arena: AccountId,
        pub worker: WorkerSource,
        pub cfg: GatewayConfig,
        pub reserve_near: u128,
        pub claims: near_sdk::store::LookupMap<String, ClaimStatus>,
        pub near_bal: near_sdk::store::LookupMap<String, u128>,
        pub nonces: near_sdk::store::LookupMap<String, u64>,
        pub relay_fee: u128,
        pub hires: near_sdk::store::LookupMap<u64, HireRec>,
        pub wrap: AccountId,
        pub tok_bal: near_sdk::store::LookupMap<String, u128>,
        pub wnear_due: near_sdk::store::LookupMap<String, u128>,
        pub lock: Option<TokenLock>,
    }
}

const TOKEN_LOCK_MS: u64 = 3 * 60 * 1000;
const MAX_REGISTRATION: u128 = 10_000_000_000_000_000_000_000; // 0.01 NEAR
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const MAX_RELAY_FEE: u128 = 10_000_000_000_000_000_000_000; // 0.01 NEAR
const MAX_SIGNATURE_AGE_MS: u64 = 24 * 60 * 60 * 1000;

#[near]
impl Contract {
    #[init]
    pub fn new(outlayer: AccountId, mpc: AccountId, arena: AccountId, worker: WorkerSource, cfg: GatewayConfig, relay_fee: U128, wrap: Option<AccountId>, factory: AccountId, crown: AccountId) -> Self {
        require!(relay_fee.0 <= MAX_RELAY_FEE, "relay fee above 0.01 NEAR");
        require!(cfg.spread_bps <= 1_000, "spread above 10%");
        require!(cfg.min_claim_near.0 < cfg.max_claim_near.0, "min claim must be below max claim");
        require!(worker.hash.len() == 64 && worker.hash.chars().all(|c| c.is_ascii_hexdigit()), "worker hash must be a sha256 in hex");
        let wrap = wrap.unwrap_or_else(|| default_wrap(&arena));
        Self { outlayer, mpc, arena, worker, cfg, reserve_near: 0, claims: near_sdk::store::LookupMap::new(b"c"), near_bal: near_sdk::store::LookupMap::new(b"b"), nonces: near_sdk::store::LookupMap::new(b"n"), relay_fee: relay_fee.0, hires: near_sdk::store::LookupMap::new(b"h"), wrap, tok_bal: near_sdk::store::LookupMap::new(b"t"), wnear_due: near_sdk::store::LookupMap::new(b"w"), lock: None, launches: near_sdk::store::LookupMap::new(b"l"), crowns: near_sdk::store::LookupMap::new(b"k"), factory, crown }
    }

    /// One-time upgrade from the 2026-11-09 layout: adds the launch and PumpBag records (D-59). Everything else is kept as it is.
    #[private]
    #[init(ignore_state)]
    pub fn migrate_v3(factory: AccountId, crown: AccountId) -> Self {
        let old: v2::Contract = env::state_read().unwrap_or_else(|| env::panic_str("no v2 state to migrate"));
        Self {
            outlayer: old.outlayer, mpc: old.mpc, arena: old.arena, worker: old.worker, cfg: old.cfg, reserve_near: old.reserve_near, claims: old.claims,
            near_bal: old.near_bal, nonces: old.nonces, relay_fee: old.relay_fee, hires: old.hires, wrap: old.wrap, tok_bal: old.tok_bal, wnear_due: old.wnear_due,
            lock: old.lock, launches: near_sdk::store::LookupMap::new(b"l"), crowns: near_sdk::store::LookupMap::new(b"k"), factory, crown,
        }
    }

    /// Anyone adds NEAR to the reserve that pays out deposits. Inflow only: there is no withdraw.
    #[payable]
    pub fn fund_reserve(&mut self) {
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach NEAR");
        self.reserve_near += amount;
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "gateway_reserve_funded", "data": [{"by": env::predecessor_account_id(), "amount": U128(amount), "reserve": U128(self.reserve_near)}]})));
    }

    /// The derivation path of the deposit address for this beneficiary and intent. Derive the Solana address from it
    /// (`derived_public_key(path, gateway, domain 1)` on the MPC contract; base58 of the key is the address).
    pub fn deposit_path(&self, beneficiary: AccountId, intent: DepositIntent) -> String {
        Self::path_for(&beneficiary, &intent)
    }

    /// Permissionless (a relayer, the user, a bot). Checks the SOL deposit `tx_id` was sent to the address belonging to
    /// (beneficiary, intent); on success the NEAR value is spent for the beneficiary. One payout per Solana transaction.
    pub fn claim_deposit(&mut self, tx_id: String, beneficiary: AccountId, intent: DepositIntent) -> Promise {
        require!(tx_id.len() >= 64 && tx_id.len() <= 100 && tx_id.chars().all(|c| c.is_ascii_alphanumeric()), "tx_id must be a Solana signature (base58)");
        let key = format!("solana:{tx_id}");
        let now = env::block_timestamp_ms();
        match self.claims.get(&key) {
            Some(ClaimStatus::Done { .. }) | Some(ClaimStatus::Verified { .. }) => env::panic_str("this deposit was already paid out"),
            Some(ClaimStatus::Pending { since_ms }) if now.saturating_sub(*since_ms) < RETRY_AFTER_MS => env::panic_str("this claim is already being verified"),
            _ => {}
        }
        let ticker = match &intent {
            DepositIntent::Buy { ticker, .. } | DepositIntent::FundChest { ticker } => ticker.clone(),
        };
        require!(!ticker.is_empty() && ticker.len() <= 63, "bad ticker");
        self.claims.insert(key, ClaimStatus::Pending { since_ms: now });
        let path = Self::path_for(&beneficiary, &intent);
        Promise::new(self.mpc.clone())
            .function_call(
                "derived_public_key",
                json!({ "path": path, "predecessor": env::current_account_id(), "domain_id": 1 }).to_string().into_bytes(),
                NearToken::from_yoctonear(0),
                DERIVE_GAS,
            )
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(160)).with_unused_gas_weight(1).on_derived(tx_id, beneficiary, intent))
    }

    #[private]
    pub fn on_derived(&mut self, tx_id: String, beneficiary: AccountId, intent: DepositIntent) -> Option<Promise> {
        let Some(address) = env::promise_result_checked(0, 256)
            .ok()
            .and_then(|b| serde_json::from_slice::<String>(&b).ok())
            .and_then(|k| k.strip_prefix("ed25519:").map(|s| s.to_string()))
        else {
            self.reject(&tx_id, "could not derive the deposit address");
            return None;
        };
        let input = json!({ "chain": "solana", "tx_id": tx_id, "expected_to": address }).to_string();
        let args = json!({
            "source": { "WasmUrl": { "url": self.worker.url, "hash": self.worker.hash, "build_target": "wasm32-wasip2" } },
            "resource_limits": { "max_instructions": 2_000_000_000u64, "max_memory_mb": 64, "max_execution_seconds": 90 },
            "input_data": input,
            "secrets_ref": null,
            "response_format": "Text",
            "payer_account_id": env::current_account_id(),
        })
        .to_string();
        // OutLayer's unused deposit comes back to this contract; the fee is paid from the reserve's float (see `OUTLAYER_FEE`).
        Some(
            Promise::new(self.outlayer.clone())
                .function_call("request_execution", args.into_bytes(), OUTLAYER_FEE, REQUEST_GAS)
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(30)).with_unused_gas_weight(1).on_verified(tx_id, beneficiary, intent, address)),
        )
    }

    /// The result of the arena-independent verification (the callback of this contract's own `request_execution`, I5).
    #[private]
    pub fn on_verified(&mut self, tx_id: String, beneficiary: AccountId, intent: DepositIntent, address: String) -> bool {
        let parsed: Result<VerifierOut, String> = match env::promise_result_checked(0, 8192) {
            Err(_) => Err("the verifier run failed or timed out".into()),
            Ok(bytes) => serde_json::from_slice::<String>(&bytes)
                .map_err(|_| "unreadable verifier result".to_string())
                .and_then(|line| serde_json::from_str::<VerifierOut>(&line).map_err(|_| "the verifier returned no JSON".to_string())),
        };
        let out = match parsed {
            Ok(o) if o.ok => o,
            Ok(o) => return self.reject(&tx_id, &format!("not verified: {}", o.error.chars().take(100).collect::<String>())),
            Err(e) => return self.reject(&tx_id, &e),
        };
        // Defence in depth: the worker must have checked THIS transaction against THIS address.
        if out.tx_id != tx_id || out.expected_to != address {
            return self.reject(&tx_id, "the verifier checked a different transaction or address");
        }
        let value: u128 = out.near_yocto.parse().unwrap_or(0);
        let net = value.saturating_sub(value / 10_000 * self.cfg.spread_bps as u128);
        if net < self.cfg.min_claim_near.0 {
            return self.reject(&tx_id, "the deposit is worth less than the minimum claim");
        }
        let pay = net.min(self.cfg.max_claim_near.0);
        if pay > self.reserve_near {
            return self.reject(&tx_id, "the reserve cannot cover this deposit right now");
        }
        self.reserve_near -= pay; // set aside now; spent by `execute_claim`, restored if the arena refuses the call
        self.claims.insert(format!("solana:{tx_id}"), ClaimStatus::Verified { near_pay: U128(pay), lamports: out.lamports, beneficiary: beneficiary.clone(), intent: intent.clone() });
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "gateway_deposit_verified", "data": [{"tx_id": tx_id, "beneficiary": beneficiary, "intent": intent.canonical(), "lamports": out.lamports, "near_pay": U128(pay)}]})));
        true
    }

    /// Permissionless, own transaction (a buy needs a whole transaction of gas, which the OutLayer callback does not
    /// have): spends a verified deposit's NEAR for the beneficiary. Once per deposit: the status flips to `Done` first.
    pub fn execute_claim(&mut self, tx_id: String) -> Promise {
        let key = format!("solana:{tx_id}");
        let Some(ClaimStatus::Verified { near_pay, lamports, beneficiary, intent }) = self.claims.get(&key).cloned() else {
            env::panic_str("no verified deposit waiting for this transaction")
        };
        self.claims.insert(key, ClaimStatus::Done { near_paid: near_pay, lamports });
        let pay = near_pay.0;
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "gateway_deposit_claimed", "data": [{"tx_id": tx_id, "beneficiary": beneficiary, "intent": intent.canonical(), "lamports": lamports, "near_paid": near_pay}]})));
        let arena = self.arena.clone();
        let call = match &intent {
            DepositIntent::Buy { ticker, min_out } => Promise::new(arena).function_call(
                "buy_for",
                json!({ "beneficiary": beneficiary, "key": ticker, "min_out": min_out }).to_string().into_bytes(),
                NearToken::from_yoctonear(pay),
                INTENT_GAS,
            ),
            DepositIntent::FundChest { ticker } => Promise::new(arena).function_call("fund_chest", json!({ "key": ticker }).to_string().into_bytes(), NearToken::from_yoctonear(pay), Gas::from_tgas(60)),
        };
        call.then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_intent_done(tx_id, U128(pay)))
    }

    /// If the arena refused the call itself (unknown coin, not launched), the NEAR comes back: the reserve is restored
    /// and the claim is rejected. The SOL stays at the deposit address (core has no refund path, documented). A *buy*
    /// that fails inside the arena refunds the beneficiary directly.
    #[private]
    pub fn on_intent_done(&mut self, tx_id: String, pay: U128) -> bool {
        if env::promise_result_checked(0, 1024).is_ok() {
            return true;
        }
        self.reserve_near += pay.0;
        self.claims.insert(format!("solana:{tx_id}"), ClaimStatus::Rejected { reason: "the arena refused the intent".into() });
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "gateway_intent_failed", "data": [{"tx_id": tx_id, "restored": pay}]})));
        false
    }


    // ================================================================ wallet balances (D-57)

    /// The derivation path of a wallet's deposit address (Solana: ed25519 domain 1, EVM: secp256k1 domain 0). The path commits to
    /// the owner key, so a deposit can only ever credit the wallet whose key it names.
    pub fn wallet_deposit_path(&self, owner: String) -> String {
        format!("wallet:{}", Self::owner(&owner).key())
    }

    /// Permissionless (the user, a relayer, a bot). Checks the deposit `tx_id` (Solana signature or EVM transaction hash) went to
    /// the owner's deposit address; on success the NEAR value (minus the spread) is credited to the owner's gateway balance.
    pub fn claim_wallet_deposit(&mut self, tx_id: String, owner: String) -> Promise {
        let o = Self::owner(&owner);
        let (chain, domain) = match o {
            Owner::Sol(_) => ("solana", 1u64),
            Owner::Evm(_) => ("evm", 0u64),
        };
        match o {
            Owner::Sol(_) => require!(tx_id.len() <= 100 && bs58::decode(&tx_id).into_vec().map(|b| b.len() == 64).unwrap_or(false), "tx_id must be a Solana signature (base58, 64 bytes)"),
            Owner::Evm(_) => require!(tx_id.len() == 66 && tx_id.starts_with("0x") && tx_id[2..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "tx_id must be an EVM transaction hash (0x + 64 lowercase hex)"),
        }
        let key = format!("{chain}:{tx_id}");
        let now = env::block_timestamp_ms();
        match self.claims.get(&key) {
            Some(ClaimStatus::Done { .. }) | Some(ClaimStatus::Verified { .. }) | Some(ClaimStatus::Credited { .. }) => env::panic_str("this deposit was already paid out"),
            Some(ClaimStatus::Pending { since_ms }) if now.saturating_sub(*since_ms) < RETRY_AFTER_MS => env::panic_str("this claim is already being verified"),
            _ => {}
        }
        self.claims.insert(key, ClaimStatus::Pending { since_ms: now });
        Promise::new(self.mpc.clone())
            .function_call(
                "derived_public_key",
                json!({ "path": format!("wallet:{}", o.key()), "predecessor": env::current_account_id(), "domain_id": domain }).to_string().into_bytes(),
                NearToken::from_yoctonear(0),
                DERIVE_GAS,
            )
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(160)).with_unused_gas_weight(1).on_wallet_derived(chain.to_string(), tx_id, o.key()))
    }

    #[private]
    pub fn on_wallet_derived(&mut self, chain: String, tx_id: String, owner: String) -> Option<Promise> {
        let key = format!("{chain}:{tx_id}");
        let derived = env::promise_result_checked(0, 256).ok().and_then(|b| serde_json::from_slice::<String>(&b).ok());
        let address = match (chain.as_str(), derived) {
            ("solana", Some(k)) => k.strip_prefix("ed25519:").map(|s| s.to_string()),
            ("evm", Some(k)) => k
                .strip_prefix("secp256k1:")
                .and_then(|s| bs58::decode(s).into_vec().ok())
                .filter(|pk| pk.len() == 64)
                .map(|pk| format!("0x{}", env::keccak256(&pk)[12..].iter().map(|b| format!("{b:02x}")).collect::<String>())),
            _ => None,
        };
        let Some(address) = address else {
            self.reject_key(&key, &tx_id, "could not derive the deposit address");
            return None;
        };
        let input = json!({ "chain": chain, "tx_id": tx_id, "expected_to": address }).to_string();
        let args = json!({
            "source": { "WasmUrl": { "url": self.worker.url, "hash": self.worker.hash, "build_target": "wasm32-wasip2" } },
            "resource_limits": { "max_instructions": 2_000_000_000u64, "max_memory_mb": 64, "max_execution_seconds": 90 },
            "input_data": input,
            "secrets_ref": null,
            "response_format": "Text",
            "payer_account_id": env::current_account_id(),
        })
        .to_string();
        Some(
            Promise::new(self.outlayer.clone())
                .function_call("request_execution", args.into_bytes(), OUTLAYER_FEE, REQUEST_GAS)
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(30)).with_unused_gas_weight(1).on_wallet_verified(chain, tx_id, owner, address)),
        )
    }

    /// The callback of this contract's own `request_execution` (I5): the TEE worker checked a finalized transfer to exactly
    /// this address. Credits the owner's balance from the reserve.
    #[private]
    pub fn on_wallet_verified(&mut self, chain: String, tx_id: String, owner: String, address: String) -> bool {
        let key = format!("{chain}:{tx_id}");
        let parsed: Result<VerifierOut, String> = match env::promise_result_checked(0, 8192) {
            Err(_) => Err("the verifier run failed or timed out".into()),
            Ok(bytes) => serde_json::from_slice::<String>(&bytes)
                .map_err(|_| "unreadable verifier result".to_string())
                .and_then(|line| serde_json::from_str::<VerifierOut>(&line).map_err(|_| "the verifier returned no JSON".to_string())),
        };
        let out = match parsed {
            Ok(o) if o.ok => o,
            Ok(o) => return self.reject_key(&key, &tx_id, &format!("not verified: {}", o.error.chars().take(100).collect::<String>())),
            Err(e) => return self.reject_key(&key, &tx_id, &e),
        };
        if out.tx_id != tx_id || out.expected_to != address {
            return self.reject_key(&key, &tx_id, "the verifier checked a different transaction or address");
        }
        let raw: u128 = if out.raw_amount.is_empty() { out.lamports as u128 } else { out.raw_amount.parse().unwrap_or(0) };
        let value: u128 = out.near_yocto.parse().unwrap_or(0);
        let net = value.saturating_sub(value / 10_000 * self.cfg.spread_bps as u128);
        if net < self.cfg.min_claim_near.0 {
            return self.reject_key(&key, &tx_id, "the deposit is worth less than the minimum claim");
        }
        let pay = net.min(self.cfg.max_claim_near.0);
        if pay > self.reserve_near {
            return self.reject_key(&key, &tx_id, "the reserve cannot cover this deposit right now");
        }
        self.reserve_near -= pay;
        let bal = self.near_bal.get(&owner).copied().unwrap_or(0);
        self.near_bal.insert(owner.clone(), bal + pay);
        self.claims.insert(key, ClaimStatus::Credited { owner: owner.clone(), near_paid: U128(pay), raw_amount: U128(raw) });
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_credited", "data": [{"chain": chain, "tx_id": tx_id, "owner": owner, "near_paid": U128(pay), "raw_amount": U128(raw), "balance": U128(bal + pay)}]})));
        true
    }

    /// Permissionless: anyone may submit an owner's signed action and earns the relay fee for it (the fee comes out of the owner's
    /// balance and is the same whoever submits). The message is rebuilt here from these arguments; see `wallet_message`.
    /// Spends only the signer's own balance: a wrong, stale or foreign signature changes nothing.
    pub fn wallet_action(&mut self, owner: String, action: WalletAction, nonce: u64, expires_ms: u64, signature: Base64VecU8) -> Promise {
        let o = Self::owner(&owner);
        let key = o.key();
        let now = env::block_timestamp_ms();
        require!(now <= expires_ms, "this signed message has expired");
        require!(expires_ms <= now + MAX_SIGNATURE_AGE_MS, "the expiry is more than 24 hours away");
        require!(nonce == self.nonces.get(&key).copied().unwrap_or(0), "wrong nonce (the next valid one is in get_wallet)");
        let text = walletsig::message(&env::current_account_id(), &key, &action.canonical(), nonce, expires_ms);
        walletsig::verify(&o, text.as_bytes(), &signature.0);
        let amount = action.amount();
        require!(amount > 0 || action.zero_amount_ok(), "the amount must be above zero");
        let fee = if action.takes_relay_fee() { self.relay_fee } else { 0 };
        let spend = action.near_spend();
        let total = spend.checked_add(fee).unwrap_or_else(|| env::panic_str("amount overflow"));
        let bal = self.near_bal.get(&key).copied().unwrap_or(0);
        require!(bal >= total, "not enough balance for this action plus the relay fee");
        self.check_plain(&action);
        self.check_launch_action(&key, &action);
        if let WalletAction::Sell { token, .. } | WalletAction::TransferTokens { token, .. } = &action {
            require!(self.tok_bal.get(&tok_key(&key, token)).copied().unwrap_or(0) >= amount, "not enough tokens in the wallet balance");
        }
        // Token operations hold the one gateway-wide token lock (see `lock`); a taken lock refuses the call before anything changes.
        if matches!(action, WalletAction::Buy { .. } | WalletAction::JoinWar { .. } | WalletAction::Sell { .. } | WalletAction::TransferTokens { .. }) {
            self.take_lock(&key);
        }
        if let WalletAction::Sell { token, .. } | WalletAction::TransferTokens { token, .. } = &action {
            let tk = tok_key(&key, token);
            let have = self.tok_bal.get(&tk).copied().unwrap_or(0);
            self.tok_bal.insert(tk, have - amount);
        }
        self.nonces.insert(key.clone(), nonce + 1);
        self.near_bal.insert(key.clone(), bal - total);
        let relayer = env::predecessor_account_id();
        if fee > 0 {
            Promise::new(relayer.clone()).transfer(NearToken::from_yoctonear(fee)).detach();
        }
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_action", "data": [{"owner": key, "action": action.canonical(), "nonce": nonce, "relayer": relayer, "fee": U128(fee)}]})));
        let call = match &action {
            WalletAction::FundChest { ticker, amount } => Promise::new(self.arena.clone()).function_call("fund_chest", json!({ "key": ticker }).to_string().into_bytes(), NearToken::from_yoctonear(amount.0), Gas::from_tgas(60)),
            WalletAction::TransferNear { to, amount } => Promise::new(to.clone()).transfer(NearToken::from_yoctonear(amount.0)),
            WalletAction::Hire { ticker, kind, target, prompt, amount } => {
                return Promise::new(self.arena.clone())
                    .function_call("hire", json!({ "ticker": ticker, "kind": kind, "target": target, "prompt": prompt }).to_string().into_bytes(), NearToken::from_yoctonear(amount.0), Gas::from_tgas(200))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_wallet_hire(key, U128(amount.0)));
            }
            WalletAction::Buy { key: coin, amount, min_out } | WalletAction::JoinWar { key: coin, amount, min_out } => {
                let war = matches!(action, WalletAction::JoinWar { .. });
                return Promise::new(self.arena.clone())
                    .function_call("buy_for_wallet", json!({ "owner": key, "key": coin, "min_out": min_out, "require_war": war }).to_string().into_bytes(), NearToken::from_yoctonear(amount.0), BUY_GAS)
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_wallet_buy_started(key, *amount));
            }
            WalletAction::Sell { token, amount, min_out } => {
                return Promise::new(self.wrap.clone())
                    .function_call("ft_balance_of", json!({ "account_id": env::current_account_id() }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(250)).on_sell_start(key, token.clone(), *amount, *min_out));
            }
            WalletAction::CreateCoin { args_json, deposit } => return self.start_create_coin(key, args_json, *deposit),
            WalletAction::CrownClaimFees { ticker } => {
                return Promise::new(self.crown_account())
                    .function_call("crown_status", json!({ "token_id": ticker }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(120)).on_crown_fees_status(key, ticker.clone()));
            }
            WalletAction::CrownClaimVested { ticker } => {
                return Promise::new(self.crown_account())
                    .function_call("crown_claim_vested", json!({ "token_id": ticker }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(100))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_crown_vested_done(key, ticker.clone()));
            }
            WalletAction::CrownHeartbeat { ticker } => {
                return Promise::new(self.crown_account())
                    .function_call("crown_heartbeat", json!({ "token_id": ticker }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(30))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_crown_simple_done(key, "heartbeat".into(), ticker.clone()));
            }
            WalletAction::CrownSend { ticker, to } => {
                return Promise::new(self.crown_account())
                    .function_call("nft_transfer", json!({ "receiver_id": to, "token_id": ticker, "memo": "nme wallet crown send" }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(30))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_crown_sent(key, ticker.clone()));
            }
            WalletAction::TransferTokens { token, to, amount } => {
                return Promise::new(token.clone())
                    .function_call("ft_transfer", json!({ "receiver_id": to, "amount": amount, "memo": "nme wallet transfer" }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(30))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_tokens_moved(key, token.clone(), *amount));
            }
        };
        call.then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_wallet_action_done(key, U128(amount)))
    }

    // ---------------------------------------------------------------- wallet launch and PumpBag (D-59)

    fn crown_account(&self) -> AccountId {
        self.crown.clone()
    }

    fn factory_account(&self) -> AccountId {
        self.factory.clone()
    }

    fn coin_account(&self, ticker: &str) -> AccountId {
        format!("{ticker}.{}", self.factory).parse().unwrap_or_else(|_| env::panic_str("bad coin account"))
    }

    fn event(name: &str, data: serde_json::Value) {
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": name, "data": [data]})));
    }

    /// The `create_coin` args object, checked to the extent the gateway needs: a normalized ticker, an icon, no first buy (its tokens would
    /// land at the gateway and could not be attributed). Everything else is the factory's to validate.
    fn parse_launch_args(args_json: &str) -> (serde_json::Value, String, usize) {
        require!(args_json.len() <= 40_000, "the launch args are too large");
        let v: serde_json::Value = serde_json::from_str(args_json).unwrap_or_else(|_| env::panic_str("args_json is not JSON"));
        let ticker = v["ticker"].as_str().unwrap_or_else(|| env::panic_str("ticker is missing")).to_string();
        require!(
            (2..=32).contains(&ticker.len()) && ticker.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_'),
            "the ticker must already be normalized (a-z, 0-9, '-', '_', 2 to 32 characters)"
        );
        let icon_len = v["icon_data_url"].as_str().unwrap_or_else(|| env::panic_str("icon_data_url is missing")).len();
        require!(v["launch"]["first_buy"].is_null(), "a wallet launch cannot include a first buy");
        (v, ticker, icon_len)
    }

    /// Checks that run before anything is debited: a new ticker for a launch, the owner's own PumpBag for a crown action.
    fn check_launch_action(&self, owner: &str, action: &WalletAction) {
        match action {
            WalletAction::CreateCoin { args_json, .. } => {
                let (_, ticker, _) = Self::parse_launch_args(args_json);
                require!(self.launches.get(&ticker).is_none() && self.crowns.get(&ticker).is_none(), "this ticker was already launched through the gateway");
            }
            WalletAction::CrownClaimFees { ticker } | WalletAction::CrownClaimVested { ticker } | WalletAction::CrownHeartbeat { ticker } => {
                require!(self.crowns.get(ticker).map(|o| o == owner).unwrap_or(false), "the gateway holds no PumpBag of this ticker for this wallet");
            }
            WalletAction::CrownSend { ticker, to } => {
                require!(self.crowns.get(ticker).map(|o| o == owner).unwrap_or(false), "the gateway holds no PumpBag of this ticker for this wallet");
                require!(*to != env::current_account_id(), "send the PumpBag to another account");
            }
            _ => {}
        }
    }

    /// The signed `deposit` is attached to `create_coin` as it is (the gateway adds no step before the factory: the whole 300 Tgas a
    /// transaction may carry is needed by the factory's registration chain). A deposit above the factory's price leaves the excess at the
    /// gateway, so the preview and the relayer use exactly `factory.required_deposit`; one below it makes the factory refuse and the
    /// payment comes back through `settle_launch`.
    fn start_create_coin(&mut self, owner: String, args_json: &str, deposit: U128) -> Promise {
        let (v, ticker, _) = Self::parse_launch_args(args_json);
        self.launches.insert(ticker.clone(), LaunchRec { owner: owner.clone(), paid: deposit.0, settled: false });
        self.crowns.insert(ticker.clone(), owner.clone());
        Promise::new(self.factory_account())
            .function_call_weight("create_coin".to_string(), json!({ "args": v }).to_string().into_bytes(), NearToken::from_yoctonear(deposit.0), Gas::from_tgas(100), near_sdk::GasWeight(1))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_create_done(owner, ticker, deposit))
    }

    /// `true` from the factory means the coin was created and registered. Anything else is not trusted either way: the paid amount stays
    /// recorded and `settle_launch` (anyone) asks the factory whether the coin exists before returning the money.
    #[private]
    pub fn on_create_done(&mut self, owner: String, ticker: String, paid: U128) -> bool {
        let ok = env::promise_result_checked(0, 64).ok().and_then(|b| serde_json::from_slice::<bool>(&b).ok()).unwrap_or(false);
        Self::event(if ok { "wallet_coin_created" } else { "wallet_launch_unconfirmed" }, json!({"owner": owner, "ticker": ticker, "paid": paid}));
        ok
    }

    /// Permissionless: a launch that did not report success. If the factory has no such coin of the gateway's, the payment came back
    /// (as a refund) and goes to the owner's balance, once.
    pub fn settle_launch(&mut self, ticker: String) -> Promise {
        let l = self.launches.get_mut(&ticker).unwrap_or_else(|| env::panic_str("no such launch"));
        require!(!l.settled && l.paid > 0, "nothing to settle");
        l.settled = true;
        Promise::new(self.factory_account())
            .function_call("get_coin", json!({ "ticker": ticker }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_settle_checked(ticker))
    }

    #[private]
    pub fn on_settle_checked(&mut self, ticker: String) -> bool {
        let Some(rec) = self.launches.get(&ticker).cloned() else { return false };
        let answer = env::promise_result_checked(0, 4096).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let Some(answer) = answer else {
            if let Some(l) = self.launches.get_mut(&ticker) { l.settled = false; }
            return false;
        };
        let ours = answer["creator"].as_str().map(|c| c == env::current_account_id().as_str()).unwrap_or(false);
        if ours {
            if let Some(l) = self.launches.get_mut(&ticker) { l.settled = false; }
            return false;
        }
        let bal = self.near_bal.get(&rec.owner).copied().unwrap_or(0);
        self.near_bal.insert(rec.owner.clone(), bal + rec.paid);
        self.launches.remove(&ticker);
        self.crowns.remove(&ticker);
        Self::event("wallet_launch_refunded", json!({"owner": rec.owner, "ticker": ticker, "amount": U128(rec.paid)}));
        true
    }

    /// Permissionless: the coin's launch failed (every DCL fee tier taken). The coin pays what it still holds to its creator, the gateway;
    /// the part that arrives is measured (under the token lock, capped at what the owner paid) and credited to the owner, once.
    pub fn claim_launch_refund(&mut self, ticker: String) -> Promise {
        let l = self.launches.get(&ticker).unwrap_or_else(|| env::panic_str("no such launch"));
        require!(!l.settled && l.paid > 0, "nothing to refund");
        Promise::new(self.coin_account(&ticker))
            .function_call("get_launch", b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(100)).on_refund_checked(ticker))
    }

    #[private]
    pub fn on_refund_checked(&mut self, ticker: String) -> Option<Promise> {
        let failed = env::promise_result_checked(0, 2048).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).map(|v| v == json!("Failed")).unwrap_or(false);
        let rec = self.launches.get(&ticker).cloned()?;
        if !failed || rec.settled || !self.try_lock(&rec.owner) {
            return None;
        }
        if let Some(l) = self.launches.get_mut(&ticker) { l.settled = true; }
        let before = env::account_balance().as_yoctonear();
        Some(
            Promise::new(self.coin_account(&ticker))
                .function_call("refund_failed", b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(60))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_launch_refund_done(ticker, U128(before))),
        )
    }

    #[private]
    pub fn on_launch_refund_done(&mut self, ticker: String, before: U128) -> bool {
        let Some(rec) = self.launches.get(&ticker).cloned() else { return false };
        self.release_lock(&rec.owner);
        if env::promise_result_checked(0, 64).is_err() {
            if let Some(l) = self.launches.get_mut(&ticker) { l.settled = false; }
            return false;
        }
        let got = env::account_balance().as_yoctonear().saturating_sub(before.0).min(rec.paid);
        let bal = self.near_bal.get(&rec.owner).copied().unwrap_or(0);
        self.near_bal.insert(rec.owner.clone(), bal + got);
        Self::event("wallet_launch_refunded", json!({"owner": rec.owner, "ticker": ticker, "amount": U128(got)}));
        true
    }

    /// PumpBag fees, step 2: a forfeited crown pays its fees to the coin's treasury, not to the gateway, so nothing is claimed then.
    #[private]
    pub fn on_crown_fees_status(&mut self, owner: String, ticker: String) -> Option<Promise> {
        let st = env::promise_result_checked(0, 2048).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let held = st.as_ref().map(|v| v["owner"] == json!(env::current_account_id()) && v["forfeited"] == json!(false)).unwrap_or(false);
        if !held || !self.try_lock(&owner) {
            Self::event("wallet_crown_claim_refused", json!({"owner": owner, "ticker": ticker, "kind": "fees"}));
            return None;
        }
        let before = env::account_balance().as_yoctonear();
        Some(
            Promise::new(self.crown_account())
                .function_call("crown_claim_fees", json!({ "token_id": ticker }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(100))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_crown_fees_done(owner, ticker, U128(before))),
        )
    }

    /// Step 3: the vault reports the amount; what is credited is the part that really arrived (a forfeit between the check and the claim
    /// would route the fees to the treasury).
    #[private]
    pub fn on_crown_fees_done(&mut self, owner: String, ticker: String, before: U128) -> bool {
        self.release_lock(&owner);
        let reported = env::promise_result_checked(0, 64).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|a| a.0).unwrap_or(0);
        let got = reported.min(env::account_balance().as_yoctonear().saturating_sub(before.0));
        if got == 0 {
            return false;
        }
        let bal = self.near_bal.get(&owner).copied().unwrap_or(0);
        self.near_bal.insert(owner.clone(), bal + got);
        Self::event("wallet_crown_fees", json!({"owner": owner, "ticker": ticker, "amount": U128(got), "balance": U128(bal + got)}));
        true
    }

    /// The vault transferred vested tokens to the gateway and reports the exact amount: credited to the owner's token balance.
    #[private]
    pub fn on_crown_vested_done(&mut self, owner: String, ticker: String) -> bool {
        let amount = env::promise_result_checked(0, 64).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|a| a.0).unwrap_or(0);
        if amount == 0 {
            Self::event("wallet_crown_claim_refused", json!({"owner": owner, "ticker": ticker, "kind": "vested"}));
            return false;
        }
        let coin = self.coin_account(&ticker);
        self.give_back_tokens(&owner, &coin, amount);
        Self::event("wallet_crown_vested", json!({"owner": owner, "ticker": ticker, "amount": U128(amount)}));
        true
    }

    #[private]
    pub fn on_crown_simple_done(&mut self, owner: String, what: String, ticker: String) -> bool {
        let ok = env::promise_result_checked(0, 64).is_ok();
        Self::event(if ok { "wallet_crown_done" } else { "wallet_crown_failed" }, json!({"owner": owner, "ticker": ticker, "what": what}));
        ok
    }

    #[private]
    pub fn on_crown_sent(&mut self, owner: String, ticker: String) -> bool {
        if env::promise_result_checked(0, 64).is_err() {
            Self::event("wallet_crown_failed", json!({"owner": owner, "ticker": ticker, "what": "send"}));
            return false;
        }
        self.crowns.remove(&ticker);
        Self::event("wallet_crown_sent", json!({"owner": owner, "ticker": ticker}));
        true
    }

    pub fn get_launch(&self, ticker: String) -> Option<serde_json::Value> {
        self.launches.get(&ticker).map(|l| json!({ "owner": l.owner, "paid": U128(l.paid), "settled": l.settled }))
    }

    pub fn get_crown_owner(&self, ticker: String) -> Option<String> {
        self.crowns.get(&ticker).cloned()
    }

    fn try_lock(&mut self, owner: &str) -> bool {
        if self.lock.as_ref().map(|l| env::block_timestamp_ms() < l.until_ms).unwrap_or(false) {
            return false;
        }
        self.take_lock(owner);
        true
    }

    // ---------------------------------------------------------------- token operations (buy / sell / move)

    /// The arena refused the buy before moving anything (unknown coin, below the minimum, no war open): the NEAR comes back to
    /// the owner's balance and the lock ends. A buy that the arena accepted is settled by `credit_tokens` / `credit_near` instead.
    #[private]
    pub fn on_wallet_buy_started(&mut self, owner: String, amount: U128) -> bool {
        if env::promise_result_checked(0, 256).is_ok() {
            return true;
        }
        self.release_lock(&owner);
        self.restore(owner, amount)
    }

    /// Arena only: NEAR that belongs to a wallet owner (a refused or partly used buy). Inflow to the owner's balance only.
    #[payable]
    pub fn credit_near(&mut self, owner: String) {
        require!(env::predecessor_account_id() == self.arena, "only the arena can credit a wallet");
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach NEAR");
        let bal = self.near_bal.get(&owner).copied().unwrap_or(0);
        self.near_bal.insert(owner.clone(), bal + amount);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_near_credited", "data": [{"owner": owner, "amount": U128(amount), "balance": U128(bal + amount)}]})));
    }

    /// Arena only: the tokens a wallet owner's buy brought (0 when it bought nothing). Ends the token lock of that owner's operation.
    pub fn credit_tokens(&mut self, owner: String, coin: AccountId, amount: U128) {
        require!(env::predecessor_account_id() == self.arena, "only the arena can credit a wallet");
        if amount.0 > 0 {
            let tk = tok_key(&owner, &coin);
            let have = self.tok_bal.get(&tk).copied().unwrap_or(0);
            self.tok_bal.insert(tk, have.saturating_add(amount.0));
        }
        self.release_lock(&owner);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_tokens_credited", "data": [{"owner": owner, "coin": coin, "amount": amount}]})));
    }

    /// Step 2 of a sale: the gateway's wNEAR balance before the sale, then the tokens go to the arena with the owner named in the message
    /// (the arena honors that only from this account). Failure here sells nothing: the tokens return to the owner's balance.
    #[private]
    pub fn on_sell_start(&mut self, owner: String, token: AccountId, amount: U128, min_out: U128) -> Option<Promise> {
        let Some(before) = env::promise_result_checked(0, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()) else {
            self.give_back_tokens(&owner, &token, amount.0);
            self.release_lock(&owner);
            env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_sell_failed", "data": [{"owner": owner, "reason": "could not read the wNEAR balance"}]})));
            return None;
        };
        let msg = json!({ "sell": { "min_out": min_out, "owner": owner } }).to_string();
        Some(
            Promise::new(token.clone())
                .function_call("ft_transfer_call", json!({ "receiver_id": self.arena, "amount": amount, "msg": msg }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(200))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(30)).on_sell_sent(owner, token, amount, before)),
        )
    }

    /// Step 3: `ft_transfer_call` returns how many tokens the arena used. What it did not use is back at the gateway and goes back
    /// to the owner's token balance. If something was sold, the wNEAR it brought is measured next.
    #[private]
    pub fn on_sell_sent(&mut self, owner: String, token: AccountId, amount: U128, before: U128) -> Option<Promise> {
        let used = env::promise_result_checked(0, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0.min(amount.0)).unwrap_or(0);
        self.give_back_tokens(&owner, &token, amount.0 - used);
        if used == 0 {
            self.release_lock(&owner);
            env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_sell_failed", "data": [{"owner": owner, "reason": "the arena sold nothing", "returned": amount}]})));
            return None;
        }
        Some(
            Promise::new(self.wrap.clone())
                .function_call("ft_balance_of", json!({ "account_id": env::current_account_id() }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(30)).on_sell_measured(owner, token, U128(used), before)),
        )
    }

    /// Step 4: proceeds = wNEAR balance now - before (exact under the lock). They are owed to the owner at once and unwrapped next.
    #[private]
    pub fn on_sell_measured(&mut self, owner: String, token: AccountId, sold: U128, before: U128) -> Option<Promise> {
        let after = env::promise_result_checked(0, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0);
        let delta = after.map(|a| a.saturating_sub(before.0)).unwrap_or(0);
        if delta == 0 {
            self.release_lock(&owner);
            env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_sell_no_proceeds", "data": [{"owner": owner, "token": token, "sold": sold}]})));
            return None;
        }
        let due = self.wnear_due.get(&owner).copied().unwrap_or(0);
        self.wnear_due.insert(owner.clone(), due + delta);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_sold", "data": [{"owner": owner, "token": token, "sold": sold, "near": U128(delta)}]})));
        Some(self.unwrap_due(owner, delta))
    }

    /// Step 5: the wNEAR is unwrapped, then the NEAR is credited to the owner. A failed unwrap leaves the amount owed (`retry_unwrap`).
    #[private]
    pub fn on_unwrapped(&mut self, owner: String, amount: U128) -> bool {
        self.release_lock(&owner);
        if env::promise_result_checked(0, 64).is_err() {
            env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_unwrap_failed", "data": [{"owner": owner, "owed": amount}]})));
            return false;
        }
        let due = self.wnear_due.get(&owner).copied().unwrap_or(0);
        self.wnear_due.insert(owner.clone(), due.saturating_sub(amount.0));
        let bal = self.near_bal.get(&owner).copied().unwrap_or(0);
        self.near_bal.insert(owner.clone(), bal + amount.0);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_near_credited", "data": [{"owner": owner, "amount": amount, "balance": U128(bal + amount.0)}]})));
        true
    }

    /// Permissionless: finishes an unwrap that failed after a sale (the wNEAR is still here and still owed to `owner`).
    pub fn retry_unwrap(&mut self, owner: String) -> Promise {
        let due = self.wnear_due.get(&owner).copied().unwrap_or(0);
        require!(due > 0, "nothing is owed to this wallet");
        self.take_lock(&owner);
        self.unwrap_due(owner, due)
    }

    #[private]
    pub fn on_tokens_moved(&mut self, owner: String, token: AccountId, amount: U128) -> bool {
        self.release_lock(&owner);
        if env::promise_result_checked(0, 64).is_ok() {
            return true;
        }
        self.give_back_tokens(&owner, &token, amount.0);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_transfer_failed", "data": [{"owner": owner, "token": token, "restored": amount}]})));
        false
    }

    /// Permissionless: registers the gateway on a token's contract (NEP-145) so wallet buys can deliver to it. The attached NEAR
    /// (at most 0.01) pays the storage; the token returns any excess to the gateway's float. Needed once per token, and once for wNEAR.
    #[payable]
    pub fn register_token(&mut self, token: AccountId) -> Promise {
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0 && amount <= MAX_REGISTRATION, "attach between 0 and 0.01 NEAR");
        Promise::new(token).function_call("storage_deposit", json!({ "account_id": env::current_account_id(), "registration_only": true }).to_string().into_bytes(), NearToken::from_yoctonear(amount), Gas::from_tgas(15))
    }

    /// The arena accepted the job (it returns the job id): remember who hired. If it refused the call (unknown coin, price too low),
    /// the payment came back to the gateway and goes back to the owner's balance.
    #[private]
    pub fn on_wallet_hire(&mut self, owner: String, amount: U128) -> bool {
        match env::promise_result_checked(0, 64).ok().and_then(|b| serde_json::from_slice::<u64>(&b).ok()) {
            Some(job_id) => {
                self.hires.insert(job_id, HireRec { owner: owner.clone(), credited: false });
                env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_hired", "data": [{"owner": owner, "job_id": job_id, "amount": amount}]})));
                true
            }
            None => self.restore(owner, amount),
        }
    }

    /// Permissionless. If the arena refunded a job that a wallet hired through the gateway (the brain refused, failed or timed
    /// out), the escrow is now in the gateway: credit it to the wallet that paid, once.
    pub fn claim_hire_refund(&mut self, job_id: u64) -> Promise {
        match self.hires.get(&job_id) {
            Some(h) if !h.credited => {}
            Some(_) => env::panic_str("this job's refund was already credited"),
            None => env::panic_str("this job was not hired through the gateway"),
        }
        Promise::new(self.arena.clone())
            .function_call("get_job", json!({ "job_id": job_id }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_hire_job(job_id))
    }

    #[private]
    pub fn on_hire_job(&mut self, job_id: u64) -> bool {
        #[derive(Deserialize)]
        #[serde(crate = "near_sdk::serde")]
        struct JobView {
            payer: AccountId,
            amount: U128,
            status: String,
        }
        let Some(job) = env::promise_result_checked(0, 2048).ok().and_then(|b| serde_json::from_slice::<Option<JobView>>(&b).ok()).flatten() else { return false };
        let Some(h) = self.hires.get(&job_id).cloned() else { return false };
        if h.credited || job.status != "Refunded" || job.payer != env::current_account_id() {
            return false;
        }
        self.hires.insert(job_id, HireRec { owner: h.owner.clone(), credited: true });
        let bal = self.near_bal.get(&h.owner).copied().unwrap_or(0);
        self.near_bal.insert(h.owner.clone(), bal + job.amount.0);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_hire_refunded", "data": [{"owner": h.owner, "job_id": job_id, "amount": job.amount}]})));
        true
    }

    /// If the arena (or the transfer) refused, the amount goes back to the owner's balance. The relay fee stays paid.
    #[private]
    pub fn on_wallet_action_done(&mut self, owner: String, amount: U128) -> bool {
        if env::promise_result_checked(0, 1024).is_ok() {
            return true;
        }
        self.restore(owner, amount)
    }

    fn restore(&mut self, owner: String, amount: U128) -> bool {
        let bal = self.near_bal.get(&owner).copied().unwrap_or(0);
        self.near_bal.insert(owner.clone(), bal + amount.0);
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "wallet_action_failed", "data": [{"owner": owner, "restored": amount}]})));
        false
    }

    /// The exact text the owner's wallet must sign for this action (what the UI shows).
    pub fn wallet_message(&self, owner: String, action: WalletAction, nonce: u64, expires_ms: u64) -> String {
        walletsig::message(&env::current_account_id(), &Self::owner(&owner).key(), &action.canonical(), nonce, expires_ms)
    }

    /// Who hired this job through the gateway, and whether its refund was credited.
    pub fn get_hire(&self, job_id: u64) -> Option<serde_json::Value> {
        self.hires.get(&job_id).map(|h| json!({ "owner": h.owner, "credited": h.credited }))
    }

    pub fn get_wallet(&self, owner: String) -> serde_json::Value {
        let key = Self::owner(&owner).key();
        json!({ "near": U128(self.near_bal.get(&key).copied().unwrap_or(0)), "nonce": self.nonces.get(&key).copied().unwrap_or(0) })
    }

    /// A wallet owner's balance of one token, and what a sale still owes in wNEAR (normally zero).
    pub fn get_wallet_tokens(&self, owner: String, token: AccountId) -> serde_json::Value {
        let key = Self::owner(&owner).key();
        json!({ "tokens": U128(self.tok_bal.get(&tok_key(&key, &token)).copied().unwrap_or(0)), "wnear_due": U128(self.wnear_due.get(&key).copied().unwrap_or(0)) })
    }

    /// The running token operation, if any (it times out by itself).
    pub fn get_lock(&self) -> Option<TokenLock> {
        self.lock.clone().filter(|l| env::block_timestamp_ms() < l.until_ms)
    }

    pub fn get_claim(&self, tx_id: String) -> Option<ClaimStatus> {
        self.claims.get(&format!("solana:{tx_id}")).cloned()
    }

    pub fn get_reserve(&self) -> U128 {
        U128(self.reserve_near)
    }

    pub fn get_config(&self) -> serde_json::Value {
        json!({ "outlayer": self.outlayer, "mpc": self.mpc, "arena": self.arena, "worker": self.worker, "cfg": self.cfg, "reserve_near": U128(self.reserve_near), "relay_fee": U128(self.relay_fee), "wrap": self.wrap })
    }
}

/// Attached to each verifier request; OutLayer refunds what it does not use to the gateway (it stays in the contract's
/// balance, not in the reserve ledger: a small float that only ever grows).
const OUTLAYER_FEE: NearToken = NearToken::from_millinear(20);

/// The ledger key of a wallet owner's balance of one token.
fn tok_key(owner: &str, token: &AccountId) -> String {
    format!("{owner}|{token}")
}

/// wNEAR of the network the arena lives on, when `wrap` is not passed explicitly.
fn default_wrap(arena: &AccountId) -> AccountId {
    let s = arena.as_str();
    let wrap = if s.ends_with(".testnet") {
        "wrap.testnet"
    } else if s.ends_with(".near") {
        "wrap.near"
    } else {
        env::panic_str("pass `wrap` explicitly on this network")
    };
    wrap.parse().unwrap_or_else(|_| env::panic_str("bad wrap account"))
}

impl Contract {
    /// Ticker, kind, target and the token fields end up in a signed text line (`name:field:field`), so they must be plain.
    fn check_plain(&self, action: &WalletAction) {
        let fields: Vec<&str> = match action {
            WalletAction::FundChest { ticker, .. } => vec![ticker],
            WalletAction::Hire { ticker, kind, target, .. } => vec![ticker, kind, target],
            WalletAction::Buy { key, .. } | WalletAction::JoinWar { key, .. } => vec![key],
            WalletAction::CrownClaimFees { ticker } | WalletAction::CrownClaimVested { ticker } | WalletAction::CrownHeartbeat { ticker } | WalletAction::CrownSend { ticker, .. } => vec![ticker],
            WalletAction::TransferNear { .. } | WalletAction::Sell { .. } | WalletAction::TransferTokens { .. } | WalletAction::CreateCoin { .. } => vec![],
        };
        require!(fields.iter().all(|f| !f.is_empty() && f.len() <= 64 && !f.chars().any(|c| c.is_control() || c == ':')), "ticker, kind, target and key must be plain text without ':' or control characters");
    }

    /// One token operation at a time (see `lock`). Panics (refuses the call) while another one is running.
    fn take_lock(&mut self, owner: &str) {
        let now = env::block_timestamp_ms();
        if let Some(l) = &self.lock {
            require!(now >= l.until_ms, "another wallet token operation is running, try again in a moment");
        }
        self.lock = Some(TokenLock { owner: owner.to_string(), until_ms: now + TOKEN_LOCK_MS });
    }

    /// Ends the lock if it still belongs to this owner (a late callback of an operation that timed out must not free a newer one).
    fn release_lock(&mut self, owner: &str) {
        if self.lock.as_ref().map(|l| l.owner == owner).unwrap_or(false) {
            self.lock = None;
        }
    }

    fn give_back_tokens(&mut self, owner: &str, token: &AccountId, amount: u128) {
        if amount == 0 {
            return;
        }
        let tk = tok_key(owner, token);
        let have = self.tok_bal.get(&tk).copied().unwrap_or(0);
        self.tok_bal.insert(tk, have.saturating_add(amount));
    }

    fn unwrap_due(&self, owner: String, amount: u128) -> Promise {
        Promise::new(self.wrap.clone())
            .function_call("near_withdraw", json!({ "amount": U128(amount) }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_unwrapped(owner, U128(amount)))
    }

    fn path_for(beneficiary: &AccountId, intent: &DepositIntent) -> String {
        let h = env::sha256(format!("{beneficiary}|{}", intent.canonical()).as_bytes());
        let hex: String = h.iter().take(16).map(|b| format!("{b:02x}")).collect();
        format!("sol:{beneficiary}:{hex}")
    }

    fn owner(s: &str) -> Owner {
        Owner::parse(s).unwrap_or_else(|e| env::panic_str(&e))
    }

    fn reject(&mut self, tx_id: &str, reason: &str) -> bool {
        self.reject_key(&format!("solana:{tx_id}"), tx_id, reason)
    }

    fn reject_key(&mut self, key: &str, tx_id: &str, reason: &str) -> bool {
        self.claims.insert(key.to_string(), ClaimStatus::Rejected { reason: reason.to_string() });
        env::log_str(&format!("EVENT_JSON:{}", json!({"standard": "nme", "version": "1.0.0", "event": "gateway_deposit_rejected", "data": [{"tx_id": tx_id, "reason": reason}]})));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::{testing_env, PromiseResult};

    fn acct(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    fn gw() -> Contract {
        testing_env!(VMContextBuilder::new().build());
        Contract::new(
            acct("outlayer.testnet"),
            acct("v1.signer-prod.testnet"),
            acct("arena.nme.testnet"),
            WorkerSource { url: "https://x".into(), hash: "a".repeat(64) },
            GatewayConfig { spread_bps: 200, max_claim_near: U128(10 * 10u128.pow(24)), min_claim_near: U128(10u128.pow(22)) },
            U128(10u128.pow(21)),
            None,
            acct("nme.testnet"),
            acct("crown.nme.testnet"),
        )
    }

    const TX: &str = "35CHpwuN5unh1LtFQTgMFiq18xMsphYxsEbJQKJ4XTnBX3d5QKLfdqBZNvYvRGWdHymM182GPHJL5ShjdciHHRWz";

    fn buy() -> DepositIntent {
        DepositIntent::Buy { ticker: "pepe".into(), min_out: U128(1) }
    }

    fn result(r: PromiseResult) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("gateway.nme.testnet")).current_account_id(acct("gateway.nme.testnet")).build(), near_sdk::test_vm_config(), near_sdk::RuntimeFeesConfig::test(), Default::default(), vec![r]);
    }

    fn ok_line(near: u128, to: &str) -> PromiseResult {
        PromiseResult::Successful(serde_json::to_vec(&json!({"ok": true, "tx_id": TX, "expected_to": to, "lamports": 5_000_000_000u64, "near_yocto": near.to_string()}).to_string()).unwrap())
    }

    #[test]
    fn the_deposit_path_commits_to_beneficiary_and_intent() {
        let g = gw();
        let a = g.deposit_path(acct("alice.testnet"), buy());
        assert!(a.starts_with("sol:alice.testnet:"));
        assert_ne!(a, g.deposit_path(acct("bob.testnet"), buy()), "another beneficiary: another address");
        assert_ne!(a, g.deposit_path(acct("alice.testnet"), DepositIntent::Buy { ticker: "pepe".into(), min_out: U128(2) }), "another min_out: another address");
        assert_ne!(a, g.deposit_path(acct("alice.testnet"), DepositIntent::FundChest { ticker: "pepe".into() }), "another intent: another address");
        assert_eq!(a, g.deposit_path(acct("alice.testnet"), buy()), "deterministic");
    }

    #[test]
    #[should_panic(expected = "tx_id must be a Solana signature")]
    fn a_malformed_tx_id_is_rejected() {
        let mut g = gw();
        let _ = g.claim_deposit("short".into(), acct("alice.testnet"), buy());
    }

    #[test]
    fn a_claim_is_single_flight_and_a_paid_one_is_final() {
        let mut g = gw();
        let _ = g.claim_deposit(TX.into(), acct("alice.testnet"), buy());
        assert!(matches!(g.get_claim(TX.into()), Some(ClaimStatus::Pending { .. })));
        let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = g.claim_deposit(TX.into(), acct("alice.testnet"), buy()); }));
        assert!(again.is_err(), "no second claim while the first is being verified");
    }

    #[test]
    fn a_verified_deposit_sets_the_net_value_aside_and_executes_once() {
        let mut g = gw();
        g.reserve_near = 100 * 10u128.pow(24);
        g.claims.insert(format!("solana:{TX}"), ClaimStatus::Pending { since_ms: 0 });
        result(ok_line(5 * 10u128.pow(24), "ADDR"));
        assert!(g.on_verified(TX.into(), acct("alice.testnet"), buy(), "ADDR".into()));
        let paid = 5 * 10u128.pow(24) - 5 * 10u128.pow(24) / 10_000 * 200;
        assert!(matches!(g.get_claim(TX.into()), Some(ClaimStatus::Verified { near_pay, lamports: 5_000_000_000, .. }) if near_pay.0 == paid), "the 2% spread was kept");
        assert_eq!(g.get_reserve().0, 100 * 10u128.pow(24) - paid, "set aside from the reserve");
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("anyone.testnet")).build());
        let _ = g.execute_claim(TX.into());
        assert_eq!(g.get_claim(TX.into()), Some(ClaimStatus::Done { near_paid: U128(paid), lamports: 5_000_000_000 }));
        let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = g.execute_claim(TX.into()); }));
        assert!(again.is_err(), "a deposit is spent once");
        let reclaim = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = g.claim_deposit(TX.into(), acct("alice.testnet"), buy()); }));
        assert!(reclaim.is_err(), "a paid deposit is final");
    }

    #[test]
    #[should_panic(expected = "no verified deposit")]
    fn nothing_executes_without_a_verified_deposit() {
        let mut g = gw();
        let _ = g.execute_claim(TX.into());
    }

    #[test]
    fn rejections_pay_nothing() {
        let cases: Vec<(PromiseResult, &str)> = vec![
            (PromiseResult::Failed, "failed or timed out"),
            (PromiseResult::Successful(b"null".to_vec()), "unreadable"),
            (PromiseResult::Successful(serde_json::to_vec(&"garbage").unwrap()), "no JSON"),
            (PromiseResult::Successful(serde_json::to_vec(&json!({"ok": false, "error": "no SOL transfer to the expected deposit address"}).to_string()).unwrap()), "not verified"),
            (ok_line(5 * 10u128.pow(24), "WRONG"), "different transaction or address"),
            (ok_line(1_000, "ADDR"), "minimum claim"),
        ];
        for (r, why) in cases {
            let mut g = gw();
            g.reserve_near = 100 * 10u128.pow(24);
            result(r);
            let v = g.on_verified(TX.into(), acct("alice.testnet"), buy(), "ADDR".into());
            assert!(!v);
            match g.get_claim(TX.into()) {
                Some(ClaimStatus::Rejected { reason }) => assert!(reason.contains(why), "{reason} vs {why}"),
                other => panic!("{other:?}"),
            }
            assert_eq!(g.get_reserve().0, 100 * 10u128.pow(24), "nothing left the reserve");
        }
    }

    #[test]
    fn the_caps_bind_the_payout_and_an_empty_reserve_rejects() {
        let mut g = gw();
        g.reserve_near = 100 * 10u128.pow(24);
        result(ok_line(500 * 10u128.pow(24), "ADDR")); // worth 500 NEAR, cap is 10
        let _ = g.on_verified(TX.into(), acct("alice.testnet"), buy(), "ADDR".into());
        assert!(matches!(g.get_claim(TX.into()), Some(ClaimStatus::Verified { near_pay, .. }) if near_pay == g.cfg.max_claim_near));
        let mut g = gw();
        g.reserve_near = 10u128.pow(23);
        result(ok_line(5 * 10u128.pow(24), "ADDR"));
        let _ = g.on_verified(TX.into(), acct("alice.testnet"), buy(), "ADDR".into());
        assert!(matches!(g.get_claim(TX.into()), Some(ClaimStatus::Rejected { .. })), "the reserve cannot cover it");
        assert_eq!(g.get_reserve().0, 10u128.pow(23));
    }

    #[test]
    fn a_refused_intent_restores_the_reserve() {
        let mut g = gw();
        g.reserve_near = 0;
        g.claims.insert(format!("solana:{TX}"), ClaimStatus::Done { near_paid: U128(7), lamports: 1 });
        result(PromiseResult::Failed);
        assert!(!g.on_intent_done(TX.into(), U128(7)));
        assert_eq!(g.get_reserve().0, 7);
        assert!(matches!(g.get_claim(TX.into()), Some(ClaimStatus::Rejected { .. })));
        result(PromiseResult::Successful(vec![]));
        assert!(g.on_intent_done(TX.into(), U128(7)));
        assert_eq!(g.get_reserve().0, 7, "a successful intent restores nothing");
    }

    // ---------------------------------------------------------------- wallet balances (D-57)

    fn sol_sk() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
    }
    fn sol_owner() -> String {
        format!("sol:{}", bs58::encode(sol_sk().verifying_key().to_bytes()).into_string())
    }
    fn evm_sk() -> k256::ecdsa::SigningKey {
        k256::ecdsa::SigningKey::from_bytes((&[9u8; 32]).into()).unwrap()
    }
    fn evm_owner() -> String {
        let pk = evm_sk().verifying_key().to_encoded_point(false);
        format!("evm:0x{}", near_sdk::env::keccak256(&pk.as_bytes()[1..])[12..].iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    fn sign(owner: &str, text: &str) -> Base64VecU8 {
        use ed25519_dalek::Signer;
        if owner.starts_with("sol:") {
            Base64VecU8(sol_sk().sign(text.as_bytes()).to_bytes().to_vec())
        } else {
            let (sig, rec) = evm_sk().sign_prehash_recoverable(&walletsig::eip191_digest(text.as_bytes())).unwrap();
            let mut v = sig.to_bytes().to_vec();
            v.push(27 + rec.to_byte());
            Base64VecU8(v)
        }
    }
    const NOW: u64 = 1_794_000_000_000;
    fn at(who: &str) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct(who)).current_account_id(acct("gateway.nme.testnet")).block_timestamp(NOW * 1_000_000).build());
    }
    fn chest(amount: u128) -> WalletAction {
        WalletAction::FundChest { ticker: "pepe".into(), amount: U128(amount) }
    }
    fn funded(owner: &str, near: u128) -> Contract {
        let mut g = gw();
        g.near_bal.insert(owner.to_string(), near);
        g
    }
    fn act(g: &mut Contract, owner: &str, a: WalletAction, nonce: u64, exp: u64) {
        let text = g.wallet_message(owner.to_string(), a.clone(), nonce, exp);
        let sig = sign(owner, &text);
        let _ = g.wallet_action(owner.to_string(), a, nonce, exp, sig);
    }
    fn fails(f: impl FnOnce() + std::panic::UnwindSafe) {
        assert!(std::panic::catch_unwind(f).is_err());
    }

    #[test]
    fn a_wallet_deposit_path_commits_to_the_owner_on_both_chains() {
        let g = gw();
        let (a, b) = (g.wallet_deposit_path(sol_owner()), g.wallet_deposit_path(evm_owner()));
        assert_eq!(a, format!("wallet:{}", sol_owner()));
        assert_eq!(b, format!("wallet:{}", evm_owner()));
        assert_ne!(a, b);
        fails(|| { gw().wallet_deposit_path("alice.testnet".into()); });
    }

    #[test]
    fn deposit_claims_validate_the_transaction_id_per_chain() {
        at("relayer.testnet");
        fails(|| { let _ = gw().claim_wallet_deposit(TX.into(), evm_owner()); }); // a Solana signature is not an EVM hash
        fails(|| { let _ = gw().claim_wallet_deposit(format!("0x{}", "a".repeat(64)), sol_owner()); });
        fails(|| { let _ = gw().claim_wallet_deposit(format!("0x{}", "A".repeat(64)), evm_owner()); }); // uppercase: not canonical
        let mut g = gw();
        let _ = g.claim_wallet_deposit(format!("0x{}", "a".repeat(64)), evm_owner());
        assert!(matches!(g.claims.get(&format!("evm:0x{}", "a".repeat(64))), Some(ClaimStatus::Pending { .. })));
        let _ = g.claim_wallet_deposit(TX.into(), sol_owner());
        assert!(matches!(g.get_claim(TX.into()), Some(ClaimStatus::Pending { .. })));
    }

    fn evm_line(near: u128, to: &str, tx: &str) -> PromiseResult {
        PromiseResult::Successful(serde_json::to_vec(&json!({"ok": true, "tx_id": tx, "expected_to": to, "raw_amount": "1000000000000000000", "near_yocto": near.to_string()}).to_string()).unwrap())
    }

    #[test]
    fn a_verified_deposit_credits_the_owner_from_the_reserve_on_both_chains() {
        let evm_tx = format!("0x{}", "b".repeat(64));
        for (chain, tx, owner, line) in [("solana", TX.to_string(), sol_owner(), ok_line(5 * 10u128.pow(24), "ADDR")), ("evm", evm_tx.clone(), evm_owner(), evm_line(5 * 10u128.pow(24), "ADDR", &evm_tx))] {
            let mut g = gw();
            g.reserve_near = 100 * 10u128.pow(24);
            result(line);
            assert!(g.on_wallet_verified(chain.into(), tx.clone(), owner.clone(), "ADDR".into()));
            let paid = 5 * 10u128.pow(24) - 5 * 10u128.pow(24) / 10_000 * 200;
            assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(paid)), "credited the net value");
            assert_eq!(g.get_reserve().0, 100 * 10u128.pow(24) - paid, "paid from the reserve");
            assert!(matches!(g.claims.get(&format!("{chain}:{tx}")), Some(ClaimStatus::Credited { near_paid, .. }) if near_paid.0 == paid));
            at("relayer.testnet");
            let (t, o) = (tx.clone(), owner.clone());
            fails(std::panic::AssertUnwindSafe(|| { let _ = g.claim_wallet_deposit(t, o); }));
        }
    }

    #[test]
    fn wallet_deposit_rejections_credit_nothing() {
        let mut g = gw();
        g.reserve_near = 100 * 10u128.pow(24);
        for (r, why) in [(PromiseResult::Failed, "failed or timed out"), (ok_line(5 * 10u128.pow(24), "WRONG"), "different transaction or address"), (ok_line(1_000, "ADDR"), "minimum claim")] {
            result(r);
            assert!(!g.on_wallet_verified("solana".into(), TX.into(), sol_owner(), "ADDR".into()));
            assert!(matches!(g.claims.get(&format!("solana:{TX}")), Some(ClaimStatus::Rejected { reason }) if reason.contains(why)));
            assert_eq!(g.get_wallet(sol_owner())["near"], json!(U128(0)));
            assert_eq!(g.get_reserve().0, 100 * 10u128.pow(24));
        }
    }

    #[test]
    fn the_evm_deposit_address_is_the_keccak_of_the_derived_secp256k1_key() {
        // the MPC contract's real answer for path "evm:0xabc" under gateway.nme.testnet (domain 0), from testnet
        let derived = "secp256k1:54r6voLgzGA4DngYjsx8ikDW5cqNNV93saWpiZJhKWCQkAJBByZTf1Bj7x1B79d7W8BmRhNbiTd7TpKZxipxfGKr";
        let mut g = gw();
        let tx = format!("0x{}", "c".repeat(64));
        g.claims.insert(format!("evm:{tx}"), ClaimStatus::Pending { since_ms: 0 });
        result(PromiseResult::Successful(serde_json::to_vec(&derived).unwrap()));
        assert!(g.on_wallet_derived("evm".into(), tx.clone(), evm_owner()).is_some());
        // a failed derivation rejects the claim
        let mut g = gw();
        g.claims.insert(format!("evm:{tx}"), ClaimStatus::Pending { since_ms: 0 });
        result(PromiseResult::Failed);
        assert!(g.on_wallet_derived("evm".into(), tx.clone(), evm_owner()).is_none());
        assert!(matches!(g.claims.get(&format!("evm:{tx}")), Some(ClaimStatus::Rejected { .. })));
    }

    #[test]
    fn a_signed_action_spends_the_signers_balance_once_on_both_chains() {
        for owner in [sol_owner(), evm_owner()] {
            let mut g = funded(&owner, 5 * 10u128.pow(24));
            at("relayer.testnet");
            act(&mut g, &owner, chest(2 * 10u128.pow(24)), 0, NOW + 60_000);
            let w = g.get_wallet(owner.clone());
            assert_eq!(w["nonce"], json!(1));
            assert_eq!(w["near"], json!(U128(5 * 10u128.pow(24) - 2 * 10u128.pow(24) - 10u128.pow(21))), "amount and the relay fee left the balance");
            // the same signed message again: the nonce is spent
            let text = g.wallet_message(owner.clone(), chest(2 * 10u128.pow(24)), 0, NOW + 60_000);
            let sig = sign(&owner, &text);
            let o = owner.clone();
            fails(std::panic::AssertUnwindSafe(move || { let _ = g.wallet_action(o, chest(2 * 10u128.pow(24)), 0, NOW + 60_000, sig); }));
        }
    }

    #[test]
    fn forged_stale_or_foreign_signatures_change_nothing() {
        let owner = sol_owner();
        let attempts: Vec<(&str, Box<dyn Fn(&mut Contract) + std::panic::RefUnwindSafe>)> = vec![
            ("another action than the one signed", Box::new(|g| {
                let sig = sign(&sol_owner(), &g.wallet_message(sol_owner(), chest(10u128.pow(24)), 0, NOW + 60_000));
                let _ = g.wallet_action(sol_owner(), chest(4 * 10u128.pow(24)), 0, NOW + 60_000, sig);
            })),
            ("signed by another key", Box::new(|g| {
                let sig = sign(&evm_owner(), &g.wallet_message(sol_owner(), chest(10u128.pow(24)), 0, NOW + 60_000));
                let _ = g.wallet_action(sol_owner(), chest(10u128.pow(24)), 0, NOW + 60_000, sig);
            })),
            ("expired", Box::new(|g| act(g, &sol_owner(), chest(10u128.pow(24)), 0, NOW - 1))),
            ("expiry too far away", Box::new(|g| act(g, &sol_owner(), chest(10u128.pow(24)), 0, NOW + 2 * MAX_SIGNATURE_AGE_MS))),
            ("wrong nonce", Box::new(|g| act(g, &sol_owner(), chest(10u128.pow(24)), 1, NOW + 60_000))),
            ("more than the balance", Box::new(|g| act(g, &sol_owner(), chest(50 * 10u128.pow(24)), 0, NOW + 60_000))),
            ("zero amount", Box::new(|g| act(g, &sol_owner(), chest(0), 0, NOW + 60_000))),
        ];
        for (name, f) in attempts {
            let mut g = funded(&owner, 5 * 10u128.pow(24));
            at("relayer.testnet");
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut g)));
            assert!(r.is_err(), "{name} must be rejected");
        }
        // a stranger with no balance cannot spend someone else's: the balance is looked up under the signer's own key
        let mut g = funded(&owner, 5 * 10u128.pow(24));
        at("relayer.testnet");
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| act(&mut g, &evm_owner(), chest(10u128.pow(24)), 0, NOW + 60_000)));
        assert!(r.is_err(), "the EVM owner has no balance");
        assert_eq!(g.get_wallet(owner)["near"], json!(U128(5 * 10u128.pow(24))));
    }

    #[test]
    fn a_refused_action_restores_the_amount_but_the_relay_fee_stays_paid() {
        let owner = sol_owner();
        let mut g = funded(&owner, 5 * 10u128.pow(24));
        at("relayer.testnet");
        act(&mut g, &owner, WalletAction::TransferNear { to: acct("bob.testnet"), amount: U128(2 * 10u128.pow(24)) }, 0, NOW + 60_000);
        let after = 5 * 10u128.pow(24) - 2 * 10u128.pow(24) - 10u128.pow(21);
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(after)));
        result(PromiseResult::Failed);
        assert!(!g.on_wallet_action_done(owner.clone(), U128(2 * 10u128.pow(24))));
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(after + 2 * 10u128.pow(24))), "amount restored, fee kept");
        result(PromiseResult::Successful(vec![]));
        assert!(g.on_wallet_action_done(owner.clone(), U128(7)));
        assert_eq!(g.get_wallet(owner)["near"], json!(U128(after + 2 * 10u128.pow(24))), "a successful action restores nothing");
    }


    // ---------------------------------------------------------------- token actions: buy / join war / sell / move (D-57)

    const N1: u128 = 10u128.pow(24);

    fn buy_a(amount: u128) -> WalletAction {
        WalletAction::Buy { key: "pepe".into(), amount: U128(amount), min_out: U128(1) }
    }
    fn sell_a(amount: u128) -> WalletAction {
        WalletAction::Sell { token: acct("pepe.nme.testnet"), amount: U128(amount), min_out: U128(1) }
    }
    fn pepe() -> AccountId {
        acct("pepe.nme.testnet")
    }
    fn as_arena(deposit: u128) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("arena.nme.testnet")).current_account_id(acct("gateway.nme.testnet")).attached_deposit(NearToken::from_yoctonear(deposit)).block_timestamp(NOW * 1_000_000).build());
    }
    fn with_tokens(owner: &str, near: u128, tokens: u128) -> Contract {
        let mut g = funded(owner, near);
        g.tok_bal.insert(tok_key(owner, &pepe()), tokens);
        g
    }
    fn tokens_of(g: &Contract, owner: &str) -> u128 {
        g.tok_bal.get(&tok_key(owner, &pepe())).copied().unwrap_or(0)
    }
    fn u128_result(v: u128) -> PromiseResult {
        PromiseResult::Successful(serde_json::to_vec(&U128(v)).unwrap())
    }

    #[test]
    fn the_wrap_account_follows_the_network() {
        assert_eq!(default_wrap(&acct("arena.nme.testnet")), acct("wrap.testnet"));
        assert_eq!(default_wrap(&acct("arena.nme.near")), acct("wrap.near"));
        fails(|| { default_wrap(&acct("arena.sandbox")); });
    }

    #[test]
    fn a_signed_buy_spends_near_without_a_relay_fee_and_takes_the_token_lock() {
        for owner in [sol_owner(), evm_owner()] {
            let mut g = funded(&owner, 5 * N1);
            at("relayer.testnet");
            act(&mut g, &owner, buy_a(2 * N1), 0, NOW + 60_000);
            assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(3 * N1)), "the amount left, no relay fee on a buy");
            assert_eq!(g.get_lock().map(|l| l.owner), Some(owner.clone()));
            // a second token operation is refused while the first one runs, and changes nothing
            let o = owner.clone();
            fails(std::panic::AssertUnwindSafe(|| act(&mut g, &o, buy_a(N1), 1, NOW + 60_000)));
            assert_eq!(g.get_wallet(owner.clone())["nonce"], json!(1));
            assert_eq!(g.get_wallet(owner)["near"], json!(U128(3 * N1)));
        }
    }

    #[test]
    fn the_token_lock_times_out_and_only_its_owner_releases_it() {
        let owner = sol_owner();
        let mut g = funded(&owner, 5 * N1);
        g.lock = Some(TokenLock { owner: evm_owner(), until_ms: NOW + 1_000 });
        at("relayer.testnet");
        fails(std::panic::AssertUnwindSafe(|| act(&mut g, &owner, buy_a(N1), 0, NOW + 60_000))); // still locked
        g.release_lock(&owner);
        assert!(g.lock.is_some(), "a stranger's late callback does not free the lock");
        g.lock = Some(TokenLock { owner: evm_owner(), until_ms: NOW });
        act(&mut g, &owner, buy_a(N1), 0, NOW + 60_000); // expired: allowed
        assert_eq!(g.lock.as_ref().map(|l| l.owner.clone()), Some(owner));
    }

    #[test]
    fn a_buy_the_arena_refuses_synchronously_restores_the_amount_and_the_lock() {
        let owner = sol_owner();
        let mut g = funded(&owner, 5 * N1);
        at("relayer.testnet");
        act(&mut g, &owner, buy_a(2 * N1), 0, NOW + 60_000);
        result(PromiseResult::Successful(vec![]));
        assert!(g.on_wallet_buy_started(owner.clone(), U128(2 * N1)));
        assert!(g.lock.is_some(), "an accepted buy keeps the lock until the arena reports");
        result(PromiseResult::Failed);
        assert!(!g.on_wallet_buy_started(owner.clone(), U128(2 * N1)));
        assert!(g.lock.is_none());
        assert_eq!(g.get_wallet(owner)["near"], json!(U128(5 * N1)), "the whole amount is back");
    }

    #[test]
    fn only_the_arena_credits_a_wallet_and_credit_tokens_ends_the_lock() {
        let owner = sol_owner();
        let mut g = funded(&owner, 5 * N1);
        at("relayer.testnet");
        act(&mut g, &owner, buy_a(2 * N1), 0, NOW + 60_000);
        for who in ["relayer.testnet", "alice.testnet", "gateway.nme.testnet"] {
            at(who);
            let o = owner.clone();
            fails(std::panic::AssertUnwindSafe(|| { let mut c = funded(&o, 0); c.credit_tokens(o.clone(), pepe(), U128(5)); }));
            let o = owner.clone();
            fails(std::panic::AssertUnwindSafe(|| { let mut c = funded(&o, 0); c.credit_near(o.clone()); }));
        }
        as_arena(0);
        g.credit_tokens(owner.clone(), pepe(), U128(1_000));
        assert_eq!(tokens_of(&g, &owner), 1_000);
        assert!(g.lock.is_none(), "the arena's report ends the lock");
        as_arena(N1 / 2);
        g.credit_near(owner.clone());
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(3 * N1 + N1 / 2)), "a refund lands in the owner's NEAR balance");
        as_arena(0);
        fails(std::panic::AssertUnwindSafe(|| { let mut c = funded(&sol_owner(), 0); as_arena(0); c.credit_near(sol_owner()); })); // no NEAR attached
        // zero tokens (nothing bought) still ends the lock and credits nothing
        let mut g = funded(&owner, 5 * N1);
        at("relayer.testnet");
        act(&mut g, &owner, buy_a(N1), 0, NOW + 60_000);
        as_arena(0);
        g.credit_tokens(owner.clone(), pepe(), U128(0));
        assert_eq!(tokens_of(&g, &owner), 0);
        assert!(g.lock.is_none());
    }

    #[test]
    fn join_war_is_a_different_signed_text_than_buy() {
        let g = gw();
        let o = sol_owner();
        let a = g.wallet_message(o.clone(), buy_a(N1), 0, NOW + 1);
        let b = g.wallet_message(o.clone(), WalletAction::JoinWar { key: "pepe".into(), amount: U128(N1), min_out: U128(1) }, 0, NOW + 1);
        assert_ne!(a, b, "a signature for a buy cannot be replayed as a war entry");
        assert!(b.contains("join_war:pepe:") && a.contains("buy:pepe:"));
    }

    #[test]
    fn keys_with_separators_or_control_characters_are_refused() {
        let owner = sol_owner();
        for bad in ["pe:pe", "pe\npe", ""] {
            let mut g = funded(&owner, 5 * N1);
            at("relayer.testnet");
            let a = WalletAction::Buy { key: bad.into(), amount: U128(N1), min_out: U128(1) };
            fails(std::panic::AssertUnwindSafe(|| act(&mut g, &owner, a, 0, NOW + 60_000)));
        }
    }

    #[test]
    fn a_sale_debits_tokens_and_needs_no_near_balance_or_relay_fee() {
        let owner = sol_owner();
        let mut g = with_tokens(&owner, 0, 1_000);
        at("relayer.testnet");
        act(&mut g, &owner, sell_a(400), 0, NOW + 60_000);
        assert_eq!(tokens_of(&g, &owner), 600);
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(0)), "no relay fee on a sale");
        assert!(g.lock.is_some());
        // more tokens than the balance
        let mut g = with_tokens(&owner, 0, 100);
        at("relayer.testnet");
        let o = owner.clone();
        fails(std::panic::AssertUnwindSafe(|| act(&mut g, &o, sell_a(101), 0, NOW + 60_000)));
        assert_eq!(tokens_of(&g, &owner), 100);
        assert!(g.lock.is_none(), "a refused action leaves no lock");
    }

    #[test]
    fn a_sale_returns_every_token_it_did_not_sell_and_credits_the_measured_proceeds_once() {
        let owner = sol_owner();
        // the wNEAR balance could not be read: nothing was sold
        let mut g = with_tokens(&owner, 0, 600);
        g.lock = Some(TokenLock { owner: owner.clone(), until_ms: NOW + 1 });
        result(PromiseResult::Failed);
        assert!(g.on_sell_start(owner.clone(), pepe(), U128(400), U128(1)).is_none());
        assert_eq!(tokens_of(&g, &owner), 1_000);
        assert!(g.lock.is_none());
        // the arena sold nothing (ft_transfer_call failed or returned 0)
        for r in [PromiseResult::Failed, u128_result(0)] {
            let mut g = with_tokens(&owner, 0, 600);
            g.lock = Some(TokenLock { owner: owner.clone(), until_ms: NOW + 1 });
            result(r);
            assert!(g.on_sell_sent(owner.clone(), pepe(), U128(400), U128(7)).is_none());
            assert_eq!(tokens_of(&g, &owner), 1_000);
            assert!(g.lock.is_none());
        }
        // a partial sale: the unused part is returned, the sold part is measured next
        let mut g = with_tokens(&owner, 0, 600);
        g.lock = Some(TokenLock { owner: owner.clone(), until_ms: NOW + 1 });
        result(u128_result(150));
        assert!(g.on_sell_sent(owner.clone(), pepe(), U128(400), U128(7)).is_some());
        assert_eq!(tokens_of(&g, &owner), 600 + 250);
        assert!(g.lock.is_some(), "the lock stays until the proceeds are credited");
        // proceeds = wNEAR now - before
        result(u128_result(7 + 3 * N1));
        assert!(g.on_sell_measured(owner.clone(), pepe(), U128(150), U128(7)).is_some());
        assert_eq!(g.get_wallet_tokens(owner.clone(), pepe())["wnear_due"], json!(U128(3 * N1)));
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(0)), "not credited before the unwrap");
        result(PromiseResult::Successful(vec![]));
        assert!(g.on_unwrapped(owner.clone(), U128(3 * N1)));
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(3 * N1)));
        assert_eq!(g.get_wallet_tokens(owner.clone(), pepe())["wnear_due"], json!(U128(0)));
        assert!(g.lock.is_none());
    }

    #[test]
    fn nothing_received_or_an_unreadable_balance_credits_nothing() {
        let owner = sol_owner();
        for r in [PromiseResult::Failed, u128_result(5)] {
            let mut g = with_tokens(&owner, 0, 0);
            g.lock = Some(TokenLock { owner: owner.clone(), until_ms: NOW + 1 });
            result(r);
            assert!(g.on_sell_measured(owner.clone(), pepe(), U128(10), U128(5)).is_none());
            assert_eq!(g.get_wallet_tokens(owner.clone(), pepe())["wnear_due"], json!(U128(0)));
            assert!(g.lock.is_none());
        }
    }

    #[test]
    fn a_failed_unwrap_keeps_the_amount_owed_and_retry_finishes_it() {
        let owner = sol_owner();
        let mut g = funded(&owner, 0);
        g.wnear_due.insert(owner.clone(), 5 * N1);
        g.lock = Some(TokenLock { owner: owner.clone(), until_ms: NOW + 1 });
        result(PromiseResult::Failed);
        assert!(!g.on_unwrapped(owner.clone(), U128(5 * N1)));
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(0)));
        assert_eq!(g.get_wallet_tokens(owner.clone(), pepe())["wnear_due"], json!(U128(5 * N1)));
        assert!(g.lock.is_none());
        at("anyone.testnet");
        let _ = g.retry_unwrap(owner.clone()); // permissionless, takes the lock
        assert!(g.lock.is_some());
        result(PromiseResult::Successful(vec![]));
        assert!(g.on_unwrapped(owner.clone(), U128(5 * N1)));
        assert_eq!(g.get_wallet(owner.clone())["near"], json!(U128(5 * N1)));
        at("anyone.testnet");
        fails(std::panic::AssertUnwindSafe(|| { let mut c = funded(&sol_owner(), 0); at("anyone.testnet"); let _ = c.retry_unwrap(sol_owner()); })); // nothing owed
    }

    #[test]
    fn a_token_transfer_debits_first_and_a_failed_transfer_gives_the_tokens_back() {
        let owner = evm_owner();
        let mut g = with_tokens(&owner, 0, 1_000);
        at("relayer.testnet");
        act(&mut g, &owner, WalletAction::TransferTokens { token: pepe(), to: acct("bob.testnet"), amount: U128(300) }, 0, NOW + 60_000);
        assert_eq!(tokens_of(&g, &owner), 700);
        assert!(g.lock.is_some());
        result(PromiseResult::Failed);
        assert!(!g.on_tokens_moved(owner.clone(), pepe(), U128(300)));
        assert_eq!(tokens_of(&g, &owner), 1_000);
        assert!(g.lock.is_none());
        // success keeps the debit
        let mut g = with_tokens(&owner, 0, 1_000);
        at("relayer.testnet");
        act(&mut g, &owner, WalletAction::TransferTokens { token: pepe(), to: acct("bob.testnet"), amount: U128(300) }, 0, NOW + 60_000);
        result(PromiseResult::Successful(vec![]));
        assert!(g.on_tokens_moved(owner.clone(), pepe(), U128(300)));
        assert_eq!(tokens_of(&g, &owner), 700);
    }

    #[test]
    fn one_wallets_tokens_cannot_be_spent_by_another_signature() {
        let (a, b) = (sol_owner(), evm_owner());
        let mut g = with_tokens(&a, 0, 1_000);
        at("relayer.testnet");
        let text = g.wallet_message(b.clone(), sell_a(500), 0, NOW + 60_000);
        let sig = sign(&b, &text);
        fails(std::panic::AssertUnwindSafe(|| { let _ = g.wallet_action(b.clone(), sell_a(500), 0, NOW + 60_000, sig); })); // b owns no tokens
        assert_eq!(tokens_of(&g, &a), 1_000);
        // a's signature over b's key does not verify
        let text = g.wallet_message(b.clone(), sell_a(500), 0, NOW + 60_000);
        let sig = sign(&a, &text);
        fails(std::panic::AssertUnwindSafe(|| { let _ = g.wallet_action(b, sell_a(500), 0, NOW + 60_000, sig); }));
    }

    #[test]
    fn register_token_is_bounded() {
        at("relayer.testnet");
        fails(|| { let _ = gw().register_token(pepe()); }); // nothing attached
        let mut g = gw();
        testing_env!(VMContextBuilder::new().attached_deposit(NearToken::from_near(1)).build());
        fails(std::panic::AssertUnwindSafe(|| { let _ = g.register_token(pepe()); })); // more than 0.01
        testing_env!(VMContextBuilder::new().attached_deposit(NearToken::from_yoctonear(1_250_000_000_000_000_000_000)).build());
        let _ = g.register_token(pepe());
    }

    #[test]
    fn the_source_has_no_withdraw_path() {
        let src = include_str!("lib.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for forbidden in ["pub fn withdraw", "pub fn sweep", "pub fn rescue", "pub fn set_", "pub fn update_"] {
            assert!(!code.contains(forbidden), "{forbidden} must not exist (no admin, no way out)");
        }
        // `migrate` is `#[private]` (only the account itself can call it) and `#[init(ignore_state)]`; it is the one upgrade path
        assert!(code.contains("#[private]\n    #[init(ignore_state)]\n    pub fn migrate_v3"));
        // the only way the reserve leaves is the two arena calls of `execute_claim`
        assert_eq!(code.matches("NearToken::from_yoctonear(pay)").count(), 2);
        // wallet balances leave only inside `wallet_action`, after the owner's signature was verified: the relay fee and a TransferNear
        let action = &code[code.find("pub fn wallet_action").unwrap()..code.find("pub fn on_wallet_action_done").unwrap()];
        assert_eq!(code.matches(".transfer(").count(), action.matches(".transfer(").count());
        assert_eq!(action.matches(".transfer(").count(), 2);
        assert!(action.find("walletsig::verify").unwrap() < action.find(".transfer(").unwrap(), "verify before any value moves");
        // the token ledger is debited only inside `wallet_action`, after the signature check
        let debit_at = code.find("self.tok_bal.insert(tk, have - amount)").unwrap();
        assert!(code.find("walletsig::verify").unwrap() < debit_at && debit_at < code.find("pub fn on_wallet_action_done").unwrap());
        assert_eq!(code.matches("have - amount").count(), 1, "one debit of the token ledger");
        // only the arena credits a wallet
        for f in ["pub fn credit_near", "pub fn credit_tokens"] {
            let body = &code[code.find(f).unwrap()..];
            assert!(body[..body.find("self.arena").unwrap() + 20].contains("predecessor_account_id() == self.arena"), "{f} must check the arena");
        }
    }

    // ------------------------------------------------------------------ D-59: wallet launch and PumpBag

    fn args(ticker: &str) -> String {
        json!({"ticker": ticker, "name": "Froggy", "icon_data_url": "data:image/webp;base64,AAAA", "description": "d", "socials": "", "persona": {"vibe": "v", "rivals": []},
               "launch": {"start_point": 92000, "range_points": 40000, "first_buy": null}, "crown": {"cliff_ms": 1, "vest_ms": 2, "inactivity_ms": 3}, "colosseum_opt_in": true}).to_string()
    }
    fn launch_action(ticker: &str, deposit: u128) -> WalletAction {
        WalletAction::CreateCoin { args_json: args(ticker), deposit: U128(deposit) }
    }
    fn result_with(r: PromiseResult, balance: u128) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("gateway.nme.testnet")).current_account_id(acct("gateway.nme.testnet")).account_balance(near_sdk::NearToken::from_yoctonear(balance)).block_timestamp(NOW * 1_000_000).build(),
            near_sdk::test_vm_config(), near_sdk::RuntimeFeesConfig::test(), Default::default(), vec![r]);
    }
    fn json_result(v: serde_json::Value) -> PromiseResult {
        PromiseResult::Successful(serde_json::to_vec(&v).unwrap())
    }
    fn near_of(g: &Contract, owner: &str) -> u128 {
        g.near_bal.get(owner).copied().unwrap_or(0)
    }
    fn with_crown(owner: &str, near: u128, ticker: &str) -> Contract {
        let mut g = funded(owner, near);
        g.crowns.insert(ticker.to_string(), owner.to_string());
        g
    }

    #[test]
    fn a_launch_is_bound_to_its_args_and_debits_the_deposit_and_the_fee() {
        let owner = sol_owner();
        let mut g = funded(&owner, 3 * N1);
        at("relayer.testnet");
        // a signature over one args object does not authorise another
        let text = g.wallet_message(owner.clone(), launch_action("froggy", N1), 0, NOW + 60_000);
        let sig = sign(&owner, &text);
        fails(std::panic::AssertUnwindSafe(|| { let _ = g.wallet_action(owner.clone(), launch_action("toady", N1), 0, NOW + 60_000, sig.clone()); }));
        assert_eq!(near_of(&g, &owner), 3 * N1);
        act(&mut g, &owner, launch_action("froggy", N1), 0, NOW + 60_000);
        assert_eq!(near_of(&g, &owner), 3 * N1 - N1 - 10u128.pow(21));
        assert_eq!(g.get_crown_owner("froggy".into()), Some(owner.clone()));
        assert_eq!(g.get_launch("froggy".into()).unwrap()["owner"], json!(owner));
    }

    #[test]
    fn launch_refusals_happen_before_anything_is_debited() {
        let owner = sol_owner();
        let mut g = funded(&owner, 3 * N1);
        at("relayer.testnet");
        let mut bad_first_buy: serde_json::Value = serde_json::from_str(&args("froggy")).unwrap();
        bad_first_buy["launch"]["first_buy"] = json!({"tokens": "1", "max_near": "1"});
        for a in [
            WalletAction::CreateCoin { args_json: bad_first_buy.to_string(), deposit: U128(N1) },
            WalletAction::CreateCoin { args_json: args("Froggy"), deposit: U128(N1) },          // not normalized
            WalletAction::CreateCoin { args_json: args("f"), deposit: U128(N1) },               // too short
            WalletAction::CreateCoin { args_json: "not json".into(), deposit: U128(N1) },
            WalletAction::CreateCoin { args_json: "x".repeat(40_001), deposit: U128(N1) },
            WalletAction::CreateCoin { args_json: args("froggy"), deposit: U128(5 * N1) },      // more than the balance
        ] {
            fails(std::panic::AssertUnwindSafe(|| act(&mut g, &owner, a.clone(), 0, NOW + 60_000)));
        }
        assert_eq!(near_of(&g, &owner), 3 * N1);
        act(&mut g, &owner, launch_action("froggy", N1), 0, NOW + 60_000);
        // the same ticker cannot be launched through the gateway twice, by anyone
        let other = evm_owner();
        g.near_bal.insert(other.clone(), 3 * N1);
        fails(std::panic::AssertUnwindSafe(|| act(&mut g, &other, launch_action("froggy", N1), 0, NOW + 60_000)));
    }

    #[test]
    fn the_signed_deposit_is_recorded_as_paid_and_attached_to_the_factory() {
        let owner = evm_owner();
        let mut g = funded(&owner, 3 * N1);
        at("relayer.testnet");
        act(&mut g, &owner, launch_action("froggy", 2 * N1), 0, NOW + 60_000);
        assert_eq!(g.launches.get("froggy").unwrap().paid, 2 * N1);
        assert_eq!(near_of(&g, &owner), 3 * N1 - 2 * N1 - 10u128.pow(21));
        // the factory reported success / not success: events only, the record stays until settled
        result_with(json_result(json!(true)), 10 * N1);
        assert!(g.on_create_done(owner.clone(), "froggy".into(), U128(2 * N1)));
        result_with(PromiseResult::Failed, 10 * N1);
        assert!(!g.on_create_done(owner.clone(), "froggy".into(), U128(2 * N1)));
        assert!(g.get_launch("froggy".into()).is_some() && g.get_crown_owner("froggy".into()).is_some());
    }

    #[test]
    fn an_unconfirmed_launch_is_refunded_once_and_only_if_the_factory_has_no_coin_of_ours() {
        let owner = sol_owner();
        let mk = || {
            let mut g = funded(&owner, 3 * N1);
            at("relayer.testnet");
            act(&mut g, &owner, launch_action("froggy", N1), 0, NOW + 60_000);
            g.launches.get_mut("froggy").unwrap().paid = N1;
            g
        };
        // the factory has no such coin: paid comes back to the owner, the records go
        let mut g = mk();
        let before = near_of(&g, &owner);
        let _ = g.settle_launch("froggy".into());
        fails(std::panic::AssertUnwindSafe(|| { let _ = g.settle_launch("froggy".into()); })); // already settling
        result_with(json_result(json!(null)), N1);
        assert!(g.on_settle_checked("froggy".into()));
        assert_eq!(near_of(&g, &owner), before + N1);
        assert!(g.get_launch("froggy".into()).is_none() && g.get_crown_owner("froggy".into()).is_none());
        // someone else's coin under that ticker: our payment also came back
        let mut g = mk();
        let _ = g.settle_launch("froggy".into());
        result_with(json_result(json!({"creator": "bob.testnet"})), N1);
        assert!(g.on_settle_checked("froggy".into()));
        // the coin exists and is the gateway's: nothing is credited
        let mut g = mk();
        let before = near_of(&g, &owner);
        let _ = g.settle_launch("froggy".into());
        result_with(json_result(json!({"creator": "gateway.nme.testnet"})), N1);
        assert!(!g.on_settle_checked("froggy".into()));
        assert_eq!(near_of(&g, &owner), before);
        assert!(g.get_crown_owner("froggy".into()).is_some());
        // an unreadable answer changes nothing and allows a retry
        let mut g = mk();
        let _ = g.settle_launch("froggy".into());
        result_with(PromiseResult::Failed, N1);
        assert!(!g.on_settle_checked("froggy".into()));
        assert!(!g.launches.get("froggy").unwrap().settled);
    }

    #[test]
    fn a_failed_launch_refund_is_measured_capped_and_credited_once() {
        let owner = sol_owner();
        let mut g = funded(&owner, 3 * N1);
        at("relayer.testnet");
        act(&mut g, &owner, launch_action("froggy", N1), 0, NOW + 60_000);
        g.launches.get_mut("froggy").unwrap().paid = N1;
        let _ = g.claim_launch_refund("froggy".into());
        // a launch that did not fail refunds nothing
        result_with(json_result(json!({"Live": {"pool_id": "x", "lpt_id": "y"}})), 5 * N1);
        assert!(g.on_refund_checked("froggy".into()).is_none());
        result_with(json_result(json!("Failed")), 5 * N1);
        assert!(g.on_refund_checked("froggy".into()).is_some());
        assert!(g.lock.is_some());
        let before = near_of(&g, &owner);
        // 5 NEAR before, 5.4 after: the credit is the measured 0.4
        result_with(PromiseResult::Successful(vec![]), 5 * N1 + 4 * N1 / 10);
        assert!(g.on_launch_refund_done("froggy".into(), U128(5 * N1)));
        assert_eq!(near_of(&g, &owner), before + 4 * N1 / 10);
        assert!(g.lock.is_none());
        fails(std::panic::AssertUnwindSafe(|| { let _ = g.claim_launch_refund("froggy".into()); })); // settled
        // never more than the owner paid, even if more arrived meanwhile
        let mut g = funded(&owner, 3 * N1);
        at("relayer.testnet");
        act(&mut g, &owner, launch_action("froggy", N1), 0, NOW + 60_000);
        g.launches.get_mut("froggy").unwrap().paid = N1;
        let before = near_of(&g, &owner);
        let _ = g.claim_launch_refund("froggy".into());
        result_with(json_result(json!("Failed")), 5 * N1);
        let _ = g.on_refund_checked("froggy".into());
        result_with(PromiseResult::Successful(vec![]), 50 * N1);
        g.on_launch_refund_done("froggy".into(), U128(5 * N1));
        assert_eq!(near_of(&g, &owner), before + N1);
    }

    #[test]
    fn only_the_recorded_owner_can_act_on_a_pumpbag() {
        let (owner, other) = (sol_owner(), evm_owner());
        let mut g = with_crown(&owner, N1, "froggy");
        g.near_bal.insert(other.clone(), N1);
        at("relayer.testnet");
        for a in [
            WalletAction::CrownClaimFees { ticker: "froggy".into() }, WalletAction::CrownClaimVested { ticker: "froggy".into() },
            WalletAction::CrownHeartbeat { ticker: "froggy".into() }, WalletAction::CrownSend { ticker: "froggy".into(), to: acct("bob.testnet") },
        ] {
            fails(std::panic::AssertUnwindSafe(|| act(&mut g, &other, a.clone(), 0, NOW + 60_000)));
        }
        assert_eq!(near_of(&g, &other), N1);
        // the owner may; claims cost no relay fee, heartbeat and send do
        act(&mut g, &owner, WalletAction::CrownClaimFees { ticker: "froggy".into() }, 0, NOW + 60_000);
        assert_eq!(near_of(&g, &owner), N1);
        act(&mut g, &owner, WalletAction::CrownHeartbeat { ticker: "froggy".into() }, 1, NOW + 60_000);
        assert_eq!(near_of(&g, &owner), N1 - 10u128.pow(21));
        fails(std::panic::AssertUnwindSafe(|| act(&mut g, &owner, WalletAction::CrownSend { ticker: "froggy".into(), to: acct("gateway.nme.testnet") }, 2, NOW + 60_000)));
        fails(std::panic::AssertUnwindSafe(|| act(&mut g, &owner, WalletAction::CrownHeartbeat { ticker: "nothere".into() }, 2, NOW + 60_000)));
    }

    #[test]
    fn a_sent_pumpbag_leaves_the_gateway_only_when_the_transfer_worked() {
        let owner = sol_owner();
        let mut g = with_crown(&owner, N1, "froggy");
        result_with(PromiseResult::Failed, N1);
        assert!(!g.on_crown_sent(owner.clone(), "froggy".into()));
        assert!(g.get_crown_owner("froggy".into()).is_some());
        result_with(PromiseResult::Successful(vec![]), N1);
        assert!(g.on_crown_sent(owner.clone(), "froggy".into()));
        assert!(g.get_crown_owner("froggy".into()).is_none());
    }

    #[test]
    fn crown_fees_credit_only_what_arrived_and_never_after_a_forfeit() {
        let owner = sol_owner();
        let held = json!({"owner": "gateway.nme.testnet", "forfeited": false});
        for refused in [json!({"owner": "gateway.nme.testnet", "forfeited": true}), json!({"owner": "bob.testnet", "forfeited": false}), json!(null)] {
            let mut g = with_crown(&owner, 0, "froggy");
            result_with(json_result(refused), N1);
            assert!(g.on_crown_fees_status(owner.clone(), "froggy".into()).is_none());
            assert!(g.lock.is_none());
            std::mem::forget(g);
        }
        let mut g = with_crown(&owner, 0, "froggy");
        result_with(json_result(held), 10 * N1);
        assert!(g.on_crown_fees_status(owner.clone(), "froggy".into()).is_some());
        assert!(g.lock.is_some());
        // reported 2 NEAR, only 0.5 reached the balance (a forfeit routed the rest to the treasury)
        result_with(json_result(json!((2 * N1).to_string())), 10 * N1 + N1 / 2);
        assert!(g.on_crown_fees_done(owner.clone(), "froggy".into(), U128(10 * N1)));
        assert_eq!(near_of(&g, &owner), N1 / 2);
        assert!(g.lock.is_none());
        // nothing arrived: nothing credited
        let mut g = with_crown(&owner, 0, "froggy");
        result_with(json_result(json!((2 * N1).to_string())), 10 * N1);
        assert!(!g.on_crown_fees_done(owner.clone(), "froggy".into(), U128(10 * N1)));
        assert_eq!(near_of(&g, &owner), 0);
        // a running token operation refuses the claim instead of panicking in the callback
        let mut g = with_crown(&owner, 0, "froggy");
        g.lock = Some(TokenLock { owner: "someone".into(), until_ms: NOW + 60_000 });
        result_with(json_result(json!({"owner": "gateway.nme.testnet", "forfeited": false})), N1);
        assert!(g.on_crown_fees_status(owner.clone(), "froggy".into()).is_none());
    }

    #[test]
    fn vested_tokens_are_credited_by_the_exact_reported_amount() {
        let owner = evm_owner();
        let mut g = with_crown(&owner, 0, "froggy");
        result_with(json_result(json!("12345")), N1);
        assert!(g.on_crown_vested_done(owner.clone(), "froggy".into()));
        assert_eq!(tokens_of_coin(&g, &owner, "froggy.nme.testnet"), 12_345);
        result_with(json_result(json!("0")), N1);
        assert!(!g.on_crown_vested_done(owner.clone(), "froggy".into()));
        result_with(PromiseResult::Failed, N1);
        assert!(!g.on_crown_vested_done(owner.clone(), "froggy".into()));
        assert_eq!(tokens_of_coin(&g, &owner, "froggy.nme.testnet"), 12_345);
    }
    fn tokens_of_coin(g: &Contract, owner: &str, coin: &str) -> u128 {
        g.tok_bal.get(&tok_key(owner, &acct(coin))).copied().unwrap_or(0)
    }
}
