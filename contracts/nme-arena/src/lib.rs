//! nme-arena v2 (M2): coin registry (native + outside), launch record, pool-fee split, buy-side router,
//! sell pass-through, war chests, permissionless `enlist`, and the buy-and-burn machinery (native `burn` or the
//! keyless graveyard). Colosseum rounds, jobs, defense and `tick` execution arrive in M3+ (docs/06).
//!
//! Money rules (I3): treasury and chest NEAR can only leave through `buy_and_burn`; there is no withdraw method.
//! Every value-moving flow debits its ledger first and restores it in a `#[private]` callback if a step fails.
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::serde_json::{self, json};
use near_sdk::store::{LookupMap, TreeMap, Vector};
use near_sdk::{env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};
use nme_common::events::{self, name};
use nme_common::ext::ext_vault;
use nme_common::math::{bps_of, mul_div};
use nme_common::{dcl_pool_id, parse_dcl_pool_id, Feed, FeedItem};

mod raids;
pub use raids::{Raid, RaidState};
mod proofs;
mod spoils;
pub use spoils::Spoils;

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
/// Smallest router buy (keeps dust out of the feed and the fee math meaningful).
const MIN_TRADE_NEAR: u128 = 10u128.pow(21); // 0.001 NEAR
/// Registration deposit on a token (NEP-145 typical minimum). Unused deposit is refunded by the token.
const REG_DEPOSIT: NearToken = NearToken::from_yoctonear(1_250_000_000_000_000_000_000);
/// What `enlist` needs attached: two registrations, entry storage and a margin. The excess is refunded.
const ENLIST_MIN_DEPOSIT: NearToken = NearToken::from_millinear(20);
const CB: Gas = Gas::from_tgas(30);

#[near(serializers = [json, borsh])]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Created by the factory: has a PumpBag vault, mintable supply, brain.
    Native,
    /// Any NEP-141 with a Rhea DCL pool against wNEAR, enlisted permissionlessly: wars and chests only.
    Outside,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct CoinEntry {
    pub kind: Kind,
    /// The token contract.
    pub account: AccountId,
    pub crown_vault: Option<AccountId>,
    pub colosseum_opt_in: bool,
    /// DCL pool id once launched / enlisted.
    pub pool_id: Option<String>,
    pub lpt_id: Option<String>,
    pub treasury_near: U128,
    /// War chest: funded by anyone, never withdrawable, spent only by buy-and-burn.
    pub chest_near: U128,
    pub launched: bool,
}

/// Due-index tasks (docs/10-AUTONOMY.md §4). Each re-checks its precondition when it runs, so a duplicate or a
/// race is harmless.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum Task {
    /// Calls `coin.claim_fees()` and schedules the next claim.
    ClaimFees { ticker: String },
    /// Starts the next Colosseum round if two funded coins can be paired; otherwise polls again later.
    StartRound,
    /// Settles the next pair of a round whose window has closed.
    SettleRound { round_id: u64 },
    /// Calls `coin.launch()` until the arena has recorded the coin's launch (a launch chain pauses when it runs low on gas).
    ResumeLaunch { ticker: String, tries: u32 },
    /// Buys and burns the coin with a delivered job's payment (own task: the burn chain needs a full transaction of gas).
    JobBurn { job_id: u64 },
    /// Deadline backstop: refunds a job whose brain never answered.
    RefundJob { job_id: u64 },
    /// Self-defense (docs/12 §4): looks at the pool, and when the price has dropped asks the brain (or the fixed rule).
    DefenseCheck { ticker: String },
    /// Buys and burns `bps` of the coin's treasury, decided by a validated defense decision.
    DefenseBurn { ticker: String, bps: u32 },
    /// Asks the coin's brain for one in-character line about something that just happened to it.
    Commentary { ticker: String, event: String, detail: String },
    /// One per PumpBag: when its deadline has passed, asks the PumpBag collection to forfeit it (anyone could; this makes it automatic).
    CrownForfeit { ticker: String },
    /// After a raid's window: burns its share of the coin's treasury and closes it (appended last: borsh order kept).
    RaidSettle { raid_id: u64 },
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct TickReport {
    /// Due tasks that ran.
    pub processed: u32,
    /// Of those, the ones that did real work (a poll that found nothing to do pays no bounty).
    pub progressed: u32,
    pub bounty: U128,
}

/// Gas a task needs (it spawns its own cross-contract work).
fn task_gas(t: &Task) -> Gas {
    Gas::from_tgas(match t {
        Task::StartRound => 25,
        Task::ClaimFees { .. } => 215,
        Task::SettleRound { .. } => 235,
        Task::ResumeLaunch { .. } => 265,
        Task::JobBurn { .. } => 235,
        Task::RefundJob { .. } => 20,
        Task::DefenseCheck { .. } => 200,
        Task::DefenseBurn { .. } => 235,
        Task::Commentary { .. } => 200,
        Task::CrownForfeit { .. } => 220,
        Task::RaidSettle { .. } => 235,
    })
}

/// Tasks that start a chain of cross-contract calls which finishes on the *leftover* gas of this transaction (gas
/// weights): such a task must be the only one of its kind in a tick, or the leftover is shared and the chain starves.
/// A `meme` delivery needs a real picture reference: an https url and a sha256.
fn valid_media(url: &str, hash: &str) -> bool {
    url.starts_with("https://") && url.len() <= 200 && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())
}

fn task_is_greedy(t: &Task) -> bool {
    !matches!(t, Task::StartRound | Task::RefundJob { .. })
}

/// How long to wait before looking again when a task found nothing to do.
const POLL_MS: u64 = 60_000;
/// Protocol-bucket balance a tick-started round needs for its storage (the bucket pays it, I3-safe: never a coin ledger).
const ROUND_STORAGE_RESERVE: u128 = 50_000_000_000_000_000_000_000;
const MAX_LAUNCH_TRIES: u32 = 40;

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct DueTask {
    pub due_ms: u64,
    pub seq: u64,
    pub task: Task,
}

/// Immutable config (set at init / migration).
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct ArenaConfig {
    pub bounty_per_task: U128,
    pub bounty_cap_per_tick: U128,
    pub inline_tick_min_gas_tgas: u64,
    pub price_check_interval_ms: u64,
    /// Split of fees (pool fees and router fees): PumpBag vault / coin treasury; the rest is the protocol bucket.
    pub fee_crown_bps: u16,
    pub fee_treasury_bps: u16,
    /// Delay between a coin's registration and its first fee-claim task.
    pub claim_interval_ms: u64,
    /// Router fee on BUYS only (sells are a fee-free pass-through). ≤ 200.
    pub router_fee_bps: u16,
    /// `enlist` refuses pools with less wNEAR-side liquidity than this (yocto).
    pub min_pool_near: U128,
    /// Buy-and-burn accepts this much slippage versus the DCL quote.
    pub burn_slippage_bps: u16,
}

/// Colosseum parameters, immutable (set at init / migration).
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct WarConfig {
    /// How long a round's scoring window stays open.
    pub round_duration_ms: u64,
    /// Minimum pause between a round's end and the next round's start.
    pub round_interval_ms: u64,
    /// Share of the weaker chest each side commits to a pair.
    pub chest_bps: u16,
    /// One wallet's counted buy volume is capped at this share of the pair's stake.
    pub wallet_cap_bps: u16,
    /// A side with fewer distinct scoring wallets makes the pair invalid (stakes return).
    pub min_unique: u32,
    /// A pair is only made if each side's stake is at least this much (yoctoNEAR).
    pub min_stake: U128,
    /// Pairs per round (each settles in its own call).
    pub max_pairs: u8,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum PairState {
    Pending,
    Settled,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Pair {
    pub a: String,
    pub b: String,
    /// Locked from each side's chest for the round.
    pub stake: U128,
    pub score_a: U128,
    pub score_b: U128,
    pub unique_a: u32,
    pub unique_b: u32,
    pub state: PairState,
    /// `a`, `b` or `none`, once settled.
    pub winner: String,
    pub note: String,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Round {
    pub id: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub pairs: Vec<Pair>,
    pub settled_pairs: u32,
}

/// Working Memes (docs/12 §3): immutable settings, fixed at init / migration.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct JobConfig {
    /// The OutLayer contract (`outlayer.testnet`).
    pub outlayer: AccountId,
    /// Brain worker, pinned by hash (I5): the wasm bytes at `brain_url` must hash to `brain_hash`.
    pub brain_url: String,
    pub brain_hash: String,
    /// Account that owns the encrypted secrets (the NEAR AI key) and the profile name.
    pub secrets_account: AccountId,
    pub secrets_profile: String,
    /// RPC the worker uses to read the coin's persona.
    pub rpc_url: String,
    /// Smallest accepted payment for a job (yoctoNEAR), including `outlayer_fee`.
    pub min_price: U128,
    /// Attached to `request_execution`; OutLayer refunds what it does not use to the payer.
    pub outlayer_fee: U128,
    /// Kept for the job record's storage (never credited to any ledger).
    pub record_reserve: U128,
    /// Share of the escrow kept by the protocol bucket (pays the brain's inference); the rest buys and burns the coin.
    pub cut_bps: u16,
    /// Unanswered jobs refund after this long.
    pub deadline_ms: u64,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    /// Sent to the brain, waiting for the OutLayer callback.
    Requested,
    /// The brain answered; the payment is queued for buy-and-burn.
    Delivered,
    /// The payment was handed to buy-and-burn.
    Burned,
    Refunded,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Job {
    pub id: u64,
    pub ticker: String,
    pub kind: String,
    /// sha256 of the prompt (hex). The prompt itself is in the `hire` transaction, not in state.
    pub prompt_hash: String,
    pub payer: AccountId,
    /// Escrowed NEAR (after the OutLayer fee and the record reserve).
    pub amount: U128,
    pub status: JobStatus,
    pub deadline_ms: u64,
}

/// What the brain delivered. The ring keeps the last `DELIVERY_RING` results (bounded storage).
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Delivery {
    pub seq: u64,
    pub job_id: u64,
    pub ticker: String,
    pub kind: String,
    pub text: String,
    pub model: String,
    /// NEAR AI chat id: `GET /v1/signature/{chat_id}` proves which model run produced the text.
    pub chat_id: String,
    pub ts_ms: u64,
}

/// What the brain worker prints (one JSON line), see workers/brain.
#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct BrainOut {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    refused: bool,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    chat_id: String,
    #[serde(default)]
    media_url: String,
    #[serde(default)]
    media_hash: String,
    #[serde(default)]
    request_hash: String,
    #[serde(default)]
    response_hash: String,
}

/// A delivery as the views return it: the stored text plus the picture of a `meme` job, if any.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct DeliveryOut {
    pub seq: u64,
    pub job_id: u64,
    pub ticker: String,
    pub kind: String,
    pub text: String,
    pub model: String,
    pub chat_id: String,
    pub ts_ms: u64,
    pub media_url: String,
    pub media_hash: String,
    /// Proof of Thought: sha256 of the exact NEAR AI request and response (empty for deliveries made before proofs).
    pub request_hash: String,
    pub response_hash: String,
}

/// Self-Defending Memes (docs/12 §4): immutable caps and timings. The AI only chooses *when*; these decide *how much*.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct DefenseConfig {
    /// Most of the treasury one action may spend (hundredths of a percent; ≤ 500 = 5%).
    pub per_action_bps: u16,
    /// Most of the treasury per 24 h (≤ 1500 = 15%).
    pub daily_bps: u16,
    pub cooldown_ms: u64,
    /// Time between checks of one coin.
    pub interval_ms: u64,
    /// The brain is only consulted when the price is at least this many DCL points below its rolling high.
    pub look_points: u32,
    /// The no-AI rule fires at this drop (2232 points = price ≤ 80% of the high).
    pub trigger_points: u32,
    /// After this many consecutive checks without a valid decision the fixed rule takes over (D-32).
    pub fallback_after: u32,
    /// A coin with less treasury than this is not defended.
    pub min_treasury: U128,
    /// Paid from the protocol bucket per brain consultation, within `fee_cap_per_day` (D-29).
    pub outlayer_fee: U128,
    pub fee_cap_per_day: U128,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, Default)]
pub struct DefenseState {
    pub high_point: i64,
    pub high_ms: u64,
    pub last_action_ms: u64,
    pub day_start_ms: u64,
    pub day_spent_bps: u32,
    /// Consecutive checks that produced no valid decision.
    pub misses: u32,
    /// A check is in flight until then (keeps one consultation per coin at a time).
    pub in_flight_until_ms: u64,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct DefenseOut {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    action: String,
    #[serde(default)]
    bps: u64,
    #[serde(default)]
    reason: String,
}

/// Brain commentary (docs/11 §5, I8): the brain may only append lines here. Immutable settings.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct CommentaryConfig {
    /// Tier: a coin with less treasury than this is silent.
    pub min_treasury: U128,
    /// Lines one coin may get per 24 h.
    pub per_day_per_coin: u32,
    /// Paid from the protocol bucket per line, within `fee_cap_per_day` (D-29).
    pub outlayer_fee: U128,
    pub fee_cap_per_day: U128,
}

/// One AI-written line. The ring keeps the last `COMMENTARY_RING`.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Commentary {
    pub seq: u64,
    pub ticker: String,
    /// What it is about: war_started, war_won, war_lost.
    pub event: String,
    pub text: String,
    pub model: String,
    pub chat_id: String,
    pub ts_ms: u64,
}

const COMMENTARY_RING: u64 = 64;
const MAX_COMMENTARY_BYTES: usize = 300;

/// The rolling high restarts when it is older than this.
const HIGH_WINDOW_MS: u64 = 3_600_000;
const DAY_MS: u64 = 86_400_000;
const IN_FLIGHT_MS: u64 = 600_000;

const DELIVERY_RING: u64 = 32;
const MAX_PROMPT_BYTES: usize = 500;
/// No creator can price a job above 10 NEAR.
const MAX_JOB_PRICE: u128 = 10_000_000_000_000_000_000_000_000;
const MAX_TARGET_BYTES: usize = 40;
const MAX_TEXT_BYTES: usize = 1000;
const JOB_KINDS: [&str; 4] = ["roast", "shill", "lore", "meme"];

/// Per wallet, per round, per coin: router flow that decides the wallet's score.
#[near(serializers = [borsh])]
#[derive(Clone, Default)]
pub struct WalletFlow {
    pub near_bought: u128,
    pub tokens_bought: u128,
    pub tokens_sold: u128,
}

/// Buys below this are not scored: each scoring wallet costs one storage entry, paid from the router fee.
const MIN_SCORED_BUY: u128 = 250_000_000_000_000_000_000_000;
const MAX_ROSTER: u32 = 64;

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    factory: AccountId,
    wrap: AccountId,
    dcl: AccountId,
    /// Keyless account with no code: tokens sent here are out of circulation for good (I3).
    graveyard: AccountId,
    cfg: ArenaConfig,
    coins: LookupMap<String, CoinEntry>,
    protocol_near: u128,
    due: TreeMap<(u64, u64), Task>,
    next_seq: u64,
    feed: Feed,
    /// Tokens of a coin that are inside the arena only because a seller's transfer is in flight.
    /// Buy-and-burn burns `balance - in_flight`, never a seller's tokens.
    sell_in_flight: LookupMap<AccountId, u128>,
    // ---- Colosseum (M3)
    war: WarConfig,
    /// Every coin that may fight (native coins at registration, outside coins at enlist). Bounded by MAX_ROSTER.
    roster: Vector<String>,
    rounds: LookupMap<u64, Round>,
    last_round_id: u64,
    next_round_ms: u64,
    /// coin key -> (round id, pair index) while that coin is locked in a round.
    active: LookupMap<String, (u64, u32)>,
    /// (round, coin, wallet) -> what the wallet bought and sold through the router during the round.
    wallet_flow: LookupMap<(u64, String, AccountId), WalletFlow>,
    /// A `StartRound` task is already in the due index (keeps the polling to one task).
    start_pending: bool,
    // ---- Working Memes (M5)
    brain: JobConfig,
    jobs: LookupMap<u64, Job>,
    next_job_id: u64,
    deliveries: LookupMap<u64, Delivery>,
    next_delivery: u64,
    // ---- Self-Defending Memes (M6)
    defense: DefenseConfig,
    defense_state: LookupMap<String, DefenseState>,
    fee_day_start_ms: u64,
    fee_day_spent: u128,
    // ---- Commentary (M6b)
    commentary_cfg: CommentaryConfig,
    commentary: LookupMap<u64, Commentary>,
    next_commentary: u64,
    /// ticker -> (day start ms, lines so far today)
    commentary_day: LookupMap<String, (u64, u32)>,
    commentary_fee_day_start_ms: u64,
    commentary_fee_day_spent: u128,
    // ---- PumpBag deadlines (M4)
    /// The PumpBag collection (`crown.<root>`): the only caller of `crown_deadline`.
    crown: AccountId,
    /// ticker -> (deadline ms, a `CrownForfeit` task is queued). One task per PumpBag: heartbeats only move the deadline.
    crown_deadlines: LookupMap<String, (u64, bool)>,
    // ---- Intent Memes (M7)
    /// The gateway (`gateway.<root>`): the only caller of `buy_for`.
    gateway: AccountId,
    // ---- per-coin job prices (M5b)
    /// ticker -> price a coin's creator fixed at launch (via the factory); coins without one use `brain.min_price`.
    job_price: LookupMap<String, U128>,
    /// delivery ring slot -> (picture url, sha256 hex) for `meme` jobs; the text of the delivery is in `deliveries`.
    delivery_media: LookupMap<u64, (String, String)>,
}

// The M7 layout, read once by `migrate`.
mod v9 {
    use super::*;

    #[near(serializers = [borsh])]
    pub struct Contract {
        pub factory: AccountId,
        pub wrap: AccountId,
        pub dcl: AccountId,
        pub graveyard: AccountId,
        pub cfg: ArenaConfig,
        pub coins: LookupMap<String, CoinEntry>,
        pub protocol_near: u128,
        pub due: TreeMap<(u64, u64), Task>,
        pub next_seq: u64,
        pub feed: Feed,
        pub sell_in_flight: LookupMap<AccountId, u128>,
        pub war: WarConfig,
        pub roster: Vector<String>,
        pub rounds: LookupMap<u64, Round>,
        pub last_round_id: u64,
        pub next_round_ms: u64,
        pub active: LookupMap<String, (u64, u32)>,
        pub wallet_flow: LookupMap<(u64, String, AccountId), WalletFlow>,
        pub start_pending: bool,
        pub brain: JobConfig,
        pub jobs: LookupMap<u64, Job>,
        pub next_job_id: u64,
        pub deliveries: LookupMap<u64, Delivery>,
        pub next_delivery: u64,
        pub defense: DefenseConfig,
        pub defense_state: LookupMap<String, DefenseState>,
        pub fee_day_start_ms: u64,
        pub fee_day_spent: u128,
        pub commentary_cfg: CommentaryConfig,
        pub commentary: LookupMap<u64, Commentary>,
        pub next_commentary: u64,
        pub commentary_day: LookupMap<String, (u64, u32)>,
        pub commentary_fee_day_start_ms: u64,
        pub commentary_fee_day_spent: u128,
        pub crown: AccountId,
        pub crown_deadlines: LookupMap<String, (u64, bool)>,
        pub gateway: AccountId,
    }
}

// ---- typed results of the cross-contract views / calls the arena reads ----

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct MetaView {
    spec: String,
    decimals: u8,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct PoolView {
    token_x: String,
    token_y: String,
    state: String,
    total_x: String,
    total_y: String,
    #[serde(default)]
    current_point: i64,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct QuoteOut {
    amount: U128,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct SellMsg {
    sell: SellArgs,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct SellArgs {
    min_out: U128,
    /// Gateway only (D-57): the wallet owner key (`sol:…`/`evm:…`) the sale belongs to. The proceeds are delivered to the
    /// gateway as wNEAR (so it can measure them exactly) and the war score goes to the owner's own virtual fighter.
    #[serde(default)]
    owner: Option<String>,
}

#[near]
impl Contract {
    #[init]
    pub fn new(factory: AccountId, wrap: AccountId, dcl: AccountId, graveyard: AccountId, cfg: ArenaConfig, war: WarConfig, brain: JobConfig, defense: DefenseConfig, commentary: CommentaryConfig, crown: AccountId, gateway: AccountId) -> Self {
        Self::check_cfg(&cfg);
        Self::check_war(&war);
        Self::check_brain(&brain);
        Self::check_defense(&defense);
        let mut c = Self {
            factory,
            wrap,
            dcl,
            graveyard,
            cfg,
            coins: LookupMap::new(b"c"),
            protocol_near: 0,
            due: TreeMap::new(b"d"),
            next_seq: 0,
            feed: Feed::new(b"f"),
            sell_in_flight: LookupMap::new(b"s"),
            war,
            roster: Vector::new(b"r"),
            rounds: LookupMap::new(b"w"),
            last_round_id: 0,
            next_round_ms: 0,
            active: LookupMap::new(b"a"),
            wallet_flow: LookupMap::new(b"b"),
            start_pending: false,
            brain,
            jobs: LookupMap::new(b"j"),
            next_job_id: 1,
            deliveries: LookupMap::new(b"y"),
            next_delivery: 0,
            defense,
            defense_state: LookupMap::new(b"e"),
            fee_day_start_ms: 0,
            fee_day_spent: 0,
            commentary_cfg: commentary,
            commentary: LookupMap::new(b"m"),
            next_commentary: 0,
            commentary_day: LookupMap::new(b"n"),
            commentary_fee_day_start_ms: 0,
            commentary_fee_day_spent: 0,
            crown,
            crown_deadlines: LookupMap::new(b"k"),
            gateway,
            job_price: LookupMap::new(b"p"),
            delivery_media: LookupMap::new(b"q"),
        };
        c.schedule_start(env::block_timestamp_ms());
        c
    }

    /// Same layout, one field: the RPC the brain worker reads coin settings from (and uploads media through). The public
    /// FastNear endpoint stalls requests from OutLayer's enclaves, which made every brain call time out. Keyless RPCs only:
    /// this URL is public in `get_brain_config`.
    #[private]
    #[init(ignore_state)]
    pub fn migrate_rpc(rpc_url: String) -> Self {
        let mut c: Self = env::state_read().unwrap_or_else(|| env::panic_str("no state to migrate"));
        Self::check_rpc(&rpc_url);
        c.brain.rpc_url = rpc_url;
        c
    }

    /// Same layout, one pin: the brain worker's URL and sha256. Used when the worker changes (2026-11-11: faster model with
    /// thinking off, because the old one ran past OutLayer's ~30 s limit). Nothing else moves.
    #[private]
    #[init(ignore_state)]
    pub fn migrate_brain(brain_url: String, brain_hash: String) -> Self {
        let mut c: Self = env::state_read().unwrap_or_else(|| env::panic_str("no state to migrate"));
        require!(brain_url.starts_with("https://") && brain_url.ends_with(&format!("{brain_hash}.wasm")), "brain url must be https and end with <hash>.wasm");
        c.brain.brain_url = brain_url;
        c.brain.brain_hash = brain_hash;
        Self::check_brain(&c.brain);
        c
    }

    fn check_rpc(url: &str) {
        require!(url.starts_with("https://") && url.len() <= 120 && url.chars().all(|ch| ch.is_ascii_graphic()), "rpc url must be a short https url");
        require!(!url.contains('?') && !url.contains('@'), "rpc url must not carry credentials or query parameters");
    }

    /// One-time upgrade from the M7 layout: adds per-coin job prices (coins registered before this use the global minimum).
    #[private]
    #[init(ignore_state)]
    pub fn migrate(brain_url: String, brain_hash: String) -> Self {
        let old: v9::Contract = env::state_read().unwrap_or_else(|| env::panic_str("no M7 state to migrate"));
        // The brain worker gained the `meme` image path, so it has a new hash; the pin is replaced here, once.
        let mut brain = old.brain;
        brain.brain_url = brain_url;
        brain.brain_hash = brain_hash;
        Self::check_brain(&brain);
        Self {
            factory: old.factory,
            wrap: old.wrap,
            dcl: old.dcl,
            graveyard: old.graveyard,
            cfg: old.cfg,
            coins: old.coins,
            protocol_near: old.protocol_near,
            due: old.due,
            next_seq: old.next_seq,
            feed: old.feed,
            sell_in_flight: old.sell_in_flight,
            war: old.war,
            roster: old.roster,
            rounds: old.rounds,
            last_round_id: old.last_round_id,
            next_round_ms: old.next_round_ms,
            active: old.active,
            wallet_flow: old.wallet_flow,
            start_pending: old.start_pending,
            brain,
            jobs: old.jobs,
            next_job_id: old.next_job_id,
            deliveries: old.deliveries,
            next_delivery: old.next_delivery,
            defense: old.defense,
            defense_state: old.defense_state,
            fee_day_start_ms: old.fee_day_start_ms,
            fee_day_spent: old.fee_day_spent,
            commentary_cfg: old.commentary_cfg,
            commentary: old.commentary,
            next_commentary: old.next_commentary,
            commentary_day: old.commentary_day,
            commentary_fee_day_start_ms: old.commentary_fee_day_start_ms,
            commentary_fee_day_spent: old.commentary_fee_day_spent,
            crown: old.crown,
            crown_deadlines: old.crown_deadlines,
            gateway: old.gateway,
            job_price: LookupMap::new(b"p"),
            delivery_media: LookupMap::new(b"q"),
        }
    }

    // ================================================================ registry

    /// Factory only. Idempotent for the same (ticker, account) so the factory can retry. The attached deposit pays
    /// the storage; the excess is refunded to the factory.
    #[payable]
    pub fn register_coin(&mut self, ticker: String, account: AccountId, crown_vault: AccountId, colosseum_opt_in: bool, first_claim_due_ms: u64) -> Promise {
        require!(env::predecessor_account_id() == self.factory, "only the factory can register coins");
        let attached = env::attached_deposit();
        if let Some(existing) = self.coins.get(&ticker) {
            require!(existing.account == account, "ticker registered to a different account");
            return Promise::new(self.factory.clone()).transfer(attached);
        }
        let before = env::storage_usage();
        self.coins.insert(
            ticker.clone(),
            CoinEntry {
                kind: Kind::Native,
                account: account.clone(),
                crown_vault: Some(crown_vault),
                colosseum_opt_in,
                pool_id: None,
                lpt_id: None,
                treasury_near: U128(0),
                chest_near: U128(0),
                launched: false,
            },
        );
        self.enqueue(first_claim_due_ms, Task::ClaimFees { ticker: ticker.clone() });
        if self.roster.len() < MAX_ROSTER {
            self.roster.push(ticker.clone());
        }
        self.enqueue(env::block_timestamp_ms() + POLL_MS, Task::ResumeLaunch { ticker: ticker.clone(), tries: 0 });
        self.enqueue(env::block_timestamp_ms() + self.defense.interval_ms, Task::DefenseCheck { ticker: ticker.clone() });
        self.push_feed("coin_registered", &ticker, account.as_str(), 0, "");
        // `store` collections write lazily: flush so the measured delta is the real storage cost.
        self.coins.flush();
        self.due.flush();
        self.feed.flush();
        self.roster.flush();
        let used = env::storage_usage().saturating_sub(before);
        let cost = env::storage_byte_cost().saturating_mul(used as u128);
        require!(attached >= cost, "attached deposit does not cover registration storage");
        Promise::new(self.factory.clone()).transfer(attached.saturating_sub(cost))
    }

    /// Registered coin only, once: the launch is complete (pool created, whole supply in the locked position).
    /// Kept deliberately cheap: the published coin reserves only 12 TGas for this call. (The factory registers the
    /// arena on each new coin so it can receive sells and burn.)
    pub fn record_launch(&mut self, ticker: String, pool_id: String, lpt_id: String) {
        let mut entry = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("unknown ticker"));
        require!(env::predecessor_account_id() == entry.account, "only the coin can record its launch");
        require!(!entry.launched, "launch already recorded");
        entry.launched = true;
        entry.pool_id = Some(pool_id.clone());
        entry.lpt_id = Some(lpt_id.clone());
        self.coins.insert(ticker.clone(), entry);
        self.push_feed(name::LAUNCH_FINALIZED, &ticker, env::predecessor_account_id().as_str(), 0, &format!("pool={pool_id}"));
        events::emit(name::LAUNCH_FINALIZED, json!({ "ticker": ticker, "pool_id": pool_id, "lpt_id": lpt_id }));
    }

    /// Permissionless: bring any NEP-141 with a Rhea DCL pool against wNEAR into the arena (wars and chests only).
    /// `pool_id` is the DCL pool id `x|y|fee` (tokens sorted). Attach at least 0.02 NEAR; the excess is refunded.
    /// Checks: the token answers `ft_metadata`, the pool exists, is Running, pairs the token with wNEAR and holds at
    /// least `min_pool_near` on the NEAR side; then the arena and the graveyard are registered on the token.
    #[payable]
    pub fn enlist(&mut self, token: AccountId, pool_id: String) -> Promise {
        require!(env::attached_deposit() >= ENLIST_MIN_DEPOSIT, "attach at least 0.02 NEAR for registrations and storage");
        let key = token.to_string();
        require!(self.coins.get(&key).is_none(), "already enlisted");
        require!(self.key_of(&token) == key, "NME coins are registered by the factory, not enlisted");
        let (x, y, fee) = parse_dcl_pool_id(&pool_id).unwrap_or_else(|| env::panic_str("pool_id must be x|y|fee"));
        require!(
            pool_id == dcl_pool_id(token.as_str(), self.wrap.as_str(), fee) && (x == token.as_str() || y == token.as_str()),
            "pool does not pair this token with wNEAR"
        );
        let caller = env::predecessor_account_id();
        let attached = env::attached_deposit();
        Promise::new(token.clone())
            .function_call("ft_metadata", b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .and(Promise::new(self.dcl.clone()).function_call("get_pool", json!({ "pool_id": pool_id }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10)))
            .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).on_enlist_checked(token, pool_id, caller, U128(attached.as_yoctonear())))
    }

    #[private]
    pub fn on_enlist_checked(&mut self, token: AccountId, pool_id: String, caller: AccountId, attached: U128) -> PromiseOrValue<bool> {
        let meta = env::promise_result_checked(0, 16_384).ok().and_then(|b| serde_json::from_slice::<MetaView>(&b).ok());
        let pool = env::promise_result_checked(1, 16_384).ok().and_then(|b| serde_json::from_slice::<Option<PoolView>>(&b).ok()).flatten();
        let verdict = self.judge_enlist(meta.as_ref(), pool.as_ref());
        if let Err(why) = verdict {
            return self.enlist_refund(caller, attached.0, &token, why);
        }
        let me = env::current_account_id();
        let reg = |who: &AccountId| json!({ "account_id": who, "registration_only": true }).to_string().into_bytes();
        PromiseOrValue::Promise(
            Promise::new(token.clone())
                .function_call("storage_deposit", reg(&me), REG_DEPOSIT, Gas::from_tgas(10))
                .function_call("storage_deposit", reg(&self.graveyard), REG_DEPOSIT, Gas::from_tgas(10))
                .then(Self::ext(me).with_static_gas(CB).with_unused_gas_weight(1).on_enlist_registered(token, pool_id, caller, attached)),
        )
    }

    #[private]
    pub fn on_enlist_registered(&mut self, token: AccountId, pool_id: String, caller: AccountId, attached: U128) -> PromiseOrValue<bool> {
        if env::promise_result_checked(0, 256).is_err() {
            return self.enlist_refund(caller, attached.0, &token, "the token refused the arena/graveyard registration".into());
        }
        let key = token.to_string();
        if self.coins.get(&key).is_some() {
            return self.enlist_refund(caller, attached.0, &token, "already enlisted".into());
        }
        if self.roster.len() >= MAX_ROSTER {
            return self.enlist_refund(caller, attached.0, &token, "the Colosseum roster is full".into());
        }
        let before = env::storage_usage();
        self.coins.insert(
            key.clone(),
            CoinEntry {
                kind: Kind::Outside,
                account: token.clone(),
                crown_vault: None,
                colosseum_opt_in: true,
                pool_id: Some(pool_id.clone()),
                lpt_id: None,
                treasury_near: U128(0),
                chest_near: U128(0),
                launched: true,
            },
        );
        self.roster.push(key.clone());
        self.push_feed(name::COIN_ENLISTED, &key, caller.as_str(), 0, &format!("pool={pool_id}"));
        self.coins.flush();
        self.feed.flush();
        self.roster.flush();
        let storage = env::storage_byte_cost().saturating_mul(env::storage_usage().saturating_sub(before) as u128);
        events::emit(name::COIN_ENLISTED, json!({ "token": token, "pool_id": pool_id, "by": caller }));
        let back = attached.0.saturating_sub(REG_DEPOSIT.as_yoctonear() * 2).saturating_sub(storage.as_yoctonear());
        if back > 0 {
            Promise::new(caller).transfer(NearToken::from_yoctonear(back)).detach();
        }
        PromiseOrValue::Value(true)
    }

    // ================================================================ chests

    /// Anyone adds NEAR to a coin's war chest. **No method withdraws a chest** (I3): it is spent only by
    /// buy-and-burn.
    #[payable]
    pub fn fund_chest(&mut self, key: String) {
        self.inline_tick(Gas::from_tgas(10));
        let mut entry = self.coins.get(&key).cloned().unwrap_or_else(|| env::panic_str("unknown coin"));
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach NEAR to fund the chest");
        entry.chest_near = U128(entry.chest_near.0 + amount);
        let total = entry.chest_near.0;
        self.coins.insert(key.clone(), entry);
        let who = env::predecessor_account_id();
        self.push_feed(name::CHEST_FUNDED, &key, who.as_str(), amount, &format!("chest={total}"));
        events::emit(name::CHEST_FUNDED, json!({ "key": key, "by": who, "amount": U128(amount), "chest": U128(total) }));
        self.inline_start();
    }

    // ================================================================ fees

    /// Registered coin only: the NEAR side of claimed pool fees. Split PumpBag vault / coin treasury / protocol bucket.
    #[payable]
    pub fn deposit_pool_fees(&mut self, ticker: String) {
        let entry = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("unknown ticker"));
        require!(env::predecessor_account_id() == entry.account, "only the coin can deposit its fees");
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "no fees attached");
        self.credit_fee(&ticker, amount, name::FEES_CLAIMED);
    }

    /// Vault only: after a forfeit the PumpBag's accrued fees go to the coin treasury.
    #[payable]
    pub fn deposit_forfeited_fees(&mut self, ticker: String) {
        let mut entry = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("unknown ticker"));
        require!(Some(env::predecessor_account_id()) == entry.crown_vault, "only the coin's PumpBag vault can deposit forfeited fees");
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "no fees attached");
        entry.treasury_near = U128(entry.treasury_near.0 + amount);
        self.coins.insert(ticker.clone(), entry);
        self.push_feed("fees_forfeited", &ticker, env::predecessor_account_id().as_str(), amount, "to treasury");
        events::emit("fees_forfeited", json!({ "ticker": ticker, "amount": U128(amount) }));
    }

    #[private]
    pub fn on_vault_deposit(&mut self, ticker: String, crown: U128) {
        if env::promise_result_checked(0, 64).is_err() {
            if let Some(mut e) = self.coins.get(&ticker).cloned() {
                e.treasury_near = U128(e.treasury_near.0 + crown.0);
                self.coins.insert(ticker.clone(), e);
            }
            events::emit("vault_deposit_failed", json!({ "ticker": ticker, "amount": crown }));
        }
    }

    // ================================================================ router: buy

    /// Buy `key` (a ticker, or a token account for an outside coin) with the attached NEAR. The router fee
    /// (`router_fee_bps`, buys only) is held until the swap result is known; the output goes straight to the buyer
    /// (who must be registered on the token). `min_out` is enforced by DCL; if it cannot be met, everything is
    /// refunded.
    #[payable]
    pub fn buy(&mut self, key: String, min_out: U128) -> Promise {
        self.inline_tick(Gas::from_tgas(215));
        let user = env::predecessor_account_id();
        self.buy_inner(user, key, min_out, None)
    }

    /// Gateway only: the same buy for a NEAR beneficiary (the NEAR value of a Solana deposit that the gateway verified).
    /// The tokens, a refund, and the war-score credit all go to the beneficiary, never to the gateway.
    #[payable]
    pub fn buy_for(&mut self, beneficiary: AccountId, key: String, min_out: U128) -> Promise {
        require!(env::predecessor_account_id() == self.gateway, "only the gateway can buy for a beneficiary");
        self.buy_inner(beneficiary, key, min_out, None)
    }

    /// Gateway only (D-57): the same buy for a wallet owner (`sol:…` / `evm:…`) who has no NEAR account. The tokens arrive at the
    /// gateway; the arena reads the gateway's balance difference (exact because the gateway runs one token operation at a time)
    /// and reports it with `gateway.credit_tokens(owner, coin, amount)`. Every refund goes back through `gateway.credit_near(owner)`.
    /// The war score is booked under the owner's own virtual fighter (`wallet_key`), so wallet users are separate fighters.
    /// `require_war: true` (JoinWar) refuses unless a war is open for the coin and the buy is big enough to score.
    /// The chain runs detached: a panic here is synchronous (nothing moved, the deposit comes back to the gateway), anything
    /// later is settled by the two gateway calls above, never by a failure of this call.
    #[payable]
    pub fn buy_for_wallet(&mut self, owner: String, key: String, min_out: U128, require_war: Option<bool>) {
        require!(env::predecessor_account_id() == self.gateway, "only the gateway can buy for a wallet");
        require!(!owner.is_empty() && owner.len() <= 80, "bad owner key");
        if require_war == Some(true) {
            require!(self.scoring_open(&key, env::attached_deposit().as_yoctonear()), "no war is open for this coin, or the buy is too small to score");
        }
        let gateway = self.gateway.clone();
        self.buy_inner(gateway, key, min_out, Some(owner)).detach();
    }

    #[private]
    pub fn on_buy_checked(&mut self, user: AccountId, key: String, gross: U128, min_out: U128, owner: Option<String>) -> PromiseOrValue<U128> {
        let bal_before = env::promise_result_checked(1, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0).unwrap_or(0);
        let registered = env::promise_result_checked(0, 1024)
            .ok()
            .map(|b| serde_json::from_slice::<serde_json::Value>(&b).map(|v| !v.is_null()).unwrap_or(false))
            .unwrap_or(false);
        let entry = match self.coins.get(&key).cloned() {
            Some(e) if registered && e.pool_id.is_some() && e.launched => e,
            _ => {
                events::emit("trade_refused", json!({ "key": key, "side": "buy", "account": user, "near": gross, "reason": "buyer not registered on the token, or coin not live" }));
                if let Some(o) = &owner {
                    let token = self.coins.get(&key).map(|e| e.account.clone()).unwrap_or_else(env::current_account_id);
                    self.credit_wallet_tokens(o, &token, 0).detach(); // nothing bought: releases the gateway's token lock
                }
                return PromiseOrValue::Promise(self.pay_user(&user, &owner, gross.0));
            }
        };
        let fee = bps_of(gross.0, self.cfg.router_fee_bps);
        let net = gross.0 - fee;
        let msg = swap_msg(entry.pool_id.as_deref().unwrap(), entry.account.as_str(), min_out.0, Some(&user));
        PromiseOrValue::Promise(
            Promise::new(self.wrap.clone())
                .function_call("near_deposit", b"{}".to_vec(), NearToken::from_yoctonear(net), Gas::from_tgas(10))
                .function_call(
                    "ft_transfer_call",
                    json!({ "receiver_id": self.dcl, "amount": U128(net), "msg": msg }).to_string().into_bytes(),
                    ONE_YOCTO,
                    Gas::from_tgas(100),
                )
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(50)).with_unused_gas_weight(1).on_buy(user, key, gross, U128(fee), U128(net), U128(bal_before), owner)),
        )
    }

    #[private]
    pub fn on_buy(&mut self, user: AccountId, key: String, gross: U128, fee: U128, net: U128, bal_before: U128, owner: Option<String>) -> U128 {
        let used = env::promise_result_checked(0, 256).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0.min(net.0)).unwrap_or(0);
        // Fee only on what was actually bought.
        let fee_actual = if used == net.0 { fee.0 } else { mul_div(fee.0, used, net.0).unwrap_or(0) };
        let unused = net.0 - used;
        if used > 0 {
            if owner.is_some() || self.scoring_open(&key, used) {
                // A war is on (or the buyer is a wallet owner whose tokens the gateway must credit): read the buyer's balance again to
                // learn the exact tokens bought, then score and book the fee.
                let entry = self.coins.get(&key).cloned().unwrap();
                Promise::new(entry.account)
                    .function_call("ft_balance_of", json!({ "account_id": user }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(35)).with_unused_gas_weight(1).on_buy_scored(user.clone(), key.clone(), U128(used), U128(fee_actual), bal_before, owner.clone()))
                    .detach();
            } else {
                self.finish_buy(&key, &user, used, fee_actual, 0);
            }
        } else {
            events::emit("trade_refused", json!({ "key": key, "side": "buy", "account": user, "near": gross }));
            if let Some(o) = &owner {
                let token = self.coins.get(&key).map(|e| e.account.clone()).unwrap_or_else(env::current_account_id);
                self.credit_wallet_tokens(o, &token, 0).detach(); // nothing bought: releases the gateway's token lock
            }
        }
        let refund = gross.0 - used - fee_actual;
        if refund > 0 {
            if unused > 0 {
                // The refused part came back to the arena as wNEAR: unwrap first, transfer in the next step.
                Promise::new(self.wrap.clone())
                    .function_call("near_withdraw", json!({ "amount": U128(unused) }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(10))
                    .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).with_unused_gas_weight(1).on_refund(user, U128(refund), owner))
                    .detach();
            } else {
                self.pay_user(&user, &owner, refund).detach();
            }
        }
        U128(used)
    }

    /// Second half of a buy during a war: `tokens bought = balance after - balance before`.
    #[private]
    pub fn on_buy_scored(&mut self, user: AccountId, key: String, used: U128, fee_actual: U128, bal_before: U128, owner: Option<String>) {
        let after = env::promise_result_checked(0, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0).unwrap_or(0);
        let tokens = after.saturating_sub(bal_before.0);
        // A wallet owner is its own fighter: the score key is derived from the owner key, never the gateway account.
        let fighter = owner.as_deref().map(wallet_account).unwrap_or_else(|| user.clone());
        let cost = if tokens > 0 { self.score_buy(&key, &fighter, used.0, tokens) } else { 0 };
        self.finish_buy(&key, &fighter, used.0, fee_actual.0, cost);
        if let Some(o) = &owner {
            let token = self.coins.get(&key).map(|e| e.account.clone()).unwrap_or_else(env::current_account_id);
            self.credit_wallet_tokens(o, &token, tokens).detach();
        }
    }

    #[private]
    pub fn on_refund(&mut self, user: AccountId, amount: U128, owner: Option<String>) {
        if env::promise_result_checked(0, 64).is_err() {
            // The unwrap failed: the NEAR is still wNEAR in the arena. Nothing is lost, but nothing is refunded here.
            events::emit("refund_failed", json!({ "account": user, "amount": amount }));
            return;
        }
        self.pay_user(&user, &owner, amount.0).detach();
    }

    // ================================================================ router: sell (pass-through)

    /// NEP-141 receiver. A coin's tokens arriving with `msg = {"sell":{"min_out":"<yoctoNEAR>"}}` are swapped on the
    /// coin's DCL pool and the NEAR proceeds go straight to the seller. Sells carry no router fee. Anything the swap
    /// does not use is returned to the seller (via the unused-amount rule of NEP-141).
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        self.inline_tick(Gas::from_tgas(160));
        let token = env::predecessor_account_id();
        let key = self.key_of(&token);
        let entry = match self.coins.get(&key).cloned() {
            Some(e) if e.account == token && e.pool_id.is_some() && e.launched => e,
            _ => return PromiseOrValue::Value(amount), // not a known coin: refuse, tokens go back
        };
        let Ok(parsed) = serde_json::from_str::<SellMsg>(&msg) else { return PromiseOrValue::Value(amount) };
        if amount.0 == 0 {
            return PromiseOrValue::Value(amount);
        }
        // A wallet owner's sale (D-57) is honored only from the gateway: it sells for the owner's virtual fighter and keeps the
        // proceeds as wNEAR at the gateway, which measures them exactly.
        let (seller, swap) = match &parsed.sell.owner {
            Some(o) if sender_id == self.gateway && !o.is_empty() && o.len() <= 80 => (
                wallet_account(o),
                swap_msg_keep_wnear(entry.pool_id.as_deref().unwrap(), self.wrap.as_str(), parsed.sell.min_out.0, &sender_id),
            ),
            Some(_) => return PromiseOrValue::Value(amount), // only the gateway may sell for a wallet owner
            None => (sender_id.clone(), swap_msg(entry.pool_id.as_deref().unwrap(), self.wrap.as_str(), parsed.sell.min_out.0, Some(&sender_id))),
        };
        let inflight = self.sell_in_flight.get(&token).copied().unwrap_or(0);
        self.sell_in_flight.insert(token.clone(), inflight + amount.0);
        PromiseOrValue::Promise(
            Promise::new(token.clone())
                .function_call(
                    "ft_transfer_call",
                    json!({ "receiver_id": self.dcl, "amount": amount, "msg": swap }).to_string().into_bytes(),
                    ONE_YOCTO,
                    Gas::from_tgas(100),
                )
                .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).on_sold(token, seller, key, amount)),
        )
    }

    #[private]
    pub fn on_sold(&mut self, token: AccountId, seller: AccountId, key: String, amount: U128) -> U128 {
        let used = env::promise_result_checked(0, 256).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0.min(amount.0)).unwrap_or(0);
        let inflight = self.sell_in_flight.get(&token).copied().unwrap_or(0);
        self.sell_in_flight.insert(token, inflight.saturating_sub(amount.0));
        if used > 0 {
            self.score_sell(&key, &seller, used);
            self.push_feed(name::TRADE, &key, seller.as_str(), 0, &format!("sell tokens={used}"));
            events::emit(name::TRADE, json!({ "key": key, "side": "sell", "account": seller, "tokens": U128(used) }));
        } else {
            events::emit("trade_refused", json!({ "key": key, "side": "sell", "account": seller, "tokens": amount }));
        }
        U128(amount.0 - used) // unused: NEP-141 returns it to the seller
    }

    // ================================================================ buy & burn (the only way treasury/chest NEAR leaves)

    #[allow(dead_code)] // first real caller is the Colosseum settlement (M3); until then only the `test-hooks` build
    /// Debits `near` from the chest (or treasury) of `key` and runs the chain: DCL quote → swap to the arena →
    /// read the arena's balance of the token → burn (native `burn`, else the graveyard). Every failure restores the ledger.
    fn buy_and_burn(&mut self, key: &str, src: &str, from_chest: bool, near: u128) -> Promise {
        self.buy_and_burn_x(key, src, from_chest, near, None)
    }

    /// `spoils = Some(round)`: a won war's buy; half of the bought tokens go to that round's fighters (spoils.rs).
    fn buy_and_burn_x(&mut self, key: &str, src: &str, from_chest: bool, near: u128, spoils: Option<u64>) -> Promise {
        let target = self.coins.get(key).cloned().unwrap_or_else(|| env::panic_str("unknown coin"));
        require!(target.launched && target.pool_id.is_some(), "coin has no pool");
        let mut source = self.coins.get(src).cloned().unwrap_or_else(|| env::panic_str("unknown source coin"));
        let ledger = if from_chest { &mut source.chest_near } else { &mut source.treasury_near };
        require!(near > 0 && ledger.0 >= near, "not enough NEAR in the ledger");
        ledger.0 -= near; // debit first; restored to `src` by `bb_restore` on any failure
        self.coins.insert(src.to_string(), source);
        let pool_id = target.pool_id.clone().unwrap();
        let token = target.account.clone();
        Promise::new(self.dcl.clone())
            .function_call(
                "quote",
                json!({ "pool_ids": [pool_id], "input_token": self.wrap, "output_token": token, "input_amount": U128(near), "tag": null }).to_string().into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(15),
            )
            .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).bb_quoted(key.to_string(), src.to_string(), from_chest, U128(near), spoils))
    }

    /// A buy-and-burn never burns more than the quote plus this margin, so a burn can never reach a large stash that
    /// is only transiently in the arena (a seller's tokens between arrival and `ft_on_transfer`).
    const BURN_CAP_MARGIN_BPS: u16 = 2_000;

    #[private]
    pub fn bb_quoted(&mut self, key: String, src: String, from_chest: bool, near: U128, spoils: Option<u64>) -> PromiseOrValue<bool> {
        let Some(quote) = env::promise_result_checked(0, 1024).ok().and_then(|b| serde_json::from_slice::<QuoteOut>(&b).ok()) else {
            return self.bb_fail(&src, from_chest, near.0, "quote failed");
        };
        let min_out = bps_of(quote.amount.0, 10_000 - self.cfg.burn_slippage_bps);
        let max_burn = quote.amount.0.saturating_add(bps_of(quote.amount.0, Self::BURN_CAP_MARGIN_BPS));
        let entry = self.coins.get(&key).cloned().unwrap();
        let msg = swap_msg(entry.pool_id.as_deref().unwrap(), entry.account.as_str(), min_out, Some(&env::current_account_id()));
        PromiseOrValue::Promise(
            Promise::new(self.wrap.clone())
                .function_call("near_deposit", b"{}".to_vec(), NearToken::from_yoctonear(near.0), Gas::from_tgas(10))
                .function_call(
                    "ft_transfer_call",
                    json!({ "receiver_id": self.dcl, "amount": near, "msg": msg }).to_string().into_bytes(),
                    ONE_YOCTO,
                    Gas::from_tgas(100),
                )
                .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).bb_swapped(key, src, from_chest, near, U128(max_burn), spoils)),
        )
    }

    #[private]
    pub fn bb_swapped(&mut self, key: String, src: String, from_chest: bool, near: U128, max_burn: U128, spoils: Option<u64>) -> PromiseOrValue<bool> {
        let used = env::promise_result_checked(0, 256).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0.min(near.0)).unwrap_or(0);
        let unused = near.0 - used;
        if unused > 0 {
            // Refused/partial: the unused part is back in the arena as wNEAR. Unwrap it, then restore the ledger.
            Promise::new(self.wrap.clone())
                .function_call("near_withdraw", json!({ "amount": U128(unused) }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(10))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).bb_restore(src.clone(), from_chest, U128(unused)))
                .detach();
        }
        if used == 0 {
            events::emit("buyback_failed", json!({ "key": key, "reason": "swap refused", "near": near }));
            return PromiseOrValue::Value(false);
        }
        let token = self.coins.get(&key).map(|e| e.account.clone()).unwrap();
        PromiseOrValue::Promise(
            Promise::new(token)
                .function_call("ft_balance_of", json!({ "account_id": env::current_account_id() }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5))
                .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).bb_balance(key, U128(used), max_burn, spoils)),
        )
    }

    /// Burn the arena's tokens of this coin (at most `max_burn`), except tokens that belong to a seller whose transfer is in flight.
    #[private]
    pub fn bb_balance(&mut self, key: String, spent: U128, max_burn: U128, spoils: Option<u64>) -> PromiseOrValue<bool> {
        let bal = env::promise_result_checked(0, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0).unwrap_or(0);
        let entry = self.coins.get(&key).cloned().unwrap();
        // Never a seller's tokens in flight, never tokens reserved for war fighters (spoils).
        let mut burnable = bal
            .saturating_sub(self.sell_in_flight.get(&entry.account).copied().unwrap_or(0))
            .saturating_sub(spoils::reserved(&key))
            .min(max_burn.0);
        if let Some(rid) = spoils {
            let keep = spoils::fill(rid, &key, burnable);
            burnable -= keep;
            if keep > 0 {
                self.push_feed("spoils_reserved", &key, "", keep, &format!("round={rid}"));
                events::emit("spoils_reserved", json!({ "round": rid, "coin": key, "tokens": U128(keep) }));
            }
        }
        if burnable == 0 {
            events::emit("buyback_failed", json!({ "key": key, "reason": "nothing to burn", "near": spent }));
            return PromiseOrValue::Value(false);
        }
        PromiseOrValue::Promise(
            Promise::new(entry.account)
                .function_call("burn", json!({ "amount": U128(burnable) }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(15))
                .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).bb_burned(key, U128(burnable), spent)),
        )
    }

    #[private]
    pub fn bb_burned(&mut self, key: String, amount: U128, spent: U128) -> PromiseOrValue<bool> {
        if env::promise_result_checked(0, 64).is_ok() {
            self.bb_done(&key, amount.0, spent.0, "burn");
            return PromiseOrValue::Value(true);
        }
        // The token has no (working) `burn`: send the tokens to the keyless graveyard instead.
        let entry = self.coins.get(&key).cloned().unwrap();
        PromiseOrValue::Promise(
            Promise::new(entry.account)
                .function_call(
                    "ft_transfer",
                    json!({ "receiver_id": self.graveyard, "amount": amount, "memo": "nme graveyard" }).to_string().into_bytes(),
                    ONE_YOCTO,
                    Gas::from_tgas(15),
                )
                .then(Self::ext(env::current_account_id()).with_static_gas(CB).with_unused_gas_weight(1).bb_buried(key, amount, spent)),
        )
    }

    #[private]
    pub fn bb_buried(&mut self, key: String, amount: U128, spent: U128) -> bool {
        if env::promise_result_checked(0, 64).is_ok() {
            self.bb_done(&key, amount.0, spent.0, "graveyard");
            return true;
        }
        // Neither burn nor the graveyard worked: the tokens stay in the arena (burn-bound) and go with the next buyback.
        events::emit("buyback_failed", json!({ "key": key, "reason": "burn and graveyard transfer both failed", "tokens_held": amount }));
        false
    }

    #[private]
    pub fn bb_restore(&mut self, key: String, from_chest: bool, near: U128) {
        let ok = env::promise_result_checked(0, 64).is_ok();
        self.restore_ledger(&key, from_chest, near.0, ok);
    }

    // ================================================================ views

    /// The war fighter id a wallet owner (`sol:…` / `evm:…`) scores under.
    pub fn wallet_key(&self, owner: String) -> AccountId {
        wallet_account(&owner)
    }

    pub fn get_coin(&self, key: String) -> Option<CoinEntry> {
        self.coins.get(&key).cloned()
    }

    pub fn get_config(&self) -> near_sdk::serde_json::Value {
        json!({
            "factory": self.factory, "wrap": self.wrap, "dcl": self.dcl, "graveyard": self.graveyard,
            "cfg": self.cfg, "protocol_near": U128(self.protocol_near),
        })
    }

    pub fn feed_since(&self, id: u64, limit: u64) -> Vec<FeedItem> {
        self.feed.since(id, limit)
    }

    pub fn feed_last_id(&self) -> u64 {
        self.feed.last_id()
    }

    /// Earliest due time in the index (UI countdowns, Operator agent).
    pub fn next_due_ms(&self) -> Option<u64> {
        self.due.iter().next().map(|((due, _), _)| *due)
    }

    pub fn due_tasks(&self, limit: u64) -> Vec<DueTask> {
        self.due.iter().take(limit.min(50) as usize).map(|((due_ms, seq), task)| DueTask { due_ms: *due_ms, seq: *seq, task: task.clone() }).collect()
    }

    // ================================================================ test-only entry points (feature `test-hooks`)

    /// NEVER in the deployed wasm. Lets integration tests start a buy-and-burn without the Colosseum (M3).
    #[cfg(feature = "test-hooks")]
    pub fn test_buy_and_burn(&mut self, key: String, from_chest: bool, near: U128) -> Promise {
        self.buy_and_burn(&key, &key, from_chest, near.0)
    }
}

#[near]
impl Contract {
    // ================================================================ Colosseum (M3)

    /// Permissionless. Pairs the opted-in, launched coins by chest size (largest with next largest), locks
    /// `chest_bps` of the weaker chest of each pair from both sides, and opens a scoring window. Callable once the
    /// previous round is fully settled and `next_round_ms` has passed. Pokers choose timing only (I7).
    /// Storage is paid from the protocol bucket first, then from the attached deposit; the rest is refunded.
    #[payable]
    pub fn start_round(&mut self) -> u64 {
        let attached = env::attached_deposit().as_yoctonear();
        match self.do_start_round(attached) {
            Ok((id, refund)) => {
                if refund > 0 {
                    Promise::new(env::predecessor_account_id()).transfer(NearToken::from_yoctonear(refund)).detach();
                }
                id
            }
            Err(e) => env::panic_str(&e),
        }
    }

    /// Permissionless, after the round's end. Settles the next unsettled pair (one pair per call, so every call fits
    /// in one transaction's gas). Both stakes return to their chests; if one side clearly won, the loser's stake
    /// then buys and burns the winner's coin (native `burn`, else the graveyard). A pair is void (nothing burns) if
    /// either side has fewer than `min_unique` scoring wallets, or on a tie.
    pub fn settle_round(&mut self, round_id: u64) -> PromiseOrValue<bool> {
        match self.do_settle_round(round_id) {
            Ok(Some(p)) => PromiseOrValue::Promise(p),
            Ok(None) => PromiseOrValue::Value(true),
            Err(e) => env::panic_str(&e),
        }
    }

    // ================================================================ autonomy: tick (I7, docs/10-AUTONOMY.md)

    /// Permissionless. Runs due tasks (oldest first, only those that fit the gas left, at most `max_tasks`, never more
    /// than 8). The caller earns `bounty_per_task` per task that did real work, from the protocol bucket only, capped
    /// per tick and by what the bucket holds. Nothing due -> nothing paid. Same result whoever calls it.
    pub fn tick(&mut self, max_tasks: u8) -> TickReport {
        let (processed, progressed) = self.run_due(max_tasks.clamp(1, 8), Gas::from_tgas(15));
        let want = self.cfg.bounty_per_task.0.saturating_mul(progressed as u128).min(self.cfg.bounty_cap_per_tick.0);
        let bounty = want.min(self.protocol_near);
        if bounty > 0 {
            self.protocol_near -= bounty;
            Promise::new(env::predecessor_account_id()).transfer(NearToken::from_yoctonear(bounty)).detach();
        }
        if processed > 0 {
            events::emit("tick", json!({ "caller": env::predecessor_account_id(), "processed": processed, "progressed": progressed, "bounty": U128(bounty) }));
        }
        TickReport { processed, progressed, bounty: U128(bounty) }
    }

    /// Anyone may add NEAR to the protocol bucket (bounties, round storage). Inflow only: no method withdraws it
    /// except the bounty rule in `tick` and the storage of rounds.
    #[payable]
    pub fn fund_protocol(&mut self) {
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach NEAR");
        self.protocol_near += amount;
        events::emit("protocol_funded", json!({ "by": env::predecessor_account_id(), "amount": U128(amount), "protocol": U128(self.protocol_near) }));
    }

    pub fn protocol_balance(&self) -> U128 {
        U128(self.protocol_near)
    }

    // ================================================================ Working Memes (M5, docs/12 §3)

    /// Pay a coin's brain to write something. The attached NEAR is split: `outlayer_fee` goes to OutLayer with the
    /// request (what it does not use is refunded to the payer), `record_reserve` pays the job record's storage, and
    /// the rest is escrowed. If the brain delivers, `cut_bps` stays in the protocol bucket and the rest buys and
    /// burns the coin; if it refuses, fails or times out, the escrow is refunded in full, exactly once.
    /// The prompt is not stored (only its hash); it travels in this call to the TEE worker.
    #[payable]
    pub fn hire(&mut self, ticker: String, kind: String, target: String, prompt: String) -> u64 {
        let entry = self.live_entry(&ticker);
        require!(entry.kind == Kind::Native, "only NME coins have a brain");
        require!(JOB_KINDS.contains(&kind.as_str()), "kind must be roast, shill, lore or meme");
        require!(prompt.len() <= MAX_PROMPT_BYTES, "prompt is longer than 500 bytes");
        require!(target.len() <= MAX_TARGET_BYTES, "target is longer than 40 bytes");
        let attached = env::attached_deposit().as_yoctonear();
        let price = self.job_price.get(&ticker).map(|p| p.0).unwrap_or(0).max(self.brain.min_price.0);
        require!(attached >= price, "payment is below this coin's job price");
        let overhead = self.brain.outlayer_fee.0 + self.brain.record_reserve.0;
        let amount = attached.checked_sub(overhead).unwrap_or_else(|| env::panic_str("payment does not cover the fees"));
        require!(amount > 0, "nothing left to escrow");
        let payer = env::predecessor_account_id();
        let id = self.next_job_id;
        self.next_job_id += 1;
        let now = env::block_timestamp_ms();
        let prompt_hash: String = env::sha256(prompt.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        let job = Job { id, ticker: ticker.clone(), kind: kind.clone(), prompt_hash: prompt_hash.clone(), payer: payer.clone(), amount: U128(amount), status: JobStatus::Requested, deadline_ms: now + self.brain.deadline_ms };
        self.jobs.insert(id, job.clone());
        self.enqueue(job.deadline_ms, Task::RefundJob { job_id: id });
        self.push_feed("job_hired", &ticker, payer.as_str(), amount, &format!("job={id} kind={kind}"));
        events::emit("job_hired", json!({ "job_id": id, "ticker": ticker, "kind": kind, "payer": payer, "amount": U128(amount), "prompt_hash": prompt_hash, "deadline_ms": job.deadline_ms }));
        let input = json!({
            "mode": "job", "job_id": id, "kind": kind,
            "persona": { "name": ticker.to_uppercase(), "ticker": ticker.to_uppercase(), "vibe": "" },
            "coin": entry.account, "rpc": self.brain.rpc_url, "target": target, "prompt": prompt,
        })
        .to_string();
        let args = json!({
            "source": { "WasmUrl": { "url": self.brain.brain_url, "hash": self.brain.brain_hash, "build_target": "wasm32-wasip2" } },
            "resource_limits": { "max_instructions": 2_000_000_000u64, "max_memory_mb": 64, "max_execution_seconds": 90 },
            "input_data": input,
            "secrets_ref": { "profile": self.brain.secrets_profile, "account_id": self.brain.secrets_account },
            "response_format": "Text",
            "payer_account_id": payer,
        })
        .to_string();
        Promise::new(self.brain.outlayer.clone())
            .function_call("request_execution", args.into_bytes(), NearToken::from_yoctonear(self.brain.outlayer_fee.0), Gas::from_tgas(100))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(40)).with_unused_gas_weight(1).on_job_result(id))
            .detach();
        id
    }

    /// OutLayer's answer (the result of the arena's own `request_execution`, I5). A failed run, a JSON `null`, an
    /// unparsable line, a refusal or an answer that arrives after the refund all end in a refund or are ignored.
    #[private]
    pub fn on_job_result(&mut self, job_id: u64) {
        let Some(mut job) = self.jobs.get(&job_id).cloned() else { return };
        if job.status != JobStatus::Requested {
            events::emit("job_late_result_ignored", json!({ "job_id": job_id }));
            return;
        }
        let parsed: Result<BrainOut, String> = match env::promise_result_checked(0, 8192) {
            Err(_) => Err("the brain run failed or timed out".into()),
            Ok(bytes) => serde_json::from_slice::<String>(&bytes)
                .map_err(|_| "unreadable result".to_string())
                .and_then(|line| serde_json::from_str::<BrainOut>(&line).map_err(|_| "the worker returned no JSON".to_string())),
        };
        match parsed {
            Ok(out) if out.ok && !out.refused && !out.text.trim().is_empty() && (job.kind != "meme" || valid_media(&out.media_url, &out.media_hash)) => {
                let text: String = if out.text.len() > MAX_TEXT_BYTES { out.text.chars().scan(0usize, |n, c| { *n += c.len_utf8(); (*n <= MAX_TEXT_BYTES).then_some(c) }).collect() } else { out.text };
                let cut = bps_of(job.amount.0, self.brain.cut_bps);
                let net = job.amount.0 - cut;
                self.protocol_near += cut;
                let mut entry = self.coins.get(&job.ticker).cloned().unwrap();
                entry.chest_near = U128(entry.chest_near.0 + net); // spent only by the buy-and-burn task below
                self.coins.insert(job.ticker.clone(), entry);
                job.status = JobStatus::Delivered;
                self.jobs.insert(job_id, job.clone());
                let seq = self.next_delivery;
                self.next_delivery += 1;
                if job.kind == "meme" {
                    self.delivery_media.insert(seq % DELIVERY_RING, (out.media_url.clone(), out.media_hash.clone()));
                } else {
                    self.delivery_media.remove(&(seq % DELIVERY_RING));
                }
                proofs::set_delivery_proof(seq % DELIVERY_RING, &out.request_hash, &out.response_hash);
                self.deliveries.insert(
                    seq % DELIVERY_RING,
                    Delivery { seq, job_id, ticker: job.ticker.clone(), kind: job.kind.clone(), text: text.clone(), model: out.model.clone(), chat_id: out.chat_id.clone(), ts_ms: env::block_timestamp_ms() },
                );
                self.enqueue(env::block_timestamp_ms(), Task::JobBurn { job_id });
                self.push_feed("job_delivered", &job.ticker, job.payer.as_str(), net, &format!("job={job_id} {}", text.chars().take(120).collect::<String>()));
                events::emit("job_delivered", json!({ "job_id": job_id, "ticker": job.ticker, "kind": job.kind, "text": text, "model": out.model, "chat_id": out.chat_id, "media_url": out.media_url, "media_hash": out.media_hash, "burn_near": U128(net), "cut": U128(cut) }));
            }
            Ok(out) if out.ok && out.refused => self.refund_job(job, &format!("refused: {}", out.reason.chars().take(120).collect::<String>())),
            Ok(_) => self.refund_job(job, "the brain returned no usable text"),
            Err(e) => self.refund_job(job, &e),
        }
    }

    /// Permissionless: refunds a job the brain never answered, once its deadline has passed. (A `RefundJob` task does
    /// the same by itself.)
    pub fn claim_refund(&mut self, job_id: u64) {
        let job = self.jobs.get(&job_id).cloned().unwrap_or_else(|| env::panic_str("unknown job"));
        require!(job.status == JobStatus::Requested, "job is not waiting for the brain");
        require!(env::block_timestamp_ms() >= job.deadline_ms, "the job's deadline has not passed");
        self.refund_job(job, "deadline passed");
    }

    // ================================================================ Self-Defending Memes (M6, docs/12 §4)

    /// Pool read for a defense check (the callback of `get_pool`). Updates the rolling high, then either does nothing,
    /// applies the fixed rule, or consults the coin's brain.
    #[private]
    pub fn on_defense_pool(&mut self, ticker: String) {
        let now = env::block_timestamp_ms();
        let mut st = self.defense_state.get(&ticker).cloned().unwrap_or_default();
        let Some(entry) = self.coins.get(&ticker).cloned() else { return };
        let point = env::promise_result_checked(0, 4096).ok().and_then(|b| serde_json::from_slice::<PoolView>(&b).ok()).filter(|p| p.state == "Running").map(|p| p.current_point);
        let Some(point) = point else {
            st.in_flight_until_ms = 0;
            self.defense_state.insert(ticker, st);
            return;
        };
        if st.high_ms == 0 || now.saturating_sub(st.high_ms) > HIGH_WINDOW_MS || point > st.high_point {
            st.high_point = point;
            st.high_ms = now;
        }
        let drop = (st.high_point - point).max(0) as u64;
        if now.saturating_sub(st.day_start_ms) >= DAY_MS {
            st.day_start_ms = now;
            st.day_spent_bps = 0;
        }
        let left = (self.defense.daily_bps as u32).saturating_sub(st.day_spent_bps);
        let ready = drop >= self.defense.look_points as u64
            && left > 0
            && (st.last_action_ms == 0 || now.saturating_sub(st.last_action_ms) >= self.defense.cooldown_ms)
            && entry.treasury_near.0 >= self.defense.min_treasury.0;
        if !ready {
            st.in_flight_until_ms = 0;
            self.defense_state.insert(ticker, st);
            return;
        }
        // Fee budget for a brain consultation (D-29): protocol bucket, per-day cap.
        if now.saturating_sub(self.fee_day_start_ms) >= DAY_MS {
            self.fee_day_start_ms = now;
            self.fee_day_spent = 0;
        }
        let fee = self.defense.outlayer_fee.0;
        let can_ask = st.misses < self.defense.fallback_after && self.protocol_near >= fee && self.fee_day_spent + fee <= self.defense.fee_cap_per_day.0;
        if !can_ask {
            // No brain (it keeps failing, or the fee budget is spent): the fixed rule, with the same caps.
            st.in_flight_until_ms = 0;
            if drop >= self.defense.trigger_points as u64 {
                let bps = (self.defense.per_action_bps as u32).min(left);
                self.defense_state.insert(ticker.clone(), st);
                let _ = self.apply_defense(&ticker, bps as u64, "rule", "price fell to the rule's trigger; the brain is unavailable", now);
            } else {
                self.defense_state.insert(ticker, st);
            }
            return;
        }
        self.protocol_near -= fee;
        self.fee_day_spent += fee;
        self.defense_state.insert(ticker.clone(), st.clone());
        let input = json!({
            "mode": "defense", "ticker": ticker, "coin": entry.account, "rpc": self.brain.rpc_url,
            "current_point": point, "high_point": st.high_point, "high_age_secs": now.saturating_sub(st.high_ms) / 1000, "treasury_near": entry.treasury_near,
            "per_action_bps": self.defense.per_action_bps, "daily_left_bps": left,
        })
        .to_string();
        let args = json!({
            "source": { "WasmUrl": { "url": self.brain.brain_url, "hash": self.brain.brain_hash, "build_target": "wasm32-wasip2" } },
            "resource_limits": { "max_instructions": 2_000_000_000u64, "max_memory_mb": 64, "max_execution_seconds": 90 },
            "input_data": input,
            "secrets_ref": { "profile": self.brain.secrets_profile, "account_id": self.brain.secrets_account },
            "response_format": "Text",
            "payer_account_id": env::current_account_id(),
        })
        .to_string();
        Promise::new(self.brain.outlayer.clone())
            .function_call("request_execution", args.into_bytes(), NearToken::from_yoctonear(fee), Gas::from_tgas(100))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(40)).with_unused_gas_weight(1).on_defense_result(ticker))
            .detach();
    }

    /// The brain's decision (the result of the arena's own `request_execution`, I5). Only `buyback`/`hold` exist; the
    /// contract enforces every cap, so an over-cap, in-cooldown or malformed answer is rejected, never clamped.
    #[private]
    pub fn on_defense_result(&mut self, ticker: String) {
        let now = env::block_timestamp_ms();
        let mut st = self.defense_state.get(&ticker).cloned().unwrap_or_default();
        st.in_flight_until_ms = 0;
        let parsed: Result<DefenseOut, String> = match env::promise_result_checked(0, 8192) {
            Err(_) => Err("the brain run failed or timed out".into()),
            Ok(bytes) => serde_json::from_slice::<String>(&bytes)
                .map_err(|_| "unreadable result".to_string())
                .and_then(|line| serde_json::from_str::<DefenseOut>(&line).map_err(|_| "the worker returned no JSON".to_string())),
        };
        match parsed {
            Ok(out) if out.ok && out.action == "hold" => {
                st.misses = 0;
                self.defense_state.insert(ticker.clone(), st);
                self.push_feed("defense", &ticker, "brain", 0, &format!("hold: {}", out.reason.chars().take(150).collect::<String>()));
                events::emit("defense", json!({ "ticker": ticker, "action": "hold", "bps": 0, "source": "ai", "reason": out.reason }));
            }
            Ok(out) if out.ok && out.action == "buyback" => {
                self.defense_state.insert(ticker.clone(), st);
                if let Err(why) = self.apply_defense(&ticker, out.bps, "ai", &out.reason, now) {
                    let mut st = self.defense_state.get(&ticker).cloned().unwrap_or_default();
                    st.misses += 1;
                    self.defense_state.insert(ticker.clone(), st);
                    events::emit("defense_rejected", json!({ "ticker": ticker, "bps": out.bps, "why": why }));
                }
            }
            Ok(_) | Err(_) => {
                st.misses += 1;
                let n = st.misses;
                self.defense_state.insert(ticker.clone(), st);
                events::emit("defense_miss", json!({ "ticker": ticker, "consecutive_misses": n }));
            }
        }
    }

    pub fn get_defense_config(&self) -> DefenseConfig {
        self.defense.clone()
    }

    pub fn get_defense_state(&self, ticker: String) -> DefenseState {
        self.defense_state.get(&ticker).cloned().unwrap_or_default()
    }

    // ================================================================ Commentary (docs/11 §5, I8)

    /// The result of the arena's own `request_execution` for a commentary line. The brain can only append here:
    /// nothing in this method touches a ledger, a round or a job. A failed, refused or unusable run drops the line.
    #[private]
    pub fn on_commentary_result(&mut self, ticker: String, event: String) {
        let Ok(bytes) = env::promise_result_checked(0, 8192) else { return };
        let Some(out) = serde_json::from_slice::<String>(&bytes).ok().and_then(|line| serde_json::from_str::<BrainOut>(&line).ok()) else { return };
        if !out.ok || out.refused || out.text.trim().is_empty() {
            return;
        }
        let text: String = out.text.trim().chars().scan(0usize, |n, c| { *n += c.len_utf8(); (*n <= MAX_COMMENTARY_BYTES).then_some(c) }).collect();
        let seq = self.next_commentary;
        self.next_commentary += 1;
        let now = env::block_timestamp_ms();
        proofs::set_commentary_proof(seq % COMMENTARY_RING, seq, &out.request_hash, &out.response_hash);
        self.commentary.insert(seq % COMMENTARY_RING, Commentary { seq, ticker: ticker.clone(), event: event.clone(), text: text.clone(), model: out.model, chat_id: out.chat_id, ts_ms: now });
        self.push_feed("commentary", &ticker, "brain", 0, &format!("{event}: {}", text.chars().take(150).collect::<String>()));
        events::emit("commentary", json!({ "ticker": ticker, "event": event, "text": text, "ai": true }));
    }

    /// The latest AI commentary, newest first (the ring keeps the last 64). Every line is AI-written.
    pub fn get_commentary(&self, limit: u64) -> Vec<Commentary> {
        let n = self.next_commentary;
        (0..limit.min(COMMENTARY_RING).min(n)).filter_map(|i| self.commentary.get(&((n - 1 - i) % COMMENTARY_RING)).cloned()).collect()
    }

    pub fn get_commentary_config(&self) -> CommentaryConfig {
        self.commentary_cfg.clone()
    }

    // ================================================================ PumpBag deadlines (M4)

    /// PumpBag collection only: this PumpBag becomes forfeitable at `due_ms` (every mint, heartbeat and transfer says so).
    /// One `CrownForfeit` task per PumpBag; a later deadline only moves the date the task checks.
    pub fn crown_deadline(&mut self, ticker: String, due_ms: u64) {
        require!(env::predecessor_account_id() == self.crown, "only the PumpBag collection can set deadlines");
        let pending = self.crown_deadlines.get(&ticker).map(|(_, p)| *p).unwrap_or(false);
        self.crown_deadlines.insert(ticker.clone(), (due_ms, true));
        if !pending {
            self.enqueue(due_ms + 1, Task::CrownForfeit { ticker: ticker.clone() });
        }
        events::emit("crown_deadline", json!({ "ticker": ticker, "due_ms": due_ms }));
    }

    /// The gateway allowed to call `buy_for`.
    pub fn get_gateway(&self) -> AccountId {
        self.gateway.clone()
    }

    pub fn get_crown_deadline(&self, ticker: String) -> Option<u64> {
        self.crown_deadlines.get(&ticker).map(|(d, _)| *d)
    }

    /// Factory only, once per coin: the price its creator fixed at launch (immutable afterwards). Never below the global
    /// minimum and never above `MAX_JOB_PRICE`; a coin without one uses the minimum.
    pub fn set_job_price(&mut self, ticker: String, price: U128) {
        require!(env::predecessor_account_id() == self.factory, "only the factory can set a job price");
        require!(matches!(self.coins.get(&ticker), Some(e) if e.kind == Kind::Native), "unknown NME coin");
        require!(self.job_price.get(&ticker).is_none(), "the job price is fixed at launch");
        require!(price.0 >= self.brain.min_price.0 && price.0 <= MAX_JOB_PRICE, "job price outside the allowed range");
        self.job_price.insert(ticker.clone(), price);
        events::emit("job_price_set", json!({ "ticker": ticker, "price": price }));
    }

    /// What one job of this coin costs (yoctoNEAR, everything attached to `hire`).
    pub fn get_job_price(&self, ticker: String) -> U128 {
        U128(self.job_price.get(&ticker).map(|p| p.0).unwrap_or(0).max(self.brain.min_price.0))
    }

    pub fn get_job(&self, job_id: u64) -> Option<Job> {
        self.jobs.get(&job_id).cloned()
    }

    /// The latest deliveries, newest first (the ring keeps the last 32).
    pub fn get_deliveries(&self, limit: u64) -> Vec<DeliveryOut> {
        let n = self.next_delivery;
        (0..limit.min(DELIVERY_RING).min(n))
            .filter_map(|i| {
                let slot = (n - 1 - i) % DELIVERY_RING;
                let d = self.deliveries.get(&slot).cloned()?;
                let (media_url, media_hash) = self.delivery_media.get(&slot).cloned().unwrap_or_default();
                let (request_hash, response_hash) = proofs::delivery_proof(slot);
                Some(DeliveryOut { seq: d.seq, job_id: d.job_id, ticker: d.ticker, kind: d.kind, text: d.text, model: d.model, chat_id: d.chat_id, ts_ms: d.ts_ms, media_url, media_hash, request_hash, response_hash })
            })
            .collect()
    }

    pub fn get_brain_config(&self) -> JobConfig {
        self.brain.clone()
    }

    pub fn get_round(&self, id: u64) -> Option<Round> {
        self.rounds.get(&id).cloned()
    }

    pub fn current_round(&self) -> Option<Round> {
        if self.last_round_id == 0 {
            None
        } else {
            self.rounds.get(&self.last_round_id).cloned()
        }
    }

    pub fn next_round_ms(&self) -> u64 {
        self.next_round_ms
    }

    pub fn get_war_config(&self) -> WarConfig {
        self.war.clone()
    }

    pub fn roster(&self, from: u64, limit: u64) -> Vec<String> {
        let from = from.min(u32::MAX as u64) as u32;
        let to = self.roster.len().min(from.saturating_add(limit.min(100) as u32));
        (from..to).filter_map(|i| self.roster.get(i).cloned()).collect()
    }
}

impl Contract {
    /// At most one task, only if the transaction carries `inline_tick_min_gas` and the task still leaves the caller
    /// `reserve_after` for its own work. No bounty. Heavy tasks therefore run inline only from cheap calls such as
    /// `fund_chest`; a buy mostly advances the light ones (round start).
    fn inline_tick(&mut self, reserve_after: Gas) {
        let remaining = env::prepaid_gas().as_gas().saturating_sub(env::used_gas().as_gas());
        if remaining < Gas::from_tgas(self.cfg.inline_tick_min_gas_tgas).as_gas() || self.cfg.inline_tick_min_gas_tgas == 0 {
            return;
        }
        self.run_due(1, reserve_after);
    }

    /// A chest funding that completes a pair starts the war right away (synchronous, cheap, no bounty). The queued
    /// `StartRound` poll stays as the backstop.
    fn inline_start(&mut self) {
        let remaining = env::prepaid_gas().as_gas().saturating_sub(env::used_gas().as_gas());
        if self.cfg.inline_tick_min_gas_tgas == 0 || remaining < Gas::from_tgas(60).as_gas() {
            return;
        }
        if env::block_timestamp_ms() < self.next_round_ms || self.protocol_near < ROUND_STORAGE_RESERVE {
            return;
        }
        let _ = self.do_start_round(0);
    }

    fn schedule_start(&mut self, due_ms: u64) {
        if !self.start_pending {
            self.start_pending = true;
            self.enqueue(due_ms, Task::StartRound);
        }
    }

    /// Runs due tasks. Returns (processed, progressed).
    fn run_due(&mut self, max: u8, reserve_after: Gas) -> (u32, u32) {
        let now = env::block_timestamp_ms();
        let (mut processed, mut progressed) = (0u32, 0u32);
        let mut greedy_ran = false;
        while processed < max as u32 {
            let remaining = env::prepaid_gas().as_gas().saturating_sub(env::used_gas().as_gas());
            let pick = self
                .due
                .iter()
                .take(8)
                .take_while(|((due, _), _)| *due <= now)
                .find(|(_, t)| task_gas(t).as_gas() + reserve_after.as_gas() <= remaining && !(greedy_ran && task_is_greedy(t)))
                .map(|(k, t)| (*k, t.clone()));
            let Some((key, task)) = pick else { break };
            self.due.remove(&key);
            processed += 1;
            greedy_ran |= task_is_greedy(&task);
            if self.run_task(task, now) {
                progressed += 1;
            }
        }
        (processed, progressed)
    }

    /// Executes one task; true when it did real work. Never panics.
    fn run_task(&mut self, task: Task, now: u64) -> bool {
        match task {
            Task::StartRound => {
                self.start_pending = false;
                if now < self.next_round_ms {
                    self.schedule_start(self.next_round_ms);
                    return false;
                }
                if self.protocol_near < ROUND_STORAGE_RESERVE {
                    self.schedule_start(now + POLL_MS);
                    return false;
                }
                match self.do_start_round(0) {
                    Ok(_) => true,
                    Err(_) => {
                        self.schedule_start(now + POLL_MS);
                        false
                    }
                }
            }
            Task::SettleRound { round_id } => {
                let Some(round) = self.rounds.get(&round_id) else { return false };
                if (round.settled_pairs as usize) >= round.pairs.len() {
                    return false;
                }
                if now < round.end_ms {
                    let end = round.end_ms;
                    self.enqueue(end, Task::SettleRound { round_id });
                    return false;
                }
                match self.do_settle_round(round_id) {
                    Ok(p) => {
                        if let Some(p) = p {
                            p.detach();
                        }
                        let more = self.rounds.get(&round_id).map(|r| (r.settled_pairs as usize) < r.pairs.len()).unwrap_or(false);
                        if more {
                            self.enqueue(now, Task::SettleRound { round_id });
                        }
                        true
                    }
                    Err(_) => false,
                }
            }
            Task::ClaimFees { ticker } => {
                let Some(entry) = self.coins.get(&ticker).cloned() else { return false };
                self.enqueue(now + self.cfg.claim_interval_ms.max(POLL_MS), Task::ClaimFees { ticker });
                if !entry.launched || entry.kind != Kind::Native {
                    return false;
                }
                Promise::new(entry.account).function_call("claim_fees", b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(200)).detach();
                true
            }
            Task::DefenseCheck { ticker } => self.defense_check(ticker, now),
            Task::Commentary { ticker, event, detail } => self.run_commentary(ticker, event, detail, now),
            Task::RaidSettle { raid_id } => self.settle_raid(raid_id, now),
            Task::CrownForfeit { ticker } => {
                let Some((due, _)) = self.crown_deadlines.get(&ticker).copied() else { return false };
                if now <= due {
                    // a heartbeat or a transfer moved the deadline: look again then (still one task per PumpBag)
                    self.enqueue(due + 1, Task::CrownForfeit { ticker: ticker.clone() });
                    self.crown_deadlines.insert(ticker, (due, true));
                    return false;
                }
                self.crown_deadlines.insert(ticker.clone(), (due, false));
                // The collection checks the deadline itself and the vault does the burning; a refusal leaves everything as it was.
                Promise::new(self.crown.clone()).function_call("crown_forfeit", json!({ "token_id": ticker }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(180)).detach();
                true
            }
            Task::DefenseBurn { ticker, bps } => {
                let Some(entry) = self.coins.get(&ticker).cloned() else { return false };
                let amount = bps_of(entry.treasury_near.0, bps.min(10_000) as u16);
                if amount == 0 || entry.treasury_near.0 < self.defense.min_treasury.0 || !entry.launched {
                    return false;
                }
                // The treasury leaves only into this buy-and-burn of the coin's own supply (I3/I8).
                self.buy_and_burn(&ticker, &ticker, false, amount).detach();
                true
            }
            Task::JobBurn { job_id } => {
                let Some(mut job) = self.jobs.get(&job_id).cloned() else { return false };
                if job.status != JobStatus::Delivered {
                    return false;
                }
                let net = job.amount.0 - bps_of(job.amount.0, self.brain.cut_bps);
                job.status = JobStatus::Burned;
                self.jobs.insert(job_id, job.clone());
                // The payment sits in the coin's chest (credited at delivery) and leaves it only into this buy-and-burn.
                self.buy_and_burn(&job.ticker, &job.ticker, true, net).detach();
                true
            }
            Task::RefundJob { job_id } => {
                let Some(job) = self.jobs.get(&job_id).cloned() else { return false };
                if job.status != JobStatus::Requested {
                    return false;
                }
                self.refund_job(job, "deadline passed");
                true
            }
            Task::ResumeLaunch { ticker, tries } => {
                let Some(entry) = self.coins.get(&ticker).cloned() else { return false };
                if entry.launched {
                    return false;
                }
                if tries + 1 < MAX_LAUNCH_TRIES {
                    self.enqueue(now + POLL_MS, Task::ResumeLaunch { ticker, tries: tries + 1 });
                }
                Promise::new(entry.account).function_call("launch", b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(250)).detach();
                true
            }
        }
    }

    /// Opens the next round. `attached` is what the caller attached (0 for a tick). Returns the round id and the
    /// part of `attached` to refund.
    fn do_start_round(&mut self, attached: u128) -> Result<(u64, u128), String> {
        let now = env::block_timestamp_ms();
        if now < self.next_round_ms {
            return Err("too early for the next round".into());
        }
        if self.last_round_id > 0 {
            let prev = self.rounds.get(&self.last_round_id).ok_or("round missing")?;
            if prev.settled_pairs as usize != prev.pairs.len() {
                return Err("the previous round is not settled yet".into());
            }
        }
        let before = env::storage_usage();
        let mut elig: Vec<(u128, String)> = Vec::new();
        for i in 0..self.roster.len() {
            let key = self.roster.get(i).cloned().unwrap_or_default();
            if self.active.get(&key).is_some() {
                continue;
            }
            if let Some(e) = self.coins.get(&key) {
                if e.launched && e.pool_id.is_some() && e.colosseum_opt_in && e.chest_near.0 > 0 {
                    elig.push((e.chest_near.0, key));
                }
            }
        }
        elig.sort_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
        let id = self.last_round_id + 1;
        // Decide the pairs before touching any ledger.
        let mut plan: Vec<(usize, u128)> = Vec::new();
        let mut i = 0;
        while i + 1 < elig.len() && plan.len() < self.war.max_pairs as usize {
            let stake = bps_of(elig[i + 1].0, self.war.chest_bps); // the weaker chest sets the stake
            if stake == 0 || stake < self.war.min_stake.0 {
                break;
            }
            plan.push((i, stake));
            i += 2;
        }
        if plan.is_empty() {
            return Err("no eligible pair: need two opted-in launched coins with funded chests".into());
        }
        let mut pairs: Vec<Pair> = Vec::new();
        for (idx, (i, stake)) in plan.into_iter().enumerate() {
            for k in [i, i + 1] {
                let key = elig[k].1.clone();
                let mut e = self.coins.get(&key).cloned().unwrap();
                e.chest_near = U128(e.chest_near.0 - stake);
                self.coins.insert(key.clone(), e);
                self.active.insert(key, (id, idx as u32));
            }
            pairs.push(Pair {
                a: elig[i].1.clone(),
                b: elig[i + 1].1.clone(),
                stake: U128(stake),
                score_a: U128(0),
                score_b: U128(0),
                unique_a: 0,
                unique_b: 0,
                state: PairState::Pending,
                winner: String::new(),
                note: String::new(),
            });
        }
        let end_ms = now + self.war.round_duration_ms;
        for (n, p) in pairs.iter().enumerate() {
            self.push_feed("war_started", &p.a, &p.b, p.stake.0, &format!("round={id} pair={n}"));
            events::emit("war_started", json!({ "round": id, "pair": n, "a": p.a, "b": p.b, "stake": p.stake, "end_ms": end_ms }));
        }
        let announce: Vec<(String, String)> = pairs.iter().flat_map(|p| [(p.a.clone(), p.b.clone()), (p.b.clone(), p.a.clone())]).collect();
        for (me, rival) in announce {
            self.queue_commentary(&me, "war_started", &format!("a Colosseum war against {rival} just started"));
        }
        self.rounds.insert(id, Round { id, start_ms: now, end_ms, pairs, settled_pairs: 0 });
        self.last_round_id = id;
        self.next_round_ms = end_ms + self.war.round_interval_ms;
        self.enqueue(end_ms, Task::SettleRound { round_id: id });
        self.rounds.flush();
        self.active.flush();
        self.coins.flush();
        self.feed.flush();
        self.due.flush();
        let cost = env::storage_byte_cost().saturating_mul(env::storage_usage().saturating_sub(before) as u128).as_yoctonear();
        let from_protocol = cost.min(self.protocol_near);
        self.protocol_near -= from_protocol;
        let rest = cost - from_protocol;
        if rest > attached && attached > 0 {
            return Err("attach NEAR to cover the round's storage".into());
        }
        if rest > attached {
            // A tick pays no deposit: the shortfall (tiny; guarded by ROUND_STORAGE_RESERVE) stays with the arena.
            return Ok((id, 0));
        }
        Ok((id, attached - rest))
    }

    /// Settles the next unsettled pair of `round_id`; returns the buy-and-burn promise of the winner, if any.
    fn do_settle_round(&mut self, round_id: u64) -> Result<Option<Promise>, String> {
        let mut round = self.rounds.get(&round_id).cloned().ok_or("unknown round")?;
        if env::block_timestamp_ms() < round.end_ms {
            return Err("the round is still running".into());
        }
        if (round.settled_pairs as usize) >= round.pairs.len() {
            return Err("the round is already settled".into());
        }
        let idx = round.settled_pairs as usize;
        let mut pair = round.pairs[idx].clone();
        let (winner, note) = if pair.unique_a < self.war.min_unique || pair.unique_b < self.war.min_unique {
            ("none", "too few distinct wallets")
        } else if pair.score_a.0 == pair.score_b.0 {
            ("none", "tie")
        } else if pair.score_a.0 > pair.score_b.0 {
            ("a", "")
        } else {
            ("b", "")
        };
        for key in [pair.a.clone(), pair.b.clone()] {
            self.active.remove(&key);
            if let Some(mut e) = self.coins.get(&key).cloned() {
                e.chest_near = U128(e.chest_near.0 + pair.stake.0);
                self.coins.insert(key, e);
            }
        }
        pair.state = PairState::Settled;
        pair.winner = winner.to_string();
        pair.note = note.to_string();
        round.pairs[idx] = pair.clone();
        round.settled_pairs += 1;
        let finished = (round.settled_pairs as usize) == round.pairs.len();
        self.rounds.insert(round_id, round);
        if finished {
            self.schedule_start(self.next_round_ms.max(env::block_timestamp_ms()));
        }
        let (w, l) = match winner {
            "a" => (pair.a.clone(), pair.b.clone()),
            "b" => (pair.b.clone(), pair.a.clone()),
            _ => {
                self.push_feed("war_void", &pair.a, &pair.b, pair.stake.0, note);
                events::emit("war_void", json!({ "round": round_id, "pair": idx, "a": pair.a, "b": pair.b, "reason": note, "scores": [pair.score_a, pair.score_b], "unique": [pair.unique_a, pair.unique_b] }));
                return Ok(None);
            }
        };
        self.queue_commentary(&w, "war_won", &format!("you just beat {l} in the Colosseum and its war chest will buy and burn your supply"));
        self.queue_commentary(&l, "war_lost", &format!("you just lost the Colosseum war to {w} and your war chest will be spent buying {w}"));
        self.push_feed("war_settled", &w, &l, pair.stake.0, &format!("round={round_id} pair={idx}"));
        events::emit(
            "war_settled",
            json!({ "round": round_id, "pair": idx, "winner": w, "loser": l, "burn_near": pair.stake, "scores": [pair.score_a, pair.score_b], "unique": [pair.unique_a, pair.unique_b] }),
        );
        // The loser's committed NEAR buys the winner on its Rhea pool. Half of what it buys is burned, half goes to the
        // winning side's fighters (spoils.rs). A failed swap restores the loser's chest.
        let total = if pair.a == w { pair.score_a.0 } else { pair.score_b.0 }.saturating_sub(self.raid_points_in(&w, round_id));
        let token = self.coins.get(&w).map(|e| e.account.clone()).unwrap_or_else(|| env::panic_str("unknown coin"));
        let share = spoils::open(round_id, &w, token, total, bps_of(pair.stake.0, self.war.wallet_cap_bps)).then_some(round_id);
        Ok(Some(self.buy_and_burn_x(&w, &l, true, pair.stake.0, share)))
    }

    /// Queues a commentary line for a native coin (outside coins have no brain). The task re-checks the tier, the
    /// daily limit and the fee budget when it runs.
    fn queue_commentary(&mut self, ticker: &str, event: &str, detail: &str) {
        if matches!(self.coins.get(ticker), Some(e) if e.kind == Kind::Native) {
            self.enqueue(env::block_timestamp_ms(), Task::Commentary { ticker: ticker.to_string(), event: event.to_string(), detail: detail.to_string() });
        }
    }

    fn run_commentary(&mut self, ticker: String, event: String, detail: String, now: u64) -> bool {
        let Some(entry) = self.coins.get(&ticker).cloned() else { return false };
        if entry.kind != Kind::Native || !entry.launched || entry.treasury_near.0 < self.commentary_cfg.min_treasury.0 {
            return false; // the brain is silent below its tier
        }
        let (mut day, mut count) = self.commentary_day.get(&ticker).copied().unwrap_or((now, 0));
        if now.saturating_sub(day) >= DAY_MS {
            day = now;
            count = 0;
        }
        if count >= self.commentary_cfg.per_day_per_coin {
            return false;
        }
        if now.saturating_sub(self.commentary_fee_day_start_ms) >= DAY_MS {
            self.commentary_fee_day_start_ms = now;
            self.commentary_fee_day_spent = 0;
        }
        let fee = self.commentary_cfg.outlayer_fee.0;
        if self.protocol_near < fee || self.commentary_fee_day_spent + fee > self.commentary_cfg.fee_cap_per_day.0 {
            return false;
        }
        self.protocol_near -= fee;
        self.commentary_fee_day_spent += fee;
        self.commentary_day.insert(ticker.clone(), (day, count + 1));
        let input = json!({
            "mode": "job", "job_id": 0, "kind": "line",
            "persona": { "name": ticker.to_uppercase(), "ticker": ticker.to_uppercase(), "vibe": "" },
            "coin": entry.account, "rpc": self.brain.rpc_url, "target": "",
            // composed by the contract from chain events, never from user text
            "prompt": detail,
        })
        .to_string();
        let args = json!({
            "source": { "WasmUrl": { "url": self.brain.brain_url, "hash": self.brain.brain_hash, "build_target": "wasm32-wasip2" } },
            "resource_limits": { "max_instructions": 2_000_000_000u64, "max_memory_mb": 64, "max_execution_seconds": 90 },
            "input_data": input,
            "secrets_ref": { "profile": self.brain.secrets_profile, "account_id": self.brain.secrets_account },
            "response_format": "Text",
            "payer_account_id": env::current_account_id(),
        })
        .to_string();
        Promise::new(self.brain.outlayer.clone())
            .function_call("request_execution", args.into_bytes(), NearToken::from_yoctonear(fee), Gas::from_tgas(100))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(40)).with_unused_gas_weight(1).on_commentary_result(ticker, event))
            .detach();
        true
    }

    fn check_defense(d: &DefenseConfig) {
        require!(d.per_action_bps > 0 && d.per_action_bps <= 500, "per-action cap must be 1..=500 bps (5%)");
        require!(d.daily_bps >= d.per_action_bps && d.daily_bps <= 1_500, "daily cap must be at least the per-action cap and at most 1500 bps (15%)");
        require!(d.cooldown_ms >= 1_000 && d.interval_ms >= 1_000, "cooldown and interval must be at least a second");
        require!(d.fallback_after >= 1, "fallback_after must be at least 1");
        require!(d.look_points >= 1 && d.trigger_points >= d.look_points, "trigger must be at least the look threshold");
    }

    /// Queues the next check of the coin and, if one can run, reads its pool.
    fn defense_check(&mut self, ticker: String, now: u64) -> bool {
        let Some(entry) = self.coins.get(&ticker).cloned() else { return false };
        self.enqueue(now + self.defense.interval_ms, Task::DefenseCheck { ticker: ticker.clone() });
        if !entry.launched || entry.kind != Kind::Native || entry.treasury_near.0 < self.defense.min_treasury.0 {
            return false;
        }
        let mut st = self.defense_state.get(&ticker).cloned().unwrap_or_default();
        if now < st.in_flight_until_ms {
            return false; // one consultation per coin at a time
        }
        st.in_flight_until_ms = now + IN_FLIGHT_MS;
        self.defense_state.insert(ticker.clone(), st);
        Promise::new(self.dcl.clone())
            .function_call("get_pool", json!({ "pool_id": entry.pool_id.unwrap() }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(30)).with_unused_gas_weight(1).on_defense_pool(ticker))
            .detach();
        true
    }

    /// Validates a buyback decision against the immutable caps and, if allowed, books it and queues the burn.
    fn apply_defense(&mut self, ticker: &str, bps: u64, source: &str, reason: &str, now: u64) -> Result<(), String> {
        let mut st = self.defense_state.get(ticker).cloned().unwrap_or_default();
        if bps == 0 || bps > self.defense.per_action_bps as u64 {
            return Err(format!("{bps} bps is outside the per-action cap of {}", self.defense.per_action_bps));
        }
        if st.last_action_ms != 0 && now.saturating_sub(st.last_action_ms) < self.defense.cooldown_ms {
            return Err("inside the cooldown".into());
        }
        if now.saturating_sub(st.day_start_ms) >= DAY_MS {
            st.day_start_ms = now;
            st.day_spent_bps = 0;
        }
        if st.day_spent_bps as u64 + bps > self.defense.daily_bps as u64 {
            return Err("over the daily cap".into());
        }
        let treasury = self.coins.get(ticker).map(|e| e.treasury_near.0).unwrap_or(0);
        if treasury < self.defense.min_treasury.0 {
            return Err("treasury below the minimum".into());
        }
        st.last_action_ms = now;
        st.day_spent_bps += bps as u32;
        if source == "ai" {
            st.misses = 0;
        }
        self.defense_state.insert(ticker.to_string(), st);
        let reason: String = reason.chars().take(200).collect();
        self.push_feed("defense", ticker, source, bps as u128, &format!("buyback {bps} bps: {}", reason.chars().take(150).collect::<String>()));
        events::emit("defense", json!({ "ticker": ticker, "action": "buyback", "bps": bps, "source": source, "reason": reason }));
        self.enqueue(now, Task::DefenseBurn { ticker: ticker.to_string(), bps: bps as u32 });
        self.queue_commentary(ticker, "defense_buyback", &format!("the price just dropped and you spent {bps} hundredths of a percent of your own treasury buying back and burning your supply to defend it"));
        Ok(())
    }

    fn check_brain(b: &JobConfig) {
        require!(b.cut_bps <= 3_000, "protocol cut above 30%");
        require!(b.min_price.0 > b.outlayer_fee.0 + b.record_reserve.0, "min price must exceed the fees");
        require!(b.deadline_ms >= 60_000, "job deadline under a minute");
        require!(b.brain_hash.len() == 64 && b.brain_hash.chars().all(|c| c.is_ascii_hexdigit()), "brain hash must be a sha256 in hex");
    }

    /// Hands the escrow back to the payer and closes the job (once: the status check is the guard).
    fn refund_job(&mut self, mut job: Job, why: &str) {
        if job.status != JobStatus::Requested {
            return;
        }
        job.status = JobStatus::Refunded;
        let (id, payer, amount) = (job.id, job.payer.clone(), job.amount.0);
        self.jobs.insert(id, job);
        Promise::new(payer.clone()).transfer(NearToken::from_yoctonear(amount)).detach();
        self.push_feed("job_refunded", "", payer.as_str(), amount, &format!("job={id} {why}"));
        events::emit("job_refunded", json!({ "job_id": id, "payer": payer, "amount": U128(amount), "reason": why }));
    }

    /// The shared body of `buy` and `buy_for`: `user` is who receives the tokens (or the refund) and the war credit.
    fn buy_inner(&mut self, user: AccountId, key: String, min_out: U128, owner: Option<String>) -> Promise {
        let entry = self.live_entry(&key);
        let gross = env::attached_deposit().as_yoctonear();
        require!(gross >= MIN_TRADE_NEAR, "minimum buy is 0.001 NEAR");
        // The swap output is delivered straight to the buyer. If the buyer is not registered on the token the output
        // would be stranded inside DCL, so check registration first and refund if it is missing.
        // The buyer's token balance is read too: the difference after the swap is the exact number of tokens bought.
        let registered = Promise::new(entry.account.clone()).function_call("storage_balance_of", json!({ "account_id": user }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(8));
        let balance = Promise::new(entry.account.clone()).function_call("ft_balance_of", json!({ "account_id": user }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5));
        registered
            .and(balance)
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(180)).with_unused_gas_weight(1).on_buy_checked(user, key, U128(gross), min_out, owner))
    }

    /// Pays `amount` back to whoever the buy was for: a NEAR account directly, a wallet owner through the gateway ledger.
    fn pay_user(&self, user: &AccountId, owner: &Option<String>, amount: u128) -> Promise {
        match owner {
            Some(o) => Promise::new(self.gateway.clone()).function_call("credit_near", json!({ "owner": o }).to_string().into_bytes(), NearToken::from_yoctonear(amount), Gas::from_tgas(10)),
            None => Promise::new(user.clone()).transfer(NearToken::from_yoctonear(amount)),
        }
    }

    /// Tells the gateway how many tokens a wallet owner's buy brought (0 when nothing was bought); this also ends the gateway's token lock.
    fn credit_wallet_tokens(&self, owner: &str, token: &AccountId, amount: u128) -> Promise {
        Promise::new(self.gateway.clone()).function_call("credit_tokens", json!({ "owner": owner, "coin": token, "amount": U128(amount) }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
    }

    fn check_war(w: &WarConfig) {
        require!(w.round_duration_ms >= 1_000, "round shorter than 1 second");
        require!(w.chest_bps > 0 && w.chest_bps <= 10_000, "chest_bps out of range");
        require!(w.wallet_cap_bps > 0 && w.wallet_cap_bps <= 10_000, "wallet_cap_bps out of range");
        require!(w.min_unique >= 1, "min_unique must be at least 1");
        require!(w.max_pairs >= 1 && w.max_pairs <= 4, "max_pairs must be 1..=4");
    }

    /// A war is open for this coin and the buy is big enough to score.
    fn scoring_open(&self, key: &str, near: u128) -> bool {
        if near < MIN_SCORED_BUY {
            return false;
        }
        let Some((rid, _)) = self.active.get(key).copied() else { return false };
        self.rounds.get(&rid).map(|r| env::block_timestamp_ms() < r.end_ms).unwrap_or(false)
    }

    fn finish_buy(&mut self, key: &str, user: &AccountId, used: u128, fee_actual: u128, storage_cost: u128) {
        self.credit_fee(key, fee_actual.saturating_sub(storage_cost), "router_fee");
        self.push_feed(name::TRADE, key, user.as_str(), used + fee_actual, "buy");
        events::emit(name::TRADE, json!({ "key": key, "side": "buy", "account": user, "near": U128(used + fee_actual), "fee": U128(fee_actual) }));
    }

    /// A wallet's counted score: `near_bought x (1 - min(1, tokens_sold / tokens_bought))`, clamped at `cap`.
    /// Selling back what you bought through the arena cancels the score, so a buy/sell loop earns nothing.
    fn wallet_score(f: &WalletFlow, cap: u128) -> u128 {
        if f.tokens_bought == 0 {
            return 0;
        }
        let kept = f.tokens_bought - f.tokens_sold.min(f.tokens_bought);
        mul_div(f.near_bought, kept, f.tokens_bought).unwrap_or(0).min(cap)
    }

    /// Applies the change of one wallet's counted score to its pair (score sum and number of scoring wallets).
    fn apply_wallet_change(pair: &mut Pair, side_a: bool, old: u128, new: u128) {
        let (score, unique) = if side_a { (&mut pair.score_a, &mut pair.unique_a) } else { (&mut pair.score_b, &mut pair.unique_b) };
        score.0 = score.0 - old + new;
        if old == 0 && new > 0 {
            *unique += 1;
        } else if old > 0 && new == 0 {
            *unique -= 1;
        }
    }

    /// Counts a router buy (`near` spent, `tokens` received) toward the coin's pair in the active round.
    /// Returns the storage cost of a new wallet entry (taken out of the router fee by the caller).
    fn score_buy(&mut self, key: &str, wallet: &AccountId, near: u128, tokens: u128) -> u128 {
        if !self.scoring_open(key, near) || tokens == 0 {
            return 0;
        }
        let (rid, idx) = self.active.get(key).copied().unwrap();
        let mut round = self.rounds.get(&rid).cloned().unwrap();
        let before = env::storage_usage();
        let pair = &mut round.pairs[idx as usize];
        let cap = bps_of(pair.stake.0, self.war.wallet_cap_bps);
        let k = (rid, key.to_string(), wallet.clone());
        let mut flow = self.wallet_flow.get(&k).cloned().unwrap_or_default();
        let old = Self::wallet_score(&flow, cap);
        flow.near_bought = flow.near_bought.saturating_add(near);
        flow.tokens_bought = flow.tokens_bought.saturating_add(tokens);
        let new = Self::wallet_score(&flow, cap);
        Self::apply_wallet_change(pair, pair.a == key, old, new);
        self.wallet_flow.insert(k, flow);
        self.rounds.insert(rid, round);
        self.wallet_flow.flush();
        self.rounds.flush();
        env::storage_byte_cost().saturating_mul(env::storage_usage().saturating_sub(before) as u128).as_yoctonear()
    }

    /// Counts a sell through the arena against the seller's score in the active round (no new storage: a wallet
    /// that never bought has no entry and nothing to cancel).
    fn score_sell(&mut self, key: &str, wallet: &AccountId, tokens: u128) {
        let Some((rid, idx)) = self.active.get(key).copied() else { return };
        let Some(mut round) = self.rounds.get(&rid).cloned() else { return };
        if env::block_timestamp_ms() >= round.end_ms {
            return;
        }
        let k = (rid, key.to_string(), wallet.clone());
        let Some(mut flow) = self.wallet_flow.get(&k).cloned() else { return };
        let pair = &mut round.pairs[idx as usize];
        let cap = bps_of(pair.stake.0, self.war.wallet_cap_bps);
        let old = Self::wallet_score(&flow, cap);
        flow.tokens_sold = flow.tokens_sold.saturating_add(tokens);
        let new = Self::wallet_score(&flow, cap);
        Self::apply_wallet_change(pair, pair.a == key, old, new);
        self.wallet_flow.insert(k, flow);
        self.rounds.insert(rid, round);
    }

    fn check_cfg(cfg: &ArenaConfig) {
        require!(cfg.fee_crown_bps as u32 + cfg.fee_treasury_bps as u32 <= 10_000, "fee split exceeds 100%");
        require!(cfg.router_fee_bps <= 200, "router fee above 2%");
        require!(cfg.burn_slippage_bps <= 2_000, "burn slippage above 20%");
    }

    fn enqueue(&mut self, due_ms: u64, task: Task) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.due.insert((due_ms, seq), task);
    }

    fn push_feed(&mut self, kind: &str, ticker: &str, actor: &str, amount: u128, extra: &str) {
        let ts = env::block_timestamp_ms();
        self.feed.push(ts, kind, ticker, actor, amount, extra);
    }

    /// Key of a token in `coins`: the ticker for `<ticker>.<factory>`, otherwise the token account itself.
    fn key_of(&self, token: &AccountId) -> String {
        if let Some(prefix) = token.as_str().strip_suffix(&format!(".{}", self.factory)) {
            if !prefix.contains('.') {
                return prefix.to_string();
            }
        }
        token.to_string()
    }

    fn live_entry(&self, key: &str) -> CoinEntry {
        let e = self.coins.get(key).cloned().unwrap_or_else(|| env::panic_str("unknown coin"));
        require!(e.launched && e.pool_id.is_some(), "coin is not launched yet");
        e
    }

    /// Splits `amount` NEAR into PumpBag / treasury / protocol, credits the ledgers, and sends the PumpBag share to the
    /// vault (outside coins have no vault: their PumpBag share goes to the treasury).
    fn credit_fee(&mut self, key: &str, amount: u128, kind: &str) {
        let Some(mut entry) = self.coins.get(key).cloned() else { return };
        let mut crown = bps_of(amount, self.cfg.fee_crown_bps);
        let treasury = bps_of(amount, self.cfg.fee_treasury_bps);
        let protocol = amount - crown - treasury;
        let vault = entry.crown_vault.clone();
        let mut treasury_total = treasury;
        if vault.is_none() {
            treasury_total += crown;
            crown = 0;
        }
        entry.treasury_near = U128(entry.treasury_near.0 + treasury_total);
        self.coins.insert(key.to_string(), entry);
        self.protocol_near += protocol;
        if kind == name::FEES_CLAIMED {
            self.push_feed(name::FEES_CLAIMED, key, key, amount, &format!("crown={crown} treasury={treasury_total} protocol={protocol}"));
            events::emit(name::FEES_CLAIMED, json!({ "ticker": key, "amount": U128(amount), "crown": U128(crown), "treasury": U128(treasury_total), "protocol": U128(protocol) }));
        } else {
            events::emit(kind, json!({ "key": key, "amount": U128(amount), "crown": U128(crown), "treasury": U128(treasury_total), "protocol": U128(protocol) }));
        }
        if let (Some(vault), true) = (vault, crown > 0) {
            ext_vault::ext(vault)
                .with_attached_deposit(NearToken::from_yoctonear(crown))
                .with_static_gas(Gas::from_tgas(10))
                .with_unused_gas_weight(0)
                .deposit_fees()
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(5)).on_vault_deposit(key.to_string(), U128(crown)))
                .detach();
        }
    }

    fn judge_enlist(&self, meta: Option<&MetaView>, pool: Option<&PoolView>) -> Result<(), String> {
        let meta = meta.ok_or("the token does not answer ft_metadata (not an NEP-141)")?;
        if !meta.spec.starts_with("ft-") || meta.decimals == 0 {
            return Err("the token's metadata is not NEP-148".into());
        }
        let pool = pool.ok_or("the DCL pool does not exist")?;
        validate_pool(pool, self.wrap.as_str(), self.cfg.min_pool_near.0)
    }

    fn enlist_refund(&self, caller: AccountId, attached: u128, token: &AccountId, why: String) -> PromiseOrValue<bool> {
        events::emit("enlist_rejected", json!({ "token": token, "reason": why }));
        Promise::new(caller).transfer(NearToken::from_yoctonear(attached)).detach();
        PromiseOrValue::Value(false)
    }

    fn bb_fail(&mut self, key: &str, from_chest: bool, near: u128, why: &str) -> PromiseOrValue<bool> {
        self.restore_ledger(key, from_chest, near, true);
        events::emit("buyback_failed", json!({ "key": key, "reason": why, "near": U128(near) }));
        PromiseOrValue::Value(false)
    }

    fn restore_ledger(&mut self, key: &str, from_chest: bool, near: u128, ok: bool) {
        if !ok {
            // Could not unwrap the refused amount: the NEAR is still wNEAR in the arena. Credit it back anyway so the
            // ledger matches what the arena owns (it can be unwrapped later).
            events::emit("buyback_restore_wrapped", json!({ "key": key, "near": U128(near) }));
        }
        if let Some(mut e) = self.coins.get(key).cloned() {
            if from_chest {
                e.chest_near = U128(e.chest_near.0 + near);
            } else {
                e.treasury_near = U128(e.treasury_near.0 + near);
            }
            self.coins.insert(key.to_string(), e);
        }
    }

    fn bb_done(&mut self, key: &str, tokens: u128, spent: u128, via: &str) {
        self.push_feed(name::TREASURY_BUYBACK, key, key, spent, &format!("tokens={tokens} via={via}"));
        events::emit(name::TREASURY_BUYBACK, json!({ "key": key, "near_spent": U128(spent), "tokens_burned": U128(tokens), "via": via }));
        events::emit(name::BURN, json!({ "key": key, "amount": U128(tokens), "via": via }));
    }
}

/// DCL `Swap` message. `swap_out_recipient` sends the output straight to that account (no empty referral fields: DCL
/// rejects them, docs/spikes/S12.md).
fn swap_msg(pool_id: &str, output_token: &str, min_out: u128, recipient: Option<&AccountId>) -> String {
    let rcp = recipient.map(|r| format!(r#","swap_out_recipient":"{r}""#)).unwrap_or_default();
    format!(r#"{{"Swap":{{"pool_ids":["{pool_id}"],"output_token":"{output_token}","min_output_amount":"{min_out}"{rcp}}}}}"#)
}

/// A wallet owner's sale: the proceeds stay wNEAR (not unwrapped) and go to `recipient` (the gateway), which measures the wNEAR it receives.
fn swap_msg_keep_wnear(pool_id: &str, output_token: &str, min_out: u128, recipient: &AccountId) -> String {
    format!(r#"{{"Swap":{{"pool_ids":["{pool_id}"],"output_token":"{output_token}","min_output_amount":"{min_out}","skip_unwrap_near":true,"swap_out_recipient":"{recipient}"}}}}"#)
}

/// The virtual war fighter of a wallet owner: the hex sha256 of the owner key, a valid 64-hex (implicit-style) account id that no
/// one controls, so wallet users are scored separately and never as the gateway account.
fn wallet_account(owner: &str) -> AccountId {
    let hex: String = env::sha256(owner.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    hex.parse().unwrap_or_else(|_| env::panic_str("bad owner key"))
}

/// Enlist gate: the pool must be Running, non-empty (it holds a position), and its wNEAR side must hold at least
/// `min_near` (default 0: a fresh single-sided launch holds no wNEAR until someone buys).
fn validate_pool(pool: &PoolView, wrap: &str, min_near: u128) -> Result<(), String> {
    if pool.state != "Running" {
        return Err("the DCL pool is not Running".into());
    }
    let (near_side, token_side) = if pool.token_y == wrap {
        (&pool.total_y, &pool.total_x)
    } else if pool.token_x == wrap {
        (&pool.total_x, &pool.total_y)
    } else {
        return Err("the pool does not contain wNEAR".into());
    };
    let near_side: u128 = near_side.parse().map_err(|_| "unreadable pool liquidity".to_string())?;
    let token_side: u128 = token_side.parse().map_err(|_| "unreadable pool liquidity".to_string())?;
    if near_side == 0 && token_side == 0 {
        return Err("the DCL pool is empty".into());
    }
    if near_side < min_near {
        return Err(format!("pool has {near_side} yoctoNEAR on the NEAR side, below the {min_near} floor"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn rpc_url_must_be_plain_https() {
        Contract::check_rpc("https://near-testnet.drpc.org");
        for bad in ["http://x.org", "https://k@x.org", "https://x.org/?apikey=1", "https://x .org", ""] {
            assert!(std::panic::catch_unwind(|| Contract::check_rpc(bad)).is_err(), "{bad} accepted");
        }
    }
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;

    fn acct(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    fn cfg() -> ArenaConfig {
        ArenaConfig {
            bounty_per_task: U128(0),
            bounty_cap_per_tick: U128(0),
            inline_tick_min_gas_tgas: 0,
            price_check_interval_ms: 0,
            fee_crown_bps: 4_000,
            fee_treasury_bps: 4_000,
            claim_interval_ms: 600_000,
            router_fee_bps: 100,
            min_pool_near: U128(10u128.pow(24)),
            burn_slippage_bps: 500,
        }
    }

    fn brain() -> JobConfig {
        JobConfig {
            outlayer: acct("outlayer.testnet"),
            brain_url: "https://test.fastfs.io/x/outlayer.testnet/".to_string() + &"a".repeat(64) + ".wasm",
            brain_hash: "a".repeat(64),
            secrets_account: acct("nme-brain.testnet"),
            secrets_profile: "default".into(),
            rpc_url: "https://rpc.testnet.fastnear.com".into(),
            min_price: U128(10u128.pow(23)),
            outlayer_fee: U128(2 * 10u128.pow(22)),
            record_reserve: U128(3 * 10u128.pow(21)),
            cut_bps: 1_000,
            deadline_ms: 600_000,
        }
    }

    fn defense() -> DefenseConfig {
        DefenseConfig {
            per_action_bps: 500,
            daily_bps: 1_000,
            cooldown_ms: 60_000,
            interval_ms: 60_000,
            look_points: 100,
            trigger_points: 2_232,
            fallback_after: 3,
            min_treasury: U128(1_000),
            outlayer_fee: U128(10u128.pow(21)),
            fee_cap_per_day: U128(3 * 10u128.pow(21)),
        }
    }

    fn commentary() -> CommentaryConfig {
        CommentaryConfig { min_treasury: U128(1_000), per_day_per_coin: 3, outlayer_fee: U128(10u128.pow(21)), fee_cap_per_day: U128(3 * 10u128.pow(21)) }
    }

    fn war() -> WarConfig {
        WarConfig { round_duration_ms: 60_000, round_interval_ms: 0, chest_bps: 5_000, wallet_cap_bps: 5_000, min_unique: 2, min_stake: U128(10u128.pow(21)), max_pairs: 2 }
    }

    fn arena() -> Contract {
        testing_env!(VMContextBuilder::new().build());
        Contract::new(acct("nme.testnet"), acct("wrap.testnet"), acct("dcl.testnet"), acct("burn.nme.testnet"), cfg(), war(), brain(), defense(), commentary(), acct("crown.nme.testnet"), acct("gateway.nme.testnet"))
    }

    fn as_caller(who: &str, deposit_yocto: u128) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct(who)).attached_deposit(NearToken::from_yoctonear(deposit_yocto)).build());
    }

    fn register(a: &mut Contract) {
        as_caller("nme.testnet", 20 * 10u128.pow(21));
        let _ = a.register_coin("pepe".into(), acct("pepe.nme.testnet"), acct("crown.pepe.nme.testnet"), true, 1_000);
    }

    fn launch(a: &mut Contract) {
        register(a);
        as_caller("pepe.nme.testnet", 0);
        a.record_launch("pepe".into(), "pepe.nme.testnet|wrap.testnet|10000".into(), "pepe.nme.testnet|wrap.testnet|10000#1".into());
    }

    #[test]
    fn register_is_idempotent_and_enqueues_once() {
        let mut a = arena();
        register(&mut a);
        as_caller("nme.testnet", 20 * 10u128.pow(21));
        let _ = a.register_coin("pepe".into(), acct("pepe.nme.testnet"), acct("crown.pepe.nme.testnet"), true, 1_000);
        let claims: Vec<_> = a.due_tasks(10).into_iter().filter(|t| matches!(t.task, Task::ClaimFees { .. })).collect();
        assert_eq!(claims.len(), 1, "the fee claim is queued once");
        assert_eq!(claims[0].task, Task::ClaimFees { ticker: "pepe".into() });
        assert_eq!(claims[0].due_ms, 1_000);
        let resumes = a.due_tasks(10).into_iter().filter(|t| matches!(t.task, Task::ResumeLaunch { .. })).count();
        assert_eq!(resumes, 1, "and so is the launch watchdog");
    }

    #[test]
    #[should_panic(expected = "registered to a different account")]
    fn register_rejects_ticker_hijack() {
        let mut a = arena();
        register(&mut a);
        as_caller("nme.testnet", 20 * 10u128.pow(21));
        let _ = a.register_coin("pepe".into(), acct("evil.nme.testnet"), acct("crown.evil.nme.testnet"), true, 1_000);
    }

    #[test]
    #[should_panic(expected = "only the factory")]
    fn register_factory_only() {
        let mut a = arena();
        as_caller("mallory.testnet", 20 * 10u128.pow(21));
        let _ = a.register_coin("pepe".into(), acct("pepe.nme.testnet"), acct("c.testnet"), true, 1);
    }

    #[test]
    fn launch_is_recorded_once_by_the_coin() {
        let mut a = arena();
        launch(&mut a);
        let c = a.get_coin("pepe".into()).unwrap();
        assert!(c.launched && c.kind == Kind::Native);
        assert_eq!(c.lpt_id.unwrap(), "pepe.nme.testnet|wrap.testnet|10000#1");
        assert_eq!(a.feed_since(0, 10).last().unwrap().kind, "launch_finalized");
    }

    #[test]
    #[should_panic(expected = "launch already recorded")]
    fn launch_cannot_be_recorded_twice() {
        let mut a = arena();
        launch(&mut a);
        as_caller("pepe.nme.testnet", 0);
        a.record_launch("pepe".into(), "p2".into(), "l2".into());
    }

    #[test]
    #[should_panic(expected = "only the coin can record its launch")]
    fn launch_only_by_the_coin() {
        let mut a = arena();
        register(&mut a);
        as_caller("mallory.testnet", 0);
        a.record_launch("pepe".into(), "p".into(), "l".into());
    }

    #[test]
    fn pool_fee_split_is_exact() {
        let mut a = arena();
        register(&mut a);
        as_caller("pepe.nme.testnet", 1_001);
        a.deposit_pool_fees("pepe".into());
        let c = a.get_coin("pepe".into()).unwrap();
        assert_eq!(c.treasury_near.0, 400);
        assert_eq!(a.protocol_near, 201);
    }

    #[test]
    #[should_panic(expected = "only the coin can deposit its fees")]
    fn fees_only_from_the_coin() {
        let mut a = arena();
        register(&mut a);
        as_caller("mallory.testnet", 1_000);
        a.deposit_pool_fees("pepe".into());
    }

    #[test]
    #[should_panic(expected = "no fees attached")]
    fn fees_need_a_deposit() {
        let mut a = arena();
        register(&mut a);
        as_caller("pepe.nme.testnet", 0);
        a.deposit_pool_fees("pepe".into());
    }

    #[test]
    fn forfeited_fees_go_to_the_treasury_from_the_vault_only() {
        let mut a = arena();
        register(&mut a);
        as_caller("crown.pepe.nme.testnet", 500);
        a.deposit_forfeited_fees("pepe".into());
        assert_eq!(a.get_coin("pepe".into()).unwrap().treasury_near.0, 500);
    }

    #[test]
    #[should_panic(expected = "only the coin's PumpBag vault")]
    fn forfeited_fees_reject_other_callers() {
        let mut a = arena();
        register(&mut a);
        as_caller("pepe.nme.testnet", 500);
        a.deposit_forfeited_fees("pepe".into());
    }

    #[test]
    #[should_panic(expected = "fee split exceeds 100%")]
    fn config_rejects_oversplit() {
        testing_env!(VMContextBuilder::new().build());
        let mut c = cfg();
        c.fee_crown_bps = 6_000;
        c.fee_treasury_bps = 5_000;
        Contract::new(acct("nme.testnet"), acct("wrap.testnet"), acct("dcl.testnet"), acct("burn.nme.testnet"), c, war(), brain(), defense(), commentary(), acct("crown.nme.testnet"), acct("gateway.nme.testnet"));
    }

    #[test]
    #[should_panic(expected = "router fee above 2%")]
    fn config_rejects_high_router_fee() {
        testing_env!(VMContextBuilder::new().build());
        let mut c = cfg();
        c.router_fee_bps = 201;
        Contract::new(acct("nme.testnet"), acct("wrap.testnet"), acct("dcl.testnet"), acct("burn.nme.testnet"), c, war(), brain(), defense(), commentary(), acct("crown.nme.testnet"), acct("gateway.nme.testnet"));
    }

    // ---- chests

    #[test]
    fn chest_grows_with_every_funding_and_has_no_exit() {
        let mut a = arena();
        register(&mut a);
        as_caller("carol.testnet", 700);
        a.fund_chest("pepe".into());
        as_caller("dave.testnet", 300);
        a.fund_chest("pepe".into());
        assert_eq!(a.get_coin("pepe".into()).unwrap().chest_near.0, 1_000);
        assert_eq!(a.feed_since(0, 20).iter().filter(|i| i.kind == "chest_funded").count(), 2);
    }

    #[test]
    #[should_panic(expected = "attach NEAR to fund the chest")]
    fn empty_chest_funding_rejected() {
        let mut a = arena();
        register(&mut a);
        as_caller("carol.testnet", 0);
        a.fund_chest("pepe".into());
    }

    #[test]
    #[should_panic(expected = "unknown coin")]
    fn chest_of_unknown_coin_rejected() {
        let mut a = arena();
        as_caller("carol.testnet", 5);
        a.fund_chest("nope".into());
    }

    // ---- keys and messages

    #[test]
    fn keys_are_tickers_for_native_coins_and_accounts_for_outside_ones() {
        let a = arena();
        assert_eq!(a.key_of(&acct("pepe.nme.testnet")), "pepe");
        assert_eq!(a.key_of(&acct("crown.pepe.nme.testnet")), "crown.pepe.nme.testnet", "a vault is not a coin");
        assert_eq!(a.key_of(&acct("nearly.token.near")), "nearly.token.near");
        assert_eq!(a.key_of(&acct("other.testnet")), "other.testnet");
    }

    #[test]
    fn swap_message_has_no_empty_referral_fields() {
        let m = swap_msg("a|wrap.testnet|10000", "a", 5, Some(&acct("bob.testnet")));
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["Swap"]["pool_ids"][0], "a|wrap.testnet|10000");
        assert_eq!(v["Swap"]["min_output_amount"], "5");
        assert_eq!(v["Swap"]["swap_out_recipient"], "bob.testnet");
        assert!(v["Swap"].get("referral_id").is_none() && v["Swap"].get("client_id").is_none());
        let m = swap_msg("p", "t", 1, None);
        assert!(!m.contains("swap_out_recipient"));
    }

    #[test]
    fn sell_message_parses() {
        let ok: SellMsg = serde_json::from_str(r#"{"sell":{"min_out":"123"}}"#).unwrap();
        assert_eq!(ok.sell.min_out.0, 123);
        assert!(serde_json::from_str::<SellMsg>(r#"{"buy":{}}"#).is_err());
        assert!(serde_json::from_str::<SellMsg>("garbage").is_err());
    }

    #[test]
    fn sell_of_an_unknown_token_is_returned_untouched() {
        let mut a = arena();
        as_caller("rando.testnet", 0);
        match a.ft_on_transfer(acct("bob.testnet"), U128(500), r#"{"sell":{"min_out":"1"}}"#.into()) {
            PromiseOrValue::Value(v) => assert_eq!(v.0, 500, "everything is returned as unused"),
            _ => panic!("must not forward an unknown token"),
        }
    }

    #[test]
    fn sell_with_a_bad_message_is_returned_untouched() {
        let mut a = arena();
        launch(&mut a);
        as_caller("pepe.nme.testnet", 0);
        match a.ft_on_transfer(acct("bob.testnet"), U128(500), "not json".into()) {
            PromiseOrValue::Value(v) => assert_eq!(v.0, 500),
            _ => panic!("must not forward a malformed message"),
        }
    }

    // ---- enlist gate

    fn pool(state: &str, x: &str, y: &str, tx: &str, ty: &str) -> PoolView {
        PoolView { token_x: tx.into(), token_y: ty.into(), state: state.into(), total_x: x.into(), total_y: y.into(), current_point: 0 }
    }

    #[test]
    fn enlist_gate_checks_state_wnear_and_liquidity() {
        let floor = 10u128.pow(24);
        // token is X, wNEAR is Y
        assert!(validate_pool(&pool("Running", "0", "2000000000000000000000000", "t.testnet", "wrap.testnet"), "wrap.testnet", floor).is_ok());
        // wNEAR is X
        assert!(validate_pool(&pool("Running", "2000000000000000000000000", "0", "wrap.testnet", "z.testnet"), "wrap.testnet", floor).is_ok());
        assert!(validate_pool(&pool("Paused", "0", "2000000000000000000000000", "t.testnet", "wrap.testnet"), "wrap.testnet", floor).unwrap_err().contains("not Running"));
        assert!(validate_pool(&pool("Running", "0", "5", "t.testnet", "wrap.testnet"), "wrap.testnet", floor).unwrap_err().contains("below"));
        // a fresh single-sided launch: tokens only, no wNEAR yet -> fine with the default floor of 0
        assert!(validate_pool(&pool("Running", "950000000000000000000000000", "0", "t.testnet", "wrap.testnet"), "wrap.testnet", 0).is_ok());
        assert!(validate_pool(&pool("Running", "0", "0", "t.testnet", "wrap.testnet"), "wrap.testnet", 0).unwrap_err().contains("empty"));
        assert!(validate_pool(&pool("Running", "9", "9", "a.testnet", "b.testnet"), "wrap.testnet", floor).unwrap_err().contains("wNEAR"));
        assert!(validate_pool(&pool("Running", "0", "oops", "t.testnet", "wrap.testnet"), "wrap.testnet", floor).is_err());
    }

    #[test]
    fn enlist_verdict_rejects_non_tokens_and_missing_pools() {
        let a = arena();
        let good_meta = MetaView { spec: "ft-1.0.0".into(), decimals: 18 };
        let good_pool = pool("Running", "0", "2000000000000000000000000", "t.testnet", "wrap.testnet");
        assert!(a.judge_enlist(Some(&good_meta), Some(&good_pool)).is_ok());
        assert!(a.judge_enlist(None, Some(&good_pool)).unwrap_err().contains("NEP-141"));
        assert!(a.judge_enlist(Some(&good_meta), None).unwrap_err().contains("does not exist"));
        assert!(a.judge_enlist(Some(&MetaView { spec: "nft-1".into(), decimals: 18 }), Some(&good_pool)).is_err());
    }

    #[test]
    #[should_panic(expected = "attach at least 0.02 NEAR")]
    fn enlist_needs_a_deposit() {
        let mut a = arena();
        as_caller("bob.testnet", 5);
        let _ = a.enlist(acct("t.testnet"), "t.testnet|wrap.testnet|10000".into());
    }

    #[test]
    #[should_panic(expected = "pool does not pair this token with wNEAR")]
    fn enlist_rejects_a_pool_of_another_token() {
        let mut a = arena();
        as_caller("bob.testnet", 20 * 10u128.pow(21));
        let _ = a.enlist(acct("t.testnet"), "other.testnet|wrap.testnet|10000".into());
    }

    #[test]
    #[should_panic(expected = "NME coins are registered by the factory")]
    fn enlist_rejects_native_coins() {
        let mut a = arena();
        as_caller("bob.testnet", 20 * 10u128.pow(21));
        let _ = a.enlist(acct("sneaky.nme.testnet"), "sneaky.nme.testnet|wrap.testnet|10000".into());
    }

    #[test]
    #[should_panic(expected = "pool_id must be x|y|fee")]
    fn enlist_rejects_a_malformed_pool_id() {
        let mut a = arena();
        as_caller("bob.testnet", 20 * 10u128.pow(21));
        let _ = a.enlist(acct("t.testnet"), "nonsense".into());
    }

    // ---- buy

    #[test]
    #[should_panic(expected = "coin is not launched yet")]
    fn buy_needs_a_launched_coin() {
        let mut a = arena();
        register(&mut a);
        as_caller("bob.testnet", 10u128.pow(24));
        let _ = a.buy("pepe".into(), U128(1));
    }

    #[test]
    #[should_panic(expected = "minimum buy")]
    fn buy_has_a_floor() {
        let mut a = arena();
        launch(&mut a);
        as_caller("bob.testnet", 5);
        let _ = a.buy("pepe".into(), U128(1));
    }

    #[test]
    #[should_panic(expected = "unknown coin")]
    fn buy_of_an_unknown_coin_is_rejected() {
        let mut a = arena();
        as_caller("bob.testnet", 10u128.pow(24));
        let _ = a.buy("nope".into(), U128(1));
    }

    // ---- buy-and-burn ledger discipline

    #[test]
    fn buy_and_burn_debits_first_and_restore_returns_it() {
        let mut a = arena();
        launch(&mut a);
        as_caller("carol.testnet", 1_000);
        a.fund_chest("pepe".into());
        as_caller("arena.testnet", 0);
        let _ = a.buy_and_burn("pepe", "pepe", true, 400);
        assert_eq!(a.get_coin("pepe".into()).unwrap().chest_near.0, 600, "debited before any async step");
        a.restore_ledger("pepe", true, 400, true);
        assert_eq!(a.get_coin("pepe".into()).unwrap().chest_near.0, 1_000, "restored on failure");
    }

    #[test]
    #[should_panic(expected = "not enough NEAR in the ledger")]
    fn buy_and_burn_cannot_overspend_a_ledger() {
        let mut a = arena();
        launch(&mut a);
        as_caller("carol.testnet", 100);
        a.fund_chest("pepe".into());
        let _ = a.buy_and_burn("pepe", "pepe", true, 101);
    }

    #[test]
    #[should_panic(expected = "not enough NEAR in the ledger")]
    fn the_treasury_ledger_is_protected_the_same_way() {
        let mut a = arena();
        launch(&mut a);
        let _ = a.buy_and_burn("pepe", "pepe", false, 1);
    }

    #[test]
    fn the_arena_exposes_no_withdraw_of_any_ledger() {
        // I3, checked on the source: no public method moves treasury/chest NEAR except through buy_and_burn.
        let src = include_str!("lib.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for forbidden in ["pub fn withdraw", "pub fn claim_treasury", "pub fn take_", "pub fn sweep", "pub fn rescue"] {
            assert!(!code.contains(forbidden), "{forbidden} must not exist");
        }
        let transfers = code.matches(".transfer(").count();
        // transfers: factory refund x2 (register), caller refund (enlist) x2, buyer refunds x2, on_refund x1,
        // start_round deposit refund x1, tick bounty x1 (from the protocol bucket only)
        assert!(transfers <= 10, "unexpected number of native transfers ({transfers}); review each for ledger safety");
    }

    // ---------------------------------------------------------------- Colosseum (M3)

    fn at(who: &str, deposit_yocto: u128, ms: u64) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(acct(who))
            .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
            .block_timestamp(ms * 1_000_000)
            .build());
    }

    const E21: u128 = 10u128.pow(21);
    const NEAR1: u128 = 10u128.pow(24);

    /// pepe (native) and batman (outside) launched; chests `pepe_chest` / `bat_chest`.
    fn two_coins(pepe_chest: u128, bat_chest: u128) -> Contract {
        let mut a = arena();
        launch(&mut a);
        a.coins.insert(
            "bat.testnet".into(),
            CoinEntry {
                kind: Kind::Outside,
                account: acct("bat.testnet"),
                crown_vault: None,
                colosseum_opt_in: true,
                pool_id: Some("bat.testnet|wrap.testnet|10000".into()),
                lpt_id: None,
                treasury_near: U128(0),
                chest_near: U128(0),
                launched: true,
            },
        );
        a.roster.push("bat.testnet".into());
        for (k, v) in [("pepe", pepe_chest), ("bat.testnet", bat_chest)] {
            let mut e = a.coins.get(k).cloned().unwrap();
            e.chest_near = U128(v);
            a.coins.insert(k.into(), e);
        }
        a
    }

    fn chest(a: &Contract, k: &str) -> u128 {
        a.get_coin(k.into()).unwrap().chest_near.0
    }

    #[test]
    fn start_round_locks_half_of_the_weaker_chest_from_both_sides() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        let id = a.start_round();
        assert_eq!(id, 1);
        let r = a.get_round(1).unwrap();
        assert_eq!(r.pairs.len(), 1);
        assert_eq!(r.pairs[0].a, "pepe");
        assert_eq!(r.pairs[0].stake.0, 2 * NEAR1, "50% of the weaker chest");
        assert_eq!(chest(&a, "pepe"), 8 * NEAR1);
        assert_eq!(chest(&a, "bat.testnet"), 2 * NEAR1);
        assert_eq!(r.end_ms, 61_000);
    }

    #[test]
    #[should_panic(expected = "no eligible pair")]
    fn start_round_needs_two_funded_coins() {
        let mut a = two_coins(10 * NEAR1, 0);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
    }

    #[test]
    #[should_panic(expected = "previous round is not settled")]
    fn no_second_round_until_the_first_is_settled() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        at("anyone.testnet", NEAR1 / 10, 100_000);
        a.start_round();
    }

    #[test]
    #[should_panic(expected = "too early")]
    fn rounds_respect_the_interval() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        a.next_round_ms = 5_000;
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
    }

    #[test]
    fn scoring_clamps_each_wallet_and_ignores_small_or_late_buys() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1); // stake 2 NEAR, wallet cap 50% = 1 NEAR
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        at("arena.testnet", 0, 2_000);
        let (w1, w2) = (acct("w1.testnet"), acct("w2.testnet"));
        assert_eq!(a.score_buy("pepe", &w1, E21, E21), 0, "below the scoring floor: not counted, no storage");
        assert!(a.score_buy("pepe", &w1, 3 * NEAR1, 3 * NEAR1) > 0, "first entry for a wallet costs storage");
        a.score_buy("pepe", &w1, NEAR1, NEAR1); // already over the cap
        a.score_buy("pepe", &w2, NEAR1 / 2, NEAR1 / 2);
        a.score_buy("bat.testnet", &w1, NEAR1 / 2, NEAR1 / 2);
        let p = &a.get_round(1).unwrap().pairs[0];
        assert_eq!(p.score_a.0, NEAR1 + NEAR1 / 2, "w1 clamped to 1 NEAR + w2 0.5");
        assert_eq!(p.unique_a, 2);
        assert_eq!((p.score_b.0, p.unique_b), (NEAR1 / 2, 1));
        at("arena.testnet", 0, 70_000);
        assert_eq!(a.score_buy("pepe", &w2, NEAR1, NEAR1), 0, "after the end nothing counts");
        a.score_sell("pepe", &w2, NEAR1 / 2);
        assert_eq!(a.get_round(1).unwrap().pairs[0].score_a.0, NEAR1 + NEAR1 / 2, "late sells do not count either");
    }

    #[test]
    fn selling_back_through_the_arena_cancels_the_score() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        at("arena.testnet", 0, 2_000);
        let (w1, w2) = (acct("w1.testnet"), acct("w2.testnet"));
        // w1: buys 1 NEAR worth (100 tokens), sells all of it -> 0 and no longer a scoring wallet (a wash loop)
        a.score_buy("pepe", &w1, NEAR1, 100);
        assert_eq!(a.get_round(1).unwrap().pairs[0].score_a.0, NEAR1);
        a.score_sell("pepe", &w1, 100);
        let p = &a.get_round(1).unwrap().pairs[0];
        assert_eq!((p.score_a.0, p.unique_a), (0, 0), "a full round trip scores nothing");
        // w2: buys 1 NEAR (100 tokens), sells a quarter -> keeps 75%
        a.score_buy("pepe", &w2, NEAR1, 100);
        a.score_sell("pepe", &w2, 25);
        let p = &a.get_round(1).unwrap().pairs[0];
        assert_eq!((p.score_a.0, p.unique_a), (NEAR1 * 3 / 4, 1));
        // selling more than was bought through the arena cannot go below zero; a seller with no buy is ignored
        a.score_sell("pepe", &w2, 1_000);
        assert_eq!(a.get_round(1).unwrap().pairs[0].score_a.0, 0);
        a.score_sell("pepe", &acct("stranger.testnet"), 50);
        assert_eq!(a.get_round(1).unwrap().pairs[0].score_a.0, 0);
        // buying again after a sell restores some score: it counts the new buy against the old sells
        a.score_buy("pepe", &w1, NEAR1, 100);
        let p = &a.get_round(1).unwrap().pairs[0];
        assert!(p.score_a.0 > 0, "a fresh buy scores");
    }

    fn score_both(a: &mut Contract, pepe_wallets: &[(&str, u128)], bat_wallets: &[(&str, u128)]) {
        at("arena.testnet", 0, 2_000);
        for (w, v) in pepe_wallets {
            a.score_buy("pepe", &acct(w), *v, *v);
        }
        for (w, v) in bat_wallets {
            a.score_buy("bat.testnet", &acct(w), *v, *v);
        }
    }

    #[test]
    #[should_panic(expected = "still running")]
    fn settle_waits_for_the_end() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        at("anyone.testnet", 0, 30_000);
        let _ = a.settle_round(1);
    }

    #[test]
    fn settle_with_too_few_wallets_returns_both_stakes_and_burns_nothing() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        score_both(&mut a, &[("w1.testnet", NEAR1), ("w2.testnet", NEAR1)], &[("w3.testnet", NEAR1)]); // b has one wallet, min is 2
        at("anyone.testnet", 0, 70_000);
        assert!(matches!(a.settle_round(1), PromiseOrValue::Value(true)));
        let p = &a.get_round(1).unwrap().pairs[0];
        assert_eq!((p.winner.as_str(), p.note.as_str()), ("none", "too few distinct wallets"));
        assert_eq!((chest(&a, "pepe"), chest(&a, "bat.testnet")), (10 * NEAR1, 4 * NEAR1));
        assert!(a.active.get("pepe").is_none() && a.active.get("bat.testnet").is_none(), "both coins free again");
    }

    #[test]
    fn a_tie_is_void() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        score_both(&mut a, &[("w1.testnet", NEAR1), ("w2.testnet", NEAR1)], &[("w3.testnet", NEAR1), ("w4.testnet", NEAR1)]);
        at("anyone.testnet", 0, 70_000);
        assert!(matches!(a.settle_round(1), PromiseOrValue::Value(true)));
        assert_eq!(a.get_round(1).unwrap().pairs[0].note, "tie");
        assert_eq!(chest(&a, "pepe") + chest(&a, "bat.testnet"), 14 * NEAR1, "no NEAR left the chests");
    }

    #[test]
    fn the_loser_s_stake_leaves_its_chest_only_into_a_buy_of_the_winner() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        // bat wins: 2 wallets x 1 NEAR vs pepe 2 x 0.5
        score_both(&mut a, &[("w1.testnet", NEAR1 / 2), ("w2.testnet", NEAR1 / 2)], &[("w3.testnet", NEAR1), ("w4.testnet", NEAR1)]);
        at("anyone.testnet", 0, 70_000);
        let r = a.settle_round(1);
        assert!(matches!(r, PromiseOrValue::Promise(_)));
        let p = &a.get_round(1).unwrap().pairs[0];
        assert_eq!(p.winner, "b");
        // Stakes were returned, then the loser's stake (2 NEAR) debited again for the buy: pepe 10 - 2, bat 4.
        assert_eq!(chest(&a, "pepe"), 8 * NEAR1);
        assert_eq!(chest(&a, "bat.testnet"), 4 * NEAR1);
        assert!(a.active.get("pepe").is_none());
        // a failed swap restores the loser's chest
        a.restore_ledger("pepe", true, 2 * NEAR1, true);
        assert_eq!(chest(&a, "pepe"), 10 * NEAR1);
    }

    #[test]
    #[should_panic(expected = "already settled")]
    fn a_settled_round_cannot_settle_again() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        at("anyone.testnet", 0, 70_000);
        let _ = a.settle_round(1);
        let _ = a.settle_round(1);
    }

    #[test]
    fn a_new_round_starts_after_settlement_and_unused_deposit_is_refunded() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1, 1_000);
        a.start_round();
        at("anyone.testnet", 0, 70_000);
        let _ = a.settle_round(1);
        at("anyone.testnet", NEAR1, 71_000);
        assert_eq!(a.start_round(), 2);
    }

    // ---------------------------------------------------------------- autonomy: tick (M8)

    fn at_gas(who: &str, deposit_yocto: u128, ms: u64, tgas: u64) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(acct(who))
            .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
            .block_timestamp(ms * 1_000_000)
            .prepaid_gas(Gas::from_tgas(tgas))
            .build());
    }

    fn autonomous() -> Contract {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        a.cfg.bounty_per_task = U128(NEAR1 / 100);
        a.cfg.bounty_cap_per_tick = U128(NEAR1 / 50);
        a.cfg.inline_tick_min_gas_tgas = 250;
        a.protocol_near = NEAR1;
        // a clean queue: only the StartRound the tests ask for (the claim / launch watchdog tasks have their own tests)
        a.due.clear();
        a.start_pending = false;
        a.schedule_start(0);
        a
    }

    fn kinds(a: &Contract) -> Vec<Task> {
        a.due_tasks(50).into_iter().map(|d| d.task).collect()
    }

    #[test]
    fn tick_with_nothing_due_pays_nothing() {
        let mut a = autonomous();
        at_gas("poker.testnet", 0, 0, 300); // StartRound is due at 0 but we ask before anything is due: clear the queue
        let keys: Vec<_> = a.due_tasks(50).into_iter().map(|d| (d.due_ms, d.seq)).collect();
        for k in keys {
            a.due.remove(&k);
        }
        let p0 = a.protocol_near;
        let r = a.tick(5);
        assert_eq!((r.processed, r.progressed, r.bounty.0), (0, 0, 0));
        assert_eq!(a.protocol_near, p0);
    }

    #[test]
    fn tick_starts_the_round_pays_the_bounty_from_the_protocol_bucket_and_queues_the_settlement() {
        let mut a = autonomous();
        at_gas("poker.testnet", 0, 5_000, 300);
        let p0 = a.protocol_near;
        let r = a.tick(5);
        assert_eq!((r.processed, r.progressed), (2, 1), "StartRound ran; the war-start commentary task it queued ran too but the coin is below the brain tier, so it was silent and paid no bounty");
        assert_eq!(r.bounty.0, NEAR1 / 100);
        assert!(a.protocol_near <= p0 - NEAR1 / 100, "the bounty (and round storage) came out of the protocol bucket");
        assert_eq!(a.get_round(1).unwrap().pairs.len(), 1);
        assert!(kinds(&a).contains(&Task::SettleRound { round_id: 1 }));
        assert!(!kinds(&a).contains(&Task::StartRound), "the next StartRound waits for the settlement");
        // chests only moved into the locked stake
        assert_eq!(chest(&a, "pepe") + chest(&a, "bat.testnet") + 2 * a.get_round(1).unwrap().pairs[0].stake.0, 14 * NEAR1);
    }

    #[test]
    fn the_bounty_is_capped_and_never_exceeds_the_bucket() {
        let mut a = autonomous();
        a.cfg.bounty_cap_per_tick = U128(1_000);
        a.cfg.bounty_per_task = U128(10_000);
        a.protocol_near = NEAR1;
        at_gas("poker.testnet", 0, 5_000, 300);
        assert_eq!(a.tick(5).bounty.0, 1_000, "per-tick cap");
    }

    #[test]
    fn the_bounty_never_exceeds_what_the_bucket_holds() {
        let mut b = autonomous();
        b.protocol_near = ROUND_STORAGE_RESERVE; // exactly enough to start the round
        b.cfg.bounty_cap_per_tick = U128(NEAR1);
        b.cfg.bounty_per_task = U128(NEAR1);
        at_gas("poker.testnet", 0, 5_000, 300);
        let before = b.protocol_near;
        let r = b.tick(5);
        assert!(r.bounty.0 <= before, "never more than the bucket held");
        assert!(b.protocol_near <= before, "and the bucket never goes negative (u128)");
    }

    #[test]
    fn a_poll_that_finds_nothing_to_do_earns_no_bounty_and_stays_a_single_task() {
        let mut a = two_coins(10 * NEAR1, 0); // no pair possible
        a.cfg.bounty_per_task = U128(NEAR1 / 100);
        a.cfg.bounty_cap_per_tick = U128(NEAR1);
        a.protocol_near = NEAR1;
        a.due.clear();
        a.start_pending = false;
        a.schedule_start(1_000);
        at_gas("poker.testnet", 0, 5_000, 300);
        let r = a.tick(8);
        assert_eq!((r.processed, r.progressed, r.bounty.0), (1, 0, 0), "polled, paid nothing");
        let starts = kinds(&a).iter().filter(|t| **t == Task::StartRound).count();
        assert_eq!(starts, 1, "exactly one StartRound stays queued");
        assert_eq!(a.due_tasks(50).iter().find(|d| d.task == Task::StartRound).unwrap().due_ms, 5_000 + POLL_MS);
    }

    #[test]
    fn a_round_does_not_start_without_protocol_funds_for_its_storage() {
        let mut a = autonomous();
        a.protocol_near = 1;
        at_gas("poker.testnet", 0, 5_000, 300);
        assert_eq!(a.tick(5).progressed, 0);
        assert!(a.get_round(1).is_none());
        // anyone can top the bucket up
        at_gas("donor.testnet", NEAR1, 5_000, 300);
        a.fund_protocol();
        assert_eq!(a.protocol_balance().0, NEAR1 + 1);
    }

    #[test]
    fn tick_stops_when_the_gas_left_cannot_run_the_next_task() {
        let mut a = autonomous();
        at_gas("poker.testnet", 0, 5_000, 20); // < StartRound's 25 TGas need + reserve
        let r = a.tick(5);
        assert_eq!(r.processed, 0, "nothing runs on too little gas");
        assert!(kinds(&a).contains(&Task::StartRound), "the task stays due");
    }

    #[test]
    fn heavy_tasks_wait_for_enough_gas_but_a_light_task_behind_them_still_runs() {
        let mut a = autonomous();
        a.enqueue(100, Task::ClaimFees { ticker: "pepe".into() }); // earlier than StartRound? both due; claim needs 215
        at_gas("poker.testnet", 0, 5_000, 100);
        let r = a.tick(8);
        assert_eq!(r.processed, 1, "only StartRound fits 100 TGas");
        assert!(kinds(&a).iter().any(|t| matches!(t, Task::ClaimFees { .. })), "the claim is still queued");
        at_gas("poker.testnet", 0, 5_000, 300);
        a.enqueue(5_000, Task::ClaimFees { ticker: "pepe".into() });
    }

    #[test]
    fn claim_fees_task_reschedules_itself_and_skips_unlaunched_or_outside_coins() {
        let mut a = autonomous();
        a.due.clear();
        at_gas("poker.testnet", 0, 5_000, 300);
        assert!(a.run_task(Task::ClaimFees { ticker: "pepe".into() }, 5_000), "a launched native coin is claimed");
        assert!(!a.run_task(Task::ClaimFees { ticker: "bat.testnet".into() }, 5_000), "an outside coin has no fees to claim");
        let next: Vec<_> = a.due_tasks(10).into_iter().filter(|d| matches!(&d.task, Task::ClaimFees { ticker } if ticker == "pepe")).collect();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].due_ms, 5_000 + a.cfg.claim_interval_ms);
    }

    #[test]
    fn resume_launch_watchdog_stops_once_launched_and_after_its_tries() {
        let mut a = autonomous();
        a.due.clear();
        at_gas("poker.testnet", 0, 5_000, 300);
        assert!(!a.run_task(Task::ResumeLaunch { ticker: "pepe".into(), tries: 0 }, 5_000), "already launched: dropped");
        assert!(a.due_tasks(10).is_empty());
        // a registered but unlaunched coin is resumed and watched again
        let mut e = a.coins.get("pepe").cloned().unwrap();
        e.launched = false;
        a.coins.insert("pepe".into(), e);
        assert!(a.run_task(Task::ResumeLaunch { ticker: "pepe".into(), tries: 0 }, 5_000));
        assert_eq!(a.due_tasks(10).len(), 1);
        a.due.clear();
        at_gas("poker.testnet", 0, 5_000, 300);
        assert!(a.run_task(Task::ResumeLaunch { ticker: "pepe".into(), tries: MAX_LAUNCH_TRIES - 1 }, 5_000));
        assert!(a.due_tasks(10).is_empty(), "no endless retries");
    }

    #[test]
    fn settle_task_waits_for_the_end_then_settles_pair_by_pair_and_queues_the_next_round() {
        let mut a = autonomous();
        at_gas("poker.testnet", 0, 5_000, 300);
        a.tick(1);
        let end = a.get_round(1).unwrap().end_ms;
        a.start_pending = false;
        assert!(!a.run_task(Task::SettleRound { round_id: 1 }, 6_000), "not over yet: re-queued for the end");
        assert!(a.due_tasks(10).iter().any(|d| d.task == Task::SettleRound { round_id: 1 } && d.due_ms == end));
        a.due.clear();
        at_gas("poker.testnet", 0, end + 1, 300);
        assert!(a.run_task(Task::SettleRound { round_id: 1 }, end + 1));
        assert_eq!(a.get_round(1).unwrap().settled_pairs, 1);
        assert!(kinds(&a).contains(&Task::StartRound), "the next round is queued by the settlement");
        assert!(!a.run_task(Task::SettleRound { round_id: 1 }, end + 2), "a second settle task is a harmless no-op");
    }

    #[test]
    fn inline_tick_needs_the_gas_and_pays_nobody() {
        let mut a = autonomous();
        a.protocol_near = NEAR1;
        at_gas("trader.testnet", 0, 5_000, 200); // below inline_tick_min_gas (250)
        a.inline_tick(Gas::from_tgas(10));
        assert!(a.get_round(1).is_none(), "too little gas: skipped");
        at_gas("trader.testnet", 0, 5_000, 300);
        let p0 = a.protocol_near;
        a.inline_tick(Gas::from_tgas(10));
        assert!(a.get_round(1).is_some(), "enough gas: the due StartRound ran");
        assert!(a.protocol_near < p0 && p0 - a.protocol_near < NEAR1 / 10, "only the round's storage left the bucket, no bounty");
        // a buy-sized reserve leaves no room for a heavy task
        a.enqueue(0, Task::ClaimFees { ticker: "pepe".into() });
        a.inline_tick(Gas::from_tgas(215));
        assert!(kinds(&a).iter().any(|t| matches!(t, Task::ClaimFees { ticker } if ticker == "pepe")), "heavy claim not run inside a buy");
    }

    // ---------------------------------------------------------------- Working Memes (M5)

    fn with_result(result: Option<&str>) {
        use near_sdk::PromiseResult;
        let r = match result {
            Some(line) => PromiseResult::Successful(near_sdk::serde_json::to_vec(&line).unwrap()),
            None => PromiseResult::Failed,
        };
        testing_env!(
            VMContextBuilder::new().predecessor_account_id(acct("arena.testnet")).current_account_id(acct("arena.testnet")).block_timestamp(5_000 * 1_000_000).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![r]
        );
    }

    const GOOD: &str = r#"{"ok":true,"job_id":1,"refused":false,"reason":"","text":"DOGE is a sleepy boomer","model":"Qwen/Qwen3.8-27B","chat_id":"abc","prompt_tokens":1,"completion_tokens":2}"#;

    fn hired(a: &mut Contract, deposit: u128) -> u64 {
        at("alice.testnet", deposit, 1_000);
        a.hire("pepe".into(), "roast".into(), "DOGE".into(), "make fun of it".into())
    }

    #[test]
    fn hire_escrows_the_payment_minus_fees_and_queues_the_deadline_refund() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, 5 * NEAR1 / 10);
        let j = a.get_job(id).unwrap();
        assert_eq!(j.amount.0, 5 * NEAR1 / 10 - 2 * 10u128.pow(22) - 3 * 10u128.pow(21), "fee and record reserve come off the top");
        assert_eq!(j.status, JobStatus::Requested);
        assert!(a.due_tasks(50).iter().any(|d| d.task == Task::RefundJob { job_id: id } && d.due_ms == 1_000 + 600_000));
        assert_eq!(j.prompt_hash.len(), 64, "only the hash is kept");
    }

    #[test]
    #[should_panic(expected = "below this coin")]
    fn hire_below_the_price_is_rejected() {
        let mut a = two_coins(0, 0);
        hired(&mut a, NEAR1 / 100);
    }

    #[test]
    #[should_panic(expected = "only NME coins have a brain")]
    fn hire_for_an_outside_coin_is_rejected() {
        let mut a = two_coins(0, 0);
        at("alice.testnet", NEAR1, 1_000);
        a.hire("bat.testnet".into(), "roast".into(), "".into(), "x".into());
    }

    #[test]
    #[should_panic(expected = "kind must be")]
    fn hire_with_an_unknown_kind_is_rejected() {
        let mut a = two_coins(0, 0);
        at("alice.testnet", NEAR1, 1_000);
        a.hire("pepe".into(), "withdraw".into(), "".into(), "x".into());
    }

    #[test]
    #[should_panic(expected = "longer than 500 bytes")]
    fn hire_with_a_long_prompt_is_rejected() {
        let mut a = two_coins(0, 0);
        at("alice.testnet", NEAR1, 1_000);
        a.hire("pepe".into(), "roast".into(), "".into(), "x".repeat(501));
    }

    #[test]
    fn a_delivery_keeps_the_cut_credits_the_chest_stores_the_text_and_queues_the_burn() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        let amount = a.get_job(id).unwrap().amount.0;
        let p0 = a.protocol_near;
        with_result(Some(GOOD));
        a.on_job_result(id);
        let j = a.get_job(id).unwrap();
        assert_eq!(j.status, JobStatus::Delivered);
        assert_eq!(a.protocol_near - p0, amount / 10, "10% stays in the protocol bucket");
        assert_eq!(chest(&a, "pepe"), amount - amount / 10, "the rest waits in the chest for the burn");
        let d = a.get_deliveries(5);
        assert_eq!((d.len(), d[0].text.as_str(), d[0].chat_id.as_str()), (1, "DOGE is a sleepy boomer", "abc"));
        assert!(a.due_tasks(50).iter().any(|t| t.task == Task::JobBurn { job_id: id }));
    }

    #[test]
    fn refusals_failures_null_and_garbage_all_refund_in_full_and_once() {
        let mut a = two_coins(0, 0);
        for result in [
            Some(r#"{"ok":true,"refused":true,"reason":"hate","text":""}"#),
            Some(r#"{"ok":false,"error":"NEAR AI returned HTTP 500"}"#),
            Some("null"),
            Some("not json at all"),
            None,
        ] {
            let id = hired(&mut a, NEAR1);
            let (c0, p0) = (chest(&a, "pepe"), a.protocol_near);
            with_result(result);
            a.on_job_result(id);
            assert_eq!(a.get_job(id).unwrap().status, JobStatus::Refunded, "{result:?}");
            assert_eq!((chest(&a, "pepe"), a.protocol_near), (c0, p0), "nothing is credited to any ledger");
            // a second (late or duplicate) answer changes nothing
            with_result(Some(GOOD));
            a.on_job_result(id);
            assert_eq!(a.get_job(id).unwrap().status, JobStatus::Refunded);
            assert!(a.get_deliveries(5).is_empty());
        }
    }

    #[test]
    fn the_deadline_refunds_and_a_late_answer_is_ignored() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        at("anyone.testnet", 0, 1_000 + 599_000);
        assert!(!a.run_task(Task::RefundJob { job_id: id }, 1_000 + 599_000) || a.get_job(id).unwrap().status == JobStatus::Refunded);
    }

    #[test]
    #[should_panic(expected = "deadline has not passed")]
    fn claim_refund_waits_for_the_deadline() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        at("anyone.testnet", 0, 100_000);
        a.claim_refund(id);
    }

    #[test]
    fn claim_refund_after_the_deadline_works_once_and_blocks_a_late_delivery() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        at("anyone.testnet", 0, 1_000 + 600_000);
        a.claim_refund(id);
        assert_eq!(a.get_job(id).unwrap().status, JobStatus::Refunded);
        with_result(Some(GOOD));
        a.on_job_result(id);
        assert_eq!(a.get_job(id).unwrap().status, JobStatus::Refunded, "a late delivery cannot resurrect a refunded job");
        assert_eq!(chest(&a, "pepe"), 0);
    }

    #[test]
    #[should_panic(expected = "not waiting for the brain")]
    fn a_delivered_job_cannot_be_refunded() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        with_result(Some(GOOD));
        a.on_job_result(id);
        at("anyone.testnet", 0, 2_000_000);
        a.claim_refund(id);
    }

    #[test]
    fn the_delivery_ring_keeps_the_last_32_newest_first() {
        let mut a = two_coins(0, 0);
        for i in 0..40u64 {
            let id = hired(&mut a, NEAR1);
            let line = GOOD.replace("DOGE is a sleepy boomer", &format!("line {i}"));
            with_result(Some(&line));
            a.on_job_result(id);
        }
        let d = a.get_deliveries(100);
        assert_eq!(d.len(), 32);
        assert_eq!(d[0].text, "line 39");
        assert_eq!(d[31].text, "line 8");
    }

    #[test]
    fn a_text_over_1kb_is_cut_on_a_char_boundary() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        let line = GOOD.replace("DOGE is a sleepy boomer", &"é".repeat(2000));
        with_result(Some(&line));
        a.on_job_result(id);
        let t = &a.get_deliveries(1)[0].text;
        assert!(t.len() <= 1000 && !t.is_empty());
    }

    #[test]
    fn job_burn_task_hands_the_net_payment_to_buy_and_burn_once() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        let net = {
            let amt = a.get_job(id).unwrap().amount.0;
            amt - amt / 10
        };
        with_result(Some(GOOD));
        a.on_job_result(id);
        at_gas("anyone.testnet", 0, 6_000, 300);
        assert!(a.run_task(Task::JobBurn { job_id: id }, 6_000));
        assert_eq!(a.get_job(id).unwrap().status, JobStatus::Burned);
        assert_eq!(chest(&a, "pepe"), 0, "the whole net payment left the chest into the buyback ({net})");
        assert!(!a.run_task(Task::JobBurn { job_id: id }, 6_001), "a second burn task is a no-op");
    }

    // ---------------------------------------------------------------- Self-Defending Memes (M6)

    fn treasury_of(a: &mut Contract, ticker: &str, amount: u128) {
        let mut e = a.coins.get(ticker).cloned().unwrap();
        e.treasury_near = U128(amount);
        a.coins.insert(ticker.into(), e);
    }

    fn with_pool_point(point: i64) {
        use near_sdk::PromiseResult;
        let body = format!(r#"{{"token_x":"pepe.nme.testnet","token_y":"wrap.testnet","state":"Running","total_x":"1","total_y":"1","current_point":{point}}}"#);
        testing_env!(
            VMContextBuilder::new().predecessor_account_id(acct("arena.testnet")).current_account_id(acct("arena.testnet")).block_timestamp(5_000 * 1_000_000).prepaid_gas(Gas::from_tgas(300)).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![PromiseResult::Successful(body.into_bytes())]
        );
    }

    fn defended() -> Contract {
        let mut a = two_coins(0, 0);
        treasury_of(&mut a, "pepe", 1_000_000);
        a.protocol_near = NEAR1;
        a.due.clear();
        a
    }

    fn queued(a: &Contract) -> Vec<Task> {
        a.due_tasks(50).into_iter().map(|d| d.task).collect()
    }

    #[test]
    fn config_rejects_caps_above_the_spec() {
        for bad in [
            DefenseConfig { per_action_bps: 501, ..defense() },
            DefenseConfig { daily_bps: 1_501, ..defense() },
            DefenseConfig { per_action_bps: 500, daily_bps: 400, ..defense() },
            DefenseConfig { cooldown_ms: 0, ..defense() },
            DefenseConfig { fallback_after: 0, ..defense() },
            DefenseConfig { look_points: 500, trigger_points: 100, ..defense() },
        ] {
            let r = std::panic::catch_unwind(|| Contract::check_defense(&bad));
            assert!(r.is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_decision_is_validated_never_clamped() {
        let mut a = defended();
        at("anyone.testnet", 0, 10_000);
        assert!(a.apply_defense("pepe", 501, "ai", "x", 10_000).unwrap_err().contains("per-action cap"));
        assert!(a.apply_defense("pepe", 0, "ai", "x", 10_000).is_err());
        assert!(queued(&a).iter().all(|t| !matches!(t, Task::DefenseBurn { .. })), "a rejected decision queues no burn");
        assert!(a.apply_defense("pepe", 500, "ai", "paper hands", 10_000).is_ok());
        assert!(queued(&a).contains(&Task::DefenseBurn { ticker: "pepe".into(), bps: 500 }));
        // inside the cooldown
        assert!(a.apply_defense("pepe", 100, "ai", "x", 10_000 + 59_000).unwrap_err().contains("cooldown"));
        // after the cooldown, until the daily cap (1000 bps) is reached
        assert!(a.apply_defense("pepe", 500, "ai", "x", 10_000 + 61_000).is_ok());
        assert!(a.apply_defense("pepe", 1, "ai", "x", 10_000 + 130_000).unwrap_err().contains("daily cap"));
        // a new day resets the daily budget
        assert!(a.apply_defense("pepe", 500, "ai", "x", 10_000 + 130_000 + DAY_MS).is_ok());
    }

    #[test]
    fn a_calm_price_or_a_poor_treasury_is_never_defended() {
        let mut a = defended();
        a.defense_state.insert("pepe".into(), DefenseState { high_point: 1_000, high_ms: 4_000, ..Default::default() });
        with_pool_point(950); // 50 points below the high, look threshold is 100
        a.on_defense_pool("pepe".into());
        assert!(queued(&a).is_empty(), "no brain consultation, no burn");
        assert_eq!(a.protocol_near, NEAR1, "no fee spent");
    }

    #[test]
    fn a_drop_consults_the_brain_and_pays_the_fee_from_the_protocol_bucket_within_its_daily_cap() {
        let mut a = defended();
        a.defense_state.insert("pepe".into(), DefenseState { high_point: 1_000, high_ms: 4_000, ..Default::default() });
        with_pool_point(700);
        a.on_defense_pool("pepe".into());
        assert_eq!(a.protocol_near, NEAR1 - 10u128.pow(21), "one consultation fee left the protocol bucket");
        assert_eq!(a.fee_day_spent, 10u128.pow(21));
        // the daily fee cap is 3 consultations; after that the fixed rule decides instead of the brain
        a.fee_day_spent = 3 * 10u128.pow(21);
        a.defense_state.insert("pepe".into(), DefenseState { high_point: 5_000, high_ms: 4_000, ..Default::default() });
        let p0 = a.protocol_near;
        with_pool_point(2_000); // 3000 points down: beyond the rule's trigger (2232)
        a.on_defense_pool("pepe".into());
        assert_eq!(a.protocol_near, p0, "no fee: the brain was not asked");
        assert!(queued(&a).contains(&Task::DefenseBurn { ticker: "pepe".into(), bps: 500 }), "the fixed rule bought back, at the per-action cap");
    }

    #[test]
    fn after_n_misses_the_fixed_rule_takes_over_and_a_valid_decision_resets_the_count() {
        let mut a = defended();
        a.defense_state.insert("pepe".into(), DefenseState { high_point: 5_000, high_ms: 4_000, misses: 3, ..Default::default() });
        with_pool_point(2_000);
        a.on_defense_pool("pepe".into());
        let burns: Vec<_> = queued(&a).into_iter().filter(|t| matches!(t, Task::DefenseBurn { .. })).collect();
        assert_eq!(burns.len(), 1, "the dead brain is replaced by the rule: {burns:?}");
        // below the rule's trigger the rule does nothing
        let mut b = defended();
        b.defense_state.insert("pepe".into(), DefenseState { high_point: 1_000, high_ms: 4_000, misses: 3, ..Default::default() });
        with_pool_point(500);
        b.on_defense_pool("pepe".into());
        assert!(queued(&b).is_empty());
    }

    #[test]
    fn null_garbage_failure_and_over_cap_answers_are_misses_and_spend_nothing() {
        let mut a = defended();
        for (n, result) in [Some("null"), Some("garbage"), None, Some(r#"{"ok":true,"action":"buyback","bps":900,"reason":"all in"}"#)].into_iter().enumerate() {
            with_result(result);
            a.on_defense_result("pepe".into());
            assert_eq!(a.get_defense_state("pepe".into()).misses as usize, n + 1, "{result:?}");
        }
        assert!(queued(&a).is_empty(), "nothing was queued to burn");
        assert_eq!(chest(&a, "pepe"), 0);
    }

    #[test]
    fn a_valid_hold_costs_nothing_and_a_valid_buyback_within_caps_queues_the_burn() {
        let mut a = defended();
        a.defense_state.insert("pepe".into(), DefenseState { misses: 2, ..Default::default() });
        with_result(Some(r#"{"ok":true,"action":"hold","bps":0,"reason":"just a rotation"}"#));
        a.on_defense_result("pepe".into());
        assert_eq!(a.get_defense_state("pepe".into()).misses, 0, "a valid answer resets the miss count");
        assert!(queued(&a).is_empty());
        with_result(Some(r#"{"ok":true,"action":"buyback","bps":300,"reason":"paper hands detected"}"#));
        a.on_defense_result("pepe".into());
        assert!(queued(&a).contains(&Task::DefenseBurn { ticker: "pepe".into(), bps: 300 }));
        assert_eq!(a.get_defense_state("pepe".into()).day_spent_bps, 300);
    }

    #[test]
    fn the_burn_task_spends_exactly_the_decided_share_of_the_treasury_and_only_once_per_decision() {
        let mut a = defended();
        at_gas("anyone.testnet", 0, 6_000, 300);
        assert!(a.run_task(Task::DefenseBurn { ticker: "pepe".into(), bps: 300 }, 6_000));
        assert_eq!(a.get_coin("pepe".into()).unwrap().treasury_near.0, 1_000_000 - 30_000, "3% of the treasury, debited before the swap");
        let mut b = defended();
        treasury_of(&mut b, "pepe", 500); // below min_treasury
        assert!(!b.run_task(Task::DefenseBurn { ticker: "pepe".into(), bps: 300 }, 6_000));
    }

    #[test]
    fn a_check_keeps_one_consultation_per_coin_in_flight_and_always_requeues_itself() {
        let mut a = defended();
        at_gas("anyone.testnet", 0, 6_000, 300);
        assert!(a.run_task(Task::DefenseCheck { ticker: "pepe".into() }, 6_000));
        assert!(!a.run_task(Task::DefenseCheck { ticker: "pepe".into() }, 6_001), "the second one finds a check in flight");
        let n = queued(&a).iter().filter(|t| matches!(t, Task::DefenseCheck { .. })).count();
        assert_eq!(n, 2, "each run queued the next one");
        // a coin without treasury is not even looked at, but is still re-queued
        let mut c = defended();
        treasury_of(&mut c, "pepe", 0);
        assert!(!c.run_task(Task::DefenseCheck { ticker: "pepe".into() }, 6_000));
        assert_eq!(queued(&c).len(), 1);
    }

    #[test]
    fn a_tick_runs_only_one_chain_starting_task_so_its_gas_is_not_shared() {
        let mut a = autonomous();
        a.due.clear();
        a.start_pending = true; // keep StartRound out of this test
        a.enqueue(100, Task::ClaimFees { ticker: "pepe".into() });
        a.enqueue(200, Task::DefenseCheck { ticker: "pepe".into() });
        a.enqueue(300, Task::RefundJob { job_id: 99 });
        at_gas("poker.testnet", 0, 5_000, 300);
        let r = a.tick(8);
        assert_eq!(r.processed, 2, "the claim ran, the second chain-starter waited, the light RefundJob (a no-op here) still ran");
        assert!(kinds(&a).iter().any(|t| matches!(t, Task::DefenseCheck { .. })), "the defense check is still queued for the next tick");
    }

    // ---------------------------------------------------------------- Commentary

    const LINE: &str = r#"{"ok":true,"job_id":0,"refused":false,"reason":"","text":"I smell victory and also swamp","model":"Qwen/Qwen3.8-27B","chat_id":"c1"}"#;

    fn talking() -> Contract {
        let mut a = two_coins(0, 0);
        treasury_of(&mut a, "pepe", 1_000_000);
        a.protocol_near = NEAR1;
        a.due.clear();
        a
    }

    #[test]
    fn a_war_queues_commentary_for_native_coins_only() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        a.due.clear();
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        let lines: Vec<_> = queued(&a).into_iter().filter_map(|t| match t { Task::Commentary { ticker, event, .. } => Some((ticker, event)), _ => None }).collect();
        assert_eq!(lines, vec![("pepe".to_string(), "war_started".to_string())], "the outside coin has no brain, so no line");
    }

    #[test]
    fn the_brain_stays_silent_below_its_tier_and_after_its_daily_limit_and_without_fee_budget() {
        let mut a = talking();
        at_gas("p.testnet", 0, 5_000, 300);
        treasury_of(&mut a, "pepe", 10);
        assert!(!a.run_commentary("pepe".into(), "war_started".into(), "x".into(), 5_000), "tier: treasury below the minimum");
        treasury_of(&mut a, "pepe", 1_000_000);
        assert!(a.run_commentary("pepe".into(), "war_started".into(), "x".into(), 5_000));
        at_gas("p.testnet", 0, 5_001, 300);
        assert!(a.run_commentary("pepe".into(), "war_won".into(), "x".into(), 5_001));
        at_gas("p.testnet", 0, 5_002, 300);
        assert!(a.run_commentary("pepe".into(), "war_lost".into(), "x".into(), 5_002));
        at_gas("p.testnet", 0, 5_003, 300);
        assert!(!a.run_commentary("pepe".into(), "war_started".into(), "x".into(), 5_003), "daily limit of 3 lines per coin");
        assert_eq!(a.protocol_near, NEAR1 - 3 * 10u128.pow(21), "each line cost one fee from the protocol bucket, nothing else moved");
        // a new day resets the limit; an empty bucket silences the brain
        a.protocol_near = 0;
        at_gas("p.testnet", 0, 5_004 + DAY_MS, 300);
        assert!(!a.run_commentary("pepe".into(), "war_started".into(), "x".into(), 5_004 + DAY_MS), "no fee budget: no consultation");
    }

    #[test]
    fn a_line_is_appended_labelled_and_changes_no_ledger() {
        let mut a = talking();
        let (c0, t0, p0) = (chest(&a, "pepe"), a.get_coin("pepe".into()).unwrap().treasury_near.0, a.protocol_near);
        with_result(Some(LINE));
        a.on_commentary_result("pepe".into(), "war_started".into());
        let c = a.get_commentary(5);
        assert_eq!((c.len(), c[0].text.as_str(), c[0].event.as_str(), c[0].chat_id.as_str()), (1, "I smell victory and also swamp", "war_started", "c1"));
        assert_eq!((chest(&a, "pepe"), a.get_coin("pepe".into()).unwrap().treasury_near.0, a.protocol_near), (c0, t0, p0), "commentary is append-only (I8)");
        assert!(queued(&a).is_empty(), "and queues nothing");
    }

    #[test]
    fn failed_null_refused_and_garbage_runs_drop_the_line() {
        let mut a = talking();
        for r in [None, Some("null"), Some("garbage"), Some(r#"{"ok":true,"refused":true,"reason":"x","text":""}"#), Some(r#"{"ok":true,"refused":false,"text":"   "}"#)] {
            with_result(r);
            a.on_commentary_result("pepe".into(), "war_started".into());
        }
        assert!(a.get_commentary(5).is_empty());
    }

    #[test]
    fn the_ring_keeps_the_last_64_newest_first_and_lines_are_capped() {
        let mut a = talking();
        for i in 0..70 {
            let line = LINE.replace("I smell victory and also swamp", &format!("line {i}"));
            with_result(Some(&line));
            a.on_commentary_result("pepe".into(), "war_started".into());
        }
        let c = a.get_commentary(100);
        assert_eq!((c.len(), c[0].text.as_str(), c[63].text.as_str()), (64, "line 69", "line 6"));
        let long = LINE.replace("I smell victory and also swamp", &"é".repeat(400));
        with_result(Some(&long));
        a.on_commentary_result("pepe".into(), "war_started".into());
        assert!(a.get_commentary(1)[0].text.len() <= 300);
    }

    // ---------------------------------------------------------------- PumpBag deadlines (M4)

    #[test]
    #[should_panic(expected = "only the PumpBag collection")]
    fn only_the_collection_sets_deadlines() {
        let mut a = two_coins(0, 0);
        at("mallory.testnet", 0, 1_000);
        a.crown_deadline("pepe".into(), 5_000);
    }

    #[test]
    fn heartbeats_move_the_deadline_without_adding_tasks() {
        let mut a = two_coins(0, 0);
        a.due.clear();
        at("crown.nme.testnet", 0, 1_000);
        a.crown_deadline("pepe".into(), 10_000);
        a.crown_deadline("pepe".into(), 20_000);
        a.crown_deadline("pepe".into(), 30_000);
        let n = queued(&a).iter().filter(|t| matches!(t, Task::CrownForfeit { .. })).count();
        assert_eq!(n, 1, "one task per PumpBag, however many heartbeats");
        assert_eq!(a.get_crown_deadline("pepe".into()), Some(30_000));
    }

    #[test]
    fn the_task_waits_for_a_moved_deadline_and_forfeits_once_it_has_passed() {
        let mut a = two_coins(0, 0);
        a.due.clear();
        at("crown.nme.testnet", 0, 1_000);
        a.crown_deadline("pepe".into(), 10_000);
        a.crown_deadline("pepe".into(), 50_000); // a heartbeat
        at_gas("poker.testnet", 0, 11_000, 300);
        assert!(!a.run_task(Task::CrownForfeit { ticker: "pepe".into() }, 11_000), "not due yet: no forfeit call, no bounty");
        assert!(queued(&a).contains(&Task::CrownForfeit { ticker: "pepe".into() }), "it looks again at the new deadline");
        a.due.clear();
        at_gas("poker.testnet", 0, 51_000, 300);
        assert!(a.run_task(Task::CrownForfeit { ticker: "pepe".into() }, 51_000), "past the deadline: the forfeit was requested");
        assert!(queued(&a).is_empty(), "and nothing is re-queued after a forfeit");
        // a later heartbeat-style call starts a fresh task again (e.g. after a transfer)
        at("crown.nme.testnet", 0, 52_000);
        a.crown_deadline("pepe".into(), 90_000);
        assert_eq!(queued(&a).iter().filter(|t| matches!(t, Task::CrownForfeit { .. })).count(), 1);
    }

    #[test]
    fn an_unknown_deadline_makes_the_task_a_noop() {
        let mut a = two_coins(0, 0);
        a.due.clear();
        at_gas("poker.testnet", 0, 51_000, 300);
        assert!(!a.run_task(Task::CrownForfeit { ticker: "ghost".into() }, 51_000));
    }

    #[test]
    #[should_panic(expected = "only the gateway can buy for a beneficiary")]
    fn only_the_gateway_can_buy_for_a_beneficiary() {
        let mut a = two_coins(0, 0);
        at("mallory.testnet", NEAR1, 1_000);
        let _ = a.buy_for(acct("victim.testnet"), "pepe".into(), U128(1));
    }

    #[test]
    fn the_gateway_buys_for_the_beneficiary() {
        let mut a = two_coins(0, 0);
        at("gateway.nme.testnet", NEAR1, 1_000);
        let _ = a.buy_for(acct("alice.testnet"), "pepe".into(), U128(1)); // builds the registration/balance checks for alice, not for the gateway
    }

    // ---------------------------------------------------------------- wallet owners (D-57): buy_for_wallet, owner-tagged sells

    const SOL_OWNER: &str = "sol:GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB";

    #[test]
    #[should_panic(expected = "only the gateway can buy for a wallet")]
    fn only_the_gateway_can_buy_for_a_wallet() {
        let mut a = two_coins(0, 0);
        at("mallory.testnet", NEAR1, 1_000);
        a.buy_for_wallet(SOL_OWNER.into(), "pepe".into(), U128(1), None);
    }

    #[test]
    fn the_gateway_buys_for_a_wallet_and_the_chain_is_detached() {
        let mut a = two_coins(0, 0);
        at("gateway.nme.testnet", NEAR1, 1_000);
        a.buy_for_wallet(SOL_OWNER.into(), "pepe".into(), U128(1), None); // returns nothing: a later failure never fails this call
        at("gateway.nme.testnet", NEAR1, 1_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.buy_for_wallet(SOL_OWNER.into(), "ghost".into(), U128(1), None))).is_err(), "unknown coin: refused synchronously");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.buy_for_wallet("alice.testnet".into(), "pepe".into(), U128(1), Some(false)))).is_ok(), "the arena does not parse the key, the gateway did");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.buy_for_wallet(String::new(), "pepe".into(), U128(1), None))).is_err(), "empty owner");
    }

    #[test]
    fn joining_a_war_needs_an_open_war_for_the_coin() {
        let mut a = two_coins(0, 0);
        at("gateway.nme.testnet", NEAR1, 1_000);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.buy_for_wallet(SOL_OWNER.into(), "pepe".into(), U128(1), Some(true))));
        assert!(r.is_err(), "no round is active");
    }

    #[test]
    fn wallet_owners_score_as_their_own_distinct_fighters() {
        let a = two_coins(0, 0);
        let (x, y) = (a.wallet_key(SOL_OWNER.into()), a.wallet_key("evm:0x1111111111111111111111111111111111111111".into()));
        assert_ne!(x, y);
        assert_ne!(x, acct("gateway.nme.testnet"));
        assert_eq!(x, a.wallet_key(SOL_OWNER.into()), "deterministic");
        assert_eq!(x.as_str().len(), 64, "a hex sha256: no one can hold a key for it");
    }

    #[test]
    fn an_owner_tagged_sale_is_honored_only_from_the_gateway() {
        let mut a = two_coins(0, 0);
        let msg = json!({ "sell": { "min_out": "1", "owner": SOL_OWNER } }).to_string();
        // a plain user cannot sell "for" a wallet owner: the tokens go straight back
        at("pepe.nme.testnet", 0, 1_000);
        match a.ft_on_transfer(acct("mallory.testnet"), U128(100), msg.clone()) {
            PromiseOrValue::Value(v) => assert_eq!(v.0, 100),
            _ => panic!("must be refused"),
        }
        // the gateway can: the swap is started (a promise), and the plain form still works for anyone
        at("pepe.nme.testnet", 0, 1_000);
        assert!(matches!(a.ft_on_transfer(acct("gateway.nme.testnet"), U128(100), msg), PromiseOrValue::Promise(_)));
        at("pepe.nme.testnet", 0, 1_000);
        assert!(matches!(a.ft_on_transfer(acct("alice.testnet"), U128(100), json!({"sell": {"min_out": "1"}}).to_string()), PromiseOrValue::Promise(_)));
    }

    #[test]
    fn a_wallet_sale_keeps_the_proceeds_as_wnear_for_the_gateway() {
        let m = swap_msg_keep_wnear("p|wrap.testnet|10000", "wrap.testnet", 5, &acct("gateway.nme.testnet"));
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["Swap"]["skip_unwrap_near"], json!(true));
        assert_eq!(v["Swap"]["swap_out_recipient"], json!("gateway.nme.testnet"));
        assert_eq!(v["Swap"]["min_output_amount"], json!("5"));
    }

    // ---------------------------------------------------------------- per-coin job prices

    #[test]
    fn a_coin_without_a_price_costs_the_global_minimum_and_a_set_price_binds_hire() {
        let mut a = two_coins(0, 0);
        assert_eq!(a.get_job_price("pepe".into()).0, 10u128.pow(23), "default = the global minimum");
        at("nme.testnet", 0, 1_000);
        a.set_job_price("pepe".into(), U128(3 * 10u128.pow(23)));
        assert_eq!(a.get_job_price("pepe".into()).0, 3 * 10u128.pow(23));
        // 0.2 NEAR no longer buys a job of this coin, 0.3 does
        at("alice.testnet", 2 * 10u128.pow(23), 2_000);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { a.hire("pepe".into(), "roast".into(), "".into(), "x".into()); }));
        assert!(r.is_err(), "below the creator's price");
        at("alice.testnet", 3 * 10u128.pow(23), 2_000);
        assert_eq!(a.hire("pepe".into(), "roast".into(), "".into(), "x".into()), 1);
    }

    #[test]
    fn the_job_price_is_set_once_by_the_factory_within_bounds() {
        let mut a = two_coins(0, 0);
        at("mallory.testnet", 0, 1_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.set_job_price("pepe".into(), U128(10u128.pow(24))))).is_err(), "factory only");
        at("nme.testnet", 0, 1_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.set_job_price("pepe".into(), U128(1)))).is_err(), "below the minimum");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.set_job_price("pepe".into(), U128(11 * 10u128.pow(24))))).is_err(), "above 10 NEAR");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.set_job_price("bat.testnet".into(), U128(10u128.pow(24))))).is_err(), "outside coins have no brain");
        a.set_job_price("pepe".into(), U128(10u128.pow(24)));
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.set_job_price("pepe".into(), U128(2 * 10u128.pow(24))))).is_err(), "fixed at launch");
        assert_eq!(a.get_job_price("pepe".into()).0, 10u128.pow(24));
    }

    #[test]
    fn a_defense_buyback_makes_the_brain_say_something() {
        let mut a = defended();
        at("anyone.testnet", 0, 10_000);
        a.apply_defense("pepe", 300, "ai", "paper hands", 10_000).unwrap();
        assert!(queued(&a).iter().any(|t| matches!(t, Task::Commentary { ticker, event, .. } if ticker == "pepe" && event == "defense_buyback")));
    }

    // ---------------------------------------------------------------- meme (image) jobs

    fn hire_meme(a: &mut Contract) -> u64 {
        at("alice.testnet", NEAR1, 1_000);
        a.hire("pepe".into(), "meme".into(), "BATMAN".into(), "a frog laughing".into())
    }

    fn meme_line(url: &str, hash: &str) -> String {
        format!(r#"{{"ok":true,"job_id":1,"refused":false,"reason":"","text":"gotham is ngmi","model":"Qwen/Qwen3.8-27B","chat_id":"c","media_url":"{url}","media_hash":"{hash}"}}"#)
    }

    #[test]
    fn a_meme_delivery_stores_the_picture_reference_and_text_jobs_have_none() {
        let mut a = two_coins(0, 0);
        let id = hire_meme(&mut a);
        let (url, hash) = ("https://test.fastfs.io/nme-media.testnet/outlayer.testnet/".to_string() + &"a".repeat(64) + ".jpg", "a".repeat(64));
        with_result(Some(&meme_line(&url, &hash)));
        a.on_job_result(id);
        assert_eq!(a.get_job(id).unwrap().status, JobStatus::Delivered);
        let d = a.get_deliveries(5);
        assert_eq!((d[0].media_url.as_str(), d[0].media_hash.as_str(), d[0].text.as_str()), (url.as_str(), hash.as_str(), "gotham is ngmi"));
        // the next (text) job reuses a later slot and carries no picture
        at("alice.testnet", NEAR1, 2_000);
        let id2 = a.hire("pepe".into(), "roast".into(), "DOGE".into(), "x".into());
        with_result(Some(GOOD));
        a.on_job_result(id2);
        let d = a.get_deliveries(5);
        assert_eq!(d[0].media_url, "");
        assert_eq!(d[1].media_url, url, "the older picture stays with its own delivery");
    }

    #[test]
    fn a_meme_without_a_valid_picture_is_refunded() {
        let mut a = two_coins(0, 0);
        for (url, hash) in [("", ""), ("http://x/y.jpg", &"a".repeat(64)[..]), ("https://x/y.jpg", "short"), ("https://x/y.jpg", &"z".repeat(64)[..])] {
            let id = hire_meme(&mut a);
            let c0 = chest(&a, "pepe");
            with_result(Some(&meme_line(url, hash)));
            a.on_job_result(id);
            assert_eq!(a.get_job(id).unwrap().status, JobStatus::Refunded, "{url} {hash}");
            assert_eq!(chest(&a, "pepe"), c0);
            assert!(a.get_deliveries(5).is_empty());
        }
    }

    // ---------------------------------------------------------------- raids (NEAR Social)
    use crate::raids::{parse_social_post, post_has_tag, raid_tag, RAID_BLOCKS, RAID_CLAIM_DEPOSIT, RAID_GRACE_BLOCKS, RAID_START_DEPOSIT};

    /// A SocialDB `get` answer with `with_block_height`, in the exact shape the testnet contract returned (2026-11-11).
    fn social_answer(account: &str, text: &str, block: u64) -> String {
        let post = near_sdk::serde_json::to_string(&json!({ "type": "md", "text": text })).unwrap();
        json!({ ":block": block, account: { ":block": block, "post": { ":block": block, "main": { ":block": block, "": post } } } }).to_string()
    }

    fn raid_ctx(who: &str, deposit: u128, block: u64, ms: u64) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(acct(who))
            .current_account_id(acct("arena.nme.testnet"))
            .attached_deposit(NearToken::from_yoctonear(deposit))
            .block_index(block)
            .block_timestamp(ms * 1_000_000)
            .prepaid_gas(Gas::from_tgas(300))
            .build());
    }

    fn raid_callback(results: Vec<near_sdk::PromiseResult>, block: u64, ms: u64) {
        testing_env!(
            VMContextBuilder::new().predecessor_account_id(acct("arena.nme.testnet")).current_account_id(acct("arena.nme.testnet")).block_index(block).block_timestamp(ms * 1_000_000).prepaid_gas(Gas::from_tgas(300)).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            results
        );
    }

    fn ok(body: String) -> near_sdk::PromiseResult {
        near_sdk::PromiseResult::Successful(body.into_bytes())
    }

    /// pepe launched with a 10 NEAR treasury and raid 1 started at block 1000.
    fn raided() -> Contract {
        let mut a = two_coins(0, 0);
        treasury_of(&mut a, "pepe", 10 * NEAR1);
        raid_ctx("starter.testnet", RAID_START_DEPOSIT, 1_000, 1_000);
        let r = a.start_raid("pepe".into());
        assert_eq!((r.id, r.tag.as_str(), r.start_block, r.end_block), (1, "nme_pepe_raid_1", 1_000, 1_000 + RAID_BLOCKS));
        a
    }

    fn claim(a: &mut Contract, who: &str, text: &str, post_block: u64, balance: &str) -> bool {
        raid_ctx(who, RAID_CLAIM_DEPOSIT, 1_500, 2_000);
        let _ = a.claim_raid(1);
        assert_eq!(a.raid_claim_of(1, acct(who)), Some(false), "reserved while the check runs");
        raid_callback(vec![ok(social_answer(who, text, post_block)), ok(format!("\"{balance}\""))], 1_510, 2_010);
        a.on_raid_claim(1, acct(who))
    }

    #[test]
    fn raid_tags_are_social_hashtags_and_match_whole_words() {
        assert_eq!(raid_tag("pepe", 7), "nme_pepe_raid_7");
        assert_eq!(raid_tag("nme-batman.testnet", 2), "nme_nmebatman_raid_2");
        assert!(post_has_tag("ape in #NME_PEPE_RAID_7 now", "nme_pepe_raid_7"));
        assert!(post_has_tag("#nme_pepe_raid_7", "nme_pepe_raid_7"));
        assert!(!post_has_tag("#nme_pepe_raid_77", "nme_pepe_raid_7"), "a longer tag is another raid");
        assert!(!post_has_tag("nme_pepe_raid_7 without hash", "nme_pepe_raid_7"));
        let raw = social_answer("bob.testnet", "hi #x", 42);
        assert_eq!(parse_social_post(raw.as_bytes(), "bob.testnet"), Some(("hi #x".to_string(), 42)));
        assert_eq!(parse_social_post(raw.as_bytes(), "eve.testnet"), None);
        assert_eq!(parse_social_post(b"{}", "bob.testnet"), None);
    }

    #[test]
    fn a_holder_post_with_the_tag_inside_the_window_counts_once() {
        let mut a = raided();
        assert!(claim(&mut a, "bob.testnet", "PEPE army #nme_pepe_raid_1", 1_200, "5"));
        assert_eq!(a.raid_claim_of(1, acct("bob.testnet")), Some(true));
        assert_eq!(a.get_raid(1).unwrap().claims, 1);
        assert_eq!(a.feed_since(0, 100).last().unwrap().kind, "raid_claimed");
        raid_ctx("bob.testnet", RAID_CLAIM_DEPOSIT, 1_600, 3_000);
        let again = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.claim_raid(1);
        }));
        assert!(again.is_err(), "one claim per account per raid");
    }

    #[test]
    fn posts_that_do_not_count_release_the_reservation() {
        let mut a = raided();
        for (who, text, block, bal) in [
            ("w1.testnet", "no tag here", 1_200, "5"),
            ("w2.testnet", "#nme_pepe_raid_1", 999, "5"),
            ("w3.testnet", "#nme_pepe_raid_1", 1_000 + RAID_BLOCKS + 1, "5"),
            ("w4.testnet", "#nme_pepe_raid_1", 1_200, "0"),
        ] {
            assert!(!claim(&mut a, who, text, block, bal), "{who} should not count");
            assert_eq!(a.raid_claim_of(1, acct(who)), None, "{who} may try again after a real post");
        }
        // No post at all (SocialDB returns an empty object) and a failed balance read.
        raid_ctx("w5.testnet", RAID_CLAIM_DEPOSIT, 1_500, 2_000);
        let _ = a.claim_raid(1);
        raid_callback(vec![ok("{}".into()), near_sdk::PromiseResult::Failed], 1_510, 2_010);
        assert!(!a.on_raid_claim(1, acct("w5.testnet")));
        assert_eq!(a.get_raid(1).unwrap().claims, 0);
    }

    #[test]
    fn raid_posts_add_capped_war_points_while_the_coin_fights() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        treasury_of(&mut a, "pepe", 10 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        let stake = a.get_round(1).unwrap().pairs[0].stake.0;
        raid_ctx("starter.testnet", RAID_START_DEPOSIT, 1_000, 1_000);
        a.start_raid("pepe".into());
        for i in 0..12 {
            assert!(claim(&mut a, &format!("h{i}.testnet"), "go #nme_pepe_raid_1", 1_100, "1"));
        }
        let p = &a.get_round(1).unwrap().pairs[0];
        assert_eq!(p.score_a.0, stake * 2_000 / 10_000, "2% of the stake per post, at most 20% per raid");
        assert_eq!((p.unique_a, p.score_b.0), (0, 0), "raids never count as buying wallets");
    }

    #[test]
    fn a_raid_settles_after_its_window_by_burning_part_of_the_treasury() {
        let mut a = raided();
        for w in ["a.testnet", "b.testnet", "c.testnet"] {
            assert!(claim(&mut a, w, "#nme_pepe_raid_1", 1_100, "1"));
        }
        raid_ctx("keeper.testnet", 0, 1_000 + RAID_BLOCKS, 9_000);
        assert!(!a.settle_raid(1, 9_000), "still in the window or the grace: look again later");
        raid_ctx("keeper.testnet", 0, 1_000 + RAID_BLOCKS + RAID_GRACE_BLOCKS + 1, 10_000);
        let t0 = a.get_coin("pepe".into()).unwrap().treasury_near.0;
        assert!(a.settle_raid(1, 10_000));
        let r = a.get_raid(1).unwrap();
        assert_eq!(r.state, RaidState::Settled);
        assert_eq!(r.burned_near.0, t0 * 30 / 10_000, "3 posts x 0.1% of the treasury");
        assert_eq!(a.get_coin("pepe".into()).unwrap().treasury_near.0, t0 - r.burned_near.0, "debited into the buy-and-burn");
        assert!(!a.settle_raid(1, 10_000), "settles once");
        raid_ctx("late.testnet", RAID_CLAIM_DEPOSIT, 1_000 + RAID_BLOCKS + RAID_GRACE_BLOCKS + 2, 10_100);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = a.claim_raid(1);
        }))
        .is_err(), "no claims after the raid");
    }

    #[test]
    fn one_raid_per_coin_with_a_cooldown_and_a_storage_deposit() {
        let mut a = raided();
        raid_ctx("x.testnet", RAID_START_DEPOSIT, 1_100, 2_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            a.start_raid("pepe".into());
        }))
        .is_err(), "already running");
        raid_ctx("x.testnet", RAID_START_DEPOSIT - 1, 1_100, 2_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            a.start_raid("bat.testnet".into());
        }))
        .is_err(), "deposit required");
        raid_ctx("x.testnet", RAID_START_DEPOSIT, 1_100, 2_000);
        assert_eq!(a.start_raid("bat.testnet".into()).tag, "nme_bat_raid_2", "other coins raid independently");
        assert!(a.due_tasks(50).iter().any(|t| t.task == Task::RaidSettle { raid_id: 1 }), "settling is queued for the tick");
    }

    // ---------------------------------------------------------------- Proof of Thought
    const RQ: &str = "4fe6f78cbff4c0fb522e34e1f365f23d313f3cf57a149065a314ab3832c21897";
    const RS: &str = "FDB968570C5CCA4A18BA106C77CF7545959020ED42FF2F1934159334425BA843";

    #[test]
    fn a_delivery_keeps_its_proof_hashes_and_a_bad_or_missing_proof_is_stored_as_empty() {
        let mut a = two_coins(0, 0);
        let id = hired(&mut a, NEAR1);
        let good = GOOD.replace("\"completion_tokens\":2}", &format!("\"completion_tokens\":2,\"request_hash\":\"{RQ}\",\"response_hash\":\"{RS}\"}}"));
        with_result(Some(&good));
        a.on_job_result(id);
        let d = &a.get_deliveries(5)[0];
        assert_eq!((d.request_hash.as_str(), d.response_hash.as_str()), (RQ, RS.to_ascii_lowercase().as_str()), "stored, lower-cased");
        // A later delivery in another slot with a malformed proof records none (and never panics).
        let id2 = hired(&mut a, NEAR1);
        with_result(Some(&GOOD.replace("\"completion_tokens\":2}", "\"completion_tokens\":2,\"request_hash\":\"zz\",\"response_hash\":\"\"}")));
        a.on_job_result(id2);
        let d2 = &a.get_deliveries(5)[0];
        assert_eq!((d2.request_hash.as_str(), d2.response_hash.as_str()), ("", ""));
        assert_eq!(a.get_deliveries(5)[1].request_hash, RQ, "the first delivery keeps its own proof");
    }

    #[test]
    fn a_commentary_line_keeps_its_proof_for_its_own_seq_only() {
        let mut a = talking();
        let line = LINE.replace("\"chat_id\":\"c1\"}", &format!("\"chat_id\":\"c1\",\"request_hash\":\"{RQ}\",\"response_hash\":\"{RQ}\"}}"));
        with_result(Some(&line));
        a.on_commentary_result("pepe".into(), "war_started".into());
        let seq = a.get_commentary(5)[0].seq;
        assert_eq!(a.commentary_proof(seq).unwrap()["request_hash"], RQ);
        assert!(a.commentary_proof(seq + COMMENTARY_RING).is_none(), "another seq in the same slot has no proof");
    }

    // ---------------------------------------------------------------- spoils of war
    use crate::spoils::SPOILS_CLAIM_MS;

    fn cb(result: Option<&str>, ms: u64) {
        let r = match result {
            Some(body) => near_sdk::PromiseResult::Successful(body.as_bytes().to_vec()),
            None => near_sdk::PromiseResult::Failed,
        };
        testing_env!(
            VMContextBuilder::new().predecessor_account_id(acct("arena.testnet")).current_account_id(acct("arena.testnet")).block_timestamp(ms * 1_000_000).prepaid_gas(Gas::from_tgas(300)).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![r]
        );
    }

    /// bat wins round 1 (w3 and w4 scored 1 NEAR each), the buy lands 1000 bat tokens: 500 burned, 500 reserved.
    fn won_with_spoils() -> Contract {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        score_both(&mut a, &[("w1.testnet", NEAR1 / 2), ("w2.testnet", NEAR1 / 2)], &[("w3.testnet", NEAR1), ("w4.testnet", NEAR1)]);
        at("anyone.testnet", 0, 70_000);
        let _ = a.settle_round(1);
        let s = a.get_spoils(1, "bat.testnet".into()).expect("spoils opened for the winner");
        assert_eq!((s.total_score.0, s.tokens.0), (2 * NEAR1, 0));
        cb(Some("\"1000\""), 80_000);
        let _ = a.bb_balance("bat.testnet".into(), U128(2 * NEAR1), U128(10_000), Some(1));
        a
    }

    #[test]
    fn a_won_war_burns_half_and_reserves_half_for_the_fighters() {
        let a = won_with_spoils();
        assert_eq!(a.get_spoils(1, "bat.testnet".into()).unwrap().tokens.0, 500);
        assert_eq!(crate::spoils::reserved("bat.testnet"), 500);
        assert_eq!(a.spoils_claimable(1, "bat.testnet".into(), acct("w3.testnet")).0, 250, "half of the spoils: same score as w4");
        assert_eq!(a.spoils_claimable(1, "bat.testnet".into(), acct("w1.testnet")).0, 0, "the losing side gets nothing");
        assert_eq!(a.spoils_claimable(1, "bat.testnet".into(), acct("nobody.testnet")).0, 0);
    }

    #[test]
    fn reserved_spoils_are_never_burned_by_a_later_buyback() {
        let mut a = won_with_spoils();
        // The arena holds 500 reserved + 300 newly bought: a treasury buyback may burn only the 300.
        cb(Some("\"800\""), 90_000);
        let _ = a.bb_balance("bat.testnet".into(), U128(NEAR1), U128(10_000), None);
        assert_eq!(crate::spoils::reserved("bat.testnet"), 500, "untouched");
        let last = a.feed_since(0, 500).into_iter().filter(|f| f.kind == "spoils_reserved").count();
        assert_eq!(last, 1, "reserved once, at the war's buy");
    }

    #[test]
    fn a_fighter_claims_once_and_a_failed_transfer_gives_the_share_back() {
        let mut a = won_with_spoils();
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("w3.testnet")).current_account_id(acct("arena.testnet")).attached_deposit(NearToken::from_yoctonear(1)).block_timestamp(100_000 * 1_000_000).build());
        let _ = a.claim_spoils(1, "bat.testnet".into());
        assert_eq!(crate::spoils::reserved("bat.testnet"), 250);
        assert_eq!(a.spoils_claimable(1, "bat.testnet".into(), acct("w3.testnet")).0, 0, "booked");
        // the transfer failed (not registered on the coin): everything is restored
        cb(None, 100_100);
        assert!(!a.on_spoils_sent(1, "bat.testnet".into(), acct("w3.testnet"), U128(250)));
        assert_eq!((crate::spoils::reserved("bat.testnet"), a.spoils_claimable(1, "bat.testnet".into(), acct("w3.testnet")).0), (500, 250));
        // claim again, this time it lands; a third claim has nothing left
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("w3.testnet")).current_account_id(acct("arena.testnet")).attached_deposit(NearToken::from_yoctonear(1)).block_timestamp(100_200 * 1_000_000).build());
        let _ = a.claim_spoils(1, "bat.testnet".into());
        cb(Some(""), 100_300);
        assert!(a.on_spoils_sent(1, "bat.testnet".into(), acct("w3.testnet"), U128(250)));
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("w3.testnet")).current_account_id(acct("arena.testnet")).attached_deposit(NearToken::from_yoctonear(1)).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = a.claim_spoils(1, "bat.testnet".into()); })).is_err());
    }

    #[test]
    fn unclaimed_spoils_burn_only_after_seven_days() {
        let mut a = won_with_spoils();
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("anyone.testnet")).current_account_id(acct("arena.testnet")).block_timestamp(100_000 * 1_000_000).build());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { let _ = a.burn_unclaimed_spoils(1, "bat.testnet".into()); })).is_err(), "window open");
        let settled = a.get_spoils(1, "bat.testnet".into()).unwrap().settled_ms;
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("anyone.testnet")).current_account_id(acct("arena.testnet")).block_timestamp((settled + SPOILS_CLAIM_MS) * 1_000_000).prepaid_gas(Gas::from_tgas(300)).build());
        let _ = a.burn_unclaimed_spoils(1, "bat.testnet".into());
        let s = a.get_spoils(1, "bat.testnet".into()).unwrap();
        assert!(s.closed);
        assert_eq!(crate::spoils::reserved("bat.testnet"), 0, "released into the burn");
        assert_eq!(a.spoils_claimable(1, "bat.testnet".into(), acct("w4.testnet")).0, 0, "closed: nothing left to claim");
    }

    #[test]
    fn raid_points_earn_no_share_of_the_spoils() {
        let mut a = two_coins(10 * NEAR1, 4 * NEAR1);
        treasury_of(&mut a, "pepe", NEAR1);
        at("anyone.testnet", NEAR1 / 10, 1_000);
        a.start_round();
        raid_ctx("starter.testnet", RAID_START_DEPOSIT, 1_000, 1_000);
        a.start_raid("bat.testnet".into());
        for i in 0..3 {
            assert!(claim(&mut a, &format!("r{i}.testnet"), "go #nme_bat_raid_1", 1_100, "1"));
        }
        let stake = a.get_round(1).unwrap().pairs[0].stake.0;
        score_both(&mut a, &[("w1.testnet", NEAR1 / 2), ("w2.testnet", NEAR1 / 2)], &[("w3.testnet", NEAR1 / 2), ("w4.testnet", NEAR1 / 2)]);
        at("anyone.testnet", 0, 70_000);
        let _ = a.settle_round(1);
        let s = a.get_spoils(1, "bat.testnet".into()).expect("bat won thanks to the raid");
        assert_eq!(s.total_score.0, NEAR1, "only the two buying wallets count, not the {} raid points", stake * 600 / 10_000);
    }
}
