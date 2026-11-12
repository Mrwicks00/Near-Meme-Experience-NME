//! Cross-contract interfaces (verified call shapes: docs/spikes/S3.md for Ref/wNEAR, S2 for global code).
use near_sdk::ext_contract;
use near_sdk::json_types::{Base58CryptoHash, U128};
use near_sdk::AccountId;

#[ext_contract(ext_arena)]
pub trait ExtArena {
    fn register_coin(&mut self, ticker: String, account: AccountId, crown_vault: AccountId, colosseum_opt_in: bool, first_claim_due_ms: u64);
    fn record_launch(&mut self, ticker: String, pool_id: String, lpt_id: String);
    fn deposit_pool_fees(&mut self, ticker: String);
    /// Vault only: the Crown's accrued fees after a forfeit go to the coin treasury.
    fn deposit_forfeited_fees(&mut self, ticker: String);
}

#[ext_contract(ext_crown)]
pub trait ExtCrown {
    fn nft_mint(&mut self, token_id: String, owner: AccountId, coin: AccountId, vault: AccountId, inactivity_ms: u64);
}

#[ext_contract(ext_vault)]
pub trait ExtVault {
    fn deposit_fees(&mut self);
    #[allow(clippy::too_many_arguments)]
    fn new(authority: AccountId, coin: AccountId, arena: AccountId, ticker: String, total: U128, start_ms: u64, cliff_ms: u64, duration_ms: u64);
}

#[ext_contract(ext_wrap)]
pub trait ExtWrap {
    fn storage_deposit(&mut self, account_id: Option<AccountId>, registration_only: Option<bool>);
    fn near_deposit(&mut self);
    fn ft_transfer_call(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>, msg: String) -> U128;
}

/// Used for the coin's self-call `ft_transfer_call` into Ref (NEP-141 resolve/refund logic for free).
#[ext_contract(ext_ft)]
pub trait ExtFt {
    fn ft_transfer_call(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>, msg: String) -> U128;
}

pub type CodeHash = Base58CryptoHash;
