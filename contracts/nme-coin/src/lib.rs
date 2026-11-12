//! nme-coin: one contract per coin (`<ticker>.<root>`), global code by hash, no access keys.
//! NEP-141/145/148 + minimal single-sided launch on Rhea DCL (docs/11 §3, docs/spikes/S12.md).
//! No admin, no pause, no tax, no transfer hooks (I1/I2). The launch position can never be reduced by this
//! contract: the only DCL call that touches liquidity is `remove_liquidity` with `amount: "0"` (fee claim).
use near_contract_standards::fungible_token::events::{FtBurn, FtMint};
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::serde_json;
use near_sdk::{assert_one_yocto, env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue};
use nme_common::events::{emit_raw, name};
use nme_common::ext::{ext_arena, ext_ft, ext_wrap};
use nme_common::{dcl_pool_id, supply_split, CoinConfig, DCL_FEE_TIERS};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const DCL_REGISTRATION: NearToken = NearToken::from_millinear(500);
const DCL_POOL_DEPOSIT: NearToken = NearToken::from_millinear(100);
const VAULT_FUNDING: NearToken = NearToken::from_millinear(20);
const WRAP_STORAGE_DEPOSIT: NearToken = NearToken::from_yoctonear(1_250_000_000_000_000_000_000);
/// NEAR the coin keeps untouched when forwarding pool fees (storage + safety).
const OPERATING_RESERVE: NearToken = NearToken::from_millinear(50);
/// How many failed `create_pool` attempts before the chain parks and waits for `launch()`.
const MAX_CREATE_FAILURES: u8 = 3;
/// A dispatched step normally calls back within seconds. While one is in flight `launch()` is refused (a duplicated
/// step could e.g. register on DCL twice); if the callback never arrives this lapses.
const STEP_IN_FLIGHT_MS: u64 = 120_000;
const CB_GAS: Gas = Gas::from_tgas(10);

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Vault,
    Register,
    PlanPool,
    CreatePool { tier: u32 },
    Zap,
    Position,
    BuyWrap,
    Buy,
    /// Unwrap the unused first-buy wNEAR (its own step: a native transfer scheduled in the same call would be
    /// charged before the unwrapped NEAR exists).
    BuyUnwrap { amount: U128 },
    BuyReturn { amount: U128 },
    Reclaim,
    Announce,
}

impl Step {
    /// Static gas reserved for the step's call(s) (measured burn is far lower; see docs/spikes/S12.md).
    fn gas(&self) -> Gas {
        Gas::from_tgas(match self {
            Step::Vault => 25,
            Step::Register => 12,
            Step::PlanPool => 12,
            Step::CreatePool { .. } => 35,
            Step::Zap => 50,
            Step::Position => 12,
            Step::BuyWrap => 15,
            // An exact-output swap runs the swap, the output ft_transfer and its resolves inside DCL: needs ~150.
            Step::Buy => 150,
            Step::BuyUnwrap { .. } => 15,
            Step::BuyReturn { .. } => 10,
            Step::Reclaim => 15,
            Step::Announce => 12,
        })
    }
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum Launch {
    Creating { step: Step },
    Live { pool_id: String, lpt_id: String },
    /// Every DCL fee tier was taken at another price: nothing was launched; the creator can `refund_failed`.
    Failed,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Profile {
    pub description: String,
    pub socials: String,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    cfg: CoinConfig,
    profile: Profile,
    launch: Launch,
    coin_is_x: bool,
    tier: Option<u32>,
    pool_id: Option<String>,
    lpt_id: Option<String>,
    create_failures: u8,
    /// Block time (ms) until which a dispatched launch step counts as in flight (0 = none).
    busy_until_ms: u64,
    /// NEAR-side pool fees received but not yet forwarded to the arena.
    pending_fee_near: u128,
    fees_claimed_near: u128,
    fees_burned: u128,
    refunded: bool,
}

#[near]
impl Contract {
    /// Called by the factory in the batch that creates the account (nobody can front-run it). Mints the fixed
    /// supply once; moves are internal accounting. `launch()` then builds the pool.
    #[init]
    pub fn new(cfg: CoinConfig, metadata: FungibleTokenMetadata, profile: Profile) -> Self {
        let me = env::current_account_id();
        let parent = env::predecessor_account_id();
        require!(me.as_str().ends_with(&format!(".{parent}")), "only the parent factory can initialize a coin");
        require!(cfg.launch.validate().is_ok(), "invalid launch params");
        require!(cfg.crown.validate().is_ok(), "invalid crown params");
        require!(cfg.persona.validate().is_ok(), "invalid persona");
        metadata.assert_valid();
        require!(metadata.decimals == nme_common::DECIMALS, "decimals must be 18");

        let split = supply_split();
        let vault: AccountId = format!("crown.{me}").parse().unwrap();
        let mut token = FungibleToken::new(b"t".to_vec());
        token.internal_register_account(&me);
        token.internal_register_account(&cfg.dcl);
        token.internal_register_account(&vault);
        // DCL pays the creator's first buy out with a plain ft_transfer, which needs a registered receiver.
        token.internal_register_account(&cfg.creator);
        token.internal_deposit(&me, split.pool.0);
        token.internal_deposit(&vault, split.crown.0);
        FtMint { owner_id: &me, amount: split.pool, memo: Some("launch: pool side") }.emit();
        FtMint { owner_id: &vault, amount: split.crown, memo: Some("launch: Crown allocation") }.emit();
        let coin_is_x = me.as_str() < cfg.wrap.as_str();
        Self {
            token,
            metadata: LazyOption::new(b"m".to_vec(), Some(&metadata)),
            cfg,
            profile,
            launch: Launch::Creating { step: Step::Vault },
            coin_is_x,
            tier: None,
            pool_id: None,
            lpt_id: None,
            create_failures: 0,
            busy_until_ms: 0,
            pending_fee_near: 0,
            fees_claimed_near: 0,
            fees_burned: 0,
            refunded: false,
        }
    }

    // ------------------------------------------------------------------ launch

    /// Permissionless. Starts the launch, or continues it after a failed step or a chain that ran out of gas.
    /// Attach ~300 TGas to run as much of the chain as possible.
    pub fn launch(&mut self) -> Promise {
        let step = match &self.launch {
            Launch::Creating { step } => step.clone(),
            _ => env::panic_str("not launching"),
        };
        require!(
            dispatch_allowed(self.busy_until_ms, env::block_timestamp_ms()),
            "a launch step is in flight; try again in 2 minutes if it never completes"
        );
        self.run_step(step)
    }

    #[private]
    pub fn on_step(&mut self, step: Step) -> PromiseOrValue<bool> {
        let current = match &self.launch {
            Launch::Creating { step: s } if *s == step => s.clone(),
            _ => return PromiseOrValue::Value(false), // stale callback
        };
        self.busy_until_ms = 0;
        let result = env::promise_result_checked(0, 16_384);
        match self.handle(&current, result) {
            Out::Next(next) => {
                emit_raw(name::LAUNCH_STEP, &format!(r#"{{"ticker":"{}","step":{},"ok":true}}"#, self.cfg.ticker, step_json(&current)));
                self.launch = Launch::Creating { step: next.clone() };
                let remaining = env::prepaid_gas().as_gas().saturating_sub(env::used_gas().as_gas());
                if remaining >= next.gas().as_gas() + CB_GAS.as_gas() + Gas::from_tgas(20).as_gas() {
                    PromiseOrValue::Promise(self.run_step(next))
                } else {
                    // Out of gas for the rest of the chain: anyone continues with `launch()`.
                    emit_raw(name::LAUNCH_STEP, &format!(r#"{{"ticker":"{}","paused_before":{},"remaining_tgas":{}}}"#, self.cfg.ticker, step_json(&next), remaining / 1_000_000_000_000));
                    PromiseOrValue::Value(true)
                }
            }
            Out::Done => {
                let pool_id = self.pool_id.clone().unwrap_or_default();
                let lpt_id = self.lpt_id.clone().unwrap_or_default();
                emit_raw(name::LAUNCH_STEP, &format!(r#"{{"ticker":"{}","step":{},"ok":true}}"#, self.cfg.ticker, step_json(&current)));
                self.launch = Launch::Live { pool_id: pool_id.clone(), lpt_id: lpt_id.clone() };
                let tier = self.tier.map(|t| t.to_string()).unwrap_or_else(|| "null".into());
                emit_raw(
                    name::LAUNCH_FINALIZED,
                    &format!(
                        r#"{{"ticker":"{}","pool_id":"{pool_id}","lpt_id":"{lpt_id}","fee_tier":{tier},"supply_in_pool":"{}"}}"#,
                        self.cfg.ticker,
                        supply_split().pool.0
                    ),
                );
                PromiseOrValue::Value(true)
            }
            Out::Stay(why) => {
                emit_raw(name::LAUNCH_STEP, &format!(r#"{{"ticker":"{}","step":{},"ok":false,"reason":{}}}"#, self.cfg.ticker, step_json(&current), jq(&why)));
                PromiseOrValue::Value(false)
            }
            Out::Fail(why) => {
                self.launch = Launch::Failed;
                emit_raw(name::LAUNCH_FAILED, &format!(r#"{{"ticker":"{}","reason":{}}}"#, self.cfg.ticker, jq(&why)));
                PromiseOrValue::Value(false)
            }
        }
    }

    /// Launch failed (every fee tier squatted): the creator gets back the NEAR the coin still holds, once.
    /// This is the only path that sends the coin's NEAR anywhere, and it exists only in the `Failed` state.
    pub fn refund_failed(&mut self) -> Promise {
        require!(self.launch == Launch::Failed, "launch did not fail");
        require!(!self.refunded, "already refunded");
        self.refunded = true;
        // First take the DCL registration NEAR back (best effort), then send everything spare to the creator.
        Promise::new(self.cfg.dcl.clone())
            .function_call("storage_withdraw", br#"{"amount":null}"#.to_vec(), ONE_YOCTO, Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_failed_reclaimed())
    }

    #[private]
    pub fn on_failed_reclaimed(&mut self) -> Promise {
        let storage = env::storage_byte_cost().saturating_mul(env::storage_usage() as u128);
        let spare = env::account_balance().saturating_sub(storage).saturating_sub(NearToken::from_millinear(10));
        emit_raw(name::LAUNCH_FAILED, &format!(r#"{{"ticker":"{}","refund":"{}","to":"{}"}}"#, self.cfg.ticker, spare.as_yoctonear(), self.cfg.creator));
        Promise::new(self.cfg.creator.clone()).transfer(spare)
    }

    // ------------------------------------------------------------------ fees

    /// Permissionless (driven by `tick`). Releases only accrued pool fees (`remove_liquidity` with amount "0"):
    /// the token side arrives by `ft_transfer` and is burned (D-31); the NEAR side arrives as native NEAR and is
    /// forwarded to the arena, which splits it Crown vault / treasury / protocol.
    pub fn claim_fees(&mut self) -> Promise {
        let lpt = match &self.launch {
            Launch::Live { lpt_id, .. } => lpt_id.clone(),
            _ => env::panic_str("launch is not live"),
        };
        let me = env::current_account_id();
        Promise::new(self.cfg.dcl.clone())
            .function_call(
                "remove_liquidity",
                format!(r#"{{"lpt_id":"{lpt}","amount":"0","min_amount_x":"0","min_amount_y":"0"}}"#).into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(80),
            )
            .then(Self::ext(me).with_static_gas(Gas::from_tgas(40)).with_unused_gas_weight(1).on_claimed())
    }

    #[private]
    pub fn on_claimed(&mut self) -> PromiseOrValue<U128> {
        let bytes = match env::promise_result_checked(0, 256) {
            Ok(b) => b,
            Err(_) => {
                emit_raw(name::FEES_CLAIMED, &format!(r#"{{"ticker":"{}","ok":false}}"#, self.cfg.ticker));
                return PromiseOrValue::Value(U128(0));
            }
        };
        let Ok((fx, fy)) = serde_json::from_slice::<(U128, U128)>(&bytes) else {
            emit_raw(name::FEES_CLAIMED, &format!(r#"{{"ticker":"{}","ok":false,"reason":"bad result"}}"#, self.cfg.ticker));
            return PromiseOrValue::Value(U128(0));
        };
        let (token_fee, near_fee) = if self.coin_is_x { (fx.0, fy.0) } else { (fy.0, fx.0) };
        let me = env::current_account_id();
        let burn = token_fee.min(self.token.ft_balance_of(me.clone()).0);
        if burn > 0 {
            self.token.internal_withdraw(&me, burn);
            self.fees_burned += burn;
            FtBurn { owner_id: &me, amount: U128(burn), memo: Some("pool fees") }.emit();
        }
        self.pending_fee_near += near_fee;
        self.fees_claimed_near += near_fee;
        emit_raw(
            name::FEES_CLAIMED,
            &format!(r#"{{"ticker":"{}","ok":true,"token_fee_burned":"{burn}","near_fee":"{near_fee}"}}"#, self.cfg.ticker),
        );
        self.forward_fees()
    }

    /// Forwards pending NEAR fees to the arena if the coin can spare them; otherwise they wait for the next claim.
    pub fn forward_pending_fees(&mut self) -> PromiseOrValue<U128> {
        require!(matches!(self.launch, Launch::Live { .. }), "launch is not live");
        self.forward_fees()
    }

    /// Returns the amount that actually reached the arena.
    #[private]
    pub fn on_forwarded(&mut self, amount: U128) -> U128 {
        if env::promise_result_checked(0, 64).is_err() {
            self.pending_fee_near += amount.0; // the arena refused: keep it pending
            emit_raw(name::FEES_CLAIMED, &format!(r#"{{"ticker":"{}","forward_failed":"{}"}}"#, self.cfg.ticker, amount.0));
            return U128(0);
        }
        amount
    }

    // ------------------------------------------------------------------ token

    /// Burn your own tokens. Nobody can burn anyone else's.
    #[payable]
    pub fn burn(&mut self, amount: U128) {
        assert_one_yocto();
        require!(amount.0 > 0, "amount must be positive");
        let who = env::predecessor_account_id();
        self.token.internal_withdraw(&who, amount.0);
        FtBurn { owner_id: &who, amount, memo: None }.emit();
        emit_raw(name::BURN, &format!(r#"{{"ticker":"{}","account":"{who}","amount":"{}"}}"#, self.cfg.ticker, amount.0));
    }

    // ------------------------------------------------------------------ views

    pub fn get_config(&self) -> CoinConfig {
        self.cfg.clone()
    }

    pub fn get_profile(&self) -> Profile {
        self.profile.clone()
    }

    pub fn get_launch(&self) -> Launch {
        self.launch.clone()
    }

    pub fn get_pool(&self) -> PoolSummary {
        PoolSummary { pool_id: self.pool_id.clone(), fee_tier: self.tier, lpt_id: self.lpt_id.clone(), coin_is_x: self.coin_is_x }
    }

    pub fn get_fee_stats(&self) -> FeeStats {
        FeeStats { pending_fee_near: U128(self.pending_fee_near), fees_claimed_near: U128(self.fees_claimed_near), fees_burned: U128(self.fees_burned) }
    }

    /// Everything a UI, brain or router needs about this coin in one call.
    pub fn get_summary(&self) -> Summary {
        let supply = self.token.ft_total_supply().0;
        Summary {
            ticker: self.cfg.ticker.clone(),
            account: env::current_account_id(),
            vault: self.vault_id(),
            creator: self.cfg.creator.clone(),
            launch: self.launch.clone(),
            pool: self.get_pool(),
            supply: U128(supply),
            burned: U128(nme_common::TOTAL_SUPPLY.saturating_sub(supply)),
            persona: self.cfg.persona.clone(),
            fees: FeeSummary { pending_near: U128(self.pending_fee_near), claimed_near: U128(self.fees_claimed_near), burned_tokens: U128(self.fees_burned) },
        }
    }

    pub fn vault_account(&self) -> AccountId {
        self.vault_id()
    }
}

/// What a step's result means for the chain.
enum Out {
    Next(Step),
    Done,
    /// The step did not complete; stay put (retry with `launch()`).
    Stay(String),
    /// The launch cannot succeed.
    Fail(String),
}

/// The fields of a DCL `get_pools` entry that the launch plan reads (DCL returns more; serde ignores the rest).
#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct PoolView {
    current_point: i64,
    total_liquidity: String,
    total_x: String,
    total_y: String,
}

#[near(serializers = [json])]
pub struct PoolSummary {
    pub pool_id: Option<String>,
    pub fee_tier: Option<u32>,
    pub lpt_id: Option<String>,
    pub coin_is_x: bool,
}

#[near(serializers = [json])]
pub struct FeeStats {
    pub pending_fee_near: U128,
    pub fees_claimed_near: U128,
    pub fees_burned: U128,
}

#[near(serializers = [json])]
pub struct FeeSummary {
    pub pending_near: U128,
    pub claimed_near: U128,
    pub burned_tokens: U128,
}

#[near(serializers = [json])]
pub struct Summary {
    pub ticker: String,
    pub account: AccountId,
    pub vault: AccountId,
    pub creator: AccountId,
    pub launch: Launch,
    pub pool: PoolSummary,
    pub supply: U128,
    pub burned: U128,
    pub persona: nme_common::Persona,
    pub fees: FeeSummary,
}

/// JSON string literal for a static message (no quotes/backslashes/control chars expected; escaped defensively).
fn jq(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn step_json(step: &Step) -> String {
    serde_json::to_string(step).unwrap_or_else(|_| "null".into())
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct Row {
    lpt_id: String,
    pool_id: String,
}

impl Contract {
    fn vault_id(&self) -> AccountId {
        format!("crown.{}", env::current_account_id()).parse().unwrap()
    }

    fn callback(&self, step: &Step) -> Promise {
        Self::ext(env::current_account_id()).with_static_gas(CB_GAS).with_unused_gas_weight(1).on_step(step.clone())
    }

    fn plan_ids(&self) -> Vec<String> {
        DCL_FEE_TIERS.iter().map(|t| dcl_pool_id(env::current_account_id().as_str(), self.cfg.wrap.as_str(), *t)).collect()
    }

    /// Interprets a step's result and decides the next move.
    fn handle(&mut self, step: &Step, result: Result<Vec<u8>, PromiseError>) -> Out {
        let me = env::current_account_id();
        match step {
            Step::Vault => match result {
                Ok(_) => Out::Next(Step::Register),
                Err(_) => Out::Stay("vault creation failed".into()),
            },
            Step::Register => match result {
                Ok(_) => Out::Next(Step::PlanPool),
                Err(_) => Out::Stay("DCL registration failed".into()),
            },
            Step::PlanPool => {
                let Ok(bytes) = result else { return Out::Stay("get_pools failed".into()) };
                let Ok(pools) = serde_json::from_slice::<Vec<Option<PoolView>>>(&bytes) else { return Out::Stay("bad get_pools result".into()) };
                match plan_tier(&pools, self.cfg.launch.start_point) {
                    Plan::Create(i) => Out::Next(Step::CreatePool { tier: DCL_FEE_TIERS[i] }),
                    Plan::Adopt(i) => {
                        self.tier = Some(DCL_FEE_TIERS[i]);
                        self.pool_id = Some(self.plan_ids()[i].clone());
                        Out::Next(Step::Zap)
                    }
                    Plan::None => Out::Fail("every DCL fee tier is taken at another price".into()),
                }
            }
            Step::CreatePool { tier } => match result {
                Ok(bytes) => {
                    let want = dcl_pool_id(me.as_str(), self.cfg.wrap.as_str(), *tier);
                    match serde_json::from_slice::<String>(&bytes) {
                        Ok(id) if id == want => {
                            self.tier = Some(*tier);
                            self.pool_id = Some(id);
                            Out::Next(Step::Zap)
                        }
                        _ => Out::Stay("unexpected pool id".into()),
                    }
                }
                Err(_) => {
                    // Most likely E405 (someone created it first): look again.
                    self.create_failures += 1;
                    if self.create_failures < MAX_CREATE_FAILURES {
                        Out::Next(Step::PlanPool)
                    } else {
                        Out::Stay("create_pool keeps failing".into())
                    }
                }
            },
            Step::Zap => match result {
                Ok(bytes) => match serde_json::from_slice::<U128>(&bytes) {
                    Ok(used) if used.0 == supply_split().pool.0 => Out::Next(Step::Position),
                    _ => Out::Stay("DCL did not take the full pool side".into()),
                },
                Err(_) => Out::Stay("HotZap transfer failed".into()),
            },
            Step::Position => {
                let Ok(bytes) = result else { return Out::Stay("list_liquidities failed".into()) };
                let Ok(rows) = serde_json::from_slice::<Vec<Row>>(&bytes) else { return Out::Stay("bad list_liquidities result".into()) };
                let pid = self.pool_id.clone().unwrap_or_default();
                match rows.into_iter().find(|r| r.pool_id == pid) {
                    Some(r) => {
                        self.lpt_id = Some(r.lpt_id);
                        Out::Next(if self.cfg.launch.first_buy.is_some() { Step::BuyWrap } else { Step::Reclaim })
                    }
                    None => Out::Stay("position not found".into()),
                }
            }
            // The first buy is best effort: any failure refunds the creator and the launch still completes.
            Step::BuyWrap => match result {
                Ok(_) => Out::Next(Step::Buy),
                Err(_) => {
                    emit_raw("first_buy_skipped", &format!(r#"{{"ticker":"{}","reason":"wrap failed"}}"#, self.cfg.ticker));
                    Out::Next(Step::Reclaim)
                }
            },
            Step::Buy => {
                let max = self.cfg.launch.first_buy.as_ref().map(|f| f.max_near.0).unwrap_or(0);
                let unused = match result.ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()) {
                    Some(used) if used.0 > 0 && used.0 <= max => max - used.0,
                    _ => {
                        emit_raw("first_buy_skipped", &format!(r#"{{"ticker":"{}","reason":"swap refused"}}"#, self.cfg.ticker));
                        max
                    }
                };
                if unused > 0 {
                    Out::Next(Step::BuyUnwrap { amount: U128(unused) })
                } else {
                    Out::Next(Step::Reclaim)
                }
            }
            Step::BuyUnwrap { amount } => match result {
                Ok(_) => Out::Next(Step::BuyReturn { amount: *amount }),
                Err(_) => Out::Stay("unwrapping the unused first-buy wNEAR failed".into()),
            },
            Step::BuyReturn { .. } => match result {
                Ok(_) => Out::Next(Step::Reclaim),
                Err(_) => Out::Stay("returning the unused first-buy NEAR failed".into()),
            },
            // Best effort: the unused DCL registration NEAR comes back; a failure here must not block the launch.
            Step::Reclaim => Out::Next(Step::Announce),
            Step::Announce => match result {
                Ok(_) => Out::Done,
                Err(_) => Out::Stay("arena.record_launch failed".into()),
            },
        }
    }

    /// One launch step. Every step is a single receipt (or batch) whose result is checked in `on_step`.
    fn run_step(&mut self, step: Step) -> Promise {
        // A lost callback may have left the supply already in the position: don't send it twice.
        let step = if step == Step::Zap && self.token.ft_balance_of(env::current_account_id()).0 < supply_split().pool.0 {
            self.launch = Launch::Creating { step: Step::Position };
            Step::Position
        } else {
            step
        };
        self.busy_until_ms = env::block_timestamp_ms() + STEP_IN_FLIGHT_MS;
        let me = env::current_account_id();
        let cfg = self.cfg.clone();
        let p = match &step {
            Step::Vault => {
                let split = supply_split();
                let args = format!(
                    r#"{{"authority":"{}","coin":"{me}","arena":"{}","ticker":"{}","total":"{}","start_ms":{},"cliff_ms":{},"duration_ms":{}}}"#,
                    cfg.crown_collection, cfg.arena, cfg.ticker, split.crown.0, env::block_timestamp_ms(), cfg.crown.cliff_ms, cfg.crown.vest_ms
                );
                Promise::new(self.vault_id())
                    .create_account()
                    .transfer(VAULT_FUNDING)
                    .use_global_contract(near_sdk::CryptoHash::from(cfg.vault_code_hash))
                    .function_call("new", args.into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(15))
            }
            Step::Register => Promise::new(cfg.dcl.clone()).function_call(
                "storage_deposit",
                format!(r#"{{"account_id":"{me}","registration_only":false}}"#).into_bytes(),
                DCL_REGISTRATION,
                Gas::from_tgas(10),
            ),
            Step::PlanPool => Promise::new(cfg.dcl.clone()).function_call(
                "get_pools",
                format!(r#"{{"pool_ids":[{}]}}"#, self.plan_ids().iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",")).into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(10),
            ),
            Step::CreatePool { tier } => Promise::new(cfg.dcl.clone()).function_call(
                "create_pool",
                format!(r#"{{"token_a":"{me}","token_b":"{}","fee":{tier},"init_point":{}}}"#, cfg.wrap, cfg.launch.start_point).into_bytes(),
                DCL_POOL_DEPOSIT,
                Gas::from_tgas(30),
            ),
            Step::Zap => {
                let pool_id = self.pool_id.clone().unwrap_or_else(|| env::panic_str("missing pool id"));
                let (left, right) = cfg.launch.range(self.coin_is_x);
                let pool_side = supply_split().pool.0.to_string();
                let (ax, ay) = if self.coin_is_x { (pool_side.clone(), "0".to_string()) } else { ("0".to_string(), pool_side.clone()) };
                let (xt, yt) = if self.coin_is_x { (me.to_string(), cfg.wrap.to_string()) } else { (cfg.wrap.to_string(), me.to_string()) };
                let msg = format!(
                    r#"{{"HotZap":{{"swap_infos":[{{"pool_ids":["{pool_id}"],"input_token":"{xt}","output_token":"{yt}","amount_in":"0","min_output_amount":"0"}}],"add_liquidity_infos":[{{"pool_id":"{pool_id}","left_point":{left},"right_point":{right},"amount_x":"{ax}","amount_y":"{ay}","min_amount_x":"0","min_amount_y":"0"}}]}}}}"#
                );
                // Our own standard ft_transfer_call (NEP-141 resolve/refund for free): sender = this contract.
                ext_ft::ext(me.clone())
                    .with_attached_deposit(ONE_YOCTO)
                    .with_static_gas(step.gas())
                    .with_unused_gas_weight(0) // ext calls default to weight 1: they would eat the gas the chain needs
                    .ft_transfer_call(cfg.dcl.clone(), supply_split().pool, None, msg)
            }
            Step::Position => Promise::new(cfg.dcl.clone()).function_call(
                "list_liquidities",
                format!(r#"{{"account_id":"{me}"}}"#).into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(10),
            ),
            Step::BuyWrap => {
                let fb = cfg.launch.first_buy.clone().unwrap_or_else(|| env::panic_str("no first buy"));
                Promise::new(cfg.wrap.clone())
                    .function_call(
                        "storage_deposit",
                        format!(r#"{{"account_id":"{me}","registration_only":true}}"#).into_bytes(),
                        WRAP_STORAGE_DEPOSIT,
                        Gas::from_tgas(5),
                    )
                    .function_call("near_deposit", b"{}".to_vec(), NearToken::from_yoctonear(fb.max_near.0), Gas::from_tgas(5))
            }
            Step::Buy => {
                let fb = cfg.launch.first_buy.clone().unwrap_or_else(|| env::panic_str("no first buy"));
                let pool_id = self.pool_id.clone().unwrap_or_else(|| env::panic_str("missing pool id"));
                // Exactly `tokens` out, paying at most `max_near`; DCL returns the used input, the rest comes back.
                let msg = format!(
                    r#"{{"SwapByOutput":{{"pool_ids":["{pool_id}"],"output_token":"{me}","output_amount":"{}","swap_out_recipient":"{}"}}}}"#,
                    fb.tokens.0, cfg.creator
                );
                ext_wrap::ext(cfg.wrap.clone())
                    .with_attached_deposit(ONE_YOCTO)
                    .with_static_gas(step.gas())
                    .with_unused_gas_weight(0)
                    .ft_transfer_call(cfg.dcl.clone(), fb.max_near, None, msg)
            }
            Step::BuyUnwrap { amount } => Promise::new(cfg.wrap.clone()).function_call(
                "near_withdraw",
                format!(r#"{{"amount":"{}"}}"#, amount.0).into_bytes(),
                ONE_YOCTO,
                Gas::from_tgas(10),
            ),
            Step::BuyReturn { amount } => Promise::new(cfg.creator.clone()).transfer(NearToken::from_yoctonear(amount.0)),
            Step::Reclaim => Promise::new(cfg.dcl.clone()).function_call(
                "storage_withdraw",
                br#"{"amount":null}"#.to_vec(),
                ONE_YOCTO,
                Gas::from_tgas(10),
            ),
            Step::Announce => ext_arena::ext(cfg.arena.clone())
                .with_static_gas(step.gas())
                .with_unused_gas_weight(0)
                .record_launch(cfg.ticker.clone(), self.pool_id.clone().unwrap_or_default(), self.lpt_id.clone().unwrap_or_default()),
        };
        p.then(self.callback(&step))
    }

    /// Sends pending NEAR fees to the arena, keeping `OPERATING_RESERVE` plus the storage requirement.
    fn forward_fees(&mut self) -> PromiseOrValue<U128> {
        let amount = self.pending_fee_near.min(spare_balance());
        if amount == 0 {
            return PromiseOrValue::Value(U128(0));
        }
        self.pending_fee_near -= amount;
        PromiseOrValue::Promise(
            ext_arena::ext(self.cfg.arena.clone())
                .with_attached_deposit(NearToken::from_yoctonear(amount))
                .with_static_gas(Gas::from_tgas(20))
                .with_unused_gas_weight(0)
                .deposit_pool_fees(self.cfg.ticker.clone())
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(5)).on_forwarded(U128(amount))),
        )
    }
}

/// NEAR the coin can spare right now: balance minus its storage requirement minus the operating reserve.
fn spare_balance() -> u128 {
    let storage = env::storage_byte_cost().saturating_mul(env::storage_usage() as u128);
    env::account_balance().saturating_sub(storage).saturating_sub(OPERATING_RESERVE).as_yoctonear()
}

/// True when no launch step is currently in flight.
fn dispatch_allowed(busy_until_ms: u64, now_ms: u64) -> bool {
    now_ms >= busy_until_ms
}

#[derive(Debug, PartialEq)]
enum Plan {
    /// Index into `DCL_FEE_TIERS` of a tier with no pool yet.
    Create(usize),
    /// A pre-existing pool that is empty and sits exactly at our start point (pool squatting that costs nothing).
    Adopt(usize),
    None,
}

/// D-36: walk the fee tiers in order; use the first one that is free or adoptable.
fn plan_tier(pools: &[Option<PoolView>], start_point: i32) -> Plan {
    for (i, p) in pools.iter().enumerate().take(DCL_FEE_TIERS.len()) {
        let Some(p) = p else { return Plan::Create(i) };
        let empty = p.total_liquidity == "0" && p.total_x == "0" && p.total_y == "0";
        if p.current_point == start_point as i64 && empty {
            return Plan::Adopt(i);
        }
    }
    Plan::None
}

// ---------------------------------------------------------------------------------------------------
// NEP-141 / NEP-145 / NEP-148: plain delegation to the standard implementation. Nothing is overridden,
// so transfers can never be blocked, taxed or paused (I1).
// ---------------------------------------------------------------------------------------------------

#[near]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.token.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>, msg: String) -> PromiseOrValue<U128> {
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(&mut self, sender_id: AccountId, receiver_id: AccountId, amount: U128) -> U128 {
        let (used, burned) = self.token.internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        if burned > 0 {
            emit_raw(name::BURN, &format!(r#"{{"ticker":"{}","account":"{sender_id}","amount":"{burned}","reason":"refund_to_closed_account"}}"#, self.cfg.ticker));
        }
        used.into()
    }
}

#[near]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(&mut self, account_id: Option<AccountId>, registration_only: Option<bool>) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        // `force` burns the caller's own remaining balance when they close their account; keep supply exact.
        self.token.internal_storage_unregister(force).is_some()
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.get().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(point: i64, liq: &str, x: &str, y: &str) -> Option<PoolView> {
        Some(PoolView { current_point: point, total_liquidity: liq.into(), total_x: x.into(), total_y: y.into() })
    }

    #[test]
    fn plan_uses_the_first_free_tier() {
        let free = [None, None, None, None];
        assert_eq!(plan_tier(&free, 92_000), Plan::Create(0));
    }

    #[test]
    fn plan_skips_squatted_tiers() {
        let squatted_1 = [pool(5, "0", "0", "0"), None, None, None]; // wrong price, empty
        assert_eq!(plan_tier(&squatted_1, 92_000), Plan::Create(1));
        let funded = [pool(92_000, "9", "0", "0"), None, None, None]; // right price but has liquidity
        assert_eq!(plan_tier(&funded, 92_000), Plan::Create(1));
    }

    #[test]
    fn plan_adopts_an_empty_pool_at_our_price() {
        let p = [pool(92_000, "0", "0", "0"), None, None, None];
        assert_eq!(plan_tier(&p, 92_000), Plan::Adopt(0));
        let p = [pool(1, "0", "0", "0"), pool(92_000, "0", "0", "0"), None, None];
        assert_eq!(plan_tier(&p, 92_000), Plan::Adopt(1));
    }

    #[test]
    fn plan_fails_when_every_tier_is_squatted() {
        let all = [pool(1, "0", "0", "0"), pool(2, "0", "0", "0"), pool(3, "5", "0", "0"), pool(4, "0", "0", "7")];
        assert_eq!(plan_tier(&all, 92_000), Plan::None);
    }

    #[test]
    fn jq_quotes_and_escapes() {
        assert_eq!(jq("plain"), "\"plain\"");
        assert_eq!(jq("a\"b"), "\"a\\\"b\"");
        assert_eq!(jq("a\\b"), "\"a\\\\b\"");
        let v: String = serde_json::from_str(&jq("he said \"hi\" \\ ok")).unwrap();
        assert_eq!(v, "he said \"hi\" \\ ok");
    }

    #[test]
    fn step_json_matches_the_callback_encoding() {
        // the same JSON the `on_step(step)` callback argument uses
        assert_eq!(step_json(&Step::Vault), "\"Vault\"");
        assert_eq!(step_json(&Step::CreatePool { tier: 10_000 }), r#"{"CreatePool":{"tier":10000}}"#);
    }

    #[test]
    fn in_flight_guard() {
        assert!(dispatch_allowed(0, 5));
        assert!(!dispatch_allowed(1_000 + STEP_IN_FLIGHT_MS, 1_500));
        assert!(dispatch_allowed(1_000 + STEP_IN_FLIGHT_MS, 1_000 + STEP_IN_FLIGHT_MS));
    }

    #[test]
    fn plan_ids_cover_the_four_tiers_in_order() {
        let ids: Vec<String> = DCL_FEE_TIERS.iter().map(|t| dcl_pool_id("pepe.nme.testnet", "wrap.testnet", *t)).collect();
        assert_eq!(ids[0], "pepe.nme.testnet|wrap.testnet|10000");
        assert_eq!(ids[3], "pepe.nme.testnet|wrap.testnet|100");
    }

    /// I2/I3: the coin's code must contain exactly one `remove_liquidity` call, and it must be the fee claim
    /// (`amount: "0"`). Nothing in this contract can reduce the launch position.
    #[test]
    fn only_zero_amount_remove_liquidity_exists() {
        let src = include_str!("lib.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert_eq!(code.matches("\"remove_liquidity\"").count(), 1, "exactly one remove_liquidity call site");
        let at = code.find("\"remove_liquidity\"").unwrap();
        let call = &code[at..at + 260];
        assert!(call.contains(r#""amount":"0""#), "the single call must pass amount \"0\": {call}");
        for forbidden in ["add_liquidity\"", "burn_v_liquidity", "mint_v_liquidity", "cancel_order", "withdraw_asset"] {
            assert!(!code.contains(forbidden), "{forbidden} must not appear in the coin");
        }
    }
}
