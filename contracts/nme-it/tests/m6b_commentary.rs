//! Commentary: a war makes the coin's brain say a line (mock OutLayer, same result shape). Append-only, AI-labelled,
//! native coins only, silent below the treasury tier; it never touches a ledger.
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
async fn a_war_makes_the_native_coin_talk_and_the_outside_coin_stay_silent() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let bob = ctx.account("bob.sandbox", 100).await?;
    let poker = ctx.account("poker.sandbox", 50).await?;
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    let outside = ctx.account("outside.sandbox", 300).await?;
    let wasm = std::fs::read("../spikes/s12-launcher/target/near/s12_launcher.wasm")?;
    ctx.deploy(&outside, wasm, "new", json!({"dcl": ctx.dcl, "wrap": ctx.wrap, "supply": "1000000000000000000000000000"})).await?;
    ctx.call_yocto(&ctx.wrap, &carol, "storage_deposit", json!({"account_id": outside}), near(0.00125), 30).await?.assert_success();
    ctx.call(&outside, &carol, "launch", json!({"init_point": 92_000, "left": 92_200, "right": 132_200}), 1000, 300).await?.assert_success();
    ctx.ok(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": format!("{outside}|wrap.testnet|10000")}), 30, 150).await?;
    ctx.ok(&ctx.arena, &carol, "fund_protocol", json!({}), 1000, 30).await?;
    // fees from router buys give $PEPE a treasury above the brain tier
    ctx.call_yocto(&pepe, &bob, "storage_deposit", json!({"account_id": bob}), near(0.00125), 30).await?.assert_success();
    ctx.call(&ctx.arena, &bob, "buy", json!({"key": "pepe", "min_out": "1"}), 2000, 300).await?.assert_success();
    sleep(1500).await;
    let cfg: Value = ctx.view(&ctx.arena, "get_commentary_config", json!({})).await?;
    assert_eq!(cfg["per_day_per_coin"], 5);

    // a war starts (the chest funding that completes the pair starts it)
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 4000, 300).await?;
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": outside}), 2000, 300).await?;
    assert!(!ctx.view::<Value>(&ctx.arena, "get_round", json!({"id": 1})).await?.is_null(), "the war started");
    let protocol0 = s(&ctx.view::<Value>(&ctx.arena, "protocol_balance", json!({})).await?);
    let before: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;

    // ticks run the queued commentary task (one chain-starting task per tick)
    for _ in 0..6 {
        ctx.call(&ctx.arena, &poker, "tick", json!({"max_tasks": 5}), 0, 300).await?;
        sleep(2500).await;
        let c: Value = ctx.view(&ctx.arena, "get_commentary", json!({"limit": 5})).await?;
        if !c.as_array().unwrap().is_empty() {
            break;
        }
    }
    let c: Value = ctx.view(&ctx.arena, "get_commentary", json!({"limit": 10})).await?;
    let lines = c.as_array().unwrap();
    assert_eq!(lines.len(), 1, "one line, for the native coin only: {c}");
    assert_eq!(lines[0]["ticker"], "pepe");
    assert_eq!(lines[0]["event"], "war_started");
    assert_eq!(lines[0]["text"], "PEPE says:  is ngmi (job 0)", "the mock brain's line, stored as written");
    assert_eq!(lines[0]["chat_id"], "chat-0");

    // append-only: no ledger moved except the protocol bucket paying the fee and bounties
    let after: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    assert_eq!(after["chest_near"], before["chest_near"], "the chest is untouched");
    assert!(s(&after["treasury_near"]) >= s(&before["treasury_near"]), "the treasury only grows (router fees), never pays for talking");
    let protocol1 = s(&ctx.view::<Value>(&ctx.arena, "protocol_balance", json!({})).await?);
    assert!(protocol0 - protocol1 >= near(0.01), "the line cost one fee from the protocol bucket");
    let f: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 200})).await?;
    assert!(f.as_array().unwrap().iter().any(|i| i["kind"] == "commentary" && i["actor"] == "brain"), "the feed labels it as the brain's");
    Ok(())
}
