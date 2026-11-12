//! M7b (D-57): Phantom and EVM wallets own gateway balances with no NEAR account. A verified deposit credits the wallet; a
//! message signed by the wallet (ed25519 for Solana, EIP-191 personal_sign for EVM) spends it. The gateway wasm runs the real
//! `env::ed25519_verify` / `env::ecrecover` host functions here (spikes S7 / S7b at runtime); OutLayer and the MPC contract
//! are the sandbox mock (the real verifier worker has its own tests and live runs).
mod common;
use common::*;
use near_sdk::serde_json::{json, Value};
use sha3::{Digest, Keccak256};

fn s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

async fn sleep(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

fn keccak(b: &[u8]) -> Vec<u8> {
    Keccak256::digest(b).to_vec()
}

fn sol_sk() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])
}
fn sol_owner() -> String {
    format!("sol:{}", near_sdk::bs58::encode(sol_sk().verifying_key().to_bytes()).into_string())
}
fn evm_sk() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_bytes((&[9u8; 32]).into()).unwrap()
}
fn evm_owner() -> String {
    let pk = evm_sk().verifying_key().to_encoded_point(false);
    format!("evm:0x{}", keccak(&pk.as_bytes()[1..])[12..].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// What the wallet does with the text: Phantom signMessage (raw bytes) or MetaMask personal_sign (EIP-191).
fn sign(owner: &str, text: &str) -> String {
    use base64_lite::encode;
    if owner.starts_with("sol:") {
        use ed25519_dalek::Signer;
        encode(&sol_sk().sign(text.as_bytes()).to_bytes())
    } else {
        let mut data = format!("\x19Ethereum Signed Message:\n{}", text.len()).into_bytes();
        data.extend_from_slice(text.as_bytes());
        let (sig, rec) = evm_sk().sign_prehash_recoverable(&keccak(&data)).unwrap();
        let mut v = sig.to_bytes().to_vec();
        v.push(27 + rec.to_byte());
        encode(&v)
    }
}

/// Minimal base64 (standard alphabet) so the test needs no extra crate.
mod base64_lite {
    pub fn encode(b: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}

fn sol_tx(tag: &str) -> String {
    let h = keccak(tag.as_bytes());
    let mut b = h.clone();
    b.extend(keccak(&h)); // 64 bytes, like a Solana signature
    near_sdk::bs58::encode(b).into_string()
}
fn evm_tx(tag: &str) -> String {
    format!("0x{}", keccak(tag.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

#[tokio::test]
async fn wallets_own_balances_and_every_forgery_changes_nothing() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let gw: near_api::AccountId = "gateway.sandbox".parse()?;
    let outlayer: near_api::AccountId = "outlayer.sandbox".parse()?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 200).await?;
    let relayer = ctx.account("relayer.sandbox", 50).await?;
    let coin = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&coin, &carol).await?;
    ctx.ok(&gw, &carol, "fund_reserve", json!({}), 30_000, 30).await?;
    let fee = near(0.001);

    // ---- deposits credit the wallet that owns the path (both chains), once
    let deposits = [(sol_owner(), sol_tx("sol-dep-1")), (evm_owner(), evm_tx("evm-dep-1"))];
    for (owner, tx) in &deposits {
        let path: String = ctx.view(&gw, "wallet_deposit_path", json!({"owner": owner})).await?;
        assert_eq!(path, format!("wallet:{owner}"));
        ctx.call(&outlayer, &carol, "set_deposit", json!({"tx_id": tx, "near_yocto": near(5.0).to_string()}), 0, 30).await?.assert_success();
        ctx.call(&gw, &relayer, "claim_wallet_deposit", json!({"tx_id": tx, "owner": owner}), 0, 300).await?.assert_success();
    }
    sleep(6000).await;
    let net = near(5.0) - near(5.0) / 10_000 * 200;
    for (owner, tx) in &deposits {
        let w: Value = ctx.view(&gw, "get_wallet", json!({"owner": owner})).await?;
        assert_eq!(s(&w["near"]), net, "{owner}: credited the net value");
        let c: Value = ctx.view(&gw, "get_claim", json!({"tx_id": tx})).await?;
        let _ = c; // the claim key is chain-prefixed; get_claim is the legacy Solana view
        ctx.fails(&gw, &relayer, "claim_wallet_deposit", json!({"tx_id": tx, "owner": owner}), 0, 300, "already paid out").await?;
    }
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_reserve", json!({})).await?), near(30.0) - 2 * net, "paid from the reserve");
    // wrong transaction-id shape per chain
    ctx.fails(&gw, &relayer, "claim_wallet_deposit", json!({"tx_id": evm_tx("x"), "owner": sol_owner()}), 0, 100, "Solana signature").await?;
    ctx.fails(&gw, &relayer, "claim_wallet_deposit", json!({"tx_id": sol_tx("x"), "owner": evm_owner()}), 0, 100, "EVM transaction hash").await?;

    // ---- a Phantom-signed action funds a war chest; the relayer (anyone) submits and the owner pays the fee
    let exp = now_ms() + 3_600_000;
    let chest = json!({"FundChest": {"ticker": "pepe", "amount": near(1.0).to_string()}});
    let chest0 = s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?["chest_near"]);
    let owner = sol_owner();
    let text: String = ctx.view(&gw, "wallet_message", json!({"owner": owner, "action": chest, "nonce": 0, "expires_ms": exp})).await?;
    assert!(text.starts_with("NME v1\ngateway: gateway.sandbox\nowner: sol:"), "{text}");
    let sig = sign(&owner, &text);
    // forgeries first: nothing changes
    let call = |o: &str, a: Value, n: u64, e: u64, sg: String| json!({"owner": o, "action": a, "nonce": n, "expires_ms": e, "signature": sg});
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, chest.clone(), 0, exp, sign(&evm_owner(), &text)), 0, 300, "Solana signature").await?;
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, json!({"FundChest": {"ticker": "pepe", "amount": near(4.0).to_string()}}), 0, exp, sig.clone()), 0, 300, "Solana signature").await?;
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, chest.clone(), 1, exp, sig.clone()), 0, 300, "wrong nonce").await?;
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, chest.clone(), 0, 1, sig.clone()), 0, 300, "expired").await?;
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]), net);
    // the real one
    ctx.call(&gw, &relayer, "wallet_action", call(&owner, chest.clone(), 0, exp, sig.clone()), 0, 300).await?.assert_success();
    sleep(4000).await;
    let w: Value = ctx.view(&gw, "get_wallet", json!({"owner": owner})).await?;
    assert_eq!((s(&w["near"]), w["nonce"].as_u64()), (net - near(1.0) - fee, Some(1)), "amount and relay fee left the balance, nonce moved");
    assert_eq!(s(&ctx.view::<Value>(&ctx.arena, "get_coin", json!({"key": "pepe"})).await?["chest_near"]) - chest0, near(1.0), "the wallet's NEAR is in the war chest");
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, chest.clone(), 0, exp, sig), 0, 300, "wrong nonce").await?; // replay

    // ---- a MetaMask-signed action sends NEAR to a NEAR account (ecrecover in the real runtime)
    let owner = evm_owner();
    let before = ctx.balance(&carol).await?;
    let xfer = json!({"TransferNear": {"to": carol, "amount": near(2.0).to_string()}});
    let text: String = ctx.view(&gw, "wallet_message", json!({"owner": owner, "action": xfer, "nonce": 0, "expires_ms": exp})).await?;
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, xfer.clone(), 0, exp, sign(&sol_owner(), &text)), 0, 300, "EVM signature").await?;
    ctx.call(&gw, &relayer, "wallet_action", call(&owner, xfer.clone(), 0, exp, sign(&owner, &text)), 0, 300).await?.assert_success();
    sleep(3000).await;
    assert_eq!(ctx.balance(&carol).await? - before, near(2.0), "carol received exactly what the EVM wallet sent");
    let w: Value = ctx.view(&gw, "get_wallet", json!({"owner": owner})).await?;
    assert_eq!(s(&w["near"]), net - near(2.0) - fee);

    // ---- a transfer the NEAR account refuses (it does not exist) puts the amount back; the relay fee stays paid
    let ghost = json!({"TransferNear": {"to": "ghost.sandbox", "amount": near(1.0).to_string()}});
    let text: String = ctx.view(&gw, "wallet_message", json!({"owner": owner, "action": ghost, "nonce": 1, "expires_ms": exp})).await?;
    ctx.call(&gw, &relayer, "wallet_action", call(&owner, ghost, 1, exp, sign(&owner, &text)), 0, 300).await?.assert_success();
    sleep(4000).await;
    let w: Value = ctx.view(&gw, "get_wallet", json!({"owner": owner})).await?;
    assert_eq!((s(&w["near"]), w["nonce"].as_u64()), (net - near(2.0) - 2 * fee, Some(2)), "amount restored, second fee paid");

    // ---- a wallet hires a coin's brain; a refused job's escrow comes back to the wallet, once
    let owner = sol_owner();
    let w0 = s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]);
    let hire = |prompt: &str| json!({"Hire": {"ticker": "pepe", "kind": "roast", "target": "BATMAN", "prompt": prompt, "amount": near(1.0).to_string()}});
    for (nonce, prompt) in [(1u64, "roast him gently"), (2u64, "MOCK_REFUSE roast him")] {
        let a = hire(prompt);
        let text: String = ctx.view(&gw, "wallet_message", json!({"owner": owner, "action": a, "nonce": nonce, "expires_ms": exp})).await?;
        assert!(text.contains("action: hire:pepe:roast:BATMAN:"), "{text}");
        ctx.call(&gw, &relayer, "wallet_action", call(&owner, a, nonce, exp, sign(&owner, &text)), 0, 300).await?.assert_success();
        sleep(5000).await;
    }
    let w1 = s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]);
    assert_eq!(w0 - w1, 2 * (near(1.0) + fee), "two hires and two relay fees left the balance");
    for id in [1u64, 2] {
        let h: Value = ctx.view(&gw, "get_hire", json!({"job_id": id})).await?;
        assert_eq!((h["owner"].as_str(), h["credited"].as_bool()), (Some(owner.as_str()), Some(false)), "job {id} is remembered");
        let j: Value = ctx.view(&ctx.arena, "get_job", json!({"job_id": id})).await?;
        assert_eq!(j["payer"], "gateway.sandbox", "the arena sees the gateway as payer");
    }
    let j1: Value = ctx.view(&ctx.arena, "get_job", json!({"job_id": 1})).await?;
    let j2: Value = ctx.view(&ctx.arena, "get_job", json!({"job_id": 2})).await?;
    assert_eq!(j2["status"], "Refunded", "the brain refused job 2: {j2}");
    assert_ne!(j1["status"], "Refunded", "job 1 was delivered");
    // job 1 was not refunded: nothing to credit
    ctx.call(&gw, &relayer, "claim_hire_refund", json!({"job_id": 1}), 0, 100).await?.assert_success();
    sleep(2500).await;
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]), w1, "no refund for a delivered job");
    // job 2 was refunded: the escrow returns to the wallet that paid, exactly once
    ctx.call(&gw, &relayer, "claim_hire_refund", json!({"job_id": 2}), 0, 100).await?.assert_success();
    sleep(2500).await;
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]), w1 + s(&j2["amount"]), "the refunded escrow is the wallet's again");
    ctx.fails(&gw, &relayer, "claim_hire_refund", json!({"job_id": 2}), 0, 100, "already credited").await?;
    ctx.fails(&gw, &relayer, "claim_hire_refund", json!({"job_id": 77}), 0, 100, "not hired through the gateway").await?;
    // a hire the arena refuses outright (unknown coin) gives the payment back at once
    let w2 = s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]);
    let bad = json!({"Hire": {"ticker": "nosuch", "kind": "roast", "target": "X", "prompt": "p", "amount": near(1.0).to_string()}});
    let text: String = ctx.view(&gw, "wallet_message", json!({"owner": owner, "action": bad, "nonce": 3, "expires_ms": exp})).await?;
    ctx.call(&gw, &relayer, "wallet_action", call(&owner, bad, 3, exp, sign(&owner, &text)), 0, 300).await?.assert_success();
    sleep(4000).await;
    assert_eq!(s(&ctx.view::<Value>(&gw, "get_wallet", json!({"owner": owner})).await?["near"]), w2 - fee, "the payment returned; only the relay fee is gone");

    // ---- more than the balance is refused up front
    let owner = evm_owner();
    let big = json!({"TransferNear": {"to": carol, "amount": near(50.0).to_string()}});
    let text: String = ctx.view(&gw, "wallet_message", json!({"owner": owner, "action": big, "nonce": 2, "expires_ms": exp})).await?;
    ctx.fails(&gw, &relayer, "wallet_action", call(&owner, big, 2, exp, sign(&owner, &text)), 0, 300, "not enough balance").await?;
    Ok(())
}
