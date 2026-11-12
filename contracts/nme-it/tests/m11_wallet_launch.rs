//! D-59: a wallet with no NEAR account launches a coin by signing a message, and its PumpBag stays at the gateway under the wallet's
//! key. The real factory, coin, DCL, crown and vault run; the signatures go through the real host functions.
mod common;
use common::*;
use near_api::types::transaction::result::ExecutionFinalResult;
use near_api::AccountId;
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
fn sign(owner: &str, text: &str) -> String {
    if owner.starts_with("sol:") {
        use ed25519_dalek::Signer;
        b64(&sol_sk().sign(text.as_bytes()).to_bytes())
    } else {
        let mut data = format!("\x19Ethereum Signed Message:\n{}", text.len()).into_bytes();
        data.extend_from_slice(text.as_bytes());
        let (sig, rec) = evm_sk().sign_prehash_recoverable(&keccak(&data)).unwrap();
        let mut v = sig.to_bytes().to_vec();
        v.push(27 + rec.to_byte());
        b64(&v)
    }
}
fn b64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}
fn sol_tx(tag: &str) -> String {
    let h = keccak(tag.as_bytes());
    let mut b = h.clone();
    b.extend(keccak(&h));
    near_sdk::bs58::encode(b).into_string()
}
fn evm_tx(tag: &str) -> String {
    format!("0x{}", keccak(tag.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>())
}
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

struct W<'a> {
    ctx: &'a Ctx,
    gw: AccountId,
    relayer: AccountId,
}

impl W<'_> {
    async fn near(&self, owner: &str) -> testresult::TestResult<u128> {
        Ok(s(&self.ctx.view::<Value>(&self.gw, "get_wallet", json!({"owner": owner})).await?["near"]))
    }
    async fn nonce(&self, owner: &str) -> testresult::TestResult<u64> {
        Ok(self.ctx.view::<Value>(&self.gw, "get_wallet", json!({"owner": owner})).await?["nonce"].as_u64().unwrap())
    }
    async fn tokens(&self, owner: &str, token: &AccountId) -> testresult::TestResult<u128> {
        Ok(s(&self.ctx.view::<Value>(&self.gw, "get_wallet_tokens", json!({"owner": owner, "token": token})).await?["tokens"]))
    }
    async fn due(&self, owner: &str, token: &AccountId) -> testresult::TestResult<u128> {
        Ok(s(&self.ctx.view::<Value>(&self.gw, "get_wallet_tokens", json!({"owner": owner, "token": token})).await?["wnear_due"]))
    }
    /// Signs `action` with the next nonce and submits it (anyone may). Returns once submitted; `settle` waits for the chain.
    async fn submit(&self, owner: &str, action: Value) -> testresult::TestResult<ExecutionFinalResult> {
        let n = self.nonce(owner).await?;
        let exp = now_ms() + 3_600_000;
        let text: String = self.ctx.view(&self.gw, "wallet_message", json!({"owner": owner, "action": action, "nonce": n, "expires_ms": exp})).await?;
        self.ctx.call(&self.gw, &self.relayer, "wallet_action", json!({"owner": owner, "action": action, "nonce": n, "expires_ms": exp, "signature": sign(owner, &text)}), 0, 300).await
    }
    async fn do_ok(&self, owner: &str, action: Value) -> testresult::TestResult<()> {
        let r = self.submit(owner, action).await?;
        r.assert_success(); // a refusal inside the arena is a failed downstream receipt, not a failed wallet_action
        self.settle().await
    }
    async fn do_fails(&self, owner: &str, action: Value, needle: &str) -> testresult::TestResult<()> {
        let n = self.nonce(owner).await?;
        let exp = now_ms() + 3_600_000;
        let text: String = self.ctx.view(&self.gw, "wallet_message", json!({"owner": owner, "action": action, "nonce": n, "expires_ms": exp})).await?;
        self.ctx.fails(&self.gw, &self.relayer, "wallet_action", json!({"owner": owner, "action": action, "nonce": n, "expires_ms": exp, "signature": sign(owner, &text)}), 0, 300, needle).await
    }
    /// Waits until no token operation runs, then for the refund receipts that trail it.
    async fn settle(&self) -> testresult::TestResult<()> {
        sleep(1500).await;
        for _ in 0..60 {
            let l: Value = self.ctx.view(&self.gw, "get_lock", json!({})).await?;
            if l.is_null() {
                break;
            }
            sleep(700).await;
        }
        sleep(2500).await;
        Ok(())
    }
    async fn fund(&self, owner: &str, tag: &str, near_amount: f64) -> testresult::TestResult<()> {
        let tx = if owner.starts_with("sol:") { sol_tx(tag) } else { evm_tx(tag) };
        let outlayer: AccountId = "outlayer.sandbox".parse()?;
        self.ctx.call(&outlayer, &self.relayer, "set_deposit", json!({"tx_id": tx, "near_yocto": near(near_amount).to_string()}), 0, 30).await?.assert_success();
        self.ctx.call(&self.gw, &self.relayer, "claim_wallet_deposit", json!({"tx_id": tx, "owner": owner}), 0, 300).await?.assert_success();
        sleep(6000).await;
        Ok(())
    }
}


fn create_action(args: &Value, deposit: u128) -> Value {
    json!({"CreateCoin": {"args_json": args.to_string(), "deposit": deposit.to_string()}})
}
fn tk(name: &str, t: &str) -> Value {
    json!({name: {"ticker": t}})
}

async fn wait_coin(ctx: &Ctx, ticker: &str) -> testresult::TestResult<AccountId> {
    for _ in 0..90 {
        let c: Value = ctx.view(&ctx.factory, "get_coin", json!({"ticker": ticker})).await?;
        if !c.is_null() {
            break;
        }
        sleep(1000).await;
    }
    let coin: AccountId = format!("{ticker}.{}", ctx.factory).parse()?;
    sleep(3000).await;
    Ok(coin)
}

#[tokio::test]
async fn a_wallet_launches_a_coin_and_keeps_its_pumpbag_under_its_own_signature() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let gw: AccountId = "gateway.sandbox".parse()?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 300).await?;
    let relayer = ctx.account("relayer.sandbox", 50).await?;
    let w = W { ctx: &ctx, gw: gw.clone(), relayer: relayer.clone() };
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    ctx.ok(&gw, &carol, "fund_reserve", json!({}), 80_000, 30).await?;
    let (sol, evm) = (sol_owner(), evm_owner());
    w.fund(&sol, "sol-1", 8.0).await?;
    w.fund(&evm, "evm-1", 8.0).await?;
    let net = near(8.0) - near(8.0) / 10_000 * 200;
    let fee = near(0.001);

    // ================= launch: the deposit is the factory's quote, the rest of the payment stays with the owner
    let args = Ctx::create_args("frogx", Ctx::launch_params(92_000, 40_000, None))["args"].clone();
    let quote = s(&ctx.view::<Value>(&ctx.factory, "required_deposit", json!({"icon_len": args["icon_data_url"].as_str().unwrap().len(), "first_buy_max_near": null})).await?);
    w.do_ok(&sol, create_action(&args, quote)).await?;
    let coin = wait_coin(&ctx, "frogx").await?;
    ctx.finish_launch(&coin, &carol).await?;
    sleep(3000).await;
    assert_eq!(w.near(&sol).await?, net - quote - fee, "exactly the quote and the relay fee left");
    assert_eq!(ctx.view::<Value>(&gw, "get_crown_owner", json!({"ticker": "frogx"})).await?, json!(sol));
    let nft: Value = ctx.view(&ctx.crown, "nft_token", json!({"token_id": "frogx"})).await?;
    assert_eq!(nft["owner_id"], "gateway.sandbox", "the gateway is the creator and holds the NFT");
    let cfg: Value = ctx.view(&coin, "get_config", json!({})).await?;
    assert_eq!(cfg["creator"], "gateway.sandbox");
    assert_eq!(ctx.key_count(&coin).await?, 0, "the coin has no access keys (I2)");

    // ================= nobody but the recorded owner can touch the PumpBag, and a ticker is launched through the gateway once
    for a in [tk("CrownHeartbeat", "frogx"), tk("CrownClaimFees", "frogx"), tk("CrownClaimVested", "frogx"), json!({"CrownSend": {"ticker": "frogx", "to": "carol.sandbox"}})] {
        w.do_fails(&evm, a, "holds no PumpBag").await?;
    }
    w.do_fails(&evm, create_action(&args, quote), "already launched").await?;
    w.do_fails(&sol, json!({"CrownSend": {"ticker": "frogx", "to": "gateway.sandbox"}}), "another account").await?;

    // ================= heartbeat (signed), then fees with nothing accrued credit nothing and free the lock
    let hb0: Value = ctx.view(&ctx.crown, "crown_status", json!({"token_id": "frogx"})).await?;
    sleep(2500).await;
    let n0 = w.near(&sol).await?;
    w.do_ok(&sol, tk("CrownHeartbeat", "frogx")).await?;
    let hb1: Value = ctx.view(&ctx.crown, "crown_status", json!({"token_id": "frogx"})).await?;
    assert!(hb1["last_heartbeat_ms"].as_u64() > hb0["last_heartbeat_ms"].as_u64(), "the heartbeat reached the crown");
    assert_eq!(w.near(&sol).await?, n0 - fee, "a heartbeat costs the relay fee");
    let n1 = w.near(&sol).await?;
    w.do_ok(&sol, tk("CrownClaimFees", "frogx")).await?;
    assert_eq!(w.near(&sol).await?, n1, "nothing accrued, nothing credited, no fee on a claim");
    assert!(ctx.view::<Value>(&gw, "get_lock", json!({})).await?.is_null());

    // ================= vested tokens: credited by the exact amount the vault sent
    ctx.call_yocto(&gw, &carol, "register_token", json!({"token": coin}), near(0.00125), 60).await?.assert_success();
    ctx.fast_forward(120).await?;
    let gw_before = ctx.ft_balance(&coin, &gw).await?;
    w.do_ok(&sol, tk("CrownClaimVested", "frogx")).await?;
    let got = w.tokens(&sol, &coin).await?;
    assert!(got > 0, "vested tokens were credited");
    assert_eq!(ctx.ft_balance(&coin, &gw).await? - gw_before, got, "the credit equals what the gateway received");
    assert_eq!(w.tokens(&evm, &coin).await?, 0);

    // ================= a ticker the factory refuses: the payment comes back through settle_launch (anyone), once
    let taken = Ctx::create_args("pepe", Ctx::launch_params(92_000, 40_000, None))["args"].clone();
    let before = w.near(&evm).await?;
    w.do_ok(&evm, create_action(&taken, quote)).await?;
    sleep(4000).await;
    assert!(ctx.view::<Value>(&gw, "get_launch", json!({"ticker": "pepe"})).await?.is_object(), "recorded until settled");
    ctx.ok(&gw, &carol, "settle_launch", json!({"ticker": "pepe"}), 0, 100).await?;
    sleep(4000).await;
    assert_eq!(w.near(&evm).await?, before - fee, "everything but the relay fee is back");
    assert!(ctx.view::<Value>(&gw, "get_launch", json!({"ticker": "pepe"})).await?.is_null());
    assert!(ctx.view::<Value>(&gw, "get_crown_owner", json!({"ticker": "pepe"})).await?.is_null());
    ctx.fails(&gw, &carol, "settle_launch", json!({"ticker": "pepe"}), 0, 100, "no such launch").await?;

    // ================= a deposit below the factory's price is refused by the factory and comes back through settle_launch
    let evm_before = w.near(&evm).await?;
    let small = Ctx::create_args("tiny", Ctx::launch_params(92_000, 40_000, None))["args"].clone();
    w.do_ok(&evm, create_action(&small, quote / 2)).await?;
    sleep(3000).await;
    ctx.ok(&gw, &carol, "settle_launch", json!({"ticker": "tiny"}), 0, 100).await?;
    sleep(4000).await;
    assert_eq!(w.near(&evm).await?, evm_before - fee, "the deposit came back, the relay fee stayed");
    assert!(ctx.view::<Value>(&gw, "get_launch", json!({"ticker": "tiny"})).await?.is_null());

    // ================= sending the PumpBag releases it for good
    w.do_ok(&sol, json!({"CrownSend": {"ticker": "frogx", "to": "carol.sandbox"}})).await?;
    let nft: Value = ctx.view(&ctx.crown, "nft_token", json!({"token_id": "frogx"})).await?;
    assert_eq!(nft["owner_id"], "carol.sandbox");
    assert!(ctx.view::<Value>(&gw, "get_crown_owner", json!({"ticker": "frogx"})).await?.is_null());
    w.do_fails(&sol, tk("CrownHeartbeat", "frogx"), "holds no PumpBag").await?;
    Ok(())
}
