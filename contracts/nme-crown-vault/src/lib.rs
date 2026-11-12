//! nme-crown-vault (`crown.<ticker>.<root>`, global code by hash, no access keys).
//! Holds the 5% dev allocation (vested) and the Crown's share of pool fees; obeys ONLY the Crown collection
//! (`authority`). Every method that moves value debits first and restores in the callback if the transfer fails.
//! Interface frozen at M1 so M4 never needs a republish (docs/06 M4).
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};
use nme_common::events::{self, name};
use nme_common::ext::ext_arena;
use nme_common::math::mul_div;

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
/// NEP-145 minimum on the coin: attach at least this to register a receiver that is not yet registered.
const NEP145_MIN: NearToken = NearToken::from_yoctonear(1_250_000_000_000_000_000_000);

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct Vest {
    pub total: U128,
    pub start_ms: u64,
    pub cliff_ms: u64,
    pub duration_ms: u64,
}

#[near(serializers = [json])]
pub struct VaultView {
    pub authority: AccountId,
    pub coin: AccountId,
    pub arena: AccountId,
    pub ticker: String,
    pub vest: Vest,
    /// Vested so far (frozen at the forfeit-time value once forfeited).
    pub vested_now: U128,
    pub released: U128,
    /// `vested_now - released`: what `release_vested` would send right now.
    pub claimable_now: U128,
    pub fees_near: U128,
    pub forfeited: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    authority: AccountId,
    coin: AccountId,
    arena: AccountId,
    ticker: String,
    vest: Vest,
    released: u128,
    fees_near: u128,
    forfeited: bool,
    /// Vesting stops at this amount once forfeited.
    vest_cap: Option<u128>,
}

#[near]
impl Contract {
    /// Only the coin that owns this vault can create it.
    #[init]
    #[allow(clippy::too_many_arguments)]
    pub fn new(authority: AccountId, coin: AccountId, arena: AccountId, ticker: String, total: U128, start_ms: u64, cliff_ms: u64, duration_ms: u64) -> Self {
        require!(env::predecessor_account_id() == coin, "only the coin can create its vault");
        require!(duration_ms > 0 && duration_ms >= cliff_ms, "bad vesting schedule");
        Self {
            authority,
            coin,
            arena,
            ticker,
            vest: Vest { total, start_ms, cliff_ms, duration_ms },
            released: 0,
            fees_near: 0,
            forfeited: false,
            vest_cap: None,
        }
    }

    /// Arena only: the Crown's share of claimed pool fees (docs/11 §3).
    #[payable]
    pub fn deposit_fees(&mut self) {
        require!(env::predecessor_account_id() == self.arena, "only the arena can deposit fees");
        self.fees_near += env::attached_deposit().as_yoctonear();
    }

    // ---------------------------------------------------------------- Crown collection only

    /// Send everything vested and not yet released to `to`. Attach `NEP145_MIN` (0.00125 NEAR) only if `to` is not
    /// yet registered on the coin; it is forwarded as the registration deposit (a registered receiver's deposit
    /// comes back to this vault, so attach 0 then).
    #[payable]
    pub fn release_vested(&mut self, to: AccountId) -> Promise {
        self.only_authority();
        let amount = self.claimable();
        require!(amount > 0, "nothing vested to release");
        let attached = env::attached_deposit();
        self.released += amount; // debit first; restored in the callback if the transfer fails
        let mut p = Promise::new(self.coin.clone());
        if attached >= NEP145_MIN {
            p = p.function_call("storage_deposit", format!(r#"{{"account_id":"{to}","registration_only":true}}"#).into_bytes(), attached, Gas::from_tgas(10));
        }
        p.function_call("ft_transfer", format!(r#"{{"receiver_id":"{to}","amount":"{amount}"}}"#).into_bytes(), ONE_YOCTO, Gas::from_tgas(15))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).with_unused_gas_weight(1).on_released(to, U128(amount), U128(attached.as_yoctonear())))
    }

    #[private]
    pub fn on_released(&mut self, to: AccountId, amount: U128, attached: U128) -> U128 {
        if env::promise_result_checked(0, 64).is_err() {
            self.released -= amount.0;
            if attached.0 > 0 && self.free_near() >= attached.0 {
                Promise::new(to.clone()).transfer(NearToken::from_yoctonear(attached.0)).detach();
            }
            events::emit_raw("vault_release_failed", &format!(r#"{{"ticker":"{}","kind":"vested","to":"{to}","amount":"{}"}}"#, self.ticker, amount.0));
            return U128(0);
        }
        events::emit_raw(name::VAULT_RELEASE, &format!(r#"{{"ticker":"{}","kind":"vested","to":"{to}","amount":"{}"}}"#, self.ticker, amount.0));
        amount
    }

    /// Send the accrued Crown fees to `to` — or, once forfeited, to the coin treasury in the arena (`to` is ignored).
    pub fn release_fees(&mut self, to: AccountId) -> Promise {
        self.only_authority();
        let amount = self.fees_near;
        require!(amount > 0, "no fees accrued");
        self.fees_near = 0; // debit first
        let me = env::current_account_id();
        let cb = Self::ext(me).with_static_gas(Gas::from_tgas(10)).with_unused_gas_weight(1);
        if self.forfeited {
            ext_arena::ext(self.arena.clone())
                .with_attached_deposit(NearToken::from_yoctonear(amount))
                .with_static_gas(Gas::from_tgas(15))
                .with_unused_gas_weight(0)
                .deposit_forfeited_fees(self.ticker.clone())
                .then(cb.on_fees_released(self.arena.clone(), U128(amount)))
        } else {
            Promise::new(to.clone()).transfer(NearToken::from_yoctonear(amount)).then(cb.on_fees_released(to, U128(amount)))
        }
    }

    #[private]
    pub fn on_fees_released(&mut self, to: AccountId, amount: U128) -> U128 {
        if env::promise_result_checked(0, 64).is_err() {
            self.fees_near += amount.0; // the NEAR came back with the failed call
            events::emit_raw("vault_release_failed", &format!(r#"{{"ticker":"{}","kind":"fees","to":"{to}","amount":"{}"}}"#, self.ticker, amount.0));
            return U128(0);
        }
        events::emit_raw(name::VAULT_RELEASE, &format!(r#"{{"ticker":"{}","kind":"fees","to":"{to}","amount":"{}"}}"#, self.ticker, amount.0));
        amount
    }

    /// Burn everything that has not vested yet and stop vesting. Already-vested tokens stay claimable.
    pub fn forfeit(&mut self) -> PromiseOrValue<U128> {
        self.only_authority();
        require!(!self.forfeited, "already forfeited");
        let vested = self.vested_now();
        let to_burn = self.vest.total.0 - vested.min(self.vest.total.0);
        self.forfeited = true;
        self.vest_cap = Some(vested);
        if to_burn == 0 {
            events::emit_raw(name::VAULT_FORFEIT, &format!(r#"{{"ticker":"{}","burned":"0"}}"#, self.ticker));
            return PromiseOrValue::Value(U128(0));
        }
        PromiseOrValue::Promise(
            Promise::new(self.coin.clone())
                .function_call("burn", format!(r#"{{"amount":"{to_burn}"}}"#).into_bytes(), ONE_YOCTO, Gas::from_tgas(15))
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).with_unused_gas_weight(1).on_forfeited(U128(to_burn))),
        )
    }

    #[private]
    pub fn on_forfeited(&mut self, burned: U128) -> U128 {
        if env::promise_result_checked(0, 64).is_err() {
            self.forfeited = false; // retry later
            self.vest_cap = None;
            events::emit_raw("vault_release_failed", &format!(r#"{{"ticker":"{}","kind":"forfeit","amount":"{}"}}"#, self.ticker, burned.0));
            return U128(0);
        }
        events::emit_raw(name::VAULT_FORFEIT, &format!(r#"{{"ticker":"{}","burned":"{}"}}"#, self.ticker, burned.0));
        burned
    }

    // ---------------------------------------------------------------- views

    pub fn get_vault(&self) -> VaultView {
        VaultView {
            authority: self.authority.clone(),
            coin: self.coin.clone(),
            arena: self.arena.clone(),
            ticker: self.ticker.clone(),
            vest: self.vest.clone(),
            vested_now: U128(self.vested_now()),
            released: U128(self.released),
            claimable_now: U128(self.claimable()),
            fees_near: U128(self.fees_near),
            forfeited: self.forfeited,
        }
    }
}

impl Contract {
    fn only_authority(&self) {
        require!(env::predecessor_account_id() == self.authority, "only the Crown collection can command the vault");
    }

    /// Linear vesting from `start_ms`, nothing before the cliff, capped at `total` (and at the forfeit-time value).
    fn vested_at(&self, now_ms: u64) -> u128 {
        let v = &self.vest;
        if now_ms < v.start_ms.saturating_add(v.cliff_ms) {
            return 0;
        }
        let elapsed = (now_ms - v.start_ms).min(v.duration_ms) as u128;
        mul_div(v.total.0, elapsed, v.duration_ms as u128).unwrap_or(v.total.0)
    }

    fn vested_now(&self) -> u128 {
        let v = self.vested_at(env::block_timestamp_ms());
        match self.vest_cap {
            Some(cap) => v.min(cap),
            None => v,
        }
    }

    fn claimable(&self) -> u128 {
        self.vested_now().saturating_sub(self.released)
    }

    /// NEAR in the vault that is neither storage nor accrued Crown fees.
    fn free_near(&self) -> u128 {
        let storage = env::storage_byte_cost().saturating_mul(env::storage_usage() as u128);
        env::account_balance().saturating_sub(storage).as_yoctonear().saturating_sub(self.fees_near)
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

    fn ctx(who: &str, now_ms: u64, deposit: u128) -> near_sdk::VMContext {
        VMContextBuilder::new()
            .predecessor_account_id(acct(who))
            .block_timestamp(now_ms * 1_000_000)
            .attached_deposit(NearToken::from_yoctonear(deposit))
            .account_balance(NearToken::from_near(1))
            .build()
    }

    fn vault(total: u128) -> Contract {
        testing_env!(ctx("pepe.nme.testnet", 0, 0));
        Contract::new(acct("crown.nme.testnet"), acct("pepe.nme.testnet"), acct("arena.nme.testnet"), "pepe".into(), U128(total), 1_000, 100, 1_000)
    }

    /// Run the next call as a callback whose single promise result is a failure.
    fn as_failed_callback(now_ms: u64) {
        testing_env!(
            ctx("vault.testnet", now_ms, 0),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![PromiseResult::Failed]
        );
    }

    #[test]
    fn vesting_curve() {
        let v = vault(1_000_000);
        assert_eq!(v.vested_at(1_099), 0); // before cliff
        assert_eq!(v.vested_at(1_100), 100_000); // at cliff: linear since start
        assert_eq!(v.vested_at(1_500), 500_000);
        assert_eq!(v.vested_at(2_000), 1_000_000);
        assert_eq!(v.vested_at(99_999), 1_000_000); // capped
    }

    #[test]
    #[should_panic(expected = "only the coin can create its vault")]
    fn only_coin_inits() {
        testing_env!(ctx("evil.testnet", 0, 0));
        Contract::new(acct("c.testnet"), acct("pepe.nme.testnet"), acct("a.testnet"), "pepe".into(), U128(1), 0, 0, 10);
    }

    #[test]
    fn only_arena_deposits_fees() {
        let mut v = vault(1_000);
        testing_env!(ctx("arena.nme.testnet", 0, 7));
        v.deposit_fees();
        v.deposit_fees();
        assert_eq!(v.get_vault().fees_near.0, 14);
    }

    #[test]
    #[should_panic(expected = "only the arena can deposit fees")]
    fn others_cannot_deposit_fees() {
        let mut v = vault(1_000);
        testing_env!(ctx("evil.testnet", 0, 1));
        v.deposit_fees();
    }

    #[test]
    #[should_panic(expected = "only the Crown collection can command the vault")]
    fn release_vested_is_authority_only() {
        let mut v = vault(1_000);
        testing_env!(ctx("evil.testnet", 2_000, 0));
        let _ = v.release_vested(acct("evil.testnet"));
    }

    #[test]
    #[should_panic(expected = "only the Crown collection can command the vault")]
    fn forfeit_is_authority_only() {
        let mut v = vault(1_000);
        testing_env!(ctx("evil.testnet", 2_000, 0));
        let _ = v.forfeit();
    }

    #[test]
    #[should_panic(expected = "nothing vested to release")]
    fn nothing_before_the_cliff() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_050, 0));
        let _ = v.release_vested(acct("alice.testnet"));
    }

    #[test]
    fn release_debits_first_and_cannot_double_pay() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_500, 0)); // 50% vested
        let _ = v.release_vested(acct("alice.testnet"));
        assert_eq!(v.released, 500_000);
        assert_eq!(v.get_vault().claimable_now.0, 0, "the same vested amount cannot be released twice");
    }

    #[test]
    #[should_panic(expected = "nothing vested to release")]
    fn second_release_in_the_same_block_pays_nothing() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_500, 0));
        let _ = v.release_vested(acct("alice.testnet"));
        let _ = v.release_vested(acct("alice.testnet"));
    }

    #[test]
    fn failed_transfer_restores_the_debit() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_500, 0));
        let _ = v.release_vested(acct("alice.testnet"));
        as_failed_callback(1_500);
        assert_eq!(v.on_released(acct("alice.testnet"), U128(500_000), U128(0)).0, 0);
        assert_eq!(v.released, 0, "restored: the owner can claim again");
    }

    #[test]
    fn release_fees_debits_first_and_restores_on_failure() {
        let mut v = vault(1_000);
        testing_env!(ctx("arena.nme.testnet", 0, 900));
        v.deposit_fees();
        testing_env!(ctx("crown.nme.testnet", 0, 0));
        let _ = v.release_fees(acct("alice.testnet"));
        assert_eq!(v.fees_near, 0);
        as_failed_callback(0);
        v.on_fees_released(acct("alice.testnet"), U128(900));
        assert_eq!(v.fees_near, 900);
    }

    #[test]
    #[should_panic(expected = "no fees accrued")]
    fn no_fees_no_release() {
        let mut v = vault(1_000);
        testing_env!(ctx("crown.nme.testnet", 0, 0));
        let _ = v.release_fees(acct("alice.testnet"));
    }

    #[test]
    fn forfeit_burns_only_the_unvested_part_and_freezes_vesting() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_400, 0)); // 40% vested
        let _ = v.forfeit();
        assert!(v.forfeited);
        assert_eq!(v.vest_cap, Some(400_000));
        // later in time nothing more vests; the already-vested 40% stays claimable
        testing_env!(ctx("crown.nme.testnet", 9_000, 0));
        assert_eq!(v.vested_now(), 400_000);
        assert_eq!(v.claimable(), 400_000);
        let _ = v.release_vested(acct("alice.testnet"));
        assert_eq!(v.released, 400_000);
    }

    #[test]
    #[should_panic(expected = "already forfeited")]
    fn cannot_forfeit_twice() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_400, 0));
        let _ = v.forfeit();
        let _ = v.forfeit();
    }

    #[test]
    fn failed_burn_allows_a_retry() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 1_400, 0));
        let _ = v.forfeit();
        as_failed_callback(1_400);
        assert_eq!(v.on_forfeited(U128(600_000)).0, 0);
        assert!(!v.forfeited && v.vest_cap.is_none());
    }

    #[test]
    fn forfeit_after_full_vesting_burns_nothing() {
        let mut v = vault(1_000_000);
        testing_env!(ctx("crown.nme.testnet", 5_000, 0)); // fully vested
        match v.forfeit() {
            PromiseOrValue::Value(b) => assert_eq!(b.0, 0),
            _ => panic!("nothing to burn must not schedule a call"),
        }
        assert!(v.forfeited);
    }
}
