//! The Crown vault driven through the real coin (an ordinary account plays the Crown collection until M4).
mod common;
use common::*;
use near_sdk::serde_json::{json, Value};
use nme_common::supply_split;

#[tokio::test]
async fn vault_release_fees_forfeit_and_forfeited_fee_routing() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_opts(None, Some("crownauth.sandbox")).await?;
    let auth = ctx.crown.clone();
    let alice = ctx.account("alice.sandbox", 100).await?;
    let bob = ctx.account("bob.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 100).await?;
    let crown_total = supply_split().crown.0;

    let coin = ctx.create_coin(&alice, "vlt", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &carol).await?;
    let vault: near_api::AccountId = format!("crown.{coin}").parse()?;
    let pool_id = ctx.view::<Value>(&coin, "get_launch", json!({})).await?["Live"]["pool_id"].as_str().unwrap().to_string();
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(v["authority"], auth.as_str());
    assert_eq!(v["ticker"], "vlt");

    // ---- only the Crown collection commands the vault
    ctx.fails(&vault, &alice, "release_vested", json!({"to": alice}), 0, 60, "only the Crown collection").await?;
    ctx.fails(&vault, &alice, "release_fees", json!({"to": alice}), 0, 60, "only the Crown collection").await?;
    ctx.fails(&vault, &alice, "forfeit", json!({}), 0, 60, "only the Crown collection").await?;
    ctx.fails(&vault, &auth, "release_vested", json!({"to": alice}), 0, 60, "nothing vested").await?; // before the cliff

    // ---- fees accrue from trading: trade, claim, the Crown share lands in the vault
    ctx.dcl_buy(&bob, &coin, &pool_id, near(2.0)).await?;
    let bought = ctx.ft_balance(&coin, &bob).await?;
    ctx.dcl_sell(&bob, &coin, &pool_id, bought / 2).await?;
    ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?.assert_success();
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    let fees = s(&v["fees_near"]);
    assert!(fees > 0, "the vault accrued the Crown's share of pool fees");

    // ---- past the cliff: release what has vested (receiver already registered: attach nothing)
    ctx.fast_forward(450).await?; // ~2 minutes: cliff (1 min) passed, ~20% of a 10-minute vest
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    let claimable_view = s(&v["claimable_now"]);
    assert!(claimable_view > 0, "something vested after the cliff");
    let before = ctx.ft_balance(&coin, &alice).await?;
    ctx.call(&vault, &auth, "release_vested", json!({"to": alice}), 0, 100).await?.assert_success();
    let gained = ctx.ft_balance(&coin, &alice).await? - before;
    assert!(gained >= claimable_view && gained < crown_total / 2, "alice got the vested part ({gained} >= {claimable_view})");
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(s(&v["released"]), gained);
    assert_eq!(ctx.ft_balance(&coin, &vault).await?, crown_total - gained, "vault tokens = total - released");

    // ---- release a receiver that is NOT registered on the coin: attach the NEP-145 minimum
    let dave = ctx.account("dave.sandbox", 10).await?;
    ctx.fast_forward(200).await?;
    ctx.call(&vault, &auth, "release_vested", json!({"to": dave}), 2, 100).await?.assert_success(); // 0.002 NEAR attached
    assert!(ctx.ft_balance(&coin, &dave).await? > 0, "an unregistered receiver was registered and paid");

    // ---- release the accrued fees (NEAR) to the owner
    let bal = ctx.balance(&alice).await?;
    ctx.call(&vault, &auth, "release_fees", json!({"to": alice}), 0, 60).await?.assert_success();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(ctx.balance(&alice).await? - bal, fees, "alice received exactly the accrued fees");
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(s(&v["fees_near"]), 0);
    ctx.fails(&vault, &auth, "release_fees", json!({"to": alice}), 0, 60, "no fees accrued").await?;

    // ---- forfeit: only the unvested part is burned, vesting freezes, vested tokens stay claimable
    let supply_before: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    ctx.call(&vault, &auth, "forfeit", json!({}), 0, 100).await?.assert_success();
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(v["forfeited"], true);
    let supply_after: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    let burned = supply_before - supply_after;
    let vested_at_forfeit = s(&v["vested_now"]);
    assert_eq!(burned, crown_total - vested_at_forfeit, "burned exactly the unvested part");
    assert_eq!(ctx.ft_balance(&coin, &vault).await?, vested_at_forfeit - s(&v["released"]), "the vested-unreleased tokens are still there");
    ctx.fails(&vault, &auth, "forfeit", json!({}), 0, 60, "already forfeited").await?;
    ctx.fast_forward(300).await?;
    let v2: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(s(&v2["vested_now"]), vested_at_forfeit, "vesting is frozen after a forfeit");

    // ---- after a forfeit the Crown's fees go to the coin treasury in the arena, whoever `to` is
    ctx.dcl_buy(&bob, &coin, &pool_id, near(2.0)).await?;
    let bb = ctx.ft_balance(&coin, &bob).await?;
    ctx.dcl_sell(&bob, &coin, &pool_id, bb / 2).await?;
    ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?.assert_success();
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    let new_fees = s(&v["fees_near"]);
    assert!(new_fees > 0);
    let t0 = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "vlt"})).await?["treasury_near"]);
    let alice_bal = ctx.balance(&alice).await?;
    ctx.call(&vault, &auth, "release_fees", json!({"to": alice}), 0, 100).await?.assert_success();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let t1 = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "vlt"})).await?["treasury_near"]);
    assert_eq!(t1 - t0, new_fees, "forfeited Crown fees went to the coin treasury");
    assert!(ctx.balance(&alice).await? <= alice_bal, "and not to the former owner");
    let af: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 60})).await?;
    assert!(af.as_array().unwrap().iter().any(|i| i["kind"] == "fees_forfeited"), "feed shows fees_forfeited");

    // ---- the already-vested tokens are still releasable
    let rel_before = ctx.ft_balance(&coin, &alice).await?;
    ctx.call(&vault, &auth, "release_vested", json!({"to": alice}), 0, 100).await?.assert_success();
    assert!(ctx.ft_balance(&coin, &alice).await? > rel_before, "vested-before-forfeit tokens are still claimable");
    Ok(())
}
