//! S12: Rhea DCL from a contract, against the real DCL + wNEAR code imported from testnet.
mod common;
use common::*;
use near_api::AccountId;
use near_sdk::serde_json::{json, Value};

const DCL: &str = "dclv2.ref-dev.testnet";
const SUPPLY: u128 = 1_000_000_000 * 10u128.pow(18);

fn id(s: &str) -> AccountId {
    s.parse().unwrap()
}

async fn dcl_env() -> testresult::TestResult<Ctx> {
    // Reuse the harness only for sandbox + wNEAR; DCL is imported and initialised here.
    let sandbox = near_sandbox::Sandbox::start_sandbox().await?;
    let net = near_api::NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
    let signer = near_api::Signer::from_secret_key(near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap())?;
    let ctx = Ctx {
        sandbox, net, signer,
        factory: id("nme.sandbox"), arena: id("arena.sandbox"), crown: id("crown.sandbox"),
        wrap: id(WRAP), dcl: id(DCL), coin_hash: String::new(), vault_hash: String::new(),
    };
    for a in [WRAP, DCL] {
        ctx.sandbox.import_account(TESTNET_RPC, id(a)).send().await?;
    }
    let g: AccountId = near_sandbox::config::DEFAULT_GENESIS_ACCOUNT.to_owned();
    ctx.call(&ctx.wrap, &g, "new", json!({}), 0, 30).await?.assert_success();
    // DCL init: owner + wnear (from its metadata view on testnet); try shapes until one works.
    let mut ok = false;
    for args in [
        json!({"owner_id": g, "wnear_id": WRAP}),
        json!({"owner_id": g, "wnear_id": WRAP, "farming_contract_id": "farm.sandbox"}),
        json!({"owner_id": g, "wnear_id": WRAP, "farming_contract_id": "farm.sandbox", "operators": []}),
    ] {
        let r = ctx.call(&ctx.dcl, &g, "new", args.clone(), 0, 30).await?;
        println!("dcl.new {args}: {}", if r.failures().is_empty() { "OK".to_string() } else { format!("{:?}", r.failures()[0].clone().into_result()).chars().take(260).collect() });
        if r.failures().is_empty() { ok = true; break; }
    }
    assert!(ok, "could not initialise DCL");
    // DCL must be known to wNEAR (it holds/unwraps wNEAR).
    ctx.call_yocto(&ctx.wrap, &g, "storage_deposit", json!({"account_id": DCL}), near(0.00125), 30).await?.assert_success();
    Ok(ctx)
}

async fn view_val(ctx: &Ctx, c: &AccountId, m: &str, a: Value) -> testresult::TestResult<Value> {
    ctx.view::<Value>(c, m, a).await
}

/// Runs the whole lifecycle for a coin account name (decides token order vs wNEAR).
async fn lifecycle(ctx: &Ctx, name: &str, init_point: i32, left: i32, right: i32) -> testresult::TestResult<()> {
    let coin = ctx.account(name, 300).await?;
    let dcl = id(DCL);
    let creator = ctx.account("creator.sandbox", 100).await?;
    let buyer = ctx.account("buyer.sandbox", 100).await?;

    let wasm = std::fs::read("spikes/s12-launcher/target/near/s12_launcher.wasm").or_else(|_| std::fs::read("../spikes/s12-launcher/target/near/s12_launcher.wasm"))?;
    ctx.deploy(&coin, wasm, "new", json!({"dcl": dcl, "wrap": ctx.wrap, "supply": SUPPLY.to_string()})).await?;
    let token_is_x = coin.as_str() < WRAP;
    println!("== {name}: token is {}", if token_is_x { "X" } else { "Y" });

    // The coin must be registered on wNEAR to hold NEAR-side proceeds/refunds.
    ctx.call_yocto(&ctx.wrap, &creator, "storage_deposit", json!({"account_id": coin}), near(0.00125), 30).await?.assert_success();

    // ---- launch: DCL registration + create_pool + HotZap + position id
    let bal0 = ctx.balance(&coin).await?;
    let r = ctx.call(&coin, &creator, "launch", json!({"init_point": init_point, "left": left, "right": right}), 1000, 300).await?;
    let gas: u64 = r.outcomes().iter().map(|o| o.gas_burnt.as_gas()).sum();
    println!("COST launch: gas burnt {:.1} Tgas; coin NEAR kept {:.4} of 1.0000 attached", gas as f64 / 1e12, (ctx.balance(&coin).await.unwrap() as f64 - bal0 as f64) / 1e24);
    let logs: Vec<String> = r.logs().iter().map(|l| l.to_string()).collect();
    println!("launch logs: {logs:?}");
    assert!(r.failures().is_empty(), "launch failed: {:?}", r.failures());
    let st = view_val(ctx, &coin, "get_state", json!({})).await?;
    println!("state: {st}");
    let pool_id = st["pool_id"].as_str().expect("pool created").to_string();
    let lpt = st["lpt_id"].as_str().expect("position recorded").to_string();
    let pool = view_val(ctx, &dcl, "get_pool", json!({"pool_id": pool_id})).await?;
    assert_eq!(pool["current_point"], init_point, "pool starts at the chosen point");
    let rows = view_val(ctx, &dcl, "list_liquidities", json!({"account_id": coin})).await?;
    let amount0 = s(&rows[0]["amount"]);
    assert_eq!(rows[0]["lpt_id"], lpt.as_str());
    assert_eq!(ctx.ft_balance(&coin, &coin).await?, 0, "the whole supply left the coin for the position");
    assert_eq!(ctx.ft_balance(&coin, &dcl).await?, SUPPLY, "DCL holds the entire supply");
    assert_eq!(key_count(ctx, &coin).await, 1, "(spike account keeps a key; real coins have none)");

    // ---- router-style buy: NEAR in, tokens straight to the buyer
    ctx.call_yocto(&coin, &buyer, "storage_deposit", json!({"account_id": buyer}), near(0.00125), 30).await?.assert_success();
    let before = ctx.balance(&buyer).await?;
    let r = ctx.call(&coin, &buyer, "buy", json!({"min_out": "1"}), 2000, 250).await?;
    assert!(r.failures().is_empty(), "buy failed: {:?}", r.failures());
    let got = ctx.ft_balance(&coin, &buyer).await?;
    println!("buyer got {got} tokens for ~2 NEAR (spent {})", before - ctx.balance(&buyer).await?);
    assert!(got > 0, "buyer received tokens from the DCL swap");
    let st = view_val(ctx, &coin, "get_state", json!({})).await?;
    println!("after buy state: {st}");

    // impossible min_out -> DCL refuses, buyer's NEAR comes back
    let nb = ctx.balance(&buyer).await?;
    let r = ctx.call(&coin, &buyer, "buy", json!({"min_out": (SUPPLY * 10).to_string()}), 500, 250).await?;
    println!("impossible min_out: failures={}", r.failures().len());
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let lost = nb - ctx.balance(&buyer).await?;
    assert!(lost < near(0.05), "refused swap refunds the NEAR (lost {lost})");

    // ---- sell back through DCL directly (I1: no router needed) to generate token-side fees
    let sell = got / 2;
    let msg = json!({"Swap": {"pool_ids": [pool_id], "output_token": WRAP, "min_output_amount": "1"}}).to_string();
    ctx.call_yocto(&ctx.wrap, &buyer, "storage_deposit", json!({"account_id": buyer}), near(0.00125), 30).await?.assert_success();
    let r = ctx.call_yocto(&coin, &buyer, "ft_transfer_call", json!({"receiver_id": dcl, "amount": sell.to_string(), "msg": msg}), 1, 250).await?;
    assert!(r.failures().is_empty(), "sell failed: {:?}", r.failures());

    // ---- fee claim by anyone: token fees burned, NEAR fees land as native NEAR
    let rows = view_val(ctx, &dcl, "list_liquidities", json!({"account_id": coin})).await?;
    println!("pending fees: x={} y={}", rows[0]["unclaimed_fee_x"], rows[0]["unclaimed_fee_y"]);
    let supply_before: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    let near_before = ctx.balance(&coin).await?;
    let r = ctx.call(&coin, &creator, "claim_fees", json!({}), 0, 300).await?;
    assert!(r.failures().is_empty(), "claim failed: {:?}", r.failures());
    let st = view_val(ctx, &coin, "get_state", json!({})).await?;
    println!("claim: {}", st["last_claim"]);
    let (tok_fee, near_fee) = (s(&st["last_claim"][0]), s(&st["last_claim"][1]));
    assert!(near_fee > 0 && tok_fee > 0, "both fee sides accrued and were claimed");
    let supply_after: u128 = ctx.view::<String>(&coin, "ft_total_supply", json!({})).await?.parse()?;
    assert_eq!(supply_before - supply_after, tok_fee, "token-side fees were burned (D-31)");
    assert!(ctx.balance(&coin).await? >= near_before + near_fee - near(0.01), "NEAR-side fees arrived as native NEAR");
    let rows = view_val(ctx, &dcl, "list_liquidities", json!({"account_id": coin})).await?;
    assert_eq!(s(&rows[0]["amount"]), amount0, "claiming fees never touches the principal");
    assert_eq!(rows[0]["unclaimed_fee_x"], "0");
    assert_eq!(rows[0]["unclaimed_fee_y"], "0");
    Ok(())
}

async fn key_count(ctx: &Ctx, a: &AccountId) -> usize {
    ctx.key_count(a).await.unwrap_or(0)
}

#[tokio::test]
async fn dcl_single_sided_launch_token_is_x() -> testresult::TestResult<()> {
    let ctx = dcl_env().await?;
    // 1 token = 0.01 NEAR: raw Y/X = 1e4 -> point ~92103; start 92000 (multiple of 200), range above it.
    lifecycle(&ctx, "pepe.sandbox", 92_000, 92_200, 132_200).await
}

#[tokio::test]
async fn dcl_single_sided_launch_token_is_y() -> testresult::TestResult<()> {
    let ctx = dcl_env().await?;
    // token sorts after wrap.testnet, so it is Y; raw Y/X = 100 tokens per NEAR / 1e6 -> point ~ -92103; range below it.
    lifecycle(&ctx, "zeta.sandbox", -92_000, -132_200, -92_200).await
}

/// Exact-output buys: does DCL fill them when the pool's current point is below the single-sided range?
#[tokio::test]
async fn dcl_swap_by_output_below_range() -> testresult::TestResult<()> {
    let ctx = dcl_env().await?;
    let coin = ctx.account("pepe.sandbox", 300).await?;
    let creator = ctx.account("creator.sandbox", 100).await?;
    let buyer = ctx.account("buyer.sandbox", 200).await?;
    let wasm = std::fs::read("../spikes/s12-launcher/target/near/s12_launcher.wasm")?;
    ctx.deploy(&coin, wasm, "new", json!({"dcl": DCL, "wrap": ctx.wrap, "supply": SUPPLY.to_string()})).await?;
    ctx.call_yocto(&ctx.wrap, &creator, "storage_deposit", json!({"account_id": coin}), near(0.00125), 30).await?.assert_success();
    let r = ctx.call(&coin, &creator, "launch", json!({"init_point": 92_000, "left": 92_200, "right": 132_200}), 1000, 300).await?;
    assert!(r.failures().is_empty());
    let st = view_val(&ctx, &coin, "get_state", json!({})).await?;
    let pool_id = st["pool_id"].as_str().unwrap().to_string();
    let pool = view_val(&ctx, &id(DCL), "get_pool", json!({"pool_id": pool_id})).await?;
    println!("pool current_point={} (range starts 92200)", pool["current_point"]);

    ctx.call_yocto(&coin, &buyer, "storage_deposit", json!({"account_id": buyer}), near(0.00125), 30).await?.assert_success();
    ctx.call_yocto(&ctx.wrap, &buyer, "storage_deposit", json!({"account_id": buyer}), near(0.00125), 30).await?.assert_success();
    ctx.call_yocto(&ctx.wrap, &buyer, "near_deposit", json!({}), near(30.0), 30).await?.assert_success();
    let quote = view_val(&ctx, &id(DCL), "quote_by_output", json!({"pool_ids": [pool_id], "input_token": WRAP, "output_token": coin, "output_amount": (1_000u128 * 10u128.pow(18)).to_string(), "tag": null})).await;
    println!("quote_by_output(1000 tokens) = {quote:?}");

    let by_out = |amount: u128| json!({"SwapByOutput": {"pool_ids": [pool_id], "output_token": coin, "output_amount": amount.to_string(), "swap_out_recipient": buyer}}).to_string();
    let try_buy = |msg: String| {
        let (ctx, buyer, id_dcl) = (&ctx, buyer.clone(), id(DCL));
        async move {
            let r = ctx.call_yocto(&ctx.wrap, &buyer, "ft_transfer_call", json!({"receiver_id": id_dcl, "amount": near(15.0).to_string(), "msg": msg}), 1, 250).await.unwrap();
            for l in r.logs().iter().filter(|l| l.contains("\"swap\"") || l.contains("Refund")) { println!("   {}", l.chars().take(300).collect::<String>()); }
            r.json::<String>().unwrap_or_else(|e| format!("<{e:?}>"))
        }
    };
    println!("exact-output from BELOW the range: used = {}", try_buy(by_out(1_000 * 10u128.pow(18))).await);
    println!("buyer tokens: {}", ctx.ft_balance(&coin, &buyer).await?);
    // move the price into the range with a regular swap, then ask again
    let msg = json!({"Swap": {"pool_ids": [pool_id], "output_token": coin, "min_output_amount": "1", "swap_out_recipient": buyer}}).to_string();
    let r = ctx.call_yocto(&ctx.wrap, &buyer, "ft_transfer_call", json!({"receiver_id": id(DCL), "amount": near(0.1).to_string(), "msg": msg}), 1, 250).await?;
    assert!(r.failures().is_empty());
    let pool = view_val(&ctx, &id(DCL), "get_pool", json!({"pool_id": pool_id})).await?;
    println!("after a regular swap: current_point={}", pool["current_point"]);
    println!("exact-output from INSIDE the range: used = {}", try_buy(by_out(1_000 * 10u128.pow(18))).await);
    println!("buyer tokens: {}", ctx.ft_balance(&coin, &buyer).await?);
    Ok(())
}
