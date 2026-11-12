//! Deposit verifier worker. stdin: {"chain":"solana","tx_id":"...","expected_to":"<base58>","rpc":"https://api.devnet.solana.com"}.
//! stdout: one JSON line. The worker holds no secret: everything it reads is public.
mod logic;

use logic::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};

#[derive(Deserialize)]
struct Input {
    chain: String,
    tx_id: String,
    expected_to: String,
    /// Defaults per chain: Solana devnet, Ethereum Sepolia.
    #[serde(default)]
    rpc: String,
}

#[cfg(target_arch = "wasm32")]
fn http(method: &str, url: &str, body: Option<&str>) -> Result<Value, String> {
    use std::time::Duration;
    let c = wasi_http_client::Client::new();
    let r = match body {
        Some(b) => c.post(url).header("Content-Type", "application/json").connect_timeout(Duration::from_secs(25)).body(b.as_bytes()).send(),
        None => c.get(url).header("User-Agent", "nme-deposit-verifier/0.1 (near testnet hackathon)").connect_timeout(Duration::from_secs(25)).send(),
    }
    .map_err(|e| format!("{method} {url}: {e:?}"))?;
    serde_json::from_slice(&r.body().map_err(|e| format!("body: {e:?}"))?).map_err(|e| format!("json: {e}"))
}
#[cfg(not(target_arch = "wasm32"))]
fn http(_: &str, _: &str, _: Option<&str>) -> Result<Value, String> {
    Err("outbound HTTP exists only in the wasm32-wasip2 build".into())
}

fn out(v: Value) -> ! {
    println!("{v}");
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

/// The first two independent sources that answer; they must agree within 2%.
fn price(asset: &str) -> Result<(u64, Vec<(String, u64)>), String> {
    let mut got: Vec<(String, u64)> = Vec::new();
    let mut errs: Vec<String> = Vec::new();
    for (name, url) in sources(asset) {
        match http("GET", &url, None).and_then(|v| parse_source(name, &v)) {
            Ok(p) => got.push((name.to_string(), p)),
            Err(e) => errs.push(format!("{name}: {}", e.chars().take(80).collect::<String>())),
        }
        if got.len() == 2 {
            break;
        }
    }
    if got.len() < 2 {
        return Err(format!("fewer than two {asset} price sources answered ({})", errs.join("; ")));
    }
    Ok((agree(got[0].1, got[1].1)?, got))
}

fn evm_rpc(rpc: &str, method: &str, params: Value) -> Result<Value, String> {
    let v = http("POST", rpc, Some(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string()))?;
    if v.get("error").is_some() {
        return Err(format!("evm rpc {method}: {}", v["error"]));
    }
    Ok(v["result"].clone())
}

fn run_evm(i: Input) -> Result<Value, String> {
    let rpc = if i.rpc.is_empty() { "https://ethereum-sepolia-rpc.publicnode.com".to_string() } else { i.rpc.clone() };
    let tx = evm_rpc(&rpc, "eth_getTransactionByHash", json!([i.tx_id]))?;
    let receipt = evm_rpc(&rpc, "eth_getTransactionReceipt", json!([i.tx_id]))?;
    let fin = evm_rpc(&rpc, "eth_getBlockByNumber", json!(["finalized", false]))?;
    let finalized = fin["number"].as_str().and_then(|s| s.strip_prefix("0x")).and_then(|s| u64::from_str_radix(s, 16).ok()).ok_or("no finalized block")?;
    let d = parse_evm_deposit(&tx, &receipt, finalized, &i.expected_to)?;
    let (eth, eth_src) = price("eth")?;
    let (near, near_src) = price("near")?;
    let yocto = wei_to_near_yocto(d.wei, eth, near)?;
    Ok(json!({
        "ok": true, "chain": "evm", "tx_id": i.tx_id, "expected_to": i.expected_to,
        "raw_amount": d.wei.to_string(), "sender": d.sender, "block": d.block, "finalized_block": finalized,
        "eth_usd_micro": eth, "near_usd_micro": near, "near_yocto": yocto.to_string(),
        "sources": {"eth": eth_src, "near": near_src},
    }))
}

fn run(i: Input) -> Result<Value, String> {
    match i.chain.as_str() {
        "solana" => run_solana(i),
        "evm" => run_evm(i),
        _ => Err("only the solana and evm chains are supported".into()),
    }
}

fn run_solana(mut i: Input) -> Result<Value, String> {
    if i.rpc.is_empty() {
        i.rpc = "https://api.devnet.solana.com".into();
    }
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "getTransaction", "params": [i.tx_id, {"encoding": "jsonParsed", "commitment": "finalized", "maxSupportedTransactionVersion": 0}]});
    let tx = http("POST", &i.rpc, Some(&req.to_string()))?;
    if tx.get("error").is_some() {
        return Err(format!("solana rpc error: {}", tx["error"]));
    }
    let d = parse_deposit(&tx["result"], &i.expected_to)?;
    let (sol, sol_src) = price("sol")?;
    let (near, near_src) = price("near")?;
    let yocto = near_yocto(d.lamports, sol, near)?;
    Ok(json!({
        "ok": true, "chain": "solana", "tx_id": i.tx_id, "expected_to": i.expected_to,
        "lamports": d.lamports, "raw_amount": d.lamports.to_string(), "sender": d.sender, "slot": d.slot, "block_time": d.block_time,
        "sol_usd_micro": sol, "near_usd_micro": near, "near_yocto": yocto.to_string(),
        "sources": {"sol": sol_src, "near": near_src},
    }))
}

fn main() {
    let mut s = String::new();
    if std::io::stdin().read_to_string(&mut s).is_err() {
        out(json!({"ok": false, "error": "cannot read input"}));
    }
    match serde_json::from_str::<Input>(&s) {
        Ok(i) => match run(i) {
            Ok(v) => out(v),
            Err(e) => out(json!({"ok": false, "error": e})),
        },
        Err(e) => out(json!({"ok": false, "error": format!("bad input: {e}")})),
    }
}
