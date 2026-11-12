//! S12 spike: a coin-like FT that (1) creates a Rhea DCL pool, (2) opens a single-sided position with its whole
//! supply via HotZap, (3) claims pool fees (remove_liquidity amount 0), and (4) acts as a router-style buyer.
//! Throwaway: it exists to prove the call sequence from a contract against the real DCL code.
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::serde_json::{self, json};
use near_sdk::{env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const FEE_TIER: u32 = 10_000; // 1%, point_delta 200

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct S12 {
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
    dcl: AccountId,
    wrap: AccountId,
    supply: u128,
    pool_id: Option<String>,
    token_is_x: bool,
    lpt_id: Option<String>,
    left: i32,
    right: i32,
    last_claim: Option<(U128, U128)>,
    last_buy_used: Option<U128>,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct Row {
    lpt_id: String,
    pool_id: String,
}

#[near]
impl S12 {
    #[init]
    pub fn new(dcl: AccountId, wrap: AccountId, supply: U128) -> Self {
        let me = env::current_account_id();
        let mut token = FungibleToken::new(b"t".to_vec());
        token.internal_register_account(&me);
        token.internal_register_account(&dcl);
        token.internal_deposit(&me, supply.0);
        let metadata = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: "S12".into(),
            symbol: "S12".into(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: 18,
        };
        let token_is_x = me.as_str() < wrap.as_str();
        Self {
            token,
            metadata: LazyOption::new(b"m".to_vec(), Some(&metadata)),
            dcl,
            wrap,
            supply: supply.0,
            pool_id: None,
            token_is_x,
            lpt_id: None,
            left: 0,
            right: 0,
            last_claim: None,
            last_buy_used: None,
        }
    }

    /// Register on DCL (0.5 NEAR), create the pool at `init_point`, then open the single-sided position.
    #[payable]
    pub fn launch(&mut self, init_point: i32, left: i32, right: i32) -> Promise {
        require!(env::attached_deposit() >= NearToken::from_millinear(650), "attach >= 0.65 NEAR");
        require!(self.pool_id.is_none(), "already launched");
        self.left = left;
        self.right = right;
        let me = env::current_account_id();
        let (a, b) = (me.clone(), self.wrap.clone());
        Promise::new(self.dcl.clone())
            .function_call("storage_deposit", json!({"account_id": me, "registration_only": false}).to_string().into_bytes(), NearToken::from_millinear(500), Gas::from_tgas(10))
            .function_call("create_pool", json!({"token_a": a, "token_b": b, "fee": FEE_TIER, "init_point": init_point}).to_string().into_bytes(), NearToken::from_millinear(100), Gas::from_tgas(30))
            .then(Self::ext(me).with_static_gas(Gas::from_tgas(20)).with_unused_gas_weight(1).on_pool_created())
    }

    #[private]
    pub fn on_pool_created(&mut self) -> Promise {
        let bytes = env::promise_result_checked(0, 512).unwrap_or_else(|_| env::panic_str("create_pool failed"));
        let pool_id: String = serde_json::from_slice(&bytes).unwrap_or_else(|_| env::panic_str("bad pool id"));
        env::log_str(&format!("pool created: {pool_id}"));
        self.pool_id = Some(pool_id.clone());
        let me = env::current_account_id();
        let (ax, ay) = if self.token_is_x { (self.supply, 0) } else { (0, self.supply) };
        let (xt, yt) = if self.token_is_x { (me.to_string(), self.wrap.to_string()) } else { (self.wrap.to_string(), me.to_string()) };
        let (input, output) = (xt.clone(), yt.clone());
        let msg = json!({"HotZap": {
            "swap_infos": [{"pool_ids": [pool_id.clone()], "input_token": input, "output_token": output, "amount_in": "0", "min_output_amount": "0"}],
            "add_liquidity_infos": [{"pool_id": pool_id, "left_point": self.left, "right_point": self.right,
                "amount_x": ax.to_string(), "amount_y": ay.to_string(), "min_amount_x": "0", "min_amount_y": "0"}]
        }})
        .to_string();
        // Our own standard ft_transfer_call (NEP-141 refund/resolve for free), 1 yocto, sender = this contract.
        Self::ext(me.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(Gas::from_tgas(120))
            .with_unused_gas_weight(0)
            .ft_transfer_call(self.dcl.clone(), U128(self.supply), None, msg)
            .then(Self::ext(me).with_static_gas(Gas::from_tgas(20)).with_unused_gas_weight(1).on_zapped())
    }

    #[private]
    pub fn on_zapped(&mut self) -> Promise {
        let bytes = env::promise_result_checked(0, 256).unwrap_or_else(|_| env::panic_str("HotZap transfer failed"));
        let used: U128 = serde_json::from_slice(&bytes).unwrap_or_else(|_| env::panic_str("bad used amount"));
        require!(used.0 == self.supply, "DCL did not take the full supply");
        let me = env::current_account_id();
        Promise::new(self.dcl.clone())
            .function_call("list_liquidities", json!({"account_id": me}).to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(30))
            .then(Self::ext(me).with_static_gas(Gas::from_tgas(10)).with_unused_gas_weight(1).on_listed())
    }

    #[private]
    pub fn on_listed(&mut self) -> Option<String> {
        let bytes = env::promise_result_checked(0, 16_384).unwrap_or_else(|_| env::panic_str("list_liquidities failed"));
        let rows: Vec<Row> = serde_json::from_slice(&bytes).unwrap_or_else(|_| env::panic_str("bad rows"));
        let pid = self.pool_id.clone().unwrap();
        self.lpt_id = rows.into_iter().find(|r| r.pool_id == pid).map(|r| r.lpt_id);
        env::log_str(&format!("position: {:?}", self.lpt_id));
        self.lpt_id.clone()
    }

    /// Permissionless. remove_liquidity with amount 0 releases only accrued fees: token fees arrive via
    /// ft_transfer (burned below), NEAR-side fees arrive as native NEAR (DCL unwraps wNEAR).
    pub fn claim_fees(&mut self) -> Promise {
        let lpt = self.lpt_id.clone().unwrap_or_else(|| env::panic_str("no position"));
        let me = env::current_account_id();
        Promise::new(self.dcl.clone())
            .function_call(
                "remove_liquidity",
                json!({"lpt_id": lpt, "amount": "0", "min_amount_x": "0", "min_amount_y": "0"}).to_string().into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(100),
            )
            .then(Self::ext(me).with_static_gas(Gas::from_tgas(15)).with_unused_gas_weight(1).on_claimed())
    }

    #[private]
    pub fn on_claimed(&mut self) -> (U128, U128) {
        let bytes = env::promise_result_checked(0, 256).unwrap_or_else(|_| env::panic_str("claim failed"));
        let v: (U128, U128) = serde_json::from_slice(&bytes).unwrap_or_else(|_| env::panic_str("bad claim result"));
        let (token_fee, near_fee) = if self.token_is_x { (v.0, v.1) } else { (v.1, v.0) };
        let me = env::current_account_id();
        if token_fee.0 > 0 {
            self.token.internal_withdraw(&me, token_fee.0); // D-31: coin-token fees are burned
        }
        self.last_claim = Some((token_fee, near_fee));
        (token_fee, near_fee)
    }

    /// Router-style buy: wrap the NEAR, swap through DCL, deliver the output straight to the buyer.
    #[payable]
    pub fn buy(&mut self, min_out: U128) -> Promise {
        let pool_id = self.pool_id.clone().unwrap_or_else(|| env::panic_str("not launched"));
        let net = env::attached_deposit();
        let user = env::predecessor_account_id();
        let me = env::current_account_id();
        let msg = json!({"Swap": {"pool_ids": [pool_id], "output_token": me, "min_output_amount": min_out.0.to_string(), "swap_out_recipient": user}}).to_string();
        Promise::new(self.wrap.clone())
            .function_call("near_deposit", b"{}".to_vec(), net, Gas::from_tgas(10))
            .function_call(
                "ft_transfer_call",
                json!({"receiver_id": self.dcl, "amount": U128(net.as_yoctonear()), "msg": msg}).to_string().into_bytes(),
                ONE_YOCTO,
                Gas::from_tgas(120),
            )
            .then(Self::ext(me).with_static_gas(Gas::from_tgas(30)).with_unused_gas_weight(1).on_bought(user, U128(net.as_yoctonear())))
    }

    #[private]
    pub fn on_bought(&mut self, user: AccountId, net: U128) -> U128 {
        let used: U128 = env::promise_result_checked(0, 256).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(U128(0));
        self.last_buy_used = Some(used);
        if used.0 < net.0 {
            // DCL refunded wNEAR to this contract: unwrap and return it to the buyer.
            let back = net.0 - used.0;
            Promise::new(self.wrap.clone())
                .function_call("near_withdraw", json!({"amount": U128(back)}).to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(10))
                .then(Promise::new(user).transfer(NearToken::from_yoctonear(back)))
                .detach();
        }
        used
    }

    pub fn get_state(&self) -> serde_json::Value {
        json!({"pool_id": self.pool_id, "lpt_id": self.lpt_id, "token_is_x": self.token_is_x, "left": self.left, "right": self.right,
               "last_claim": self.last_claim, "last_buy_used": self.last_buy_used})
    }
}

#[near]
impl FungibleTokenCore for S12 {
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
impl FungibleTokenResolver for S12 {
    #[private]
    fn ft_resolve_transfer(&mut self, sender_id: AccountId, receiver_id: AccountId, amount: U128) -> U128 {
        self.token.internal_ft_resolve_transfer(&sender_id, receiver_id, amount).0.into()
    }
}

#[near]
impl StorageManagement for S12 {
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
impl FungibleTokenMetadataProvider for S12 {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.get().unwrap()
    }
}
