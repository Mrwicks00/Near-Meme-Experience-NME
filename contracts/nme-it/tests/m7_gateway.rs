//! M7 (Solana devnet gateway): a verified SOL deposit becomes a NEAR buy / chest funding for a NEAR beneficiary.
//! OutLayer and the MPC contract are the sandbox mock (same result shapes as the real ones: the real verifier worker is
//! covered by its own tests and a live OutLayer run).
mod common;
use common::*;
use near_sdk::serde_json::{json, Value};

fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

fn tx(tag: &str) -> String {
    format!("{tag}{}", "A".repeat(70))
}

#[tokio::test]
async fn solana_deposits_buy_fund_and_every_failure_pays_nothing() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let gw: near_api::AccountId = "gateway.sandbox".parse()?;
    let outlayer: near_api::AccountId = "outlayer.sandbox".parse()?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let relayer = ctx.account("relayer.sandbox", 50).await?;
    let sol_user = ctx.account("soluser.sandbox", 50).await?; // the beneficiary: a NEAR account that ape'd with SOL
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &carol).await?;
    ctx.call_yocto(&coin, &sol_user, "storage_deposit", json!({"account_id": sol_user}), near(0.00125), 30).await?.assert_success();
    sleep(1500).await;

    // ---- the deposit address belongs to (beneficiary, intent)
    let buy = json!({"Buy": {"ticker": "pepe", "min_out": "1"}});
    let p1: String = ctx.view(&gw, "deposit_path", json!({"beneficiary": sol_user, "intent": buy})).await?;
    let p2: String = ctx.view(&gw, "deposit_path", json!({"beneficiary": alice, "intent": buy})).await?;
    assert!(p1.starts_with("sol:soluser.sandbox:") && p1 != p2, "another beneficiary has another address");

    // ---- no reserve yet: a verified deposit cannot be paid and is rejected, nothing moves
    ctx.call(&outlayer, &carol, "set_deposit", json!({"tx_id": tx("ok1"), "near_yocto": near(5.0).to_string()}), 0, 30).await?.assert_success();
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("ok1"), "beneficiary": sol_user, "intent": buy}), 0, 300).await?.assert_success();
    sleep(4000).await;
    let c: Value = ctx.view(&gw, "get_claim", json!({"tx_id": tx("ok1")})).await?;
    assert!(c["Rejected"]["reason"].as_str().unwrap().contains("reserve"), "{c}");

    // ---- fund the reserve (inflow only) and claim again: the beneficiary gets tokens, the war credit and the reserve pays
    ctx.ok(&gw, &carol, "fund_reserve", json!({}), 20_000, 30).await?;
    let before_tokens = ctx.ft_balance(&coin, &sol_user).await?;
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("ok1"), "beneficiary": sol_user, "intent": buy}), 0, 300).await?.assert_success();
    sleep(5000).await;
    let c: Value = ctx.view(&gw, "get_claim", json!({"tx_id": tx("ok1")})).await?;
    assert!(c.get("Verified").is_some(), "verified, waiting for its own transaction to be spent: {c}");
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_reserve", json!({})).await?), near(20.0) - s(&c["Verified"]["near_pay"]), "the value is set aside at once");
    ctx.fails(&gw, &relayer, "execute_claim", json!({"tx_id": tx("nope")}), 0, 100, "no verified deposit").await?;
    ctx.call(&gw, &relayer, "execute_claim", json!({"tx_id": tx("ok1")}), 0, 300).await?.assert_success(); // anyone may spend it, for the beneficiary
    sleep(6000).await;
    let c: Value = ctx.view(&gw, "get_claim", json!({"tx_id": tx("ok1")})).await?;
    let paid = s(&c["Done"]["near_paid"]);
    assert_eq!(paid, near(5.0) - near(5.0) / 10_000 * 200, "the 2% spread was kept: {c}");
    assert!(ctx.ft_balance(&coin, &sol_user).await? > before_tokens, "the beneficiary received the tokens");
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_reserve", json!({})).await?), near(20.0) - paid);
    // the relayer, who sent the claim, got nothing
    assert_eq!(ctx.ft_balance(&coin, &relayer).await.unwrap_or(0), 0);
    // a paid deposit is final
    ctx.fails(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("ok1"), "beneficiary": sol_user, "intent": buy}), 0, 300, "already paid out").await?;

    // ---- fund a chest for a coin from SOL
    ctx.call(&outlayer, &carol, "set_deposit", json!({"tx_id": tx("ok2"), "near_yocto": near(3.0).to_string()}), 0, 30).await?.assert_success();
    let chest_intent = json!({"FundChest": {"ticker": "pepe"}});
    let chest0 = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?["chest_near"]);
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("ok2"), "beneficiary": sol_user, "intent": chest_intent}), 0, 300).await?.assert_success();
    sleep(5000).await;
    ctx.call(&gw, &relayer, "execute_claim", json!({"tx_id": tx("ok2")}), 0, 300).await?.assert_success();
    sleep(4000).await;
    let chest1 = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?["chest_near"]);
    assert_eq!(chest1 - chest0, near(3.0) - near(3.0) / 10_000 * 200, "the SOL value went into the war chest");

    // ---- failure branches pay nothing and leave the reserve alone
    let reserve = s(&ctx.view::<Value>(&gw, "get_reserve", json!({})).await?);
    // a transaction the verifier does not know
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("unknown"), "beneficiary": sol_user, "intent": buy}), 0, 300).await?.assert_success();
    // the verifier echoes a different address
    ctx.call(&outlayer, &carol, "set_deposit", json!({"tx_id": tx("wrongto"), "near_yocto": near(5.0).to_string()}), 0, 30).await?.assert_success();
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("wrongto"), "beneficiary": sol_user, "intent": buy}), 0, 300).await?.assert_success();
    // dust
    ctx.call(&outlayer, &carol, "set_deposit", json!({"tx_id": tx("dust"), "near_yocto": near(0.001).to_string()}), 0, 30).await?.assert_success();
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("dust"), "beneficiary": sol_user, "intent": buy}), 0, 300).await?.assert_success();
    // an unlaunched / unknown coin: the arena refuses the call itself -> the reserve is restored
    ctx.call(&outlayer, &carol, "set_deposit", json!({"tx_id": tx("ghost"), "near_yocto": near(5.0).to_string()}), 0, 30).await?.assert_success();
    ctx.call(&gw, &relayer, "claim_deposit", json!({"tx_id": tx("ghost"), "beneficiary": sol_user, "intent": {"Buy": {"ticker": "nosuchcoin", "min_out": "1"}}}), 0, 300).await?.assert_success();
    sleep(7000).await;
    // the ghost deposit verified fine; spending it is refused by the arena, which restores the reserve
    ctx.call(&gw, &relayer, "execute_claim", json!({"tx_id": tx("ghost")}), 0, 300).await?.assert_success();
    sleep(4000).await;
    for (t, why) in [("unknown", "not found"), ("wrongto", "different transaction or address"), ("dust", "minimum claim"), ("ghost", "arena refused")] {
        let c: Value = ctx.view(&gw, "get_claim", json!({"tx_id": tx(t)})).await?;
        assert!(c["Rejected"]["reason"].as_str().unwrap_or("").contains(why), "{t}: {c}");
    }
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_reserve", json!({})).await?), reserve, "no failed claim spent a yocto of the reserve");

    // ---- only the gateway may use buy_for; nobody can buy for a victim
    let r = ctx.call(&ctx.arena, &relayer, "buy_for", json!({"beneficiary": alice, "key": "pepe", "min_out": "1"}), 1000, 100).await?;
    assert!(!r.failures().is_empty(), "buy_for is gateway-only");
    // a bad tx id never reaches the verifier
    ctx.fails(&gw, &relayer, "claim_deposit", json!({"tx_id": "short", "beneficiary": sol_user, "intent": buy}), 0, 100, "tx_id must be a Solana signature").await?;
    Ok(())
}
