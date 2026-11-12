#![allow(dead_code)]
//! Shared harness: sandbox + real Ref/wNEAR code imported from testnet + NME contracts (coin/vault as global code).
use near_api::types::transaction::result::{ExecutionFinalResult, ExecutionSuccess};
use near_api::{AccountId, NearGas, NearToken, NetworkConfig, Signer};
use near_sandbox::Sandbox;
use near_sdk::serde::de::DeserializeOwned;
use near_sdk::serde_json::{json, Value};
use std::sync::Arc;

/// The archival endpoint: the public one rate-limits this machine (HTTP 429) during imports.
pub const TESTNET_RPC: &str = "https://archival-rpc.testnet.fastnear.com";
pub const WRAP: &str = "wrap.testnet";
pub const DCL: &str = "dclv2.ref-dev.testnet";
pub const ICON: &str = "data:image/png;base64,iVBORw0KGgo=";

pub struct Ctx {
    pub sandbox: Sandbox,
    pub net: NetworkConfig,
    pub signer: Arc<Signer>,
    pub factory: AccountId,
    pub arena: AccountId,
    pub crown: AccountId,
    pub wrap: AccountId,
    pub dcl: AccountId,
    pub coin_hash: String,
    pub vault_hash: String,
}

pub fn wasm(name: &str) -> Vec<u8> {
    let path = format!("../target/near/{name}/{name}.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing {path} ({e}); run scripts/build-contracts.sh first"))
}

/// Like `wasm`, for a variant built next to the normal one (`target/near/<dir>/<file>.wasm`).
pub fn wasm_named(dir: &str, file: &str) -> Vec<u8> {
    let path = format!("../target/near/{dir}/{file}.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing {path} ({e}); run scripts/build-contracts.sh first"))
}

fn id(s: &str) -> AccountId {
    s.parse().unwrap()
}

impl Ctx {
    /// Full environment. `coin_wasm_override` lets a test point the factory at a bogus coin hash.
    pub async fn setup() -> testresult::TestResult<Ctx> {
        Self::setup_with(None).await
    }

    pub async fn setup_with(bogus_coin_hash: Option<String>) -> testresult::TestResult<Ctx> {
        Self::setup_opts(bogus_coin_hash, None).await
    }

    /// `crown_authority`: use this plain account as the Crown collection instead of the NFT contract, so a test can
    /// command the vault directly (the factory's Crown mint then fails and the coin stays `Created`; the launch is unaffected).
    pub async fn setup_opts(bogus_coin_hash: Option<String>, crown_authority: Option<&str>) -> testresult::TestResult<Ctx> {
        Self::setup_full(bogus_coin_hash, crown_authority, false).await
    }

    /// M2 environment: the arena is the `test-hooks` build (adds `test_buy_and_burn`, never deployed).
    pub async fn setup_m2() -> testresult::TestResult<Ctx> {
        Self::setup_full(None, None, true).await
    }

    /// M8 environment: bounties (0.005 NEAR per task, 0.02 per tick) and inline ticks (from 250 TGas) switched on.
    /// Just a sandbox and a signer, nothing deployed (for tests that import real testnet state themselves).
    pub async fn setup_empty() -> testresult::TestResult<Ctx> {
        let sandbox = Sandbox::start_sandbox().await?;
        let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
        let signer = Signer::from_secret_key(near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap())?;
        Ok(Ctx { sandbox, net, signer, factory: id("nme.sandbox"), arena: id("arena.sandbox"), crown: id("crown.sandbox"), wrap: id(WRAP), dcl: id(DCL), coin_hash: String::new(), vault_hash: String::new() })
    }

    pub async fn setup_autonomous() -> testresult::TestResult<Ctx> {
        Self::setup_inner(None, None, false, true).await
    }

    pub async fn setup_full(bogus_coin_hash: Option<String>, crown_authority: Option<&str>, arena_test_hooks: bool) -> testresult::TestResult<Ctx> {
        Self::setup_inner(bogus_coin_hash, crown_authority, arena_test_hooks, false).await
    }

    async fn setup_inner(bogus_coin_hash: Option<String>, crown_authority: Option<&str>, arena_test_hooks: bool, autonomous: bool) -> testresult::TestResult<Ctx> {
        let sandbox = Sandbox::start_sandbox().await?;
        let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
        let signer = Signer::from_secret_key(near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap())?;
        let mut ctx = Ctx {
            sandbox,
            net,
            signer,
            factory: id("nme.sandbox"),
            arena: id("arena.sandbox"),
            crown: id("crown.sandbox"),
            wrap: id(WRAP),
            dcl: id(DCL),
            coin_hash: String::new(),
            vault_hash: String::new(),
        };
        let genesis: AccountId = near_sandbox::config::DEFAULT_GENESIS_ACCOUNT.to_owned();

        // Real DCL + wNEAR code from testnet (code only; fresh state).
        for a in [WRAP, DCL] {
            ctx.sandbox.import_account(TESTNET_RPC, id(a)).send().await?;
        }
        ctx.call(&ctx.wrap.clone(), &genesis, "new", json!({}), 0, 30).await?.assert_success();
        ctx.call(
            &ctx.dcl.clone(),
            &genesis,
            "new",
            json!({"owner_id": genesis, "wnear_id": WRAP, "farming_contract_id": "farm.sandbox"}),
            0,
            30,
        )
        .await?
        .assert_success();
        // DCL holds and unwraps wNEAR: it must be known to wNEAR.
        ctx.register_dcl_on_wrap().await?;

        // Global code (D-15): coin + vault, referenced by hash.
        let publisher = ctx.account("publisher.sandbox", 1000).await?;
        ctx.coin_hash = ctx.publish_global(&publisher, wasm("nme_coin")).await?;
        ctx.vault_hash = ctx.publish_global(&publisher, wasm("nme_crown_vault")).await?;

        // Infra accounts + contracts.
        if let Some(name) = crown_authority {
            ctx.crown = id(name);
        }
        for a in [&ctx.factory.clone(), &ctx.arena.clone(), &ctx.crown.clone()] {
            ctx.account(a.as_str(), 200).await?;
        }
        let coin_hash_for_factory = bogus_coin_hash.unwrap_or_else(|| ctx.coin_hash.clone());
        let graveyard = id(&format!("burn.{}", ctx.factory));
        ctx.account(graveyard.as_str(), 10).await?;
        // Sandbox stand-in for OutLayer: answers `request_execution` at once (see contracts/spikes/mock-outlayer).
        let outlayer = id("outlayer.sandbox");
        ctx.account(outlayer.as_str(), 20).await?;
        let mock = std::fs::read("../spikes/mock-outlayer/target/near/mock_outlayer.wasm").or_else(|_| std::fs::read("spikes/mock-outlayer/target/near/mock_outlayer.wasm"))?;
        ctx.deploy(&outlayer, mock, "new", json!({})).await?;
        let arena_wasm = if arena_test_hooks { wasm_named("nme_arena", "nme_arena_test") } else { wasm("nme_arena") };
        ctx.deploy(&ctx.arena.clone(), arena_wasm, "new", json!({
            "factory": ctx.factory, "wrap": ctx.wrap, "dcl": ctx.dcl, "graveyard": graveyard,
            "cfg": {"bounty_per_task": if autonomous { near(0.005).to_string() } else { "0".to_string() },
                    "bounty_cap_per_tick": if autonomous { near(0.02).to_string() } else { "0".to_string() },
                    "inline_tick_min_gas_tgas": if autonomous { 250 } else { 0 }, "price_check_interval_ms": 0,
                    "fee_crown_bps": 4000, "fee_treasury_bps": 4000, "claim_interval_ms": 600000,
                    "router_fee_bps": 100, "min_pool_near": "0", "burn_slippage_bps": 500},
            "war": {"round_duration_ms": 24000, "round_interval_ms": 0, "chest_bps": 5000, "wallet_cap_bps": 5000,
                    "min_unique": 2, "min_stake": near(0.1).to_string(), "max_pairs": 2},
            "brain": {"outlayer": outlayer, "brain_url": format!("https://test.fastfs.io/x/outlayer.testnet/{}.wasm", "b".repeat(64)), "brain_hash": "b".repeat(64),
                      "secrets_account": "nme-brain.sandbox", "secrets_profile": "default", "rpc_url": "http://sandbox",
                      "min_price": near(0.1).to_string(), "outlayer_fee": near(0.02).to_string(), "record_reserve": near(0.003).to_string(),
                      "cut_bps": 1000, "deadline_ms": 60000},
            "defense": {"per_action_bps": 500, "daily_bps": 1000, "cooldown_ms": 20000, "interval_ms": 8000, "look_points": 1,
                        "trigger_points": 10, "fallback_after": 2, "min_treasury": "1000000000000000000000",
                        "outlayer_fee": near(0.01).to_string(), "fee_cap_per_day": near(0.05).to_string()},
            "commentary": {"min_treasury": near(0.001).to_string(), "per_day_per_coin": 5, "outlayer_fee": near(0.01).to_string(), "fee_cap_per_day": near(0.1).to_string()},
            "crown": ctx.crown, "gateway": "gateway.sandbox"
        })).await?;
        // The gateway: Solana deposits -> NEAR value for a beneficiary (the mock OutLayer is also the MPC stand-in).
        ctx.account("gateway.sandbox", 200).await?;
        ctx.deploy(&id("gateway.sandbox"), wasm("nme_gateway"), "new", json!({
            "outlayer": outlayer, "mpc": outlayer, "arena": ctx.arena,
            "worker": {"url": format!("https://test.fastfs.io/x/outlayer.testnet/{}.wasm", "d".repeat(64)), "hash": "d".repeat(64)},
            "cfg": {"spread_bps": 200, "max_claim_near": near(20.0).to_string(), "min_claim_near": near(0.01).to_string()},
            "relay_fee": near(0.001).to_string(),
            "wrap": ctx.wrap, "factory": ctx.factory, "crown": ctx.crown
        })).await?;
        // The arena is the swapper on DCL and holds wNEAR transiently: it must be registered on both.
        let g: AccountId = near_sandbox::config::DEFAULT_GENESIS_ACCOUNT.to_owned();
        ctx.call_yocto(&ctx.dcl, &g, "storage_deposit", json!({"account_id": ctx.arena, "registration_only": false}), near(0.5), 30).await?.assert_success();
        ctx.call_yocto(&ctx.wrap, &g, "storage_deposit", json!({"account_id": ctx.arena}), near(0.00125), 30).await?.assert_success();
        if crown_authority.is_none() {
            ctx.deploy(&ctx.crown.clone(), wasm("nme_crown"), "new", json!({"factory": ctx.factory, "arena": ctx.arena})).await?;
        }
        ctx.deploy(&ctx.factory.clone(), wasm("nme_factory"), "new", json!({
            "arena": ctx.arena, "crown": ctx.crown, "wrap": ctx.wrap, "dcl": ctx.dcl,
            "coin_code_hash": coin_hash_for_factory, "vault_code_hash": ctx.vault_hash,
        })).await?;
        Ok(ctx)
    }

    pub async fn register_dcl_on_wrap(&self) -> testresult::TestResult<()> {
        let genesis: AccountId = near_sandbox::config::DEFAULT_GENESIS_ACCOUNT.to_owned();
        self.call_yocto(&self.wrap, &genesis, "storage_deposit", json!({"account_id": self.dcl}), near(0.00125), 30).await?.assert_success();
        Ok(())
    }

    pub async fn account(&self, name: &str, near: u128) -> testresult::TestResult<AccountId> {
        let a = id(name);
        self.sandbox.create_account(a.clone()).initial_balance(NearToken::from_near(near)).send().await?;
        Ok(a)
    }

    pub async fn deploy(&self, at: &AccountId, code: Vec<u8>, init: &str, args: Value) -> testresult::TestResult<()> {
        near_api::Contract::deploy(at.clone())
            .use_code(code)
            .with_init_call(init, args)?
            .with_signer(self.signer.clone())
            .send_to(&self.net)
            .await?
            .assert_success();
        Ok(())
    }

    /// Publishes global code by hash; returns the base58 hash used in contract args.
    pub async fn publish_global(&self, from: &AccountId, code: Vec<u8>) -> testresult::TestResult<String> {
        let hash = near_api::CryptoHash::hash(&code);
        near_api::Contract::deploy_global_contract_code(code)
            .as_hash()
            .with_signer(from.clone(), self.signer.clone())
            .send_to(&self.net)
            .await?
            .assert_success();
        Ok(hash.to_string())
    }

    /// Raw call; returns the final result (success or failure) without asserting.
    pub async fn call(&self, contract: &AccountId, who: &AccountId, method: &str, args: Value, deposit_milli: u128, gas_tgas: u64) -> testresult::TestResult<ExecutionFinalResult> {
        let r = near_api::Contract(contract.clone())
            .call_function(method, args)
            .transaction()
            .gas(NearGas::from_tgas(gas_tgas))
            .deposit(NearToken::from_millinear(deposit_milli))
            .with_signer(who.clone(), self.signer.clone())
            .send_to(&self.net)
            .await?;
        Ok(r.into_full().expect("final execution result"))
    }

    /// Same with an exact yocto deposit.
    pub async fn call_yocto(&self, contract: &AccountId, who: &AccountId, method: &str, args: Value, deposit_yocto: u128, gas_tgas: u64) -> testresult::TestResult<ExecutionFinalResult> {
        let r = near_api::Contract(contract.clone())
            .call_function(method, args)
            .transaction()
            .gas(NearGas::from_tgas(gas_tgas))
            .deposit(NearToken::from_yoctonear(deposit_yocto))
            .with_signer(who.clone(), self.signer.clone())
            .send_to(&self.net)
            .await?;
        Ok(r.into_full().expect("final execution result"))
    }

    pub async fn ok(&self, contract: &AccountId, who: &AccountId, method: &str, args: Value, deposit_milli: u128, gas_tgas: u64) -> testresult::TestResult<ExecutionSuccess> {
        let r = self.call(contract, who, method, args, deposit_milli, gas_tgas).await?;
        let logs: Vec<String> = r.logs().iter().map(|s| s.to_string()).collect();
        Ok(r.into_result().unwrap_or_else(|e| panic!("{method} on {contract} failed: {e:?}\nlogs: {logs:#?}")))
    }

    /// Expects the call (or any of its receipts) to fail with a message containing `needle`.
    pub async fn fails(&self, contract: &AccountId, who: &AccountId, method: &str, args: Value, deposit_milli: u128, gas_tgas: u64, needle: &str) -> testresult::TestResult<()> {
        let r = self.call(contract, who, method, args, deposit_milli, gas_tgas).await?;
        let dump = format!("{:?}", r.failures());
        assert!(!r.failures().is_empty(), "{method} unexpectedly succeeded");
        assert!(dump.contains(needle), "{method}: expected failure containing {needle:?}, got {dump}");
        Ok(())
    }

    pub async fn view<T: DeserializeOwned + Send + Sync>(&self, contract: &AccountId, method: &str, args: Value) -> testresult::TestResult<T> {
        Ok(near_api::Contract(contract.clone()).call_function(method, args).read_only::<T>().fetch_from(&self.net).await?.data)
    }

    pub async fn balance(&self, a: &AccountId) -> testresult::TestResult<u128> {
        Ok(near_api::Account(a.clone()).tokens().near_balance().fetch_from(&self.net).await?.total.as_yoctonear())
    }

    pub async fn ft_balance(&self, token: &AccountId, who: &AccountId) -> testresult::TestResult<u128> {
        let v: String = self.view(token, "ft_balance_of", json!({"account_id": who})).await?;
        Ok(v.parse()?)
    }

    pub async fn key_count(&self, a: &AccountId) -> testresult::TestResult<usize> {
        Ok(near_api::Account(a.clone()).list_keys().fetch_from(&self.net).await?.data.len())
    }

    pub async fn fast_forward(&self, blocks: u64) -> testresult::TestResult<()> {
        self.sandbox.fast_forward(blocks).await?;
        Ok(())
    }

    // ---- NME helpers ----

    /// `first_buy` = (tokens, max_near_yocto).
    pub fn launch_params(start_point: i32, range_points: i32, first_buy: Option<(u128, u128)>) -> Value {
        json!({
            "start_point": start_point, "range_points": range_points,
            "first_buy": first_buy.map(|(t, n)| json!({"tokens": t.to_string(), "max_near": n.to_string()})),
        })
    }

    pub fn crown_params() -> Value {
        json!({"cliff_ms": 60000, "vest_ms": 600000, "inactivity_ms": 300000})
    }

    pub fn create_args(ticker: &str, launch: Value) -> Value {
        json!({
            "args": {
                "ticker": ticker, "name": format!("{ticker} coin"), "icon_data_url": ICON,
                "description": "demo coin", "socials": "", "persona": {"vibe": "degen frog", "rivals": ["doge"]},
                "launch": launch, "crown": Self::crown_params(), "colosseum_opt_in": true
            }
        })
    }

    /// Deposit (milliNEAR, rounded up) that `create_coin` needs for the default test icon and a first buy of up to `fb` yocto.
    pub async fn required_milli(&self, fb: u128) -> testresult::TestResult<u128> {
        let r: String = self.view(&self.factory, "required_deposit", json!({"icon_len": ICON.len(), "first_buy_max_near": if fb > 0 { json!(fb.to_string()) } else { Value::Null }})).await?;
        Ok(r.parse::<u128>()? / 10u128.pow(21) + 1)
    }

    pub async fn create_coin(&self, creator: &AccountId, ticker: &str, launch: Value) -> testresult::TestResult<AccountId> {
        self.create_coin_gas(creator, ticker, launch, 300).await
    }

    /// Like `create_coin`, with the creator's per-coin job price (yoctoNEAR).
    pub async fn create_coin_priced(&self, creator: &AccountId, ticker: &str, launch: Value, job_price: u128) -> testresult::TestResult<AccountId> {
        let fb = launch["first_buy"]["max_near"].as_str().map(|v| v.parse::<u128>().unwrap()).unwrap_or(0);
        let need = self.required_milli(fb).await?;
        let mut args = Self::create_args(ticker, launch);
        args["args"]["job_price"] = json!(job_price.to_string());
        self.ok(&self.factory, creator, "create_coin", args, need, 300).await?;
        Ok(id(&format!("{ticker}.{}", self.factory)))
    }

    pub async fn create_coin_gas(&self, creator: &AccountId, ticker: &str, launch: Value, gas_tgas: u64) -> testresult::TestResult<AccountId> {
        let fb = launch["first_buy"]["max_near"].as_str().map(|v| v.parse::<u128>().unwrap()).unwrap_or(0);
        let need = self.required_milli(fb).await?;
        self.ok(&self.factory, creator, "create_coin", Self::create_args(ticker, launch), need, gas_tgas).await?;
        Ok(id(&format!("{ticker}.{}", self.factory)))
    }

    /// Calls `launch` (anyone) until the coin is Live; returns how many extra calls were needed.
    pub async fn finish_launch(&self, coin: &AccountId, who: &AccountId) -> testresult::TestResult<u32> {
        let mut extra = 0;
        for _ in 0..10 {
            let l: Value = self.view(coin, "get_launch", json!({})).await?;
            if l.get("Live").is_some() {
                return Ok(extra);
            }
            assert!(l != json!("Failed"), "launch failed");
            // the first call right after creation may still be in flight; give it a moment
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            let l: Value = self.view(coin, "get_launch", json!({})).await?;
            if l.get("Live").is_some() {
                return Ok(extra);
            }
            let r = self.call(coin, who, "launch", json!({}), 0, 300).await?;
            extra += 1;
            for f in r.failures() {
                let t = format!("{:?}", f);
                if t.contains("E") { println!("  [launch #{extra}] FAILED RECEIPT status: {}", t.rsplit("status:").next().unwrap_or("").chars().take(700).collect::<String>()); }
            }
            for l in r.logs() {
                if l.contains("launch_failed") {
                    println!("  [launch #{extra}] {}", l.chars().take(420).collect::<String>());
                }
            }
            if !r.failures().is_empty() {
                println!("launch call failed (likely still in flight): {}", format!("{:?}", r.failures()).chars().take(200).collect::<String>());
            }
        }
        let l: Value = self.view(coin, "get_launch", json!({})).await?;
        assert!(l.get("Live").is_some(), "launch did not finish: {l}");
        Ok(extra)
    }

    // ---- DCL helpers ----

    /// Register an account on DCL (0.5 NEAR) so it can create pools / hold assets.
    pub async fn dcl_register(&self, who: &AccountId) -> testresult::TestResult<()> {
        self.call_yocto(&self.dcl, who, "storage_deposit", json!({"account_id": who, "registration_only": false}), near(0.5), 30).await?.assert_success();
        Ok(())
    }

    pub async fn dcl_create_pool(&self, who: &AccountId, token: &AccountId, fee: u32, point: i32) -> testresult::TestResult<()> {
        self.call(&self.dcl, who, "create_pool", json!({"token_a": token, "token_b": self.wrap, "fee": fee, "init_point": point}), 100, 60).await?.assert_success();
        Ok(())
    }

    /// Buy straight through DCL (no arena needed, I1): wrap the NEAR and swap with the output going to `buyer`.
    pub async fn dcl_buy(&self, buyer: &AccountId, coin: &AccountId, pool_id: &str, near_yocto: u128) -> testresult::TestResult<()> {
        self.call_yocto(coin, buyer, "storage_deposit", json!({"account_id": buyer}), near(0.00125), 30).await?.assert_success();
        self.call_yocto(&self.wrap, buyer, "storage_deposit", json!({"account_id": buyer}), near(0.00125), 30).await?.assert_success();
        self.call_yocto(&self.wrap, buyer, "near_deposit", json!({}), near_yocto, 30).await?.assert_success();
        let msg = json!({"Swap": {"pool_ids": [pool_id], "output_token": coin, "min_output_amount": "1", "swap_out_recipient": buyer}}).to_string();
        let r = self.call_yocto(&self.wrap, buyer, "ft_transfer_call", json!({"receiver_id": self.dcl, "amount": near_yocto.to_string(), "msg": msg}), 1, 200).await?;
        assert!(r.failures().is_empty(), "DCL buy failed: {:?}", r.failures());
        Ok(())
    }

    /// Sell `amount` coin tokens straight through DCL; the NEAR proceeds arrive as native NEAR.
    pub async fn dcl_sell(&self, seller: &AccountId, coin: &AccountId, pool_id: &str, amount: u128) -> testresult::TestResult<()> {
        let msg = json!({"Swap": {"pool_ids": [pool_id], "output_token": self.wrap, "min_output_amount": "1"}}).to_string();
        let r = self.call_yocto(coin, seller, "ft_transfer_call", json!({"receiver_id": self.dcl, "amount": amount.to_string(), "msg": msg}), 1, 200).await?;
        assert!(r.failures().is_empty(), "DCL sell failed: {:?}", r.failures());
        Ok(())
    }
}

/// Exact NEAR amount in yocto from a decimal with at most 5 fractional digits.
pub fn near(x: f64) -> u128 {
    ((x * 100_000.0).round() as u128) * 10u128.pow(19)
}

pub fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}
