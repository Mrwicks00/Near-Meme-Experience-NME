//! M2: arena router (buy with fee, fee-free sell pass-through), chests, enlist, buy-and-burn (native burn + graveyard).
mod common;
use common::*;
use near_api::AccountId;
use near_sdk::serde_json::{json, Value};
use nme_common::math::bps_of;
use nme_common::TOTAL_SUPPLY;

fn id(s: &str) -> AccountId {
    s.parse().unwrap()
}

async fn supply(ctx: &Ctx, token: &AccountId) -> testresult::TestResult<u128> {
    Ok(ctx.view::<String>(token, "ft_total_supply", json!({})).await?.parse()?)
}

async fn coin_entry(ctx: &Ctx, key: &str) -> testresult::TestResult<Value> {
    ctx.view(&ctx.arena, "get_coin", json!({ "key": key })).await
}

async fn feed_kinds(ctx: &Ctx) -> testresult::TestResult<Vec<String>> {
    let f: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 100})).await?;
    Ok(f.as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap().to_string()).collect())
}

#[tokio::test]
async fn router_buy_sell_fees_refunds_and_chests() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_m2().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let bob = ctx.account("bob.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 100).await?;
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &carol).await?;
    let vault: AccountId = format!("crown.{coin}").parse()?;

    // arena is registered on the coin by the factory (needed to receive sells and to burn)
    let sb: Value = ctx.view(&coin, "storage_balance_of", json!({"account_id": ctx.arena})).await?;
    assert!(!sb.is_null(), "the factory registered the arena on the coin");

    // ---- buy 1 NEAR through the arena
    ctx.call_yocto(&coin, &bob, "storage_deposit", json!({"account_id": bob}), near(0.00125), 30).await?.assert_success();
    let before = ctx.ft_balance(&coin, &bob).await?;
    let r = ctx.call(&ctx.arena, &bob, "buy", json!({"key": "pepe", "min_out": "1"}), 1000, 300).await?;
    assert!(r.failures().is_empty(), "buy failed: {:?}", r.failures());
    let got = ctx.ft_balance(&coin, &bob).await? - before;
    assert!(got > 0, "the buyer received tokens straight from the pool");
    // fee: 1% of 1 NEAR, split 40 / 40 / 20 -> treasury, PumpBag vault, protocol
    let fee = bps_of(near(1.0), 100);
    let treasury = bps_of(fee, 4000);
    let crown = bps_of(fee, 4000);
    let protocol = fee - treasury - crown;
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let e = coin_entry(&ctx, "pepe").await?;
    assert_eq!(s(&e["treasury_near"]), treasury, "40% of the router fee to the treasury");
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(s(&v["fees_near"]), crown, "40% to the PumpBag vault");
    let acfg: Value = ctx.view(&ctx.arena, "get_config", json!({})).await?;
    assert_eq!(s(&acfg["protocol_near"]), protocol, "20% to the protocol bucket");
    assert!(feed_kinds(&ctx).await?.contains(&"trade".to_string()), "trade in the feed");

    // ---- impossible min_out: DCL refuses, everything comes back, nothing is credited
    let nb = ctx.balance(&bob).await?;
    let trades_before = feed_kinds(&ctx).await?.iter().filter(|k| *k == "trade").count();
    let r = ctx.call(&ctx.arena, &bob, "buy", json!({"key": "pepe", "min_out": (TOTAL_SUPPLY * 10).to_string()}), 1000, 300).await?;
    // (DCL fails its own receipt when min_out cannot be met; the arena's callback turns that into the refund)
    assert!(r.logs().iter().any(|l| l.contains("trade_refused")), "the arena reported the refused buy: {:?}", r.logs());
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let lost = nb - ctx.balance(&bob).await?;
    assert!(lost < near(0.05), "the NEAR came back (lost {lost})");
    assert_eq!(coin_entry(&ctx, "pepe").await?["treasury_near"], e["treasury_near"], "no fee on a refused buy");
    assert_eq!(feed_kinds(&ctx).await?.iter().filter(|k| *k == "trade").count(), trades_before, "no trade recorded");

    // ---- a buyer who is NOT registered on the token: the swap cannot deliver, so nothing may be lost or credited
    let dave = ctx.account("dave.sandbox", 100).await?;
    let nd = ctx.balance(&dave).await?;
    let t_before = coin_entry(&ctx, "pepe").await?["treasury_near"].clone();
    ctx.call(&ctx.arena, &dave, "buy", json!({"key": "pepe", "min_out": "1"}), 500, 300).await?;
    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
    let dave_tokens = ctx.ft_balance(&coin, &dave).await.unwrap_or(0);
    let dave_lost = nd - ctx.balance(&dave).await?;
    println!("unregistered buyer: tokens {dave_tokens}, NEAR lost {dave_lost}");
    assert!(dave_tokens > 0 || dave_lost < near(0.05), "an undeliverable buy must refund (tokens {dave_tokens}, lost {dave_lost})");
    if dave_tokens == 0 {
        assert_eq!(coin_entry(&ctx, "pepe").await?["treasury_near"], t_before, "no fee is kept for an undelivered buy");
    }

    // ---- sell half back through the arena: fee-free pass-through, NEAR straight to the seller
    let sell = got / 2;
    let nb = ctx.balance(&bob).await?;
    let r = ctx.call_yocto(&coin, &bob, "ft_transfer_call", json!({"receiver_id": ctx.arena, "amount": sell.to_string(), "msg": json!({"sell": {"min_out": "1"}}).to_string()}), 1, 300).await?;
    assert!(r.failures().is_empty(), "sell failed: {:?}", r.failures());
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(ctx.ft_balance(&coin, &bob).await?, before + got - sell, "exactly the sold tokens left");
    assert!(ctx.balance(&bob).await? > nb, "NEAR arrived from the pool");
    assert_eq!(ctx.ft_balance(&coin, &ctx.arena).await?, 0, "the arena keeps nothing of a sell");
    assert_eq!(coin_entry(&ctx, "pepe").await?["treasury_near"], e["treasury_near"], "sells carry no router fee");

    // ---- sell with an impossible min_out, a garbage msg: tokens come back
    let held = ctx.ft_balance(&coin, &bob).await?;
    ctx.call_yocto(&coin, &bob, "ft_transfer_call", json!({"receiver_id": ctx.arena, "amount": (held / 10).to_string(), "msg": json!({"sell": {"min_out": near(1000.0).to_string()}}).to_string()}), 1, 300).await?;
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(ctx.ft_balance(&coin, &bob).await?, held, "a refused sell returns every token");
    ctx.call_yocto(&coin, &bob, "ft_transfer_call", json!({"receiver_id": ctx.arena, "amount": "1000", "msg": "nonsense"}), 1, 300).await?;
    assert_eq!(ctx.ft_balance(&coin, &bob).await?, held, "a malformed sell message returns the tokens");
    assert_eq!(ctx.ft_balance(&coin, &ctx.arena).await?, 0);

    // ---- chests: anyone funds, nobody withdraws
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 2000, 30).await?;
    assert_eq!(s(&coin_entry(&ctx, "pepe").await?["chest_near"]), near(2.0));
    ctx.fails(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 0, 30, "attach NEAR").await?;
    ctx.fails(&ctx.arena, &carol, "fund_chest", json!({"key": "nope"}), 100, 30, "unknown coin").await?;
    for m in ["withdraw_chest", "withdraw_treasury", "withdraw", "take_chest"] {
        ctx.fails(&ctx.arena, &carol, m, json!({"key": "pepe"}), 0, 10, "MethodNotFound").await?;
    }
    assert!(feed_kinds(&ctx).await?.contains(&"chest_funded".to_string()));

    // ---- buy-and-burn is not reachable from outside the contract (the production build has no such method)
    ctx.fails(&ctx.arena, &carol, "buy_and_burn", json!({"key": "pepe", "from_chest": true, "near": "1"}), 0, 10, "MethodNotFound").await?;
    Ok(())
}

#[tokio::test]
async fn enlist_outside_coin_and_both_burn_paths() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_m2().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 100).await?;
    let graveyard = id(&format!("burn.{}", ctx.factory));

    // ---- an "outside" coin: a plain NEP-141 with a Rhea DCL pool, NOT created by the factory, with no `burn` method
    let outside = ctx.account("outside.sandbox", 300).await?;
    let wasm = std::fs::read("../spikes/s12-launcher/target/near/s12_launcher.wasm")?;
    ctx.deploy(&outside, wasm, "new", json!({"dcl": ctx.dcl, "wrap": ctx.wrap, "supply": "1000000000000000000000000000"})).await?;
    ctx.call_yocto(&ctx.wrap, &carol, "storage_deposit", json!({"account_id": outside}), near(0.00125), 30).await?.assert_success();
    ctx.call(&outside, &carol, "launch", json!({"init_point": 92_000, "left": 92_200, "right": 132_200}), 1000, 300).await?.assert_success();
    let pool_id = format!("{outside}|wrap.testnet|10000");

    // ---- enlist: rejections first
    ctx.fails(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": pool_id}), 5, 100, "attach at least 0.02").await?;
    ctx.fails(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": "nonsense"}), 30, 100, "x|y|fee").await?;
    ctx.fails(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": "other.sandbox|wrap.testnet|10000"}), 30, 100, "does not pair").await?;
    let nme_like = format!("sneaky.{}", ctx.factory);
    ctx.fails(&ctx.arena, &carol, "enlist", json!({"token": nme_like, "pool_id": format!("{nme_like}|wrap.testnet|10000")}), 30, 100, "registered by the factory").await?;
    // not a token / pool missing: the call itself succeeds, the callback rejects and refunds
    let before = ctx.balance(&carol).await?;
    let r = ctx.call(&ctx.arena, &carol, "enlist", json!({"token": alice, "pool_id": format!("{alice}|wrap.testnet|10000")}), 30, 100).await?;
    assert!(r.logs().iter().any(|l| l.contains("enlist_rejected") && l.contains("NEP-141")), "not an FT: {:?}", r.logs());
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    assert!(before - ctx.balance(&carol).await? < near(0.01), "the deposit was refunded");
    let r = ctx.call(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": format!("{outside}|wrap.testnet|2000")}), 30, 100).await?;
    assert!(r.logs().iter().any(|l| l.contains("enlist_rejected") && l.contains("does not exist")), "missing pool: {:?}", r.logs());
    assert!(coin_entry(&ctx, outside.as_str()).await?.is_null(), "nothing was created by the rejected attempts");

    // ---- enlist succeeds; the arena and the graveyard are registered on the token
    let before = ctx.balance(&carol).await?;
    let r = ctx.call(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": pool_id}), 30, 150).await?;
    assert!(r.failures().is_empty(), "enlist failed: {:?}", r.failures());
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let e = coin_entry(&ctx, outside.as_str()).await?;
    assert_eq!(e["kind"], "Outside");
    assert_eq!(e["pool_id"], pool_id.as_str());
    assert!(e["crown_vault"].is_null(), "an outside coin has no PumpBag");
    for who in [&ctx.arena, &graveyard] {
        let sb: Value = ctx.view(&outside, "storage_balance_of", json!({"account_id": who})).await?;
        assert!(!sb.is_null(), "{who} registered on the token");
    }
    assert!(feed_kinds(&ctx).await?.contains(&"coin_enlisted".to_string()));
    assert!(before - ctx.balance(&carol).await? < near(0.02), "only the registrations and storage were charged");
    ctx.fails(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": pool_id}), 30, 100, "already enlisted").await?;

    // ---- chests of outside coins work the same
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": outside}), 3000, 30).await?;
    assert_eq!(s(&coin_entry(&ctx, outside.as_str()).await?["chest_near"]), near(3.0));

    // ---- burn path 1: the token has NO burn -> tokens end in the graveyard, supply untouched
    let supply_before = supply(&ctx, &outside).await?;
    // (the token has no `burn`, so one receipt in this chain fails by design; the arena then uses the graveyard)
    let r = ctx.call(&ctx.arena, &carol, "test_buy_and_burn", json!({"key": outside, "from_chest": true, "near": near(1.0).to_string()}), 0, 300).await?;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let buried = ctx.ft_balance(&outside, &graveyard).await?;
    assert!(buried > 0, "bought tokens were sent to the graveyard");
    assert_eq!(supply(&ctx, &outside).await?, supply_before, "supply is unchanged: the tokens are out of circulation, not destroyed");
    assert_eq!(ctx.ft_balance(&outside, &ctx.arena).await?, 0, "the arena keeps nothing");
    assert_eq!(s(&coin_entry(&ctx, outside.as_str()).await?["chest_near"]), near(2.0), "exactly 1 NEAR left the chest");
    let logs: Vec<String> = r.logs().iter().map(|l| l.to_string()).collect();
    assert!(logs.iter().any(|l| l.contains("treasury_buyback") && l.contains("graveyard")), "event via graveyard: {logs:?}");

    // ---- ledger discipline: cannot spend more than the chest holds; the production arena has no hook
    ctx.fails(&ctx.arena, &carol, "test_buy_and_burn", json!({"key": outside, "from_chest": true, "near": near(50.0).to_string()}), 0, 300, "not enough NEAR in the ledger").await?;
    ctx.fails(&ctx.arena, &carol, "test_buy_and_burn", json!({"key": outside, "from_chest": false, "near": "1"}), 0, 300, "not enough NEAR in the ledger").await?;

    // ---- burn path 2: a native NME coin -> real `burn`, supply drops
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &carol).await?;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 2000, 30).await?;
    let s0 = supply(&ctx, &coin).await?;
    let r = ctx.call(&ctx.arena, &carol, "test_buy_and_burn", json!({"key": "pepe", "from_chest": true, "near": near(1.0).to_string()}), 0, 300).await?;
    assert!(r.failures().is_empty(), "native buyback failed: {:?}", r.failures());
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let s1 = supply(&ctx, &coin).await?;
    assert!(s1 < s0, "the winner's supply dropped: {s0} -> {s1}");
    assert_eq!(ctx.ft_balance(&coin, &ctx.arena).await?, 0);
    assert_eq!(ctx.ft_balance(&coin, &graveyard).await?, 0, "a native coin is burned, not buried");
    assert_eq!(s(&coin_entry(&ctx, "pepe").await?["chest_near"]), near(1.0));
    let logs: Vec<String> = r.logs().iter().map(|l| l.to_string()).collect();
    assert!(logs.iter().any(|l| l.contains("treasury_buyback") && l.contains("\"via\":\"burn\"")), "event via burn: {logs:?}");

    // ---- treasury can be the source too (fees paid into it by the router)
    let bob = ctx.account("bob.sandbox", 100).await?;
    ctx.call_yocto(&coin, &bob, "storage_deposit", json!({"account_id": bob}), near(0.00125), 30).await?.assert_success();
    ctx.call(&ctx.arena, &bob, "buy", json!({"key": "pepe", "min_out": "1"}), 5000, 300).await?.assert_success();
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    let t = s(&coin_entry(&ctx, "pepe").await?["treasury_near"]);
    assert!(t > 0);
    let s2 = supply(&ctx, &coin).await?;
    ctx.call(&ctx.arena, &carol, "test_buy_and_burn", json!({"key": "pepe", "from_chest": false, "near": t.to_string()}), 0, 300).await?.assert_success();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(supply(&ctx, &coin).await? < s2, "a treasury buyback burns supply too");
    assert_eq!(s(&coin_entry(&ctx, "pepe").await?["treasury_near"]), 0);
    Ok(())
}
