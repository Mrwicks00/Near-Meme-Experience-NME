//! M8: the machine moves itself. Nobody calls `start_round` or `settle_round`: an inline tick (a chest funding) starts
//! the war, a plain `tick` settles it and pays a bounty from the protocol bucket, and the next round is queued.
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

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

async fn due_kinds(ctx: &Ctx) -> testresult::TestResult<Vec<String>> {
    let d: Value = ctx.view(&ctx.arena, "due_tasks", json!({"limit": 50})).await?;
    Ok(d.as_array().unwrap().iter().map(|t| t["task"].to_string()).collect())
}

#[tokio::test]
async fn tick_runs_the_whole_war_cycle_without_a_human() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let poker = ctx.account("poker.sandbox", 50).await?;
    let graveyard = id(&format!("burn.{}", ctx.factory));

    // fighters
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    let outside = ctx.account("outside.sandbox", 300).await?;
    let wasm = std::fs::read("../spikes/s12-launcher/target/near/s12_launcher.wasm")?;
    ctx.deploy(&outside, wasm, "new", json!({"dcl": ctx.dcl, "wrap": ctx.wrap, "supply": "1000000000000000000000000000"})).await?;
    ctx.call_yocto(&ctx.wrap, &carol, "storage_deposit", json!({"account_id": outside}), near(0.00125), 30).await?.assert_success();
    ctx.call(&outside, &carol, "launch", json!({"init_point": 92_000, "left": 92_200, "right": 132_200}), 1000, 300).await?.assert_success();
    ctx.ok(&ctx.arena, &carol, "enlist", json!({"token": outside, "pool_id": format!("{outside}|wrap.testnet|10000")}), 30, 150).await?;
    sleep(1500).await;

    // the queue already holds the machine's first steps with no human involved
    let kinds = due_kinds(&ctx).await?;
    assert!(kinds.iter().any(|k| k.contains("StartRound")), "StartRound is queued from init: {kinds:?}");
    assert!(kinds.iter().any(|k| k.contains("ClaimFees")), "the fee claim is queued: {kinds:?}");
    assert!(kinds.iter().any(|k| k.contains("ResumeLaunch")), "the launch watchdog is queued: {kinds:?}");

    // fund the protocol bucket (bounties + storage), wallets and chests
    ctx.ok(&ctx.arena, &carol, "fund_protocol", json!({}), 1000, 30).await?;
    let bal: Value = ctx.view(&ctx.arena, "protocol_balance", json!({})).await?;
    assert!(s(&bal) >= near(1.0));
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
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 4000, 300).await?;
    assert!(ctx.view::<Value>(&ctx.arena, "get_round", json!({"id": 1})).await?.is_null(), "one funded coin: no round yet");
    // the funding that completes the pair starts the war by itself (inline start); nobody calls start_round
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": outside}), 2000, 300).await?;
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    assert!(!round.is_null(), "the war started on the funding that completed the pair");
    let q = due_kinds(&ctx).await?;
    assert!(q.iter().any(|k| k.contains("SettleRound")), "the settlement was queued by the start: {q:?}");
    assert_eq!(q.iter().filter(|k| k.contains("StartRound")).count(), 1, "one polling StartRound stays queued: {q:?}");

    for w in &pw {
        ctx.call(&ctx.arena, w, "buy", json!({"key": "pepe", "min_out": "1"}), 1000, 300).await?.assert_success();
    }
    for w in &ow {
        ctx.call(&ctx.arena, w, "buy", json!({"key": outside, "min_out": "1"}), 300, 300).await?.assert_success();
    }
    sleep(1500).await;

    // a tick before the end does nothing for the war and pays nothing
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    let end = round["end_ms"].as_u64().unwrap();
    let r = ctx.call(&ctx.arena, &poker, "tick", json!({"max_tasks": 5}), 0, 300).await?;
    assert!(r.failures().is_empty(), "tick: {:?}", r.failures());
    assert!(ctx.view::<Value>(&ctx.arena, "get_round", json!({"id": 1})).await?["settled_pairs"] == 0, "still running");

    loop {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as u64;
        if now > end + 2000 { break; }
        sleep(1000).await;
    }
    let sup = ctx.view::<String>(&pepe, "ft_total_supply", json!({})).await?.parse::<u128>()?;
    let protocol_before = s(&ctx.view::<Value>(&ctx.arena, "protocol_balance", json!({})).await?);
    // One tick runs one heavy task (300 TGas), and the queue also holds fee claims and defense checks: poke until the war is settled.
    let mut logs: Vec<String> = vec![];
    for _ in 0..8 {
        let r = ctx.call(&ctx.arena, &poker, "tick", json!({"max_tasks": 5}), 0, 300).await?;
        logs.extend(r.logs().iter().map(|l| l.to_string()));
        sleep(3000).await;
        let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
        if round["settled_pairs"] == 1 {
            break;
        }
    }
    assert!(logs.iter().any(|l| l.contains("\"event\":\"tick\"") && l.contains("bounty")), "tick event: {logs:?}");
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    assert_eq!(round["settled_pairs"], 1, "the ticks settled the war");
    sleep(4000).await; // the buy-and-burn chain started by the settlement finishes
    let winner = round["pairs"][0]["winner"].as_str().unwrap().to_string();
    // pepe side bought 1 NEAR x2 vs outside 0.3 x2 -> pepe wins; the outside coin's stake burned pepe
    assert_eq!(winner, "a");
    assert!(ctx.view::<String>(&pepe, "ft_total_supply", json!({})).await?.parse::<u128>()? < sup, "the winner's supply dropped");
    assert_eq!(ctx.ft_balance(&pepe, &graveyard).await?, 0);

    // bounty: paid to the caller out of the protocol bucket, only for real work: what left the bucket is exactly the bounties the tick events report
    let protocol_after = s(&ctx.view::<Value>(&ctx.arena, "protocol_balance", json!({})).await?);
    let paid: u128 = logs.iter().filter(|l| l.contains("\"event\":\"tick\"")).filter_map(|l| {
        let v: Value = near_sdk::serde_json::from_str(l.trim_start_matches("EVENT_JSON:")).ok()?;
        v["data"][0]["bounty"].as_str()?.parse::<u128>().ok()
    }).sum();
    assert!(paid >= near(0.005), "at least the settlement's bounty was paid: {paid}");
    assert_eq!(protocol_before - protocol_after, paid, "the bucket paid exactly the reported bounties and nothing else");

    // the next round is queued by the settlement; a second tick right away has nothing to do and pays nothing
    let q = due_kinds(&ctx).await?;
    assert_eq!(q.iter().filter(|k| k.contains("StartRound")).count(), 1, "next StartRound queued: {q:?}");
    // The settlement also queued the winner's and loser's commentary (real work, so it pays a bounty); once the queue is
    // drained, a tick has nothing to do and pays nothing.
    let mut quiet = false;
    for _ in 0..6 {
        let r = ctx.call(&ctx.arena, &poker, "tick", json!({"max_tasks": 5}), 0, 300).await?;
        let logs: Vec<String> = r.logs().iter().map(|l| l.to_string()).collect();
        if !logs.iter().any(|l| l.contains("\"event\":\"tick\"")) {
            quiet = true;
            break;
        }
        sleep(2500).await;
    }
    assert!(quiet, "after the queue drained, a tick processed nothing and paid nothing");
    Ok(())
}
