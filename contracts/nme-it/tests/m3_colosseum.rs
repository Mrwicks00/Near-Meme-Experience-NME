//! M3: the Colosseum. An NME coin and an outside coin fight over two rounds; each round's loser chest buys and burns
//! the winner (graveyard for the outside coin, native burn for the NME coin).
mod common;
use common::*;
use near_api::AccountId;
use near_sdk::serde_json::{json, Value};

fn id(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

async fn supply(ctx: &Ctx, token: &AccountId) -> testresult::TestResult<u128> {
    Ok(ctx.view::<String>(token, "ft_total_supply", json!({})).await?.parse()?)
}

async fn chest(ctx: &Ctx, key: &str) -> testresult::TestResult<u128> {
    let e: Value = ctx.view(&ctx.arena, "get_coin", json!({ "key": key })).await?;
    Ok(s(&e["chest_near"]))
}

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

#[tokio::test]
async fn colosseum_two_rounds_graveyard_then_native_burn() -> testresult::TestResult<()> {
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let graveyard = id(&format!("burn.{}", ctx.factory));

    // ---- fighters: NME coin `pepe` and an outside plain NEP-141 (no `burn`)
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    let outside = ctx.account("outside.sandbox", 300).await?;
    let wasm = std::fs::read("../spikes/s12-launcher/target/near/s12_launcher.wasm")?;
    ctx.deploy(&outside, wasm, "new", json!({"dcl": ctx.dcl, "wrap": ctx.wrap, "supply": "1000000000000000000000000000"})).await?;
    ctx.call_yocto(&ctx.wrap, &carol, "storage_deposit", json!({"account_id": outside}), near(0.00125), 30).await?.assert_success();
    ctx.call(&outside, &carol, "launch", json!({"init_point": 92_000, "left": 92_200, "right": 132_200}), 1000, 300).await?.assert_success();
    let out_pool = format!("{outside}|wrap.testnet|10000");
    ctx.ok(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": out_pool}), 30, 150).await?;
    sleep(1500).await;
    let roster: Vec<String> = ctx.view(&ctx.arena, "roster", json!({"from": 0, "limit": 10})).await?;
    assert_eq!(roster, vec!["pepe".to_string(), outside.to_string()], "both fighters are on the roster");

    // ---- nothing to fight with: no chests yet
    ctx.fails(&ctx.arena, &carol, "start_round", json!({}), 100, 100, "no eligible pair").await?;
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 4000, 30).await?;
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": outside}), 2000, 30).await?;

    // ---- six wallets, registered where they need to be
    let mut pw = vec![];
    let mut ow = vec![];
    for n in 0..2 {
        let w = ctx.account(&format!("p{n}.sandbox"), 100).await?;
        ctx.call_yocto(&pepe, &w, "storage_deposit", json!({"account_id": w}), near(0.00125), 30).await?.assert_success();
        pw.push(w);
        let w = ctx.account(&format!("o{n}.sandbox"), 100).await?;
        ctx.call_yocto(&outside, &w, "storage_deposit", json!({"account_id": w}), near(0.00125), 30).await?.assert_success();
        ow.push(w);
    }

    // ================= round 1: the outside coin wins; the loser (pepe) buys it and the tokens go to the graveyard
    let r = ctx.call(&ctx.arena, &carol, "start_round", json!({}), 100, 100).await?;
    assert!(r.failures().is_empty(), "start_round: {:?}", r.failures());
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    let pair = &round["pairs"][0];
    assert_eq!(pair["a"], "pepe");
    assert_eq!(pair["b"], outside.as_str());
    assert_eq!(s(&pair["stake"]), near(1.0), "50% of the weaker (2 NEAR) chest");
    assert_eq!(chest(&ctx, "pepe").await?, near(3.0), "stake locked out of the chest");
    assert_eq!(chest(&ctx, outside.as_str()).await?, near(1.0));
    ctx.fails(&ctx.arena, &carol, "start_round", json!({}), 100, 100, "too early").await?;
    ctx.fails(&ctx.arena, &carol, "settle_round", json!({"round_id": 1}), 0, 100, "still running").await?;

    // a wash trader on pepe's side: buys 1 NEAR and sells every token back through the arena -> counts for nothing
    let wash = ctx.account("wash.sandbox", 100).await?;
    ctx.call_yocto(&pepe, &wash, "storage_deposit", json!({"account_id": wash}), near(0.00125), 30).await?.assert_success();
    ctx.call(&ctx.arena, &wash, "buy", json!({"key": "pepe", "min_out": "1"}), 1000, 300).await?.assert_success();
    sleep(1500).await;
    let mid: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    assert_eq!(mid["pairs"][0]["unique_a"], 1, "the wash trader scores while holding the tokens");
    let held = ctx.ft_balance(&pepe, &wash).await?;
    assert!(held > 0);
    let r = ctx.call_yocto(&pepe, &wash, "ft_transfer_call", json!({"receiver_id": ctx.arena, "amount": held.to_string(), "msg": "{\"sell\":{\"min_out\":\"1\"}}"}), 1, 300).await?;
    assert!(r.failures().is_empty(), "wash sell: {:?}", r.failures());
    sleep(2500).await;
    assert_eq!(ctx.ft_balance(&pepe, &wash).await?, 0, "everything was sold");
    let after: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    assert_eq!((after["pairs"][0]["unique_a"].as_u64(), s(&after["pairs"][0]["score_a"])), (Some(0), 0), "a buy/sell round trip scores nothing");
    for w in &pw {
        ctx.call(&ctx.arena, w, "buy", json!({"key": "pepe", "min_out": "1"}), 300, 300).await?.assert_success();
    }
    for w in &ow {
        ctx.call(&ctx.arena, w, "buy", json!({"key": outside, "min_out": "1"}), 1000, 300).await?.assert_success();
    }
    sleep(1500).await;
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    let pair = &round["pairs"][0];
    assert_eq!(s(&pair["score_a"]), 2 * (near(0.3) - near(0.003)), "two wallets x 0.3 NEAR, counted after the 1% router fee");
    assert_eq!(s(&pair["score_b"]), near(1.0), "two wallets x 1 NEAR clamped to 0.5 each");
    assert_eq!((pair["unique_a"].as_u64(), pair["unique_b"].as_u64()), (Some(2), Some(2)), "the wash trader no longer counts as a scoring wallet");

    // the window must close first
    let end = round["end_ms"].as_u64().unwrap();
    loop {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        if now > end + 3000 { break; }
        sleep(1000).await;
    }
    let sup_out = supply(&ctx, &outside).await?;
    let r = ctx.call(&ctx.arena, &ow[0], "settle_round", json!({"round_id": 1}), 0, 300).await?; // anyone can settle
    // (the outside token has no `burn`: that one receipt fails by design and the arena falls back to the graveyard)
    let logs: Vec<String> = r.logs().iter().map(|l| l.to_string()).collect();
    assert!(logs.iter().any(|l| l.contains("war_settled") && l.contains(outside.as_str()) && l.contains("\"loser\":\"pepe\"")), "{logs:?}");
    sleep(2000).await;
    assert!(ctx.ft_balance(&outside, &graveyard).await? > 0, "the winner's tokens were buried in the graveyard");
    assert_eq!(supply(&ctx, &outside).await?, sup_out, "an outside coin without burn: buried, not destroyed");
    assert_eq!(chest(&ctx, "pepe").await?, near(3.0), "the loser's chest paid exactly its 1 NEAR stake");
    assert_eq!(chest(&ctx, outside.as_str()).await?, near(2.0), "the winner's stake came back");
    ctx.fails(&ctx.arena, &carol, "settle_round", json!({"round_id": 1}), 0, 100, "already settled").await?;

    // ================= round 2: pepe wins; the outside coin's stake buys pepe and burns it for real
    let r = ctx.call(&ctx.arena, &carol, "start_round", json!({}), 100, 100).await?;
    assert!(r.failures().is_empty(), "start_round 2: {:?}", r.failures());
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 2})).await?;
    assert_eq!(s(&round["pairs"][0]["stake"]), near(1.0), "50% of the weaker chest (2 NEAR)");
    for w in &pw {
        ctx.call(&ctx.arena, w, "buy", json!({"key": "pepe", "min_out": "1"}), 1000, 300).await?.assert_success();
    }
    for w in &ow {
        ctx.call(&ctx.arena, w, "buy", json!({"key": outside, "min_out": "1"}), 300, 300).await?.assert_success();
    }
    sleep(1500).await;
    let end = round["end_ms"].as_u64().unwrap();
    loop {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        if now > end + 3000 { break; }
        sleep(1000).await;
    }
    let sup_pepe = supply(&ctx, &pepe).await?;
    let r = ctx.call(&ctx.arena, &pw[1], "settle_round", json!({"round_id": 2}), 0, 300).await?;
    assert!(r.failures().is_empty(), "settle 2: {:?}", r.failures());
    sleep(2500).await;
    assert!(supply(&ctx, &pepe).await? < sup_pepe, "the winner's supply really dropped");
    assert_eq!(ctx.ft_balance(&pepe, &graveyard).await?, 0, "a native coin is burned, not buried");
    assert_eq!(chest(&ctx, outside.as_str()).await?, near(1.0), "the loser (outside) paid its stake");
    assert_eq!(chest(&ctx, "pepe").await?, near(3.0));
    let f: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 200})).await?;
    let kinds: Vec<&str> = f.as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    for k in ["war_started", "war_settled", "treasury_buyback"] {
        assert!(kinds.contains(&k), "feed has {k}: {kinds:?}");
    }
    Ok(())
}
