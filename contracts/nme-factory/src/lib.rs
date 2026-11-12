//! nme-factory (at NME_ROOT): validates, reserves the ticker, creates `<ticker>.<root>` with NO access key on
//! global coin code (D-15), initializes it, then registers it with the arena, mints its PumpBag and starts the
//! launch (docs/11 §3). Sniping is allowed by design; there is no batch and no anti-snipe logic.
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FT_METADATA_SPEC};
use near_sdk::json_types::{Base58CryptoHash, U128};
use near_sdk::serde_json::{self, json};
use near_sdk::store::IterableMap;
use near_sdk::{env, near, require, AccountId, CryptoHash, Gas, GasWeight, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue};
use nme_common::events::{self, name};
use nme_common::ext::{ext_arena, ext_crown};
use nme_common::ticker::{normalize_ticker, validate_account_len};
use nme_common::{CoinConfig, CrownParams, Feed, FeedItem, LaunchParams, Persona, DECIMALS, MAX_DESCRIPTION_LEN, MAX_ICON_BYTES, MAX_NAME_LEN, MAX_SOCIALS_LEN};

/// NEAR the coin needs to run the launch: DCL registration 0.5 (0.39 comes back), pool creation 0.1, vault 0.02,
/// plus a buffer. Everything unspent stays with the coin as its operating balance (it has no withdraw path).
const COIN_OPERATING: NearToken = NearToken::from_millinear(750);
/// Budget for coin state besides the icon (metadata, profile, persona, config).
const COIN_STATE_BYTES: u128 = 6 * 1024;
const ARENA_REG_BUDGET: NearToken = NearToken::from_millinear(20);
/// NEP-145 registration of the arena on each new coin (so it can receive sells and burn). Unused deposit is refunded.
const ARENA_TOKEN_REG: NearToken = NearToken::from_yoctonear(1_250_000_000_000_000_000_000);
const CROWN_MINT_BUDGET: NearToken = NearToken::from_millinear(30);
/// Kept by the factory for its own per-coin record and feed item (it has no owner to pay for them).
const FACTORY_STORAGE_BUDGET: NearToken = NearToken::from_millinear(10);
/// Delay before the arena schedules the first pool-fee claim for a new coin.
const FIRST_CLAIM_DELAY_MS: u64 = 10 * 60_000;
const NEW_GAS: Gas = Gas::from_tgas(40);
const CB_GAS: Gas = Gas::from_tgas(30);
const REG_GAS: Gas = Gas::from_tgas(20);

#[near(serializers = [json, borsh])]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    /// Ticker reserved, account creation in flight.
    Reserved,
    /// Coin account exists and is initialized; arena registration / PumpBag mint pending (`retry_registration`).
    Created,
    Live,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct CoinRecord {
    pub account: AccountId,
    pub creator: AccountId,
    pub created_ms: u64,
    pub status: Status,
    pub inactivity_ms: u64,
    pub colosseum_opt_in: bool,
}

#[near(serializers = [json])]
pub struct CreateCoinArgs {
    pub ticker: String,
    pub name: String,
    pub icon_data_url: String,
    pub description: String,
    pub socials: String,
    pub persona: Persona,
    pub launch: LaunchParams,
    pub crown: CrownParams,
    pub colosseum_opt_in: bool,
    /// What one paid job of this coin's brain costs (all attached to the arena's `hire`), fixed at launch: 0.1 to 10 NEAR.
    /// Omitted = the arena's global minimum.
    #[serde(default)]
    pub job_price: Option<U128>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    arena: AccountId,
    crown: AccountId,
    wrap: AccountId,
    dcl: AccountId,
    coin_code_hash: Base58CryptoHash,
    vault_code_hash: Base58CryptoHash,
    coins: IterableMap<String, CoinRecord>,
    feed: Feed,
    /// ticker -> the job price the creator chose; sent to the arena once the coin is registered there.
    job_prices: near_sdk::store::LookupMap<String, u128>,
}

// The layout before per-coin job prices, read once by `migrate`.
mod v1 {
    use super::*;

    #[near(serializers = [borsh])]
    pub struct Contract {
        pub arena: AccountId,
        pub crown: AccountId,
        pub wrap: AccountId,
        pub dcl: AccountId,
        pub coin_code_hash: Base58CryptoHash,
        pub vault_code_hash: Base58CryptoHash,
        pub coins: IterableMap<String, CoinRecord>,
        pub feed: Feed,
    }
}

/// Bounds of a creator's job price (the arena enforces its own minimum too).
const MIN_JOB_PRICE: u128 = 100_000_000_000_000_000_000_000;
const MAX_JOB_PRICE: u128 = 10_000_000_000_000_000_000_000_000;

#[near]
impl Contract {
    /// One-time upgrade: adds the per-coin job prices (existing coins keep the arena's minimum).
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let old: v1::Contract = env::state_read().unwrap_or_else(|| env::panic_str("no state to migrate"));
        Self { arena: old.arena, crown: old.crown, wrap: old.wrap, dcl: old.dcl, coin_code_hash: old.coin_code_hash, vault_code_hash: old.vault_code_hash, coins: old.coins, feed: old.feed, job_prices: near_sdk::store::LookupMap::new(b"j") }
    }

    #[init]
    pub fn new(arena: AccountId, crown: AccountId, wrap: AccountId, dcl: AccountId, coin_code_hash: Base58CryptoHash, vault_code_hash: Base58CryptoHash) -> Self {
        Self { arena, crown, wrap, dcl, coin_code_hash, vault_code_hash, coins: IterableMap::new(b"c"), feed: Feed::new(b"f"), job_prices: near_sdk::store::LookupMap::new(b"j") }
    }

    /// Create a coin. Attach at least `required_deposit(icon_len, first_buy_max_near)`; the excess is refunded at
    /// once and the whole payment is refunded if account creation or initialization fails.
    #[payable]
    pub fn create_coin(&mut self, args: CreateCoinArgs) -> Promise {
        let ticker = normalize_ticker(&args.ticker).unwrap_or_else(|e| env::panic_str(&e));
        let root = env::current_account_id();
        validate_account_len(&ticker, root.as_str()).unwrap_or_else(|e| env::panic_str(&e));
        require!(!args.name.trim().is_empty() && args.name.len() <= MAX_NAME_LEN, "name must be 1-64 bytes");
        require!(args.description.len() <= MAX_DESCRIPTION_LEN, "description too long");
        require!(args.socials.len() <= MAX_SOCIALS_LEN, "socials too long");
        require!(
            args.icon_data_url.starts_with("data:image/webp;base64,") || args.icon_data_url.starts_with("data:image/png;base64,"),
            "icon must be a data:image/webp or data:image/png base64 URL"
        );
        require!(args.icon_data_url.len() <= MAX_ICON_BYTES, "icon larger than 24 KB");
        args.launch.validate().unwrap_or_else(|e| env::panic_str(&e));
        args.crown.validate().unwrap_or_else(|e| env::panic_str(&e));
        args.persona.validate().unwrap_or_else(|e| env::panic_str(&e));
        require!(self.coins.get(&ticker).is_none(), "ticker already taken");
        if let Some(p) = args.job_price {
            require!((MIN_JOB_PRICE..=MAX_JOB_PRICE).contains(&p.0), "job_price must be between 0.1 and 10 NEAR");
        }

        let first_buy_near = args.launch.first_buy.as_ref().map(|f| f.max_near.0).unwrap_or(0);
        let funding = Self::coin_funding(args.icon_data_url.len(), first_buy_near);
        let required = Self::required_total(funding);
        let attached = env::attached_deposit();
        require!(attached >= required, format!("attach at least {} yoctoNEAR", required.as_yoctonear()));
        let creator = env::predecessor_account_id();
        let excess = attached.saturating_sub(required);
        if excess > NearToken::from_yoctonear(0) {
            Promise::new(creator.clone()).transfer(excess).detach();
        }

        let account: AccountId = format!("{ticker}.{root}").parse().unwrap();
        let now = env::block_timestamp_ms();
        if let Some(p) = args.job_price {
            self.job_prices.insert(ticker.clone(), p.0);
        }
        self.coins.insert(
            ticker.clone(),
            CoinRecord {
                account: account.clone(),
                creator: creator.clone(),
                created_ms: now,
                status: Status::Reserved,
                inactivity_ms: args.crown.inactivity_ms,
                colosseum_opt_in: args.colosseum_opt_in,
            },
        );

        let cfg = CoinConfig {
            ticker: ticker.clone(),
            creator: creator.clone(),
            launch: args.launch,
            crown: args.crown,
            persona: args.persona,
            colosseum_opt_in: args.colosseum_opt_in,
            arena: self.arena.clone(),
            crown_collection: self.crown.clone(),
            wrap: self.wrap.clone(),
            dcl: self.dcl.clone(),
            vault_code_hash: self.vault_code_hash,
            created_ms: now,
        };
        let metadata = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: args.name.trim().to_string(),
            symbol: ticker.to_uppercase(),
            icon: Some(args.icon_data_url),
            reference: None,
            reference_hash: None,
            decimals: DECIMALS,
        };
        let init_args = json!({ "cfg": cfg, "metadata": metadata, "profile": { "description": args.description, "socials": args.socials } });

        // CreateAccount + Transfer + UseGlobalContract(hash) + new(): one receipt, no AddKey -> no access keys ever (I2).
        Promise::new(account)
            .create_account()
            .transfer(funding)
            .use_global_contract(CryptoHash::from(self.coin_code_hash))
            .function_call("new", init_args.to_string().into_bytes(), NearToken::from_yoctonear(0), NEW_GAS)
            .then(
                Self::ext(root)
                    .with_static_gas(CB_GAS)
                    .with_unused_gas_weight(1) // the callback schedules the registration calls and the launch
                    .on_coin_created(ticker, creator, U128(required.as_yoctonear())),
            )
    }

    #[private]
    pub fn on_coin_created(&mut self, ticker: String, creator: AccountId, paid: U128, #[callback_result] result: Result<(), PromiseError>) -> PromiseOrValue<bool> {
        if result.is_err() {
            // Roll back: release the ticker and give the creator everything back.
            self.coins.remove(&ticker);
            events::emit("coin_create_failed", json!({ "ticker": ticker, "creator": creator, "refund": paid }));
            Promise::new(creator).transfer(NearToken::from_yoctonear(paid.0)).detach();
            return PromiseOrValue::Value(false);
        }
        let mut rec = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("missing reservation"));
        rec.status = Status::Created;
        let account = rec.account.clone();
        self.coins.insert(ticker.clone(), rec);
        // Start the launch with whatever gas is left (it pauses and resumes by itself if the chain does not fit).
        Promise::new(account)
            .function_call_weight("launch".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(60), GasWeight(1))
            .detach();
        PromiseOrValue::Promise(self.register(ticker))
    }

    /// Permissionless and idempotent: (re)send the arena registration and the PumpBag mint for a `Created` coin.
    pub fn retry_registration(&mut self, ticker: String) -> Promise {
        let rec = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("unknown ticker"));
        require!(rec.status == Status::Created, "coin is not awaiting registration");
        self.register(ticker)
    }

    #[private]
    pub fn on_registered(&mut self, ticker: String) -> bool {
        let ok = (0..env::promise_results_count()).all(|i| env::promise_result_checked(i, 64).is_ok());
        if !ok {
            events::emit("registration_failed", json!({ "ticker": ticker }));
            return false;
        }
        let mut rec = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("missing coin"));
        if rec.status == Status::Live {
            return true;
        }
        rec.status = Status::Live;
        self.coins.insert(ticker.clone(), rec.clone());
        // The arena knows the coin now: tell it the creator's job price (best effort; without it the coin uses the minimum).
        if let Some(price) = self.job_prices.get(&ticker).copied() {
            Promise::new(self.arena.clone())
                .function_call("set_job_price", json!({ "ticker": ticker, "price": near_sdk::json_types::U128(price) }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
                .detach();
        }
        self.feed.push(env::block_timestamp_ms(), name::COIN_CREATED, &ticker, rec.creator.as_str(), 0, rec.account.as_str());
        events::emit(name::COIN_CREATED, json!({ "ticker": ticker, "account": rec.account, "creator": rec.creator }));
        true
    }

    // ---------------------------------------------------------------- views

    pub fn get_coin(&self, ticker: String) -> Option<CoinRecord> {
        self.coins.get(&ticker).cloned()
    }

    pub fn list_coins(&self, from_index: u32, limit: u32) -> Vec<(String, CoinRecord)> {
        self.coins.iter().skip(from_index as usize).take(limit.min(50) as usize).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn coin_count(&self) -> u32 {
        self.coins.len()
    }

    pub fn ticker_available(&self, ticker: String) -> bool {
        normalize_ticker(&ticker).map(|t| self.coins.get(&t).is_none() && validate_account_len(&t, env::current_account_id().as_str()).is_ok()).unwrap_or(false)
    }

    /// Minimum attached deposit for an icon of `icon_len` bytes (data-URL length) and a first buy of up to
    /// `first_buy_max_near` yoctoNEAR (0 = none).
    pub fn required_deposit(&self, icon_len: u64, first_buy_max_near: Option<U128>) -> U128 {
        let fb = first_buy_max_near.map(|v| v.0).unwrap_or(0);
        U128(Self::required_total(Self::coin_funding(icon_len as usize, fb)).as_yoctonear())
    }

    pub fn feed_since(&self, id: u64, limit: u64) -> Vec<FeedItem> {
        self.feed.since(id, limit)
    }

    pub fn get_config(&self) -> serde_json::Value {
        json!({
            "arena": self.arena, "crown": self.crown, "wrap": self.wrap, "dcl": self.dcl,
            "coin_code_hash": self.coin_code_hash, "vault_code_hash": self.vault_code_hash,
        })
    }

    pub fn get_bounds(&self) -> serde_json::Value {
        use nme_common::*;
        json!({
            "start_point": [-POINT_LIMIT, POINT_LIMIT], "grid": DCL_GRID, "range_points": [RANGE_MIN_POINTS, RANGE_MAX_POINTS],
            "fee_tiers": DCL_FEE_TIERS, "supply_pool_bps": POOL_SUPPLY_BPS, "supply_crown_bps": CROWN_SUPPLY_BPS,
            "first_buy_max_bps": FIRST_BUY_MAX_BPS, "cliff_max_ms": CLIFF_MAX_MS, "vest_ms": [VEST_MIN_MS, VEST_MAX_MS],
            "inactivity_ms": [INACTIVITY_MIN_MS, INACTIVITY_MAX_MS], "max_icon_bytes": MAX_ICON_BYTES, "max_name_len": MAX_NAME_LEN,
            "max_persona_len": MAX_PERSONA_LEN,
        })
    }
}

impl Contract {
    fn coin_funding(icon_len: usize, first_buy_near: u128) -> NearToken {
        let storage = env::storage_byte_cost().saturating_mul(icon_len as u128 + COIN_STATE_BYTES);
        COIN_OPERATING.saturating_add(storage).saturating_add(NearToken::from_yoctonear(first_buy_near))
    }

    fn required_total(funding: NearToken) -> NearToken {
        funding.saturating_add(ARENA_REG_BUDGET).saturating_add(CROWN_MINT_BUDGET).saturating_add(FACTORY_STORAGE_BUDGET).saturating_add(ARENA_TOKEN_REG)
    }

    /// arena.register_coin  AND  crown.nft_mint  -> on_registered
    fn register(&self, ticker: String) -> Promise {
        let rec = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("unknown ticker"));
        let vault: AccountId = format!("crown.{}", rec.account).parse().unwrap();
        let reg = ext_arena::ext(self.arena.clone())
            .with_attached_deposit(ARENA_REG_BUDGET)
            .with_static_gas(REG_GAS)
            .with_unused_gas_weight(0)
            .register_coin(ticker.clone(), rec.account.clone(), vault.clone(), rec.colosseum_opt_in, env::block_timestamp_ms() + FIRST_CLAIM_DELAY_MS);
        let mint = ext_crown::ext(self.crown.clone())
            .with_attached_deposit(CROWN_MINT_BUDGET)
            .with_static_gas(REG_GAS)
            .with_unused_gas_weight(0)
            .nft_mint(ticker.clone(), rec.creator.clone(), rec.account.clone(), vault, rec.inactivity_ms);
        let arena_on_coin = Promise::new(rec.account.clone()).function_call(
            "storage_deposit",
            format!(r#"{{"account_id":"{}","registration_only":true}}"#, self.arena).into_bytes(),
            ARENA_TOKEN_REG,
            Gas::from_tgas(10),
        );
        reg.and(mint).and(arena_on_coin).then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_registered(ticker))
    }
}
