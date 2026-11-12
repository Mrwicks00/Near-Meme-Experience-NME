//! Real-state check for `arena.migrate_rpc` (lessons 8): copies the DEPLOYED arena (code + storage) from testnet into the
//! sandbox, deploys the new code with `migrate_rpc`, and requires every view to answer the same, except the brain's RPC URL.
//! Run: `cargo test -p nme-it --test migrate_rpc_real_state -- --nocapture`
mod common;
use common::*;
use near_api::{AccountId, NearToken};
use near_sdk::serde_json::{json, Value};

const ARENA: &str = "arena.nme.testnet";
const NEW_RPC: &str = "https://near-testnet.drpc.org";
/// The import reads the arena's whole state (now over the usual 50 KB `view_state` limit): this endpoint still serves it.
const IMPORT_RPC: &str = "https://test.rpc.fastnear.com";

#[tokio::test]
#[ignore = "done on testnet 2026-11-11 (tx 7i3JEYy5szaaKi4PGrgZF61MGRLappwNxzrDBTp6rrkc); the live arena state is now over the RPC view limit, so the import is flaky"]
async fn the_deployed_arena_changes_only_its_rpc_url() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_empty().await?;
    let arena: AccountId = ARENA.parse().unwrap();
    ctx.sandbox.import_account(IMPORT_RPC, arena.clone()).with_storage().initial_balance(NearToken::from_near(50)).send().await?;

    let views: Vec<(&str, Value)> = vec![
        ("get_coin", json!({"key": "pepe"})),
        ("get_coin", json!({"key": "toad"})),
        ("get_coin", json!({"key": "axolotl"})),
        ("get_coin", json!({"key": "nme-batman.testnet"})),
        ("get_config", json!({})),
        ("feed_last_id", json!({})),
        ("feed_since", json!({"id": 0, "limit": 500})),
        ("next_due_ms", json!({})),
        ("due_tasks", json!({"limit": 50})),
        ("protocol_balance", json!({})),
        ("get_defense_config", json!({})),
        ("get_defense_state", json!({"ticker": "toad"})),
        ("get_commentary", json!({"limit": 64})),
        ("get_commentary_config", json!({})),
        ("get_gateway", json!({})),
        ("get_deliveries", json!({"limit": 20})),
        ("get_job", json!({"job_id": 5})),
        ("get_job", json!({"job_id": 8})),
        ("get_job_price", json!({"ticker": "toad"})),
        ("current_round", json!({})),
        ("next_round_ms", json!({})),
        ("get_war_config", json!({})),
        ("roster", json!({"from": 0, "limit": 50})),
        ("get_crown_deadline", json!({"ticker": "pepe"})),
    ];
    let mut snap = Vec::new();
    for (m, a) in &views {
        snap.push(ctx.view::<Value>(&arena, m, a.clone()).await?);
    }
    let mut brain_before: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;

    // Credentials or a non-https URL are refused, and the state is untouched by the failed attempt.
    for bad in ["http://x.org", "https://k@x.org", "https://x.org/?apikey=1"] {
        let r = near_api::Contract::deploy(arena.clone())
            .use_code(wasm("nme_arena"))
            .with_init_call("migrate_rpc", json!({"rpc_url": bad}))?
            .with_signer(ctx.signer.clone())
            .send_to(&ctx.net)
            .await?;
        assert!(r.into_result().is_err(), "{bad} accepted");
    }
    ctx.deploy(&arena, wasm("nme_arena"), "migrate_rpc", json!({"rpc_url": NEW_RPC})).await?;

    for (i, (m, a)) in views.iter().enumerate() {
        assert_eq!(snap[i], ctx.view::<Value>(&arena, m, a.clone()).await?, "arena view {m}({a}) changed");
    }
    let mut brain_after: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;
    assert_eq!(brain_after["rpc_url"], NEW_RPC);
    for b in [&mut brain_before, &mut brain_after] {
        b.as_object_mut().unwrap().remove("rpc_url");
    }
    assert_eq!(brain_before, brain_after, "brain config other than rpc_url changed");
    Ok(())
}

const NEW_BRAIN: &str = "e60add258ad4ea9eaf893b44562449654ee7f546f525481e7648108674313eb0";

/// `migrate_brain` on the live arena: only the worker pin changes; a URL that does not end in the hash is refused.
#[tokio::test]
#[ignore = "done on testnet 2026-11-11 (tx 14hPeJCn34FHcUoeoMWZ765xEiq1md5Hvzh5cwK5ecwk, E9d74r9ajYKhxCEpCZJu71q8Ls71DY3GMV573ma2sLTK); the live arena state is now over the RPC view limit, so the import is flaky"]
async fn the_deployed_arena_changes_only_its_brain_pin() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_empty().await?;
    let arena: AccountId = ARENA.parse().unwrap();
    ctx.sandbox.import_account(IMPORT_RPC, arena.clone()).with_storage().initial_balance(NearToken::from_near(50)).send().await?;
    let views: Vec<(&str, Value)> = vec![
        ("get_coin", json!({"key": "toad"})),
        ("get_config", json!({})),
        ("feed_since", json!({"id": 0, "limit": 500})),
        ("due_tasks", json!({"limit": 50})),
        ("get_deliveries", json!({"limit": 20})),
        ("get_job", json!({"job_id": 10})),
        ("roster", json!({"from": 0, "limit": 50})),
        ("get_defense_state", json!({"ticker": "toad"})),
    ];
    let mut snap = Vec::new();
    for (m, a) in &views {
        snap.push(ctx.view::<Value>(&arena, m, a.clone()).await?);
    }
    let mut before: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;
    let url = format!("https://test.fastfs.io/nme-s4.testnet/outlayer.testnet/{NEW_BRAIN}.wasm");
    let r = near_api::Contract::deploy(arena.clone())
        .use_code(wasm("nme_arena"))
        .with_init_call("migrate_brain", json!({"brain_url": "https://x.org/other.wasm", "brain_hash": NEW_BRAIN}))?
        .with_signer(ctx.signer.clone())
        .send_to(&ctx.net)
        .await?;
    assert!(r.into_result().is_err(), "a url not ending in the hash was accepted");
    ctx.deploy(&arena, wasm("nme_arena"), "migrate_brain", json!({"brain_url": url, "brain_hash": NEW_BRAIN})).await?;
    for (i, (m, a)) in views.iter().enumerate() {
        assert_eq!(snap[i], ctx.view::<Value>(&arena, m, a.clone()).await?, "arena view {m}({a}) changed");
    }
    let mut after: Value = ctx.view(&arena, "get_brain_config", json!({})).await?;
    assert_eq!(after["brain_hash"], NEW_BRAIN);
    assert_eq!(after["brain_url"], url);
    for b in [&mut before, &mut after] {
        b.as_object_mut().unwrap().remove("brain_url");
        b.as_object_mut().unwrap().remove("brain_hash");
    }
    assert_eq!(before, after, "brain config other than the pin changed");
    Ok(())
}

/// Raids (2026-11-11) keep their state under their own storage prefixes and append one `Task` variant, so the new code
/// deploys over the live arena with NO init call: every existing view must answer the same, and a raid can start.
#[tokio::test]
async fn the_deployed_arena_takes_raids_without_a_migration() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_empty().await?;
    let arena: AccountId = ARENA.parse().unwrap();
    ctx.sandbox.import_account(IMPORT_RPC, arena.clone()).with_storage().initial_balance(NearToken::from_near(50)).send().await?;
    let views: Vec<(&str, Value)> = vec![
        ("get_coin", json!({"key": "pepe"})),
        ("get_coin", json!({"key": "toad"})),
        ("get_config", json!({})),
        ("get_brain_config", json!({})),
        ("feed_last_id", json!({})),
        ("feed_since", json!({"id": 0, "limit": 500})),
        ("due_tasks", json!({"limit": 50})),
        ("protocol_balance", json!({})),
        ("current_round", json!({})),
        ("get_deliveries", json!({"limit": 20})),
        ("get_defense_state", json!({"ticker": "toad"})),
        ("roster", json!({"from": 0, "limit": 50})),
    ];
    let mut snap = Vec::new();
    for (m, a) in &views {
        snap.push(ctx.view::<Value>(&arena, m, a.clone()).await?);
    }
    near_api::Contract::deploy(arena.clone()).use_code(wasm("nme_arena")).without_init_call().with_signer(ctx.signer.clone()).send_to(&ctx.net).await?.assert_success();
    for (i, (m, a)) in views.iter().enumerate() {
        assert_eq!(snap[i], ctx.view::<Value>(&arena, m, a.clone()).await?, "arena view {m}({a}) changed");
    }
    let rules: Value = ctx.view(&arena, "raid_rules", json!({})).await?;
    assert_eq!(rules["social"], "v1.social08.testnet");
    let none: Value = ctx.view(&arena, "latest_raid", json!({"ticker": "pepe"})).await?;
    assert!(none.is_null());
    // A raid starts on the real state, and its settlement is queued for the tick.
    let genesis: AccountId = near_sandbox::config::DEFAULT_GENESIS_ACCOUNT.to_owned();
    ctx.call(&arena, &genesis, "start_raid", json!({"ticker": "pepe"}), 20, 50).await?.assert_success();
    let raid: Value = ctx.view(&arena, "latest_raid", json!({"ticker": "pepe"})).await?;
    assert_eq!(raid["tag"], "nme_pepe_raid_1");
    let due: Value = ctx.view(&arena, "due_tasks", json!({"limit": 200})).await?;
    assert!(due.as_array().unwrap().iter().any(|t| t["task"]["RaidSettle"]["raid_id"] == 1), "settlement queued");
    Ok(())
}
