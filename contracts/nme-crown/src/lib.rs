//! nme-crown: the PumpBag collection (NEP-171), token_id = ticker. Only the factory can mint. The owner claims the
//! vested tokens and the accrued fees from the coin's vault, keeps it alive with a heartbeat, and a transfer is a
//! takeover (the new owner gets a fresh period). Anyone can forfeit a PumpBag whose owner went silent: the vault
//! burns what has not vested yet and routes the fees to the coin's treasury (docs/03).
use near_contract_standards::non_fungible_token::core::{NonFungibleTokenCore, NonFungibleTokenResolver};
use near_contract_standards::non_fungible_token::enumeration::NonFungibleTokenEnumeration;
use near_contract_standards::non_fungible_token::metadata::{NFTContractMetadata, NonFungibleTokenMetadataProvider, TokenMetadata, NFT_METADATA_SPEC};
use near_contract_standards::non_fungible_token::{NonFungibleToken, Token, TokenId};
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{assert_one_yocto, env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};
use nme_common::events::{self, name};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    tokens: NonFungibleToken,
    metadata: LazyOption<NFTContractMetadata>,
    factory: AccountId,
    arena: AccountId,
    /// Last heartbeat per token (ms). Tokens minted before M4 fall back to the mint time in their metadata.
    heartbeat: LookupMap<String, u64>,
    forfeited: LookupMap<String, bool>,
}

// The M1 layout (mint-only), read once by `migrate`.
mod v1 {
    use super::*;

    #[near(serializers = [borsh])]
    pub struct Contract {
        pub tokens: NonFungibleToken,
        pub metadata: LazyOption<NFTContractMetadata>,
        pub factory: AccountId,
        pub arena: AccountId,
    }
}

const VAULT_GAS: Gas = Gas::from_tgas(80);
const ARENA_NOTE_GAS: Gas = Gas::from_tgas(10);

/// What the PumpBag needs to know about a token, read from its minted metadata.
struct Seat {
    owner: AccountId,
    vault: AccountId,
    inactivity_ms: u64,
    minted_hb_ms: u64,
}

#[near]
impl Contract {
    /// One-time upgrade from the M1 layout (the tokens, metadata and owners are kept).
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        let old: v1::Contract = env::state_read().unwrap_or_else(|| env::panic_str("no M1 state to migrate"));
        Self { tokens: old.tokens, metadata: old.metadata, factory: old.factory, arena: old.arena, heartbeat: LookupMap::new(b"h"), forfeited: LookupMap::new(b"f") }
    }

    #[init]
    pub fn new(factory: AccountId, arena: AccountId) -> Self {
        let metadata = NFTContractMetadata {
            spec: NFT_METADATA_SPEC.to_string(),
            name: "NME PumpBag".to_string(),
            symbol: "CROWN".to_string(),
            icon: None,
            base_uri: None,
            reference: None,
            reference_hash: None,
        };
        Self {
            tokens: NonFungibleToken::new(b"o".to_vec(), factory.clone(), Some(b"m".to_vec()), Some(b"e".to_vec()), Some(b"a".to_vec())),
            metadata: LazyOption::new(b"x".to_vec(), Some(&metadata)),
            factory,
            arena,
            heartbeat: LookupMap::new(b"h"),
            forfeited: LookupMap::new(b"f"),
        }
    }

    /// Factory only. Idempotent: re-minting an existing token for the same owner is a no-op that refunds the deposit.
    #[payable]
    pub fn nft_mint(&mut self, token_id: TokenId, owner: AccountId, coin: AccountId, vault: AccountId, inactivity_ms: u64) {
        require!(env::predecessor_account_id() == self.factory, "only the factory can mint PumpBags");
        if let Some(existing) = self.tokens.owner_by_id.get(&token_id) {
            require!(existing == owner, "PumpBag already minted to a different owner");
            Promise::new(self.factory.clone()).transfer(env::attached_deposit()).detach();
            return;
        }
        let now = env::block_timestamp_ms();
        let extra = near_sdk::serde_json::json!({
            "coin": coin, "vault": vault, "inactivity_ms": inactivity_ms, "last_heartbeat_ms": now
        })
        .to_string();
        let metadata = TokenMetadata {
            title: Some(format!("PumpBag of ${}", token_id.to_uppercase())),
            description: Some(format!("The dev seat of {coin}: owns the vault {vault}. Transfer it to transfer the coin's leadership.")),
            media: None,
            media_hash: None,
            copies: Some(1),
            issued_at: Some(now.to_string()),
            expires_at: None,
            starts_at: None,
            updated_at: None,
            extra: Some(extra),
            reference: None,
            reference_hash: None,
        };
        // Storage is charged to the attached deposit; the excess is refunded to the factory (predecessor).
        self.tokens.internal_mint_with_refund(token_id.clone(), owner.clone(), Some(metadata), Some(self.factory.clone()));
        self.heartbeat.insert(token_id.clone(), now);
        self.tell_arena(&token_id, now + inactivity_ms);
        events::emit(name::CROWN_MINTED, near_sdk::serde_json::json!({ "ticker": token_id, "owner": owner, "vault": vault, "coin": coin }));
    }

    // ---------------------------------------------------------------- the owner's powers

    /// Owner only. Sends the vested part of the 5% dev allocation to the owner (attach the NEP-145 minimum, 0.00125 NEAR,
    /// if the owner is not yet registered on the coin; the vault forwards it as the registration deposit).
    #[payable]
    pub fn crown_claim_vested(&mut self, token_id: TokenId) -> Promise {
        assert_one_yocto_or_more();
        let seat = self.seat_for_owner(&token_id);
        events::emit("crown_claim", near_sdk::serde_json::json!({ "ticker": token_id, "kind": "vested", "to": seat.owner }));
        Promise::new(seat.vault).function_call("release_vested", near_sdk::serde_json::json!({ "to": seat.owner }).to_string().into_bytes(), env::attached_deposit(), VAULT_GAS)
    }

    /// Owner only. Sends the accrued pool-fee NEAR to the owner. (After a forfeit the vault routes it to the coin's
    /// treasury instead, whoever asks.)
    #[payable]
    pub fn crown_claim_fees(&mut self, token_id: TokenId) -> Promise {
        assert_one_yocto();
        let seat = self.seat_for_owner(&token_id);
        events::emit("crown_claim", near_sdk::serde_json::json!({ "ticker": token_id, "kind": "fees", "to": seat.owner }));
        Promise::new(seat.vault).function_call("release_fees", near_sdk::serde_json::json!({ "to": seat.owner }).to_string().into_bytes(), NearToken::from_yoctonear(0), VAULT_GAS)
    }

    /// Owner only: "I am still here". Restarts the inactivity clock.
    pub fn crown_heartbeat(&mut self, token_id: TokenId) {
        let seat = self.seat_for_owner(&token_id);
        require!(!self.forfeited.get(&token_id).copied().unwrap_or(false), "this PumpBag was forfeited");
        let now = env::block_timestamp_ms();
        self.heartbeat.insert(token_id.clone(), now);
        self.tell_arena(&token_id, now + seat.inactivity_ms);
        events::emit("crown_heartbeat", near_sdk::serde_json::json!({ "ticker": token_id, "owner": seat.owner, "next_deadline_ms": now + seat.inactivity_ms }));
    }

    /// Permissionless once the owner has been silent for `inactivity_ms`: the vault burns everything not yet vested and
    /// freezes vesting; the fees from then on go to the coin's treasury. Already vested tokens stay claimable by the owner.
    pub fn crown_forfeit(&mut self, token_id: TokenId) -> Promise {
        let seat = self.seat(&token_id);
        require!(!self.forfeited.get(&token_id).copied().unwrap_or(false), "already forfeited");
        let deadline = self.last_heartbeat(&token_id, &seat) + seat.inactivity_ms;
        require!(env::block_timestamp_ms() > deadline, "the owner is still within the inactivity period");
        self.forfeited.insert(token_id.clone(), true); // set first; restored if the vault refuses
        Promise::new(seat.vault)
            .function_call("forfeit", b"{}".to_vec(), NearToken::from_yoctonear(0), VAULT_GAS)
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_forfeit(token_id))
    }

    #[private]
    pub fn on_forfeit(&mut self, token_id: TokenId) {
        if env::promise_result_checked(0, 64).is_err() {
            self.forfeited.insert(token_id.clone(), false);
            events::emit("crown_forfeit_failed", near_sdk::serde_json::json!({ "ticker": token_id }));
        } else {
            events::emit("crown_forfeit", near_sdk::serde_json::json!({ "ticker": token_id }));
        }
    }

    /// Permissionless, harmless: tells the arena when this PumpBag becomes forfeitable (used once for PumpBags minted
    /// before the arena tracked deadlines; every heartbeat, transfer and mint does it too).
    pub fn crown_sync(&mut self, token_id: TokenId) {
        let seat = self.seat(&token_id);
        if !self.forfeited.get(&token_id).copied().unwrap_or(false) {
            let due = self.last_heartbeat(&token_id, &seat) + seat.inactivity_ms;
            self.tell_arena(&token_id, due);
        }
    }

    pub fn crown_status(&self, token_id: TokenId) -> near_sdk::serde_json::Value {
        let seat = self.seat(&token_id);
        let hb = self.last_heartbeat(&token_id, &seat);
        near_sdk::serde_json::json!({
            "owner": seat.owner, "vault": seat.vault, "inactivity_ms": seat.inactivity_ms, "last_heartbeat_ms": hb,
            "forfeitable_after_ms": hb + seat.inactivity_ms, "forfeited": self.forfeited.get(&token_id).copied().unwrap_or(false),
        })
    }

    pub fn get_config(&self) -> near_sdk::serde_json::Value {
        near_sdk::serde_json::json!({ "factory": self.factory, "arena": self.arena })
    }
}

fn assert_one_yocto_or_more() {
    require!(env::attached_deposit() >= NearToken::from_yoctonear(1), "attach at least 1 yoctoNEAR");
}

impl Contract {
    fn seat(&self, token_id: &str) -> Seat {
        let owner = self.tokens.owner_by_id.get(&token_id.to_string()).unwrap_or_else(|| env::panic_str("unknown PumpBag"));
        let extra: near_sdk::serde_json::Value = self
            .tokens
            .token_metadata_by_id
            .as_ref()
            .and_then(|m| m.get(&token_id.to_string()))
            .and_then(|m| m.extra)
            .and_then(|e| near_sdk::serde_json::from_str(&e).ok())
            .unwrap_or_else(|| env::panic_str("PumpBag metadata is missing"));
        Seat {
            owner,
            vault: extra["vault"].as_str().and_then(|v| v.parse().ok()).unwrap_or_else(|| env::panic_str("bad vault in metadata")),
            inactivity_ms: extra["inactivity_ms"].as_u64().unwrap_or_else(|| env::panic_str("bad inactivity in metadata")),
            minted_hb_ms: extra["last_heartbeat_ms"].as_u64().unwrap_or(0),
        }
    }

    fn seat_for_owner(&self, token_id: &str) -> Seat {
        let seat = self.seat(token_id);
        require!(env::predecessor_account_id() == seat.owner, "only the PumpBag's owner can do this");
        seat
    }

    fn last_heartbeat(&self, token_id: &str, seat: &Seat) -> u64 {
        self.heartbeat.get(&token_id.to_string()).copied().unwrap_or(seat.minted_hb_ms)
    }

    fn tell_arena(&self, token_id: &str, due_ms: u64) {
        Promise::new(self.arena.clone())
            .function_call("crown_deadline", near_sdk::serde_json::json!({ "ticker": token_id, "due_ms": due_ms }).to_string().into_bytes(), NearToken::from_yoctonear(0), ARENA_NOTE_GAS)
            .detach();
    }

    /// A transfer is a takeover: the new owner gets a full period.
    fn after_transfer(&mut self, token_id: &str, from: &AccountId) {
        let seat = self.seat(token_id);
        let now = env::block_timestamp_ms();
        self.heartbeat.insert(token_id.to_string(), now);
        if !self.forfeited.get(&token_id.to_string()).copied().unwrap_or(false) {
            self.tell_arena(token_id, now + seat.inactivity_ms);
        }
        events::emit("crown_transfer", near_sdk::serde_json::json!({ "ticker": token_id, "from": from, "to": seat.owner }));
    }
}

#[near]
impl NonFungibleTokenCore for Contract {
    #[payable]
    fn nft_transfer(&mut self, receiver_id: AccountId, token_id: TokenId, approval_id: Option<u64>, memo: Option<String>) {
        let from = self.tokens.owner_by_id.get(&token_id).unwrap_or_else(|| env::panic_str("unknown PumpBag"));
        self.tokens.nft_transfer(receiver_id, token_id.clone(), approval_id, memo);
        self.after_transfer(&token_id, &from);
    }

    #[payable]
    fn nft_transfer_call(&mut self, receiver_id: AccountId, token_id: TokenId, approval_id: Option<u64>, memo: Option<String>, msg: String) -> PromiseOrValue<bool> {
        let from = self.tokens.owner_by_id.get(&token_id).unwrap_or_else(|| env::panic_str("unknown PumpBag"));
        let r = self.tokens.nft_transfer_call(receiver_id, token_id.clone(), approval_id, memo, msg);
        self.after_transfer(&token_id, &from);
        r
    }

    fn nft_token(&self, token_id: TokenId) -> Option<Token> {
        self.tokens.nft_token(token_id)
    }
}

#[near]
impl NonFungibleTokenResolver for Contract {
    #[private]
    fn nft_resolve_transfer(&mut self, previous_owner_id: AccountId, receiver_id: AccountId, token_id: TokenId, approved_account_ids: Option<std::collections::HashMap<AccountId, u64>>) -> bool {
        self.tokens.nft_resolve_transfer(previous_owner_id, receiver_id, token_id, approved_account_ids)
    }
}

#[near]
impl NonFungibleTokenEnumeration for Contract {
    fn nft_total_supply(&self) -> U128 {
        self.tokens.nft_total_supply()
    }

    fn nft_tokens(&self, from_index: Option<U128>, limit: Option<u64>) -> Vec<Token> {
        self.tokens.nft_tokens(from_index, limit)
    }

    fn nft_supply_for_owner(&self, account_id: AccountId) -> U128 {
        self.tokens.nft_supply_for_owner(account_id)
    }

    fn nft_tokens_for_owner(&self, account_id: AccountId, from_index: Option<U128>, limit: Option<u64>) -> Vec<Token> {
        self.tokens.nft_tokens_for_owner(account_id, from_index, limit)
    }
}

#[near]
impl NonFungibleTokenMetadataProvider for Contract {
    fn nft_metadata(&self) -> NFTContractMetadata {
        self.metadata.get().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::{testing_env, NearToken};

    fn acct(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    fn crown() -> Contract {
        testing_env!(VMContextBuilder::new().build());
        Contract::new(acct("nme.testnet"), acct("arena.nme.testnet"))
    }

    fn as_caller(who: &str) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct(who)).attached_deposit(NearToken::from_millinear(50)).build());
    }

    fn mint(c: &mut Contract, owner: &str) {
        c.nft_mint("pepe".into(), acct(owner), acct("pepe.nme.testnet"), acct("crown.pepe.nme.testnet"), 300_000);
    }

    #[test]
    fn mint_is_idempotent_for_same_owner() {
        let mut c = crown();
        as_caller("nme.testnet");
        mint(&mut c, "alice.testnet");
        mint(&mut c, "alice.testnet");
        assert_eq!(c.nft_total_supply().0, 1);
        let t = c.nft_token("pepe".into()).unwrap();
        assert_eq!(t.owner_id, acct("alice.testnet"));
        assert_eq!(t.metadata.unwrap().title.unwrap(), "PumpBag of $PEPE");
    }

    #[test]
    #[should_panic(expected = "different owner")]
    fn mint_cannot_be_redirected() {
        let mut c = crown();
        as_caller("nme.testnet");
        mint(&mut c, "alice.testnet");
        mint(&mut c, "bob.testnet");
    }

    #[test]
    #[should_panic(expected = "only the factory can mint")]
    fn mint_factory_only() {
        let mut c = crown();
        as_caller("mallory.testnet");
        mint(&mut c, "mallory.testnet");
    }

    fn at(who: &str, deposit: NearToken, ms: u64) {
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct(who)).attached_deposit(deposit).block_timestamp(ms * 1_000_000).build());
    }

    const ONE: NearToken = NearToken::from_yoctonear(1);

    fn minted() -> Contract {
        let mut c = crown();
        at("nme.testnet", NearToken::from_millinear(50), 1_000);
        mint(&mut c, "alice.testnet");
        c
    }

    #[test]
    fn only_the_owner_can_claim_or_heartbeat() {
        let mut c = minted();
        for who in ["mallory.testnet", "nme.testnet"] {
            at(who, ONE, 2_000);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.crown_heartbeat("pepe".into()))).is_err(), "{who} cannot heartbeat");
            at(who, ONE, 2_000);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_claim_vested("pepe".into()); })).is_err(), "{who} cannot claim vested");
            at(who, ONE, 2_000);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_claim_fees("pepe".into()); })).is_err(), "{who} cannot claim fees");
        }
        at("alice.testnet", ONE, 2_000);
        c.crown_heartbeat("pepe".into());
        c.crown_claim_vested("pepe".into());
        c.crown_claim_fees("pepe".into());
    }

    #[test]
    #[should_panic(expected = "attach at least 1 yoctoNEAR")]
    fn claims_need_a_deposit_so_a_function_call_key_cannot_make_them() {
        let mut c = minted();
        at("alice.testnet", NearToken::from_yoctonear(0), 2_000);
        c.crown_claim_vested("pepe".into());
    }

    #[test]
    #[should_panic(expected = "still within the inactivity period")]
    fn forfeit_before_the_deadline_is_refused() {
        let mut c = minted(); // minted at 1000 ms, inactivity 300000 ms
        at("anyone.testnet", NearToken::from_yoctonear(0), 1_000 + 299_000);
        c.crown_forfeit("pepe".into());
    }

    #[test]
    fn a_heartbeat_pushes_the_deadline_out_and_anyone_can_forfeit_after_it() {
        let mut c = minted();
        at("alice.testnet", ONE, 200_000);
        c.crown_heartbeat("pepe".into());
        assert_eq!(c.crown_status("pepe".into())["forfeitable_after_ms"], 200_000 + 300_000);
        at("anyone.testnet", NearToken::from_yoctonear(0), 1_000 + 301_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_forfeit("pepe".into()); })).is_err(), "the heartbeat saved it");
        at("anyone.testnet", NearToken::from_yoctonear(0), 200_000 + 301_000);
        c.crown_forfeit("pepe".into());
        assert_eq!(c.crown_status("pepe".into())["forfeited"], true);
        at("anyone.testnet", NearToken::from_yoctonear(0), 600_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_forfeit("pepe".into()); })).is_err(), "only once");
        at("alice.testnet", ONE, 600_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.crown_heartbeat("pepe".into()))).is_err(), "a forfeited PumpBag cannot be revived");
    }

    #[test]
    fn a_failed_vault_forfeit_restores_the_flag_so_it_can_be_retried() {
        use near_sdk::PromiseResult;
        let mut c = minted();
        at("anyone.testnet", NearToken::from_yoctonear(0), 1_000 + 301_000);
        c.crown_forfeit("pepe".into());
        assert_eq!(c.crown_status("pepe".into())["forfeited"], true);
        testing_env!(VMContextBuilder::new().predecessor_account_id(acct("crown.testnet")).current_account_id(acct("crown.testnet")).build(), near_sdk::test_vm_config(), near_sdk::RuntimeFeesConfig::test(), Default::default(), vec![PromiseResult::Failed]);
        c.on_forfeit("pepe".into());
        assert_eq!(c.crown_status("pepe".into())["forfeited"], false, "the vault refused, so the PumpBag is not marked forfeited");
    }

    #[test]
    fn a_transfer_is_a_takeover_with_a_fresh_period_and_the_seller_loses_every_power() {
        let mut c = minted();
        at("alice.testnet", ONE, 250_000); // close to alice's deadline (301_000)
        c.nft_transfer(acct("bob.testnet"), "pepe".into(), None, None);
        let st = c.crown_status("pepe".into());
        assert_eq!((st["owner"].as_str(), st["last_heartbeat_ms"].as_u64()), (Some("bob.testnet"), Some(250_000)), "the new owner gets a full period");
        at("alice.testnet", ONE, 260_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_claim_fees("pepe".into()); })).is_err(), "the seller cannot claim after the sale");
        at("alice.testnet", ONE, 260_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_claim_vested("pepe".into()); })).is_err());
        at("bob.testnet", ONE, 260_000);
        c.crown_claim_fees("pepe".into());
        at("anyone.testnet", NearToken::from_yoctonear(0), 1_000 + 301_000);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| { c.crown_forfeit("pepe".into()); })).is_err(), "alice's old deadline no longer applies");
    }

    #[test]
    fn a_pumpbag_minted_before_m4_works_from_its_metadata() {
        // simulate: no heartbeat entry (an old token)
        let mut c = minted();
        c.heartbeat.remove(&"pepe".to_string());
        let st = c.crown_status("pepe".into());
        assert_eq!(st["last_heartbeat_ms"], 1_000, "falls back to the mint time stored in the metadata");
        assert_eq!(st["inactivity_ms"], 300_000);
        at("alice.testnet", ONE, 5_000);
        c.crown_sync("pepe".into());
    }
}
