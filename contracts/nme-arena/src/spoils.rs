//! Spoils of war (owner decision 2026-11-11, changes I3): when a war has a winner, the loser's stake buys the winner's
//! coin as before, but only HALF of the bought tokens are burned. The other half is reserved for the winning side's
//! fighters and each claims a share in proportion to the war score they earned (the same capped, sell-adjusted score the
//! war used). Raid points are not a wallet and earn no share. Tokens not claimed within 7 days can be burned by anyone.
//!
//! NEAR accounts claim directly. Phantom / MetaMask fighters (scored under a derived key, `wallet_key`) cannot claim yet:
//! the gateway credits tokens by measuring balance changes, so pushing spoils into it could double-count a buy in flight.
//! Their share stays reserved and is burned after the 7 days.
//!
//! The reserved tokens sit in the arena's own balance; `reserved` keeps every later buy-and-burn of that coin from
//! touching them. State lives under its own prefixes (`Sr`, `Sk`, `St`), outside the main struct: no migration.
use crate::*;
use near_sdk::assert_one_yocto;
use near_sdk::store::LookupMap as Map;

pub const SPOILS_BPS: u16 = 5_000;
pub const SPOILS_CLAIM_MS: u64 = 7 * 86_400_000;

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Spoils {
    pub round_id: u64,
    pub coin: String,
    /// The winning coin's token account (what is paid out).
    pub token: AccountId,
    /// Tokens reserved for the fighters (0 until the buy lands).
    pub tokens: U128,
    pub claimed: U128,
    /// Sum of the winning side's wallet scores (raid points excluded) and the per-wallet cap the war used.
    pub total_score: U128,
    pub wallet_cap: U128,
    pub settled_ms: u64,
    /// The unclaimed rest was burned after the claim window.
    pub closed: bool,
}

fn rounds_map() -> Map<(u64, String), Spoils> {
    Map::new(b"Sr".to_vec())
}
fn claims_map() -> Map<(u64, String, AccountId), U128> {
    Map::new(b"Sk".to_vec())
}
fn reserved_map() -> Map<String, u128> {
    Map::new(b"St".to_vec())
}

/// Tokens of `coin` the arena holds for fighters: never burnable.
pub(crate) fn reserved(coin: &str) -> u128 {
    reserved_map().get(coin).copied().unwrap_or(0)
}
fn add_reserved(coin: &str, delta: i128) {
    let mut m = reserved_map();
    let v = m.get(coin).copied().unwrap_or(0);
    let n = if delta >= 0 { v.saturating_add(delta as u128) } else { v.saturating_sub(delta.unsigned_abs()) };
    m.insert(coin.to_string(), n);
    m.flush();
}

/// Opens the spoils of a won pair (before the buy). Returns false when nobody can claim (only raid points scored).
pub(crate) fn open(round_id: u64, coin: &str, token: AccountId, total_score: u128, wallet_cap: u128) -> bool {
    if total_score == 0 {
        return false;
    }
    let mut m = rounds_map();
    m.insert(
        (round_id, coin.to_string()),
        Spoils { round_id, coin: coin.to_string(), token, tokens: U128(0), claimed: U128(0), total_score: U128(total_score), wallet_cap: U128(wallet_cap), settled_ms: env::block_timestamp_ms(), closed: false },
    );
    m.flush();
    true
}

/// The buy landed with `bought` burnable tokens: reserves the fighters' half. Returns how many to keep (not burn).
pub(crate) fn fill(round_id: u64, coin: &str, bought: u128) -> u128 {
    let mut m = rounds_map();
    let Some(mut s) = m.get(&(round_id, coin.to_string())).cloned() else { return 0 };
    if s.tokens.0 > 0 {
        return 0; // filled once
    }
    let keep = bps_of(bought, SPOILS_BPS);
    s.tokens = U128(keep);
    m.insert((round_id, coin.to_string()), s);
    m.flush();
    add_reserved(coin, keep as i128);
    keep
}

#[near]
impl Contract {
    pub fn get_spoils(&self, round_id: u64, coin: String) -> Option<Spoils> {
        rounds_map().get(&(round_id, coin)).cloned()
    }

    /// What `account` can still claim from round `round_id` (0 when nothing or already claimed).
    pub fn spoils_claimable(&self, round_id: u64, coin: String, account: AccountId) -> U128 {
        U128(self.spoils_share(round_id, &coin, &account).map(|(_, a)| a).unwrap_or(0))
    }

    /// The caller's share of a won war's spoils, sent as the winner's tokens (the caller must be registered on the coin).
    #[payable]
    pub fn claim_spoils(&mut self, round_id: u64, coin: String) -> Promise {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let (s, amount) = self.spoils_share(round_id, &coin, &who).unwrap_or_else(|| env::panic_str("nothing to claim"));
        require!(amount > 0, "nothing to claim");
        // Booked first; `on_spoils_sent` undoes it if the transfer fails.
        let mut c = claims_map();
        c.insert((round_id, coin.clone(), who.clone()), U128(amount));
        c.flush();
        let mut m = rounds_map();
        let mut s2 = s.clone();
        s2.claimed = U128(s2.claimed.0 + amount);
        m.insert((round_id, coin.clone()), s2);
        m.flush();
        add_reserved(&coin, -(amount as i128));
        Promise::new(s.token)
            .function_call("ft_transfer", json!({ "receiver_id": who, "amount": U128(amount), "memo": format!("nme spoils round {round_id}") }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(15))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(15)).on_spoils_sent(round_id, coin, who, U128(amount)))
    }

    #[private]
    pub fn on_spoils_sent(&mut self, round_id: u64, coin: String, account: AccountId, amount: U128) -> bool {
        if env::promise_result_checked(0, 64).is_ok() {
            self.push_feed("spoils_claimed", &coin, account.as_str(), amount.0, &format!("round={round_id}"));
            events::emit("spoils_claimed", json!({ "round": round_id, "coin": coin, "account": account, "tokens": amount }));
            return true;
        }
        let mut c = claims_map();
        c.remove(&(round_id, coin.clone(), account.clone()));
        c.flush();
        let mut m = rounds_map();
        if let Some(mut s) = m.get(&(round_id, coin.clone())).cloned() {
            s.claimed = U128(s.claimed.0.saturating_sub(amount.0));
            m.insert((round_id, coin.clone()), s);
            m.flush();
        }
        add_reserved(&coin, amount.0 as i128);
        events::emit("spoils_claim_failed", json!({ "round": round_id, "coin": coin, "account": account, "reason": "transfer failed (register on the coin first)" }));
        false
    }

    /// After the claim window, burns what nobody claimed. Anyone may call it.
    pub fn burn_unclaimed_spoils(&mut self, round_id: u64, coin: String) -> Promise {
        let mut m = rounds_map();
        let mut s = m.get(&(round_id, coin.clone())).cloned().unwrap_or_else(|| env::panic_str("no spoils"));
        require!(!s.closed, "already closed");
        require!(env::block_timestamp_ms() >= s.settled_ms + SPOILS_CLAIM_MS, "the claim window is still open");
        let rest = s.tokens.0.saturating_sub(s.claimed.0);
        s.closed = true;
        m.insert((round_id, coin.clone()), s.clone());
        m.flush();
        add_reserved(&coin, -(rest as i128));
        require!(rest > 0, "nothing left to burn");
        // Released from the reserve, the tokens are burnable: `burn` (or the graveyard) through the usual path.
        Promise::new(s.token)
            .function_call("burn", json!({ "amount": U128(rest) }).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(15))
            .then(Self::ext(env::current_account_id()).with_static_gas(CB).bb_burned(coin, U128(rest), U128(0)))
    }

    fn spoils_share(&self, round_id: u64, coin: &str, who: &AccountId) -> Option<(Spoils, u128)> {
        let s = rounds_map().get(&(round_id, coin.to_string())).cloned()?;
        if s.closed || s.tokens.0 == 0 || claims_map().get(&(round_id, coin.to_string(), who.clone())).is_some() {
            return None;
        }
        let flow = self.wallet_flow.get(&(round_id, coin.to_string(), who.clone())).cloned()?;
        let score = Self::wallet_score(&flow, s.wallet_cap.0);
        let amount = mul_div(s.tokens.0, score, s.total_score.0).unwrap_or(0).min(s.tokens.0.saturating_sub(s.claimed.0));
        Some((s, amount))
    }
}
