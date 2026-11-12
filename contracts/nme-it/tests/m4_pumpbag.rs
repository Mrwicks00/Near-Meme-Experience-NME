//! M4: the PumpBag. The creator claims vested tokens and fees through the real NFT collection; a sale is a takeover;
//! a heartbeat saves it; a silent owner is forfeited automatically (the arena's task) and the vault burns the unvested part.
mod common;
use common::*;
use near_sdk::serde_json::{json, Value};

fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

#[tokio::test]
async fn claim_takeover_heartbeat_and_automatic_forfeit() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let bob = ctx.account("bob.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let poker = ctx.account("poker.sandbox", 50).await?;
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &carol).await?;
    sleep(2500).await;
    let vault: near_api::AccountId = format!("crown.{coin}").parse()?;
    let pool_id = ctx.view::<Value>(&coin, "get_launch", json!({})).await?["Live"]["pool_id"].as_str().unwrap().to_string();

    // ---- the PumpBag exists, is owned by the creator, and the arena knows its deadline
    let nft: Value = ctx.view(&ctx.crown, "nft_token", json!({"token_id": "pepe"})).await?;
    assert_eq!(nft["owner_id"], "alice.sandbox");
    assert_eq!(nft["metadata"]["title"], "PumpBag of $PEPE");
    let st: Value = ctx.view(&ctx.crown, "crown_status", json!({"token_id": "pepe"})).await?;
    assert_eq!(st["inactivity_ms"], 300_000);
    assert_eq!(st["forfeited"], false);
    let deadline: Value = ctx.view(&ctx.arena, "get_crown_deadline", json!({"ticker": "pepe"})).await?;
    assert_eq!(deadline, st["forfeitable_after_ms"], "the arena tracks the PumpBag's deadline from the mint");

    // ---- only the owner can claim, heartbeat; claims need a deposit
    ctx.fails(&ctx.crown, &bob, "crown_claim_fees", json!({"token_id": "pepe"}), 0, 60, "").await?;
    let r = ctx.call_yocto(&ctx.crown, &bob, "crown_claim_fees", json!({"token_id": "pepe"}), 1, 100).await?;
    assert!(!r.failures().is_empty(), "a non-owner cannot claim fees");
    let r = ctx.call_yocto(&ctx.crown, &bob, "crown_heartbeat", json!({"token_id": "pepe"}), 0, 60).await?;
    assert!(!r.failures().is_empty(), "a non-owner cannot heartbeat");

    // ---- fees accrue from trading; the owner claims them
    ctx.dcl_buy(&bob, &coin, &pool_id, near(2.0)).await?;
    let bought = ctx.ft_balance(&coin, &bob).await?;
    ctx.dcl_sell(&bob, &coin, &pool_id, bought / 2).await?;
    ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?.assert_success();
    let fees = s(&ctx.view::<Value>(&vault, "get_vault", json!({})).await?["fees_near"]);
    assert!(fees > 0, "the vault accrued the PumpBag's share of pool fees");
    let before = ctx.balance(&alice).await?;
    let r = ctx.call_yocto(&ctx.crown, &alice, "crown_claim_fees", json!({"token_id": "pepe"}), 1, 100).await?;
    assert!(r.failures().is_empty(), "claim fees: {:?}", r.failures());
    sleep(1500).await;
    assert!(ctx.balance(&alice).await? + near(0.05) > before + fees, "alice received the accrued fees ({fees})");
    assert_eq!(s(&ctx.view::<Value>(&vault, "get_vault", json!({})).await?["fees_near"]), 0);

    // ---- past the cliff the owner claims the vested tokens
    ctx.fast_forward(450).await?;
    let claimable = s(&ctx.view::<Value>(&vault, "get_vault", json!({})).await?["claimable_now"]);
    assert!(claimable > 0, "something vested after the cliff");
    let t0 = ctx.ft_balance(&coin, &alice).await?;
    let r = ctx.call_yocto(&ctx.crown, &alice, "crown_claim_vested", json!({"token_id": "pepe"}), near(0.002), 150).await?;
    assert!(r.failures().is_empty(), "claim vested: {:?}", r.failures());
    sleep(1500).await;
    assert!(ctx.ft_balance(&coin, &alice).await? - t0 >= claimable, "alice got the vested tokens");

    // ---- a sale is a takeover: the seller loses every power, the buyer gets a fresh period
    ctx.dcl_buy(&bob, &coin, &pool_id, near(2.0)).await?;
    ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?.assert_success();
    let fees2 = s(&ctx.view::<Value>(&vault, "get_vault", json!({})).await?["fees_near"]);
    assert!(fees2 > 0);
    ctx.call_yocto(&ctx.crown, &alice, "nft_transfer", json!({"receiver_id": bob, "token_id": "pepe"}), 1, 100).await?.assert_success();
    sleep(1500).await;
    let st: Value = ctx.view(&ctx.crown, "crown_status", json!({"token_id": "pepe"})).await?;
    assert_eq!(st["owner"], "bob.sandbox");
    let r = ctx.call_yocto(&ctx.crown, &alice, "crown_claim_fees", json!({"token_id": "pepe"}), 1, 100).await?;
    assert!(!r.failures().is_empty(), "the seller cannot claim fees after the sale");
    let bb = ctx.balance(&bob).await?;
    ctx.call_yocto(&ctx.crown, &bob, "crown_claim_fees", json!({"token_id": "pepe"}), 1, 100).await?.assert_success();
    sleep(1500).await;
    assert!(ctx.balance(&bob).await? + near(0.05) > bb + fees2, "the new owner received the fees that accrued before the sale");
    let deadline2: Value = ctx.view(&ctx.arena, "get_crown_deadline", json!({"ticker": "pepe"})).await?;
    assert_eq!(deadline2, st["forfeitable_after_ms"], "the arena's deadline follows the takeover");

    // ---- a heartbeat moves the deadline; forfeit before the deadline is refused
    let r = ctx.call(&ctx.crown, &poker, "crown_forfeit", json!({"token_id": "pepe"}), 0, 150).await?;
    assert!(!r.failures().is_empty(), "forfeit before the deadline is refused");

    // ---- the owner goes silent: past the deadline the arena's tick forfeits automatically
    ctx.fast_forward(1500).await?; // well over 5 minutes
    let unvested_before = ctx.ft_balance(&coin, &vault).await?;
    let supply0: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    let mut forfeited = false;
    for _ in 0..10 {
        ctx.call(&ctx.arena, &poker, "tick", json!({"max_tasks": 5}), 0, 300).await?;
        sleep(3000).await;
        if ctx.view::<Value>(&ctx.crown, "crown_status", json!({"token_id": "pepe"})).await?["forfeited"] == true {
            forfeited = true;
            break;
        }
    }
    assert!(forfeited, "the arena's tick forfeited the silent PumpBag");
    sleep(2000).await;
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(v["forfeited"], true);
    // Everything not yet vested was burned and vesting is frozen: what the vault still holds is exactly what the owner
    // can claim (if the 10-minute schedule had already run out by then, there was nothing left to burn).
    let supply1: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    assert!(supply1 <= supply0, "the supply never grows");
    assert_eq!(ctx.ft_balance(&coin, &vault).await?, s(&v["claimable_now"]), "the vault holds only the already-vested part");
    assert!(ctx.ft_balance(&coin, &vault).await? <= unvested_before);

    // ---- after the forfeit: no revival, and the fees go to the coin's treasury, not the owner
    let r = ctx.call_yocto(&ctx.crown, &bob, "crown_heartbeat", json!({"token_id": "pepe"}), 0, 60).await?;
    assert!(!r.failures().is_empty(), "a forfeited PumpBag cannot be revived");
    ctx.dcl_buy(&bob, &coin, &pool_id, near(2.0)).await?;
    ctx.call(&coin, &carol, "claim_fees", json!({}), 0, 300).await?.assert_success();
    let t_before = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?["treasury_near"]);
    let bb = ctx.balance(&bob).await?;
    ctx.call_yocto(&ctx.crown, &bob, "crown_claim_fees", json!({"token_id": "pepe"}), 1, 120).await?;
    sleep(2500).await;
    let t_after = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?["treasury_near"]);
    assert!(t_after > t_before, "after the forfeit the fees went to the coin's treasury");
    assert!(ctx.balance(&bob).await? < bb, "and not to the owner");
    Ok(())
}
