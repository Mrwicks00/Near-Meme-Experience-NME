//! Raids on NEAR Social (docs/13-RAIDS.md). A coin's raid asks holders to post on NEAR Social (SocialDB) with a hashtag.
//! A holder claims; the arena reads the holder's latest post straight from the SocialDB contract (on-chain data, so no
//! oracle and no TEE), checks the tag and that the post was written inside the raid window, and checks the holder owns
//! the coin. Each verified post (a) adds war points to the coin when it is fighting in the Colosseum and (b) adds to
//! the raid's burn: when the raid ends, a `RaidSettle` task buys and burns the coin from its own treasury (I3).
//!
//! Every parameter is a constant (I2). Starting a raid is permissionless; nobody can stop one. The state lives under
//! its own one-byte storage prefixes (0x00-0x03, from `RaidKey`; the main struct uses ASCII letters), outside the main
//! struct, so adding raids changed no existing layout and needs no migration.
use crate::*;
use near_sdk::store::LookupMap as Map;
use near_sdk::{borsh, BorshStorageKey, Promise};

/// About 30-50 minutes of blocks on testnet: the window in which a post counts.
pub const RAID_BLOCKS: u64 = 3_000;
/// Claims are still accepted this long after the window (the post itself must be inside the window).
pub const RAID_GRACE_BLOCKS: u64 = 600;
/// One raid per coin at a time, and this long between raid starts.
pub const RAID_COOLDOWN_MS: u64 = 60 * 60 * 1000;
pub const RAID_MAX_CLAIMS: u32 = 25;
/// Each verified post burns 0.1% of the coin's treasury, at most 1% per raid.
pub const RAID_BURN_BPS_PER_CLAIM: u32 = 10;
pub const RAID_BURN_BPS_MAX: u32 = 100;
/// While the coin is in a war: each verified post adds 2% of the round's stake to the coin's score, at most 20% per raid.
pub const RAID_POINTS_BPS: u16 = 200;
pub const RAID_POINTS_CAP_BPS: u16 = 2_000;
/// Starting pays the raid record's storage; claiming pays the claim record's storage. Both go to the protocol bucket.
pub const RAID_START_DEPOSIT: u128 = 20_000_000_000_000_000_000_000; // 0.02 NEAR
pub const RAID_CLAIM_DEPOSIT: u128 = 2_000_000_000_000_000_000_000; // 0.002 NEAR
const RAID_MAX_POST_BYTES: u32 = 16_384;

#[derive(BorshStorageKey)]
#[near]
enum RaidKey {
    Raids,
    Claims,
    Latest,
    Counter,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub enum RaidState {
    Open,
    Settled,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Raid {
    pub id: u64,
    pub ticker: String,
    /// The hashtag to post, without `#`.
    pub tag: String,
    pub starter: AccountId,
    pub start_block: u64,
    pub end_block: u64,
    pub start_ms: u64,
    pub claims: u32,
    /// War points this raid added, and the round they went to (the cap is per raid).
    pub points: U128,
    pub points_round: u64,
    pub state: RaidState,
    /// NEAR of the treasury bought and burned at settlement.
    pub burned_near: U128,
}

/// `nme_<ticker letters/digits>_raid_<id>`: a valid NEAR Social hashtag (letters, digits, `_`).
pub fn raid_tag(ticker: &str, id: u64) -> String {
    let t: String = ticker.split('.').next().unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    format!("nme_{}_raid_{id}", if t.is_empty() { "coin" } else { &t })
}

/// The post text carries `#<tag>` as a whole word (so `#nme_pepe_raid_1` does not match `#nme_pepe_raid_12`).
pub fn post_has_tag(text: &str, tag: &str) -> bool {
    let needle = format!("#{}", tag.to_ascii_lowercase());
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle) {
        let end = from + i + needle.len();
        let next = lower[end..].chars().next();
        if !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == '_') {
            return true;
        }
        from = end;
    }
    false
}

/// Reads `<account>/post/main` from a SocialDB `get` answer made with `with_block_height`: (post text, block written).
pub fn parse_social_post(raw: &[u8], account: &str) -> Option<(String, u64)> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let main = v.get(account)?.get("post")?.get("main")?;
    let block = main.get(":block")?.as_u64()?;
    let body = main.get("")?.as_str()?;
    let post: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = post.get("text")?.as_str()?.to_string();
    Some((text, block))
}

pub fn social_contract() -> AccountId {
    let me = env::current_account_id();
    (if me.as_str().ends_with(".testnet") { "v1.social08.testnet" } else { "social.near" }).parse().unwrap()
}

fn raids() -> Map<u64, Raid> {
    Map::new(RaidKey::Raids)
}
fn claims() -> Map<(u64, AccountId), bool> {
    Map::new(RaidKey::Claims)
}
fn latest() -> Map<String, u64> {
    Map::new(RaidKey::Latest)
}
fn next_id() -> u64 {
    let k = borsh::to_vec(&RaidKey::Counter).unwrap();
    env::storage_read(&k).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))).unwrap_or(0) + 1
}
fn set_last_id(id: u64) {
    let k = borsh::to_vec(&RaidKey::Counter).unwrap();
    env::storage_write(&k, &id.to_le_bytes());
}

#[near]
impl Contract {
    /// Starts a raid for a launched coin. Anyone may, once per cooldown per coin; attach 0.02 NEAR (storage).
    #[payable]
    pub fn start_raid(&mut self, ticker: String) -> Raid {
        require!(env::attached_deposit().as_yoctonear() >= RAID_START_DEPOSIT, "attach 0.02 NEAR to start a raid");
        let entry = self.coins.get(&ticker).cloned().unwrap_or_else(|| env::panic_str("unknown coin"));
        require!(entry.launched && entry.pool_id.is_some(), "the coin is not live yet");
        let now = env::block_timestamp_ms();
        let mut latest = latest();
        if let Some(prev) = latest.get(&ticker).and_then(|id| raids().get(id).cloned()) {
            require!(prev.state == RaidState::Settled || env::block_height() > prev.end_block + RAID_GRACE_BLOCKS, "this coin already has a raid running");
            require!(now >= prev.start_ms + RAID_COOLDOWN_MS, "this coin raided less than an hour ago");
        }
        let id = next_id();
        let start = env::block_height();
        let raid = Raid {
            id,
            tag: raid_tag(&ticker, id),
            ticker: ticker.clone(),
            starter: env::predecessor_account_id(),
            start_block: start,
            end_block: start + RAID_BLOCKS,
            start_ms: now,
            claims: 0,
            points: U128(0),
            points_round: 0,
            state: RaidState::Open,
            burned_near: U128(0),
        };
        let mut all = raids();
        all.insert(id, raid.clone());
        all.flush();
        latest.insert(ticker.clone(), id);
        latest.flush();
        set_last_id(id);
        self.protocol_near += env::attached_deposit().as_yoctonear();
        // Settle after the window and the grace (block time ~0.6-1 s; the task re-checks the block height).
        self.enqueue(now + (RAID_BLOCKS + RAID_GRACE_BLOCKS) * 600, Task::RaidSettle { raid_id: id });
        self.push_feed("raid_started", &ticker, raid.starter.as_str(), 0, &format!("raid={id} #{}", raid.tag));
        events::emit("raid_started", json!({ "raid_id": id, "ticker": ticker, "tag": raid.tag, "starter": raid.starter, "start_block": start, "end_block": raid.end_block }));
        raid
    }

    /// Claims one raid post for the caller (NEAR accounts only: the post must be theirs on NEAR Social). Attach 0.002 NEAR.
    #[payable]
    pub fn claim_raid(&mut self, raid_id: u64) -> Promise {
        require!(env::attached_deposit().as_yoctonear() >= RAID_CLAIM_DEPOSIT, "attach 0.002 NEAR to claim");
        let raid = raids().get(&raid_id).cloned().unwrap_or_else(|| env::panic_str("unknown raid"));
        require!(raid.state == RaidState::Open && env::block_height() <= raid.end_block + RAID_GRACE_BLOCKS, "this raid is over");
        require!(raid.claims < RAID_MAX_CLAIMS, "this raid is full");
        let who = env::predecessor_account_id();
        let mut c = claims();
        require!(c.get(&(raid_id, who.clone())).is_none(), "you already claimed this raid");
        // Reserved now (blocks a second claim racing this one); released in the callback if the post does not count.
        c.insert((raid_id, who.clone()), false);
        c.flush();
        self.protocol_near += env::attached_deposit().as_yoctonear();
        let coin = self.coins.get(&raid.ticker).map(|e| e.account.clone()).unwrap_or_else(|| env::panic_str("unknown coin"));
        let key = format!("{who}/post/main");
        Promise::new(social_contract())
            .function_call("get", json!({ "keys": [key], "options": { "with_block_height": true } }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(20))
            .and(Promise::new(coin).function_call("ft_balance_of", json!({ "account_id": who }).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(5)))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(25)).on_raid_claim(raid_id, who))
    }

    /// Verifies the post and the holding; counts the claim or releases the reservation. Never panics (I7: state stays consistent).
    #[private]
    pub fn on_raid_claim(&mut self, raid_id: u64, account: AccountId) -> bool {
        let post = env::promise_result_checked(0, RAID_MAX_POST_BYTES as usize).ok().and_then(|b| parse_social_post(&b, account.as_str()));
        let holds = env::promise_result_checked(1, 128).ok().and_then(|b| serde_json::from_slice::<U128>(&b).ok()).map(|u| u.0 > 0).unwrap_or(false);
        let mut all = raids();
        let Some(mut raid) = all.get(&raid_id).cloned() else { return false };
        let mut c = claims();
        let why = match &post {
            None => Some("no NEAR Social post found for this account"),
            Some(_) if !holds => Some("the account holds none of the coin"),
            Some((text, _)) if !post_has_tag(text, &raid.tag) => Some("the latest post does not carry the raid hashtag"),
            Some((_, block)) if *block < raid.start_block || *block > raid.end_block => Some("the post was not written during the raid"),
            _ if raid.state != RaidState::Open || raid.claims >= RAID_MAX_CLAIMS => Some("the raid closed meanwhile"),
            _ => None,
        };
        if let Some(why) = why {
            c.remove(&(raid_id, account.clone()));
            c.flush();
            events::emit("raid_claim_rejected", json!({ "raid_id": raid_id, "account": account, "reason": why }));
            return false;
        }
        c.insert((raid_id, account.clone()), true);
        c.flush();
        raid.claims += 1;
        let points = self.raid_war_points(&mut raid);
        all.insert(raid_id, raid.clone());
        all.flush();
        self.push_feed("raid_claimed", &raid.ticker, account.as_str(), points, &format!("raid={raid_id} posts={}", raid.claims));
        events::emit("raid_claimed", json!({ "raid_id": raid_id, "ticker": raid.ticker, "account": account, "posts": raid.claims, "war_points": U128(points), "post_block": post.map(|p| p.1) }));
        true
    }

    /// Adds war points when the coin is fighting right now (capped per raid). Returns the points added.
    fn raid_war_points(&mut self, raid: &mut Raid) -> u128 {
        let Some((rid, idx)) = self.active.get(&raid.ticker).copied() else { return 0 };
        let Some(mut round) = self.rounds.get(&rid).cloned() else { return 0 };
        if env::block_timestamp_ms() >= round.end_ms {
            return 0;
        }
        let pair = &mut round.pairs[idx as usize];
        if raid.points_round != rid {
            raid.points_round = rid;
            raid.points = U128(0);
        }
        let cap = bps_of(pair.stake.0, RAID_POINTS_CAP_BPS);
        let add = bps_of(pair.stake.0, RAID_POINTS_BPS).min(cap.saturating_sub(raid.points.0));
        if add == 0 {
            return 0;
        }
        if pair.a == raid.ticker {
            pair.score_a.0 += add;
        } else {
            pair.score_b.0 += add;
        }
        raid.points.0 += add;
        self.rounds.insert(rid, round);
        add
    }

    /// War points raids added to `ticker` in round `round_id` (excluded from the fighters' spoils: points are not a wallet).
    pub(crate) fn raid_points_in(&self, ticker: &str, round_id: u64) -> u128 {
        latest().get(ticker).and_then(|id| raids().get(id).cloned()).filter(|r| r.points_round == round_id).map(|r| r.points.0).unwrap_or(0)
    }

    /// Run by `tick` after the window: burns `claims x 0.1%` (max 1%) of the coin's treasury and closes the raid.
    pub(crate) fn settle_raid(&mut self, raid_id: u64, now: u64) -> bool {
        let mut all = raids();
        let Some(mut raid) = all.get(&raid_id).cloned() else { return false };
        if raid.state == RaidState::Settled {
            return false;
        }
        if env::block_height() <= raid.end_block + RAID_GRACE_BLOCKS {
            self.enqueue(now + 60_000, Task::RaidSettle { raid_id });
            return false;
        }
        raid.state = RaidState::Settled;
        let bps = (raid.claims * RAID_BURN_BPS_PER_CLAIM).min(RAID_BURN_BPS_MAX);
        let entry = self.coins.get(&raid.ticker).cloned();
        let amount = entry.as_ref().map(|e| bps_of(e.treasury_near.0, bps as u16)).unwrap_or(0);
        let can_burn = amount > 0 && entry.as_ref().map(|e| e.launched && e.pool_id.is_some()).unwrap_or(false);
        if can_burn {
            raid.burned_near = U128(amount);
        }
        all.insert(raid_id, raid.clone());
        all.flush();
        self.push_feed("raid_settled", &raid.ticker, "", if can_burn { amount } else { 0 }, &format!("raid={raid_id} posts={}", raid.claims));
        events::emit("raid_settled", json!({ "raid_id": raid_id, "ticker": raid.ticker, "posts": raid.claims, "burn_near": raid.burned_near }));
        if can_burn {
            // The treasury leaves only into this buy-and-burn of the coin's own supply (I3).
            self.buy_and_burn(&raid.ticker, &raid.ticker, false, amount).detach();
        }
        true
    }

    pub fn get_raid(&self, raid_id: u64) -> Option<Raid> {
        raids().get(&raid_id).cloned()
    }

    /// The coin's most recent raid (open or settled), if any.
    pub fn latest_raid(&self, ticker: String) -> Option<Raid> {
        latest().get(&ticker).and_then(|id| raids().get(id).cloned())
    }

    /// `true` = counted, `false` = being checked right now, `null` = no claim.
    pub fn raid_claim_of(&self, raid_id: u64, account: AccountId) -> Option<bool> {
        claims().get(&(raid_id, account)).copied()
    }

    pub fn raid_rules(&self) -> serde_json::Value {
        json!({
            "blocks": RAID_BLOCKS, "grace_blocks": RAID_GRACE_BLOCKS, "cooldown_ms": RAID_COOLDOWN_MS, "max_claims": RAID_MAX_CLAIMS,
            "burn_bps_per_claim": RAID_BURN_BPS_PER_CLAIM, "burn_bps_max": RAID_BURN_BPS_MAX,
            "war_points_bps": RAID_POINTS_BPS, "war_points_cap_bps": RAID_POINTS_CAP_BPS,
            "start_deposit": U128(RAID_START_DEPOSIT), "claim_deposit": U128(RAID_CLAIM_DEPOSIT), "social": social_contract(),
        })
    }
}
