//! NME Operator worker. stdin: {"rpc": "...", "arena": "arena.nme.testnet", "account": "nme-operator.testnet", "max_tasks": 5}.
//! Secret env var: OPERATOR_KEY (a function-call-only key, `ed25519:...`). Prints one JSON line.
mod logic;

use logic::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};

#[derive(Deserialize)]
struct Input {
    rpc: String,
    arena: String,
    account: String,
    #[serde(default = "five")]
    max_tasks: u8,
}
fn five() -> u8 {
    5
}

#[cfg(target_arch = "wasm32")]
fn post(url: &str, body: &str) -> Result<Value, String> {
    use std::time::Duration;
    let r = wasi_http_client::Client::new()
        .post(url)
        .header("Content-Type", "application/json")
        .connect_timeout(Duration::from_secs(30))
        .body(body.as_bytes())
        .send()
        .map_err(|e| format!("http: {e:?}"))?;
    serde_json::from_slice(&r.body().map_err(|e| format!("body: {e:?}"))?).map_err(|e| format!("json: {e}"))
}
#[cfg(not(target_arch = "wasm32"))]
fn post(_: &str, _: &str) -> Result<Value, String> {
    Err("outbound HTTP exists only in the wasm32-wasip2 build".into())
}

fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let v = post(url, &json!({"jsonrpc": "2.0", "id": "op", "method": method, "params": params}).to_string())?;
    if let Some(e) = v.get("error") {
        return Err(format!("{method}: {}", e.to_string().chars().take(300).collect::<String>()));
    }
    Ok(v["result"].clone())
}

fn out(v: Value) -> ! {
    println!("{v}");
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

fn run(i: Input) -> Result<Value, String> {
    let key = parse_secret(&std::env::var("OPERATOR_KEY").map_err(|_| "OPERATOR_KEY is not set".to_string())?)?;
    let pk_str = format!("ed25519:{}", bs58::encode(key.verifying_key().to_bytes()).into_string());

    // 1. is anything due?
    let b = rpc(&i.rpc, "block", json!({"finality": "final"}))?;
    let now_ms = b["header"]["timestamp"].as_u64().ok_or("no block timestamp")? / 1_000_000;
    let block_hash = parse_block_hash(b["header"]["hash"].as_str().ok_or("no block hash")?)?;
    let r = rpc(&i.rpc, "query", json!({"request_type": "call_function", "finality": "final", "account_id": i.arena, "method_name": "next_due_ms", "args_base64": "e30="}))?;
    let bytes: Vec<u8> = r["result"].as_array().ok_or("no result")?.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect();
    let next: Option<u64> = serde_json::from_slice(&bytes).map_err(|e| format!("next_due_ms: {e}"))?;
    if !due(next, now_ms) {
        return Ok(json!({"ok": true, "ticked": false, "next_due_ms": next, "now_ms": now_ms}));
    }

    // 2. sign and send arena.tick
    let ak = rpc(&i.rpc, "query", json!({"request_type": "view_access_key", "finality": "final", "account_id": i.account, "public_key": pk_str}))?;
    let nonce = ak["nonce"].as_u64().ok_or("no nonce")? + 1;
    let args = json!({"max_tasks": i.max_tasks.clamp(1, 8)}).to_string();
    let (signed, hash) = sign_tx(&key, &i.account, nonce, &i.arena, &block_hash, args.as_bytes());
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&signed);
    let res = rpc(&i.rpc, "send_tx", json!({"signed_tx_base64": b64, "wait_until": "EXECUTED_OPTIMISTIC"}))?;
    let status = res["final_execution_status"].as_str().unwrap_or("").to_string();
    let ok = res["status"].get("SuccessValue").is_some() || res["status"].get("SuccessReceiptId").is_some();
    let mut tick = Value::Null;
    for o in res["receipts_outcome"].as_array().into_iter().flatten() {
        for l in o["outcome"]["logs"].as_array().into_iter().flatten() {
            if let Some(s) = l.as_str() {
                if s.contains("\"event\":\"tick\"") {
                    tick = serde_json::from_str::<Value>(s.trim_start_matches("EVENT_JSON:")).unwrap_or(Value::Null);
                }
            }
        }
    }
    Ok(json!({"ok": ok, "ticked": true, "tx": hash, "nonce": nonce, "status": status, "next_due_ms": next, "now_ms": now_ms, "tick_event": tick}))
}

fn main() {
    let mut s = String::new();
    if std::io::stdin().read_to_string(&mut s).is_err() {
        out(json!({"ok": false, "error": "cannot read input"}));
    }
    let i: Input = match serde_json::from_str(&s) {
        Ok(i) => i,
        Err(e) => out(json!({"ok": false, "error": format!("bad input: {e}")})),
    };
    match run(i) {
        Ok(v) => out(v),
        Err(e) => out(json!({"ok": false, "error": e})),
    }
}
