mod common;
use common::*;
use near_sdk::serde_json::{json, Value};
use nme_common::{supply_split, TOTAL_SUPPLY};

#[tokio::test]
async fn factory_creates_keyless_coin_registers_mints_crown_and_launches() -> testresult::TestResult<()> {
    let ctx = Ctx::setup().await?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let arena_before = ctx.balance(&ctx.arena).await?;
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    println!("coin account: {coin}");
    // Audit F2: the arena keeps what its registration storage costs and refunds the rest.
    let arena_gain = ctx.balance(&ctx.arena).await? - arena_before;
    assert!(arena_gain > near(0.002) && arena_gain < near(0.02), "arena keeps its storage cost (gain {arena_gain})");

    // I2: no access keys, running the published global code.
    assert_eq!(ctx.key_count(&coin).await?, 0, "coin account must have no access keys");
    let acct = near_api::Account(coin.clone()).view().fetch_from(&ctx.net).await?.data;
    assert!(format!("{:?}", acct.contract_state).contains("GlobalHash"), "coin runs global code: {acct:?}");

    // The whole supply is minted once, at creation: 95% on the pool side, 5% in the PumpBag vault.
    let split = supply_split();
    let supply: String = ctx.view(&coin, "ft_total_supply", json!({})).await?;
    assert_eq!(supply.parse::<u128>()?, TOTAL_SUPPLY);

    // Factory record, arena registration (+ queued ClaimFees), PumpBag NFT to the creator.
    let rec: Value = ctx.view(&ctx.factory, "get_coin", json!({"ticker": "pepe"})).await?;
    assert_eq!(rec["status"], "Live");
    assert_eq!(rec["creator"], alice.as_str());
    let entry: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    assert_eq!(entry["account"], coin.as_str());
    assert_eq!(entry["crown_vault"], format!("crown.{coin}"));
    let due: Value = ctx.view(&ctx.arena, "due_tasks", json!({"limit": 10})).await?;
    let tasks = due.as_array().unwrap();
    assert!(tasks.iter().any(|t| t["task"]["ClaimFees"]["ticker"] == "pepe"), "the fee claim is queued: {due}");
    assert!(tasks.iter().any(|t| t["task"]["ResumeLaunch"]["ticker"] == "pepe"), "and so is the launch watchdog: {due}");
    let nft: Value = ctx.view(&ctx.crown, "nft_token", json!({"token_id": "pepe"})).await?;
    assert_eq!(nft["owner_id"], alice.as_str());
    assert_eq!(nft["metadata"]["title"], "PumpBag of $PEPE");

    // Metadata, config (persona is immutable data), feed.
    let md: Value = ctx.view(&coin, "ft_metadata", json!({})).await?;
    assert_eq!((md["symbol"].as_str().unwrap(), md["decimals"].as_u64().unwrap()), ("PEPE", 18));
    assert_eq!(md["icon"], ICON);
    let cfg: Value = ctx.view(&coin, "get_config", json!({})).await?;
    assert_eq!(cfg["persona"]["vibe"], "degen frog");
    let ff: Value = ctx.view(&ctx.factory, "feed_since", json!({"id": 0, "limit": 10})).await?;
    assert_eq!(ff[0]["kind"], "coin_created");

    // The arena is registered on the coin by the factory (needed for sells and burns).
    let sb: Value = ctx.view(&coin, "storage_balance_of", json!({"account_id": ctx.arena})).await?;
    assert!(!sb.is_null(), "the factory registered the arena on the coin");

    // The launch runs by itself after creation (leftover gas) and is finished by anyone if it paused.
    let extra = ctx.finish_launch(&coin, &alice).await?;
    println!("launch needed {extra} extra launch() call(s) after create_coin");
    let launch: Value = ctx.view(&coin, "get_launch", json!({})).await?;
    let pool_id = launch["Live"]["pool_id"].as_str().unwrap().to_string();
    assert_eq!(pool_id, format!("{coin}|wrap.testnet|10000"), "coin is token X of its pool, 1% tier");
    // DCL holds exactly the pool side; the PumpBag vault holds the 5%.
    assert_eq!(ctx.ft_balance(&coin, &ctx.dcl).await?, split.pool.0);
    let vault: near_api::AccountId = format!("crown.{coin}").parse()?;
    assert_eq!(ctx.ft_balance(&coin, &vault).await?, split.crown.0, "dev allocation sits in the PumpBag vault (I4)");
    assert_eq!(ctx.ft_balance(&coin, &coin).await?, 0, "nothing is left loose in the coin");
    assert_eq!(ctx.key_count(&vault).await?, 0, "vault has no access keys");
    let v: Value = ctx.view(&vault, "get_vault", json!({})).await?;
    assert_eq!(v["authority"], ctx.crown.as_str());
    assert_eq!(s(&v["vest"]["total"]), split.crown.0);
    // The arena heard about it (feed item for the UI).
    let af: Value = ctx.view(&ctx.arena, "feed_since", json!({"id": 0, "limit": 20})).await?;
    assert!(af.as_array().unwrap().iter().any(|i| i["kind"] == "launch_finalized"), "feed: {af}");
    let entry: Value = ctx.view(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?;
    assert_eq!(entry["launched"], true);
    assert_eq!(entry["pool_id"], pool_id.as_str());
    Ok(())
}
