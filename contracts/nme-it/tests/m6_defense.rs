//! M6: Self-Defending Memes. A dump makes the coin's brain (mock OutLayer, same result shape as the real one) decide a
//! capped buyback; over-cap / failed / null answers spend nothing; a dead brain is replaced by the fixed rule.
mod common;
use common::*;
use near_sdk::serde_json::{json, Value};

fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

async fn treasury(ctx: &Ctx) -> testresult::TestResult<u128> {
    let e: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    Ok(s(&e["treasury_near"]))
}

async fn st(ctx: &Ctx) -> testresult::TestResult<Value> {
    ctx.view(&ctx.arena, "get_defense_state", json!({"ticker": "pepe"})).await
}

/// Pokes `tick` until `done` holds (checks run one at a time, 8 s apart).
async fn tick_until<F, Fut>(ctx: &Ctx, poker: &near_api::AccountId, what: &str, mut done: F) -> testresult::TestResult<Vec<String>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = testresult::TestResult<bool>>,
{
    let mut logs = vec![];
    for _ in 0..30 {
        let r = ctx.call(&ctx.arena, poker, "tick", json!({"max_tasks": 5}), 0, 300).await?;
        logs.extend(r.logs().iter().map(|l| l.to_string()));
        sleep(2500).await;
        if done().await? {
            return Ok(logs);
        }
        sleep(2500).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn the_brain_defends_within_caps_and_the_rule_covers_a_dead_brain() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let bob = ctx.account("bob.sandbox", 200).await?;
    let poker = ctx.account("poker.sandbox", 50).await?;
    let outlayer = near_api::AccountId::try_from("outlayer.sandbox".to_string())?;
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(20_000, 2_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    let pool = format!("{pepe}|wrap.testnet|10000");
    ctx.ok(&ctx.arena, &carol, "fund_protocol", json!({}), 1000, 30).await?;
    sleep(1500).await;

    // the config is public and bounded
    let cfg: Value = ctx.view(&ctx.arena, "get_defense_config", json!({})).await?;
    assert_eq!(cfg["per_action_bps"], 500);
    assert_eq!(cfg["daily_bps"], 1000);

    // fees from router buys fill the treasury; the buys also push the price up (the rolling high)
    ctx.call_yocto(&pepe, &bob, "storage_deposit", json!({"account_id": bob}), near(0.00125), 30).await?.assert_success();
    for _ in 0..5 {
        ctx.call(&ctx.arena, &bob, "buy", json!({"key": "pepe", "min_out": "1"}), 20_000, 300).await?.assert_success();
    }
    sleep(1500).await;
    let t0 = treasury(&ctx).await?;
    assert!(t0 > near(0.1), "router fees filled the treasury: {t0}");
    let pt = |v: Value| v["current_point"].as_i64().unwrap();
    let p_high: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    let _ = p_high;
    let pool0: Value = ctx.view(&ctx.dcl, "get_pool", json!({"pool_id": pool})).await?;
    println!("point after buys: {}", pt(pool0.clone()));

    // first check: the high is recorded, nothing to defend yet
    tick_until(&ctx, &poker, "the first check recorded the high", || async { Ok(st(&ctx).await?["high_ms"].as_u64().unwrap_or(0) > 0) }).await?;
    assert_eq!(st(&ctx).await?["day_spent_bps"], 0);
    assert_eq!(treasury(&ctx).await?, t0, "a calm check spends nothing");

    // dump: bob sells everything straight into the pool
    let held = ctx.ft_balance(&pepe, &bob).await?;
    ctx.dcl_sell(&bob, &pepe, &pool, held).await?;
    let pool1: Value = ctx.view(&ctx.dcl, "get_pool", json!({"pool_id": pool})).await?;
    println!("point after the dump: {} (was {})", pt(pool1.clone()), pt(pool0.clone()));
    assert!(pt(pool1) < pt(pool0), "the dump pushed the price down");

    // ---- the brain says buyback (300 bps) -> capped buyback & burn
    let supply0: u128 = ctx.view::<String>(&pepe, "ft_total_supply", json!({})).await?.parse()?;
    let logs = tick_until(&ctx, &poker, "a defense burn happened", || async { Ok(st(&ctx).await?["day_spent_bps"].as_u64().unwrap_or(0) == 300) }).await?;
    assert!(logs.iter().any(|l| l.contains("\"event\":\"defense\"") && l.contains("\"source\":\"ai\"") && l.contains("paper hands detected")), "{logs:?}");
    let input: String = ctx.view(&outlayer, "last_defense_input", json!({})).await?;
    let input: Value = near_sdk::serde_json::from_str(&input)?;
    assert_eq!(input["mode"], "defense");
    assert_eq!((input["per_action_bps"].as_u64(), input["daily_left_bps"].as_u64()), (Some(500), Some(1000)), "the brain is told the caps");
    // the burn runs as its own task on a later tick
    tick_until(&ctx, &poker, "the supply dropped", || async { Ok(ctx.view::<String>(&pepe, "ft_total_supply", json!({})).await?.parse::<u128>()? < supply0) }).await?;
    let t1 = treasury(&ctx).await?;
    assert!(t1 < t0 && t0 - t1 <= t0 * 300 / 10_000 + near(0.01), "the treasury paid about 3%, never more (t0 {t0}, t1 {t1})");

    // ---- over-cap decision: rejected, nothing spent, counted as a miss
    ctx.ok(&outlayer, &carol, "set_defense_mode", json!({"mode": "over_cap"}), 0, 30).await?;
    let spent = st(&ctx).await?["day_spent_bps"].clone();
    tick_until(&ctx, &poker, "an over-cap decision was rejected", || async { Ok(st(&ctx).await?["misses"].as_u64().unwrap_or(0) >= 1) }).await?;
    assert_eq!(st(&ctx).await?["day_spent_bps"], spent, "an over-cap decision spends nothing");

    // ---- the brain dies: after 2 misses the fixed rule takes over (the dump is past its trigger of 10 points)
    ctx.ok(&outlayer, &carol, "set_defense_mode", json!({"mode": "fail"}), 0, 30).await?;
    let logs = tick_until(&ctx, &poker, "the rule took over", || async { Ok(st(&ctx).await?["day_spent_bps"].as_u64().unwrap_or(0) > 300) }).await?;
    assert!(logs.iter().any(|l| l.contains("\"event\":\"defense\"") && l.contains("\"source\":\"rule\"")), "the fixed rule decided: {logs:?}");
    assert!(st(&ctx).await?["day_spent_bps"].as_u64().unwrap() <= 1000, "never above the daily cap");

    // ---- the feed shows defense events
    let f: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 200})).await?;
    let kinds: Vec<&str> = f.as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"defense"), "{kinds:?}");
    Ok(())
}
