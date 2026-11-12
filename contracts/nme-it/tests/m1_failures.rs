mod common;
use common::*;
use near_sdk::serde_json::{json, Value};

async fn launch(ctx: &Ctx, coin: &near_api::AccountId) -> testresult::TestResult<Value> {
    ctx.view(coin, "get_launch", json!({})).await
}

#[tokio::test]
async fn pool_squatting_tier_skip_adopt_and_total_failure() -> testresult::TestResult<()> {
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let squatter = ctx.account("squatter.sandbox", 100).await?;
    ctx.dcl_register(&squatter).await?;
    let acct = |t: &str| -> near_api::AccountId { format!("{t}.{}", ctx.factory).parse().unwrap() };

    // 1) 1% tier squatted at the wrong price -> the launch moves to the 0.2% tier
    ctx.dcl_create_pool(&squatter, &acct("skip"), 10_000, 40_000).await?;
    let skip = ctx.create_coin(&alice, "skip", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&skip, &alice).await?;
    let l = launch(&ctx, &skip).await?;
    assert_eq!(l["Live"]["pool_id"], format!("{skip}|wrap.testnet|2000"), "tier 10000 was squatted: {l}");

    // 2) pre-created at exactly our start point, empty -> adopted, no create_pool needed
    ctx.dcl_create_pool(&squatter, &acct("adopt"), 10_000, 92_000).await?;
    let adopt = ctx.create_coin(&alice, "adopt", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&adopt, &alice).await?;
    let l = launch(&ctx, &adopt).await?;
    assert_eq!(l["Live"]["pool_id"], format!("{adopt}|wrap.testnet|10000"));
    let rows: Value = ctx.view(&ctx.dcl, "list_liquidities", json!({"account_id": adopt})).await?;
    assert_eq!(rows.as_array().unwrap().len(), 1, "the supply is in the adopted pool");

    // 3) all four tiers squatted at other prices -> the launch fails cleanly and the creator is refunded
    for fee in [10_000u32, 2_000, 400, 100] {
        ctx.dcl_create_pool(&squatter, &acct("block"), fee, 40_000).await?;
    }
    let before = ctx.balance(&alice).await?;
    let block = ctx.create_coin(&alice, "block", Ctx::launch_params(92_000, 40_000, None)).await?;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert_eq!(launch(&ctx, &block).await?, json!("Failed"));
    ctx.fails(&block, &alice, "launch", json!({}), 0, 30, "not launching").await?;
    ctx.fails(&block, &alice, "claim_fees", json!({}), 0, 30, "launch is not live").await?;
    let mid = ctx.balance(&alice).await?;
    ctx.ok(&block, &squatter, "refund_failed", json!({}), 0, 30).await?; // anyone triggers; the creator gets the NEAR
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let after = ctx.balance(&alice).await?;
    assert!(after - mid > near(0.55), "creator got the coin's spare NEAR back, incl. the reclaimed DCL registration (+{})", after - mid);
    assert!(before - after < near(0.45), "net cost of a failed launch is small (cost {})", before - after);
    ctx.fails(&block, &alice, "refund_failed", json!({}), 0, 30, "already refunded").await?;
    // the Crown/registration side is untouched by the failed launch
    let rec: Value = ctx.view(&ctx.factory, "get_coin", json!({"ticker": "block"})).await?;
    assert_eq!(rec["status"], "Live");
    Ok(())
}

#[tokio::test]
async fn launch_resumes_with_small_gas_calls() -> testresult::TestResult<()> {
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 100).await?;
    // A normal create_coin leaves the chain paused part-way (see m1_factory); finish it with small-gas calls.
    let coin = ctx.create_coin(&alice, "slow", Ctx::launch_params(92_000, 40_000, None)).await?;
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    let first = launch(&ctx, &coin).await?;
    println!("state after create_coin: {first}");
    assert!(first.get("Creating").is_some(), "this scenario needs a paused launch to exercise resume: {first}");
    let mut rounds = 0;
    while launch(&ctx, &coin).await?.get("Live").is_none() {
        rounds += 1;
        assert!(rounds <= 12, "launch() did not converge: {}", launch(&ctx, &coin).await?);
        let r = ctx.call(&coin, &carol, "launch", json!({}), 0, 70).await?; // small gas: pauses again and again
        assert!(r.failures().is_empty(), "launch() failed: {:?}", r.failures());
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }
    println!("finished after {rounds} small-gas launch() calls");
    let supply: String = ctx.view(&coin, "ft_total_supply", json!({})).await?;
    assert_eq!(supply.parse::<u128>()?, nme_common::TOTAL_SUPPLY, "no double mint across resumes");
    let rows: Value = ctx.view(&ctx.dcl, "list_liquidities", json!({"account_id": coin})).await?;
    assert_eq!(rows.as_array().unwrap().len(), 1, "exactly one position, never two");
    assert_eq!(ctx.ft_balance(&coin, &ctx.dcl).await?, supply_split_pool());
    Ok(())
}

fn supply_split_pool() -> u128 {
    nme_common::supply_split().pool.0
}

#[tokio::test]
async fn factory_rules_trust_graph_and_rollback() -> testresult::TestResult<()> {
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let mallory = ctx.account("mallory.sandbox", 100).await?;
    let good = Ctx::launch_params(92_000, 40_000, None);
    let need = ctx.required_milli(0).await?;
    let f = &ctx.factory;

    let mk = |ticker: &str, launch: Value, icon: &str| {
        let mut a = Ctx::create_args(ticker, launch);
        a["args"]["icon_data_url"] = json!(icon);
        a
    };
    // ---- validation failures
    ctx.fails(f, &alice, "create_coin", mk("arena", good.clone(), ICON), need, 300, "reserved").await?;
    ctx.fails(f, &alice, "create_coin", mk("burn", good.clone(), ICON), need, 300, "reserved").await?;
    ctx.fails(f, &alice, "create_coin", mk("a", good.clone(), ICON), need, 300, "ticker must be").await?;
    ctx.fails(f, &alice, "create_coin", mk("pe pe", good.clone(), ICON), need, 300, "may only contain").await?;
    ctx.fails(f, &alice, "create_coin", mk("-bad", good.clone(), ICON), need, 300, "start/end").await?;
    let big = format!("data:image/png;base64,{}", "A".repeat(25_000));
    ctx.fails(f, &alice, "create_coin", mk("bigicon", good.clone(), &big), need + 300, 300, "larger than 24 KB").await?;
    ctx.fails(f, &alice, "create_coin", mk("badicon", good.clone(), "http://evil/x.png"), need, 300, "data:image").await?;
    ctx.fails(f, &alice, "create_coin", mk("offgrid", Ctx::launch_params(92_100, 40_000, None), ICON), need, 300, "multiple of 200").await?;
    ctx.fails(f, &alice, "create_coin", mk("narrow", Ctx::launch_params(92_000, 200, None), ICON), need, 300, "range_points").await?;
    ctx.fails(f, &alice, "create_coin", mk("wide", Ctx::launch_params(92_000, 600_000, None), ICON), need, 300, "range_points").await?;
    ctx.fails(f, &alice, "create_coin", mk("greedy", Ctx::launch_params(92_000, 40_000, Some((nme_common::TOTAL_SUPPLY / 10, near(1.0)))), ICON), need + 1000, 300, "5% of supply").await?;
    let mut bad_persona = Ctx::create_args("mute", good.clone());
    bad_persona["args"]["persona"]["vibe"] = json!(" ");
    ctx.fails(f, &alice, "create_coin", bad_persona, need, 300, "persona").await?;
    ctx.fails(f, &alice, "create_coin", mk("cheap", good.clone(), ICON), need - 50, 300, "attach at least").await?;
    assert!(ctx.view::<bool>(f, "ticker_available", json!({"ticker": "cheap"})).await?, "failed attempts reserve nothing");

    // ---- success, then duplicate, then excess refunded
    ctx.create_coin(&alice, "dupe", good.clone()).await?;
    assert!(!ctx.view::<bool>(f, "ticker_available", json!({"ticker": "DUPE"})).await?);
    ctx.fails(f, &mallory, "create_coin", mk("dupe", good.clone(), ICON), need, 300, "ticker already taken").await?;
    let before = ctx.balance(&alice).await?;
    ctx.ok(f, &alice, "create_coin", mk("refundme", good.clone(), ICON), need + 5000, 300).await?;
    let spent = before - ctx.balance(&alice).await?;
    assert!(spent < (need + 1000) * 10u128.pow(21), "excess above the requirement comes back (spent {spent})");

    // ---- trust graph
    ctx.fails(&ctx.crown, &mallory, "nft_mint", json!({"token_id":"x","owner":mallory,"coin":"x.nme.sandbox","vault":"crown.x.nme.sandbox","inactivity_ms":1}), 20, 30, "only the factory can mint").await?;
    ctx.fails(&ctx.arena, &mallory, "register_coin", json!({"ticker":"x","account":mallory,"crown_vault":mallory,"colosseum_opt_in":true,"first_claim_due_ms":1}), 20, 30, "only the factory").await?;
    ctx.fails(f, &mallory, "retry_registration", json!({"ticker": "dupe"}), 0, 100, "not awaiting registration").await?;
    ctx.fails(f, &mallory, "on_coin_created", json!({"ticker":"dupe","creator":mallory,"paid":"1"}), 0, 30, "Method on_coin_created is private").await?;
    let dupe: near_api::AccountId = format!("dupe.{f}").parse()?;
    ctx.fails(&dupe, &mallory, "new", json!({}), 0, 30, "").await?;

    // ---- rollback: coin initialization fails -> ticker released, creator refunded, no half-created coin.
    // The factory is pointed at the *vault* code, whose `new` rejects the coin arguments (init failure).
    let bogus = near_api::CryptoHash::hash(&wasm("nme_crown_vault")).to_string();
    let ctx2 = Ctx::setup_with(Some(bogus)).await?;
    let carol = ctx2.account("carol.sandbox", 100).await?;
    let need2 = ctx2.required_milli(0).await?;
    let before = ctx2.balance(&carol).await?;
    let r = ctx2.call(&ctx2.factory, &carol, "create_coin", Ctx::create_args("ghost", good.clone()), need2, 300).await?;
    assert!(r.logs().iter().any(|l| l.contains("coin_create_failed")), "rollback event missing: {:?}", r.logs());
    assert!(ctx2.view::<bool>(&ctx2.factory, "ticker_available", json!({"ticker": "ghost"})).await?, "reservation released");
    assert!(ctx2.view::<Value>(&ctx2.factory, "get_coin", json!({"ticker": "ghost"})).await?.is_null());
    let lost = before - ctx2.balance(&carol).await?;
    assert!(lost < near(0.05), "creator gets everything back except gas (lost {lost})");
    let ghost: near_api::AccountId = format!("ghost.{}", ctx2.factory).parse()?;
    assert!(near_api::Account(ghost).view().fetch_from(&ctx2.net).await.is_err(), "no half-created coin account");
    assert_eq!(ctx2.view::<Value>(&ctx2.factory, "coin_count", json!({})).await?, json!(0));
    Ok(())
}
