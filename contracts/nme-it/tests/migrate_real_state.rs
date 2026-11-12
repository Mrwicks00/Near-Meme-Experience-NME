//! Real-state migration check (lessons 8): copies the DEPLOYED arena and factory (code + storage) from testnet into the
//! sandbox, runs the new `migrate` over them, and requires every old view to answer the same afterwards.
//! Needs network access to testnet and the freshly built wasm (`scripts/build-contracts.sh`).
//! Run: `cargo test -p nme-it --test migrate_real_state -- --nocapture`
mod common;
use common::*;
use near_api::{AccountId, NearToken};
use near_sdk::serde_json::{json, Value};

fn id(s: &str) -> AccountId {
    s.parse().unwrap()
}

const ARENA: &str = "arena.nme.testnet";
const FACTORY: &str = "nme.testnet";
const BRAIN_HASH: &str = "e022e3fefc69b5da7b92a90cf2c54dc0780564f6867db52400325cd77b7da839";

async fn snapshot(ctx: &Ctx, acc: &AccountId, views: &[(&str, Value)]) -> testresult::TestResult<Vec<Value>> {
    let mut out = vec![];
    for (m, a) in views {
        let v: Value = ctx.view(acc, m, a.clone()).await.unwrap_or_else(|e| panic!("view {m} on {acc} failed: {e:?}"));
        out.push(v);
    }
    Ok(out)
}

#[tokio::test]
#[ignore = "done: arena and factory were migrated on testnet on 2026-11-09 (txs 5iqSAfRD9uidnLb9ffx2LGknkCTH42m9HvHCWmtAEADk, HTW9LPjigpeuMT3TCDtNQEvsdRm8h7QJeWt4Vg2DJ3fa); the live state is v10 now, so this check cannot run again"]
async fn the_deployed_arena_and_factory_migrate_without_losing_anything() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_empty().await?;
    for a in [ARENA, FACTORY] {
        ctx.sandbox.import_account(TESTNET_RPC, id(a)).with_storage().initial_balance(NearToken::from_near(50)).send().await?;
    }
    let (arena, factory) = (id(ARENA), id(FACTORY));

    let arena_views: Vec<(&str, Value)> = vec![
        ("get_coin", json!({"key": "pepe"})),
        ("get_coin", json!({"key": "frog"})),
        ("get_coin", json!({"key": "toad"})),
        ("get_coin", json!({"key": "nme-batman.testnet"})),
        ("get_config", json!({})),
        ("feed_last_id", json!({})),
        ("feed_since", json!({"id": 0, "limit": 500})),
        ("next_due_ms", json!({})),
        ("due_tasks", json!({"limit": 50})),
        ("protocol_balance", json!({})),
        ("get_defense_config", json!({})),
        ("get_defense_state", json!({"ticker": "toad"})),
        ("get_commentary", json!({"limit": 64})),
        ("get_commentary_config", json!({})),
        ("get_gateway", json!({})),
        ("get_deliveries", json!({"limit": 20})),
        ("get_job", json!({"job_id": 1})),
        ("get_round", json!({"id": 2})),
        ("current_round", json!({})),
        ("next_round_ms", json!({})),
        ("get_war_config", json!({})),
        ("roster", json!({"from": 0, "limit": 50})),
        ("get_crown_deadline", json!({"ticker": "pepe"})),
    ];
    let mut brain_before: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;
    let before = snapshot(&ctx, &arena, &arena_views).await?;

    let factory_views: Vec<(&str, Value)> = vec![
        ("get_coin", json!({"ticker": "pepe"})),
        ("get_coin", json!({"ticker": "frog"})),
        ("get_coin", json!({"ticker": "toad"})),
        ("list_coins", json!({"from_index": 0, "limit": 50})),
        ("feed_since", json!({"id": 0, "limit": 500})),
        ("get_config", json!({})),
        ("get_bounds", json!({})),
    ];
    let f_before = snapshot(&ctx, &factory, &factory_views).await?;

    let url = format!("https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/{BRAIN_HASH}.wasm");
    ctx.deploy(&arena, wasm("nme_arena"), "migrate", json!({"brain_url": url, "brain_hash": BRAIN_HASH})).await?;
    ctx.deploy(&factory, wasm("nme_factory"), "migrate", json!({})).await?;

    let after = snapshot(&ctx, &arena, &arena_views).await?;
    for (i, (m, a)) in arena_views.iter().enumerate() {
        let mut got = after[i].clone();
        if *m == "get_deliveries" {
            // The only intended difference: old deliveries gain empty media fields.
            for d in got.as_array_mut().unwrap() {
                let o = d.as_object_mut().unwrap();
                assert_eq!(o.remove("media_url"), Some(json!("")));
                assert_eq!(o.remove("media_hash"), Some(json!("")));
            }
        }
        assert_eq!(before[i], got, "arena view {m}({a}) changed across the migration");
    }
    let f_after = snapshot(&ctx, &factory, &factory_views).await?;
    for (i, (m, a)) in factory_views.iter().enumerate() {
        assert_eq!(f_before[i], f_after[i], "factory view {m}({a}) changed across the migration");
    }

    // Only the brain pin changes.
    let mut brain_after: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;
    assert_eq!(brain_after["brain_hash"], BRAIN_HASH);
    for b in [&mut brain_before, &mut brain_after] {
        b.as_object_mut().unwrap().remove("brain_url");
        b.as_object_mut().unwrap().remove("brain_hash");
    }
    assert_eq!(brain_before, brain_after, "brain config other than the pin changed");

    // New state reads sane: no price set for old coins = the global minimum; nothing stored for media.
    let min: Value = ctx.view(&arena, "get_job_price", json!({"ticker": "pepe"})).await?;
    let cfg: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;
    assert_eq!(min, cfg["min_price"]);
    let d: Value = ctx.view(&arena, "get_deliveries", json!({"limit": 20})).await?;
    println!("deliveries after migrate: {}", d.as_array().map(|a| a.len()).unwrap_or(0));
    Ok(())
}

const GATEWAY: &str = "gateway.nme.testnet";
const SOL_TX: &str = "5mkfv677EVWqTZrsDsdY9sMQQpraHtc2vFUx4GVWZfB5yCvyRfgghP6AxqA4hvAH7M8cxyCQ5Ca26NaEbWKBVEMd";
const VERIFIER_HASH: &str = "1bcab45bd561d74d0a236f3b0ee7aefbcb4d629ebefb67906ccfa8ad5a6a0332";
// Wallets that hold real balances on the live gateway (Phantom, MetaMask, the throwaway test key) and the token they hold.
const WALLETS: [&str; 3] = [
    "sol:ou1ohPLwTBPQtUr8kX3N5MWKWzrbJUNMqBz3FTe2kmK",
    "evm:0xc95cc3dc93ac91f4d0a8891ba2bfd1ace0f51d41",
    "sol:89v8NbLSyGWtEs9aFzvBoQGHy7UTsCFfNUifNXNYPrUy",
];

/// The live gateway (the 2026-11-09 layout: wallet balances and token ledger) gains the launch and PumpBag records (D-59) and
/// keeps everything else byte for byte: reserve, claims, wallet balances and nonces, token balances, relay fee, worker pin, wNEAR.
#[tokio::test]
#[ignore = "done: the gateway was migrated to this layout on testnet on 2026-11-09 (migrate_v3); the live state is already v3, so this check cannot run again"]
async fn the_deployed_gateway_migrates_keeping_its_reserve_claims_and_wallets() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_empty().await?;
    ctx.sandbox.import_account(TESTNET_RPC, id(GATEWAY)).with_storage().initial_balance(NearToken::from_near(50)).send().await?;
    let gw = id(GATEWAY);
    let mut views: Vec<(&str, Value)> = vec![("get_reserve", json!({})), ("get_claim", json!({"tx_id": SOL_TX})), ("get_config", json!({})), ("get_lock", json!({}))];
    for w in WALLETS {
        views.push(("get_wallet", json!({"owner": w})));
        views.push(("get_wallet_tokens", json!({"owner": w, "token": "pepe.nme.testnet"})));
    }
    let before = snapshot(&ctx, &gw, &views).await?;
    assert!(matches!(&before[1], Value::Object(o) if o.contains_key("Done")), "the live claim is Done: {}", before[1]);
    assert_eq!(before[2]["worker"]["hash"], VERIFIER_HASH, "the live gateway already runs the new layout");
    assert!(before[4]["near"].as_str().unwrap().parse::<u128>().unwrap() > 0, "the live wallets hold real balances: {}", before[4]);

    ctx.deploy(&gw, wasm("nme_gateway"), "migrate_v3", json!({"factory": "nme.testnet", "crown": "crown.nme.testnet"})).await?;

    let after = snapshot(&ctx, &gw, &views).await?;
    assert_eq!(before, after, "nothing that existed changed");
    assert!(ctx.view::<Value>(&gw, "get_launch", json!({"ticker": "froggy"})).await?.is_null(), "launch records start empty");
    assert!(ctx.view::<Value>(&gw, "get_crown_owner", json!({"ticker": "froggy"})).await?.is_null(), "PumpBag records start empty");
    Ok(())
}
