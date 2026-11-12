//! S2 spike: a factory that creates key-less sub-accounts running global contract code.
//! The same wasm is both the factory and the child (child = instance that had `new_child` called).
use near_sdk::json_types::Base58CryptoHash;
use near_sdk::{env, near, require, AccountId, CryptoHash, Gas, NearToken, PanicOnDefault, Promise};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    parent: AccountId,
    marker: String,
}

#[near]
impl Contract {
    #[init]
    pub fn new_root() -> Self {
        Self { parent: env::current_account_id(), marker: "root".into() }
    }

    #[init]
    pub fn new_child(parent: AccountId, marker: String) -> Self {
        Self { parent, marker }
    }

    /// Create `<name>.<this>` with NO access key, attach global code BY HASH, call `new_child`.
    #[payable]
    pub fn create_by_hash(&mut self, name: String, hash: Base58CryptoHash) -> Promise {
        let child: AccountId = format!("{}.{}", name, env::current_account_id()).parse().unwrap();
        let h: CryptoHash = hash.into();
        Promise::new(child)
            .create_account()
            .transfer(env::attached_deposit())
            .use_global_contract(h)
            .function_call(
                "new_child",
                format!("{{\"parent\":\"{}\",\"marker\":\"{}\"}}", env::current_account_id(), name).into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(10),
            )
    }

    /// Same, but referencing global code BY PUBLISHER ACCOUNT.
    #[payable]
    pub fn create_by_account(&mut self, name: String, publisher: AccountId) -> Promise {
        let child: AccountId = format!("{}.{}", name, env::current_account_id()).parse().unwrap();
        Promise::new(child)
            .create_account()
            .transfer(env::attached_deposit())
            .use_global_contract_by_account_id(publisher)
            .function_call(
                "new_child",
                format!("{{\"parent\":\"{}\",\"marker\":\"{}\"}}", env::current_account_id(), name).into_bytes(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(10),
            )
    }

    pub fn whoami(&self) -> (AccountId, String, AccountId) {
        (env::current_account_id(), self.marker.clone(), self.parent.clone())
    }

    pub fn only_parent(&self) {
        require!(env::predecessor_account_id() == self.parent, "not parent");
    }
}
