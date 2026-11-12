//! D-57, token actions: a wallet with no NEAR account buys, joins a war, sells and moves tokens by signing messages. The
//! arena credits results back to the gateway ledger (`credit_near`, `credit_tokens`); the real DCL and wNEAR contracts run,
//! the signatures go through the real `ed25519_verify` / `ecrecover` host functions. OutLayer / MPC are the sandbox mock.
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

fn buy(key: &str, amount: f64, min_out: &str) -> Value {
    json!({"Buy": {"key": key, "amount": near(amount).to_string(), "min_out": min_out}})
}
fn join(key: &str, amount: f64) -> Value {
    json!({"JoinWar": {"key": key, "amount": near(amount).to_string(), "min_out": "1"}})
}
fn sell(token: &AccountId, amount: u128, min_out: &str) -> Value {
    json!({"Sell": {"token": token, "amount": amount.to_string(), "min_out": min_out}})
}

#[tokio::test]
async fn wallets_buy_join_wars_sell_and_move_tokens_with_exact_accounting() -> testresult::TestResult<()> {
    let ctx = Ctx::setup_autonomous().await?;
    let gw: AccountId = "gateway.sandbox".parse()?;
    let alice = ctx.account("alice.sandbox", 100).await?;
    let carol = ctx.account("carol.sandbox", 300).await?;
    let relayer = ctx.account("relayer.sandbox", 50).await?;
    let w = W { ctx: &ctx, gw: gw.clone(), relayer: relayer.clone() };
    let pepe = ctx.create_coin(&alice, "pepe", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&pepe, &carol).await?;
    let frog = ctx.create_coin(&alice, "frog", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&frog, &carol).await?;
    ctx.ok(&gw, &carol, "fund_reserve", json!({}), 80_000, 30).await?;
    let (sol, evm) = (sol_owner(), evm_owner());

    // The gateway must be registered on wNEAR (sale proceeds arrive as wNEAR) and on each token it receives (NEP-145). Anyone can do it.
    ctx.call_yocto(&gw, &carol, "register_token", json!({"token": ctx.wrap}), near(0.00125), 60).await?.assert_success();
    ctx.call_yocto(&gw, &carol, "register_token", json!({"token": pepe}), near(0.00125), 60).await?.assert_success();
    ctx.call_yocto(&gw, &carol, "register_token", json!({"token": frog}), near(0.00125), 60).await?.assert_success();
    // a stranger cannot register with a big deposit
    ctx.fails(&gw, &carol, "register_token", json!({"token": pepe}), 100, 60, "between 0 and 0.01").await?;

    w.fund(&sol, "sol-1", 10.0).await?;
    w.fund(&evm, "evm-1", 10.0).await?;
    let net = near(10.0) - near(10.0) / 10_000 * 200;
    assert_eq!((w.near(&sol).await?, w.near(&evm).await?), (net, net));

    // ================= Buy: tokens are credited to the signer, NEAR leaves by exactly the amount (no relay fee on a buy)
    let gw_pepe0 = ctx.ft_balance(&pepe, &gw).await?;
    w.do_ok(&sol, buy("pepe", 2.0, "1")).await?;
    let t_sol = w.tokens(&sol, &pepe).await?;
    assert!(t_sol > 0, "the arena credited the tokens it measured");
    assert_eq!(w.near(&sol).await?, net - near(2.0), "exactly the amount, no relay fee");
    assert_eq!(ctx.ft_balance(&pepe, &gw).await? - gw_pepe0, t_sol, "the credit equals the gateway's token balance difference");
    assert_eq!(w.tokens(&evm, &pepe).await?, 0, "another wallet is untouched");
    assert!(ctx.view::<Value>(&gw, "get_lock", json!({})).await?.is_null(), "the lock ended");

    w.do_ok(&evm, buy("pepe", 1.0, "1")).await?;
    let t_evm = w.tokens(&evm, &pepe).await?;
    assert!(t_evm > 0 && t_evm != t_sol);
    assert_eq!(w.tokens(&sol, &pepe).await?, t_sol, "the first wallet's tokens did not move");
    assert_eq!(ctx.ft_balance(&pepe, &gw).await? - gw_pepe0, t_sol + t_evm, "ledger sum == gateway balance");

    // ---- refusals that must give everything back and free the lock
    // (a) unknown coin: the arena refuses the call, the gateway restores
    let before = w.near(&sol).await?;
    w.do_ok(&sol, buy("nosuch", 1.0, "1")).await?;
    assert_eq!(w.near(&sol).await?, before, "unknown coin: the full amount is back");
    // (b) below the arena's minimum buy
    w.do_ok(&sol, json!({"Buy": {"key": "pepe", "amount": "1000", "min_out": "1"}})).await?;
    assert_eq!(w.near(&sol).await?, before, "dust: refused, restored");
    // (c) a min_out the pool cannot meet: DCL refuses, the whole NEAR returns through credit_near, no tokens, lock ends
    w.do_ok(&sol, buy("pepe", 1.0, "1000000000000000000000000000000")).await?;
    assert_eq!(w.near(&sol).await?, before, "impossible min_out: full refund (no fee kept)");
    assert_eq!(w.tokens(&sol, &pepe).await?, t_sol);
    assert_eq!(ctx.ft_balance(&pepe, &gw).await? - gw_pepe0, t_sol + t_evm);
    // (d) the gateway is not registered on this token: the arena sees an unregistered buyer and refunds
    let toad = ctx.create_coin(&alice, "toad", Ctx::launch_params(92_000, 40_000, None)).await?;
    ctx.finish_launch(&toad, &carol).await?;
    w.do_ok(&sol, buy("toad", 1.0, "1")).await?;
    assert_eq!(w.near(&sol).await?, before, "unregistered on the token: refunded");
    assert_eq!(w.tokens(&sol, &toad).await?, 0);
    // (e) JoinWar with no war open
    w.do_ok(&sol, join("pepe", 1.0)).await?;
    assert_eq!(w.near(&sol).await?, before, "no war: the join is refused and restored");
    // (f) more than the balance
    w.do_fails(&sol, buy("pepe", 50.0, "1"), "not enough balance").await?;
    // (g) a key with a separator cannot be signed into the text
    w.do_fails(&sol, json!({"Buy": {"key": "pe:pe", "amount": near(1.0).to_string(), "min_out": "1"}}), "plain text").await?;

    // ================= JoinWar: every wallet is its own fighter
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "pepe"}), 4000, 30).await?;
    ctx.ok(&ctx.arena, &carol, "fund_chest", json!({"key": "frog"}), 4000, 30).await?;
    ctx.call(&ctx.arena, &carol, "start_round", json!({}), 100, 100).await?.assert_success();
    let round: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    let a_is_pepe = round["pairs"][0]["a"] == "pepe";
    let side = if a_is_pepe { ("unique_a", "score_a") } else { ("unique_b", "score_b") };
    let (fs, fe) = (ctx.view::<String>(&ctx.arena, "wallet_key", json!({"owner": sol})).await?, ctx.view::<String>(&ctx.arena, "wallet_key", json!({"owner": evm})).await?);
    assert_ne!(fs, fe);
    assert_ne!(fs, "gateway.sandbox");
    w.do_ok(&sol, join("pepe", 1.0)).await?;
    w.do_ok(&evm, join("pepe", 1.0)).await?;
    let r: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    assert_eq!(r["pairs"][0][side.0], 2, "two wallets behind one gateway account count as two fighters: {r}");
    let (score2, bought_sol) = (s(&r["pairs"][0][side.1]), w.tokens(&sol, &pepe).await?);
    assert!(score2 > 0);
    // selling everything through the gateway cancels that fighter's score (and only that fighter's)
    w.do_ok(&sol, sell(&pepe, bought_sol, "1")).await?;
    let r: Value = ctx.view(&ctx.arena, "get_round", json!({"id": 1})).await?;
    assert_eq!(r["pairs"][0][side.0], 1, "the seller dropped out; the other wallet still scores: {r}");
    assert!(s(&r["pairs"][0][side.1]) < score2);
    assert_eq!(w.tokens(&sol, &pepe).await?, 0);

    // ================= Sell: exact proceeds, no relay fee, tokens debited first
    let near0 = w.near(&evm).await?;
    let t0 = w.tokens(&evm, &pepe).await?;
    let wn0 = ctx.ft_balance(&ctx.wrap, &gw).await?;
    let sold = t0 / 2;
    w.do_ok(&evm, sell(&pepe, sold, "1")).await?;
    assert_eq!(w.tokens(&evm, &pepe).await?, t0 - sold, "exactly the amount left the token balance");
    let got = w.near(&evm).await? - near0;
    assert!(got > 0 && got < near(1.0), "proceeds credited: {got}");
    assert_eq!(w.due(&evm, &pepe).await?, 0, "nothing left owed");
    assert_eq!(ctx.ft_balance(&ctx.wrap, &gw).await?, wn0, "all received wNEAR was unwrapped into the ledger");
    assert!(ctx.view::<Value>(&gw, "get_lock", json!({})).await?.is_null());
    // a sale the pool refuses (impossible min_out): the tokens come back, nothing is credited
    let (n1, t1) = (w.near(&evm).await?, w.tokens(&evm, &pepe).await?);
    w.do_ok(&evm, sell(&pepe, t1, "1000000000000000000000000000000")).await?;
    assert_eq!((w.near(&evm).await?, w.tokens(&evm, &pepe).await?), (n1, t1), "refused sale: tokens returned, no NEAR");
    // more tokens than owned; a token the wallet does not own
    w.do_fails(&evm, sell(&pepe, t1 + 1, "1"), "not enough tokens").await?;
    w.do_fails(&evm, sell(&frog, 1, "1"), "not enough tokens").await?;
    // nobody but the gateway can sell "for" a wallet: a plain user's owner-tagged transfer is refused and the tokens return
    ctx.call_yocto(&pepe, &carol, "storage_deposit", json!({"account_id": carol, "registration_only": true}), near(0.00125), 30).await?.assert_success();
    ctx.call(&ctx.arena, &carol, "buy", json!({"key": "pepe", "min_out": "1"}), 500, 300).await?.assert_success();
    sleep(2500).await;
    let c0 = ctx.ft_balance(&pepe, &carol).await?;
    assert!(c0 > 0);
    let msg = json!({"sell": {"min_out": "1", "owner": evm}}).to_string();
    let _ = ctx.call_yocto(&pepe, &carol, "ft_transfer_call", json!({"receiver_id": ctx.arena, "amount": (c0 / 2).to_string(), "msg": msg}), 1, 300).await?;
    sleep(2500).await;
    assert_eq!(ctx.ft_balance(&pepe, &carol).await?, c0, "an owner-tagged sale from anyone but the gateway returns the tokens");

    // ================= TransferTokens to a NEAR account (must be registered on the token)
    let t2 = w.tokens(&evm, &pepe).await?;
    let c1 = ctx.ft_balance(&pepe, &carol).await?;
    w.do_ok(&evm, json!({"TransferTokens": {"token": pepe, "to": carol, "amount": (t2 / 2).to_string()}})).await?;
    assert_eq!(ctx.ft_balance(&pepe, &carol).await? - c1, t2 / 2);
    assert_eq!(w.tokens(&evm, &pepe).await?, t2 - t2 / 2);
    // an unregistered destination: ft_transfer fails, the tokens are restored
    let bob = ctx.account("bob.sandbox", 5).await?;
    let t3 = w.tokens(&evm, &pepe).await?;
    w.do_ok(&evm, json!({"TransferTokens": {"token": pepe, "to": bob, "amount": t3.to_string()}})).await?;
    assert_eq!(w.tokens(&evm, &pepe).await?, t3, "unregistered recipient: tokens restored");
    assert_eq!(w.near(&evm).await?, near0 + got - 0, "token actions never touch the NEAR balance or charge a fee");

    // ---- the gateway's whole token balance is exactly what the ledgers say
    let ledger = w.tokens(&sol, &pepe).await? + w.tokens(&evm, &pepe).await?;
    let on_chain = ctx.ft_balance(&pepe, &gw).await? - gw_pepe0;
    assert_eq!(on_chain, ledger, "no token is unaccounted for");
    Ok(())
}
