mod common;
use common::*;
use near_sdk::serde_json::{json, Value};
use nme_common::math::bps_of;
use nme_common::{supply_split, TOTAL_SUPPLY};

const TOKEN: u128 = 10u128.pow(18);

#[tokio::test]
async fn launch_first_buy_trade_fees_and_burn() -> testresult::TestResult<()> {
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let bob = ctx.account("bob.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 100).await?;
    let split = supply_split();

    // 1 token ~ 0.0099 NEAR at point 92_000, so 1000 tokens ~ 9.9 NEAR; pay at most 12.
    let first_buy_tokens = 1_000 * TOKEN;
    let creator_before = ctx.balance(&alice).await?;
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, Some((first_buy_tokens, near(12.0))))).await?;
    ctx.finish_launch(&coin, &carol).await?;
    let launch: Value = ctx.view(&coin, "get_launch", json!({})).await?;
    let pool_id = launch["Live"]["pool_id"].as_str().unwrap().to_string();
    let lpt_id = launch["Live"]["lpt_id"].as_str().unwrap().to_string();

    // ---- position: the whole pool side, single-sided above the start point, owned by the coin
    let rows: Value = ctx.view(&ctx.dcl, "list_liquidities", json!({"account_id": coin})).await?;
    assert_eq!(rows.as_array().unwrap().len(), 1, "exactly one position");
    assert_eq!(rows[0]["lpt_id"], lpt_id.as_str());
    assert_eq!((rows[0]["left_point"].as_i64().unwrap(), rows[0]["right_point"].as_i64().unwrap()), (92_200, 132_200));
    let principal = s(&rows[0]["amount"]);
    let pool: Value = ctx.view(&ctx.dcl, "get_pool", json!({"pool_id": pool_id})).await?;
    assert_eq!(pool["fee"], 10_000);
    assert!(pool["current_point"].as_i64().unwrap() >= 92_000, "first buy moved the price up");
    assert_eq!(ctx.ft_balance(&coin, &coin).await?, 0);
    assert_eq!(ctx.ft_balance(&coin, &ctx.dcl).await?, split.pool.0 - first_buy_tokens, "DCL holds the pool side minus the first buy");

    // ---- first buy: exactly the capped token amount to the creator, unused NEAR back
    assert_eq!(ctx.ft_balance(&coin, &alice).await?, first_buy_tokens, "creator got exactly the first-buy tokens");
    let spent = creator_before - ctx.balance(&alice).await?;
    let required = ctx.required_milli(near(12.0)).await? * 10u128.pow(21);
    assert!(spent < required, "unused first-buy NEAR came back (spent {spent}, deposit {required})");
    let supply: String = ctx.view(&coin, "ft_total_supply", json!({})).await?;
    assert_eq!(supply.parse::<u128>()?, TOTAL_SUPPLY, "supply fixed: nothing minted after creation");

    // ---- DCL registration NEAR was reclaimed (S12): ~0.11 stays locked, not 0.5
    let sb: Value = ctx.view(&ctx.dcl, "storage_balance_of", json!({"account_id": coin})).await?;
    assert!(s(&sb["total"]) < near(0.2), "registration NEAR reclaimed: {sb}");

    // ---- I1: anyone trades straight through DCL; plain transfers work
    ctx.dcl_buy(&bob, &coin, &pool_id, near(0.5)).await?;
    let bob_tokens = ctx.ft_balance(&coin, &bob).await?;
    assert!(bob_tokens > 0, "bob bought through DCL with no NME contract involved");
    ctx.dcl_sell(&bob, &coin, &pool_id, bob_tokens / 2).await?;
    ctx.call_yocto(&coin, &bob, "ft_transfer", json!({"receiver_id": alice, "amount": "5000"}), 1, 30).await?.assert_success();
    assert_eq!(ctx.ft_balance(&coin, &alice).await?, first_buy_tokens + 5000);

    // ---- fee claim by anyone: token side burned, NEAR side split Crown / treasury / protocol
    let rows: Value = ctx.view(&ctx.dcl, "list_liquidities", json!({"account_id": coin})).await?;
    let (fx, fy) = (s(&rows[0]["unclaimed_fee_x"]), s(&rows[0]["unclaimed_fee_y"]));
    assert!(fx > 0 && fy > 0, "both fee sides accrued (x={fx}, y={fy})");
    let supply_before: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    let r = ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?;
    assert!(r.failures().is_empty(), "claim_fees failed: {:?}", r.failures());
    let supply_after: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    assert_eq!(supply_before - supply_after, fx, "token-side fees were burned (D-31)");
    let rows: Value = ctx.view(&ctx.dcl, "list_liquidities", json!({"account_id": coin})).await?;
    assert_eq!(s(&rows[0]["amount"]), principal, "claiming fees never touches the principal");
    assert_eq!((s(&rows[0]["unclaimed_fee_x"]), s(&rows[0]["unclaimed_fee_y"])), (0, 0));

    let stats: Value = ctx.view(&coin, "get_fee_stats", json!({})).await?;
    assert_eq!(s(&stats["fees_claimed_near"]), fy);
    assert_eq!(s(&stats["pending_fee_near"]), 0, "all NEAR fees were forwarded");
    let crown = bps_of(fy, 4000);
    let treasury = bps_of(fy, 4000);
    let protocol = fy - crown - treasury;
    let entry: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    assert_eq!(s(&entry["treasury_near"]), treasury, "40% of NEAR fees to the coin treasury");
    let acfg: Value = ctx.view(&ctx.arena, "get_config", json!({})).await?;
    assert_eq!(s(&acfg["protocol_near"]), protocol, "the rest to the protocol bucket");
    let vault: near_api::AccountId = format!("crown.{coin}").parse()?;
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(s(&v["fees_near"]), crown, "40% to the Crown vault");
    let af: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 30})).await?;
    assert!(af.as_array().unwrap().iter().any(|i| i["kind"] == "fees_claimed"), "fees_claimed in the feed");

    // claiming again with nothing accrued is harmless
    let r = ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?;
    assert!(r.failures().is_empty());
    let stats: Value = ctx.view(&coin, "get_fee_stats", json!({})).await?;
    assert_eq!(s(&stats["fees_claimed_near"]), fy, "no new fees, no new accounting");

    // ---- burn: own tokens only, needs 1 yocto, bounded by balance
    ctx.fails(&coin, &bob, "burn", json!({"amount": "1000"}), 0, 30, "Requires attached deposit of exactly 1 yoctoNEAR").await?;
    let bal = ctx.ft_balance(&coin, &bob).await?;
    let r = ctx.call_yocto(&coin, &bob, "burn", json!({"amount": (bal + 1).to_string()}), 1, 30).await?;
    assert!(!r.failures().is_empty(), "burning more than the balance must fail");
    let supply_before: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    ctx.call_yocto(&coin, &bob, "burn", json!({"amount": "1000"}), 1, 30).await?.assert_success();
    let supply_after: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    assert_eq!(supply_before - supply_after, 1000);

    // ---- states and trust
    ctx.fails(&coin, &carol, "launch", json!({}), 0, 30, "not launching").await?;
    ctx.fails(&coin, &carol, "refund_failed", json!({}), 0, 30, "launch did not fail").await?;
    ctx.fails(&coin, &carol, "on_step", json!({"step": "Vault"}), 0, 30, "Method on_step is private").await?;
    ctx.fails(&ctx.arena, &carol, "record_launch", json!({"ticker": "pepe", "pool_id": "x", "lpt_id": "y"}), 0, 30, "only the coin").await?;
    ctx.fails(&ctx.arena, &carol, "deposit_pool_fees", json!({"ticker": "pepe"}), 1000, 30, "only the coin").await?;
    ctx.fails(&vault, &carol, "deposit_fees", json!({}), 1000, 30, "only the arena").await?;
    ctx.fails(&coin, &carol, "new", json!({}), 0, 30, "").await?;
    // the arena exposes no way to take treasury or protocol NEAR out
    ctx.fails(&ctx.arena, &alice, "withdraw_treasury", json!({}), 0, 10, "MethodNotFound").await?;
    Ok(())
}

#[tokio::test]
async fn launch_with_coin_as_token_y() -> testresult::TestResult<()> {
    // "zeta" sorts after wrap.testnet, so it is token Y: the range sits BELOW the start point.
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let coin = ctx.create_coin(&alice, "zeta", Ctx::launch_params(-92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &alice).await?;
    let launch: Value = ctx.view(&coin, "get_launch", json!({})).await?;
    let pool_id = launch["Live"]["pool_id"].as_str().unwrap();
    assert_eq!(pool_id, format!("wrap.testnet|{coin}|10000"));
    let rows: Value = ctx.view(&ctx.dcl, "list_liquidities", json!({"account_id": coin})).await?;
    assert_eq!((rows[0]["left_point"].as_i64().unwrap(), rows[0]["right_point"].as_i64().unwrap()), (-132_200, -92_200));
    assert_eq!(ctx.ft_balance(&coin, &ctx.dcl).await?, supply_split().pool.0);
    let p: Value = ctx.view(&coin, "get_pool", json!({})).await?;
    assert_eq!(p["coin_is_x"], false);
    // a buyer can trade it and fees accrue on the NEAR (X) side
    let bob = ctx.account("bob.sandbox", 100).await?;
    ctx.dcl_buy(&bob, &coin, pool_id, near(1.0)).await?;
    assert!(ctx.ft_balance(&coin, &bob).await? > 0);
    Ok(())
}
