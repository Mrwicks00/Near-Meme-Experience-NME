//! M5: Working Memes. Pay a coin's brain → the TEE worker (here: the mock OutLayer, same result shape) answers → the
//! payment buys and burns the coin; refusals, failures, nulls and garbage refund the payer exactly once.
mod common;
use common::*;
use near_sdk::serde_json::{json, Value};

fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

async fn job(ctx: &Ctx, id: u64) -> testresult::TestResult<Value> {
    ctx.view(&ctx.arena, "get_job", json!({ "job_id": id })).await
}

#[tokio::test]
async fn hire_deliver_burn_and_every_refund_path() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 100).await?;
    let hirer = ctx.account("hirer.sandbox", 100).await?;
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    sleep(1500).await;

    // ---- rejections
    ctx.fails(&ctx.arena, &hirer, "hire", json!({"ticker": "pepe", "kind": "roast", "target": "DOGE", "prompt": "x"}), 10, 300, "below this coin").await?;
    ctx.fails(&ctx.arena, &hirer, "hire", json!({"ticker": "pepe", "kind": "withdraw", "target": "", "prompt": "x"}), 500, 300, "kind must be").await?;
    ctx.fails(&ctx.arena, &hirer, "hire", json!({"ticker": "nope", "kind": "roast", "target": "", "prompt": "x"}), 500, 300, "unknown coin").await?;

    // ---- a delivered job: 1 NEAR payment
    let supply0: u128 = ctx.view::<String>(&pepe, "ft_total_supply", json!({})).await?.parse()?;
    let protocol0 = s(&ctx.view::<Value>(&ctx.arena, "protocol_balance", json!({})).await?);
    let before = ctx.balance(&hirer).await?;
    let r = ctx.call(&ctx.arena, &hirer, "hire", json!({"ticker": "pepe", "kind": "roast", "target": "DOGE", "prompt": "make fun of DOGE"}), 1000, 300).await?;
    assert!(r.failures().is_empty(), "hire: {:?}", r.failures());
    sleep(2500).await;
    let j = job(&ctx, 1).await?;
    assert_eq!(j["status"], "Delivered", "the callback delivered: {j}");
    let amount = s(&j["amount"]);
    assert_eq!(amount, near(1.0) - near(0.02) - near(0.003), "escrow = payment - OutLayer fee - record reserve");
    let d: Value = ctx.view(&ctx.arena, "get_deliveries", json!({"limit": 5})).await?;
    assert_eq!(d[0]["text"], "PEPE says: DOGE is ngmi (job 1)");
    assert_eq!((d[0]["model"].as_str(), d[0]["chat_id"].as_str()), (Some("mock/qwen"), Some("chat-1")));
    let protocol1 = s(&ctx.view::<Value>(&ctx.arena, "protocol_balance", json!({})).await?);
    assert_eq!(protocol1 - protocol0, amount / 10, "10% cut to the protocol bucket");
    let e: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    assert_eq!(s(&e["chest_near"]), amount - amount / 10, "the net payment waits in the chest for the burn task");
    let q: Value = ctx.view(&ctx.arena, "due_tasks", json!({"limit": 20})).await?;
    assert!(q.to_string().contains("JobBurn"), "the burn is queued: {q}");

    // the burn: anyone ticks (a plain tick carries enough gas for the heavy task)
    // (one tick runs one chain-starting task, and fee claims / defense checks are queued too: poke until the burn ran)
    for _ in 0..8 {
        let r = ctx.call(&ctx.arena, &carol, "tick", json!({"max_tasks": 5}), 0, 300).await?;
        assert!(r.failures().is_empty(), "tick: {:?}", r.failures());
        sleep(3000).await;
        if job(&ctx, 1).await?["status"] == "Burned" {
            break;
        }
    }
    assert_eq!(job(&ctx, 1).await?["status"], "Burned");
    let supply1: u128 = ctx.view::<String>(&pepe, "ft_total_supply", json!({})).await?.parse()?;
    assert!(supply1 < supply0, "the payment bought and burned $PEPE: {supply0} -> {supply1}");
    let e: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    assert_eq!(s(&e["chest_near"]), 0, "the chest paid the burn");
    assert!(ctx.balance(&hirer).await? < before - near(0.9), "the hirer paid for it");

    // ---- every failure path refunds the escrow in full, once, and credits nothing
    for (i, marker) in ["MOCK_REFUSE", "MOCK_NULL", "MOCK_GARBAGE", "MOCK_FAIL"].iter().enumerate() {
        let id = (i + 2) as u64;
        let before = ctx.balance(&hirer).await?;
        ctx.call(&ctx.arena, &hirer, "hire", json!({"ticker": "pepe", "kind": "shill", "target": "", "prompt": marker}), 500, 300).await?;
        sleep(2500).await;
        let j = job(&ctx, id).await?;
        assert_eq!(j["status"], "Refunded", "{marker}: {j}");
        let paid = before - ctx.balance(&hirer).await?;
        assert!(paid < near(0.05), "{marker}: only gas and the OutLayer fee, never the escrow (paid {paid})");
        let e: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
        assert_eq!(s(&e["chest_near"]), 0, "{marker}: nothing credited to the chest");
    }
    let d: Value = ctx.view(&ctx.arena, "get_deliveries", json!({"limit": 10})).await?;
    assert_eq!(d.as_array().unwrap().len(), 1, "only the real delivery is in the ring");

    // ---- a delivered job cannot be refunded; the brain account has no path to funds
    ctx.fails(&ctx.arena, &carol, "claim_refund", json!({"job_id": 1}), 0, 100, "not waiting for the brain").await?;
    ctx.fails(&ctx.arena, &carol, "on_job_result", json!({"job_id": 1}), 0, 100, "Method on_job_result is private").await?;
    // ---- a meme (image) job: the picture reference is stored with the delivery; a meme without a picture refunds
    ctx.call(&ctx.arena, &hirer, "hire", json!({"ticker": "pepe", "kind": "meme", "target": "BATMAN", "prompt": "a frog laughing"}), 500, 300).await?.assert_success();
    sleep(3000).await;
    let d: Value = ctx.view(&ctx.arena, "get_deliveries", json!({"limit": 5})).await?;
    assert_eq!(d[0]["kind"], "meme");
    assert!(d[0]["media_url"].as_str().unwrap().starts_with("https://test.fastfs.io/nme-media.testnet/outlayer.testnet/") && d[0]["media_hash"].as_str().unwrap().len() == 64, "picture reference stored: {d}");
    ctx.call(&ctx.arena, &hirer, "hire", json!({"ticker": "pepe", "kind": "meme", "target": "", "prompt": "MOCK_NOPIC"}), 500, 300).await?.assert_success();
    sleep(3000).await;
    let n = ctx.view::<Value>(&ctx.arena, "get_job", json!({"job_id": 7})).await?;
    assert_eq!(n["status"], "Refunded", "a meme without a picture is refunded: {n}");

    // ---- a coin whose creator fixed its own job price at launch
    let dog = ctx.create_coin_priced(&alice, "dog", Ctx::launch_params(92_000, 40_000, None), near(0.5)).await?;
    ctx.finish_launch(&dog, &carol).await?;
    sleep(3000).await;
    let price: Value = ctx.view(&ctx.arena, "get_job_price", json!({"ticker": "dog"})).await?;
    assert_eq!(s(&price), near(0.5), "the creator's price was registered with the arena");
    let default_price: Value = ctx.view(&ctx.arena, "get_job_price", json!({"ticker": "pepe"})).await?;
    assert_eq!(s(&default_price), near(0.1), "a coin without one costs the global minimum");
    ctx.fails(&ctx.arena, &hirer, "hire", json!({"ticker": "dog", "kind": "roast", "target": "", "prompt": "x"}), 300, 300, "below this coin's job price").await?;
    ctx.call(&ctx.arena, &hirer, "hire", json!({"ticker": "dog", "kind": "roast", "target": "", "prompt": "x"}), 500, 300).await?.assert_success();
    // the price is immutable and only the factory can set it
    ctx.fails(&ctx.arena, &alice, "set_job_price", json!({"ticker": "dog", "price": near(0.2).to_string()}), 0, 30, "only the factory").await?;
    let mut args = Ctx::create_args("cat", Ctx::launch_params(92_000, 40_000, None));
    args["args"]["job_price"] = json!(near(50.0).to_string());
    ctx.fails(&ctx.factory, &alice, "create_coin", args, 1500, 300, "job_price must be between").await?;
    let f: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 200})).await?;
    let kinds: Vec<&str> = f.as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    for k in ["job_hired", "job_delivered", "job_refunded", "treasury_buyback"] {
        assert!(kinds.contains(&k), "feed has {k}: {kinds:?}");
    }
    Ok(())
}
