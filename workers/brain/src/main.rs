//! NME brain worker. stdin: one JSON `Input`; stdout: one JSON line. Secrets arrive as env vars (NEAR_AI_API_KEY).
//! The key is never printed, logged or returned.
mod chain;
mod logic;

use logic::*;
use std::io::{Read, Write};

/// OutLayer kills the run at 90 s and charges a penalty. Every network wait is capped by what is left of a 75 s budget,
/// so a stalled step ends the job with a clear error (and a refund) instead.
mod budget {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    static START: OnceLock<Instant> = OnceLock::new();
    pub const TOTAL: Duration = Duration::from_secs(75);
    pub fn start() {
        let _ = START.set(Instant::now());
    }
    pub fn left() -> Duration {
        TOTAL.saturating_sub(START.get_or_init(Instant::now).elapsed())
    }
    /// The wait for one request: its own cap, but never past the budget. Err when nothing is left.
    pub fn cap(max: Duration) -> Result<Duration, String> {
        let l = left();
        if l < Duration::from_secs(2) {
            return Err("time budget used up (the job took too long)".into());
        }
        Ok(max.min(l))
    }
}

#[cfg(target_arch = "wasm32")]
mod http {
    use std::time::Duration;
    use wasi_http_client::Client;

    pub fn get(url: &str) -> Result<(u16, Vec<u8>), String> {
        let r = Client::new().get(url).connect_timeout(crate::budget::cap(Duration::from_secs(10))?).send().map_err(|e| format!("http: {e:?}"))?;
        let status = r.status();
        Ok((status, r.body().map_err(|e| format!("body: {e:?}"))?))
    }

    pub fn post_plain(url: &str, body: &str) -> Result<(u16, Vec<u8>), String> {
        let r = Client::new().post(url).header("Content-Type", "application/json").connect_timeout(crate::budget::cap(Duration::from_secs(15))?).body(body.as_bytes()).send().map_err(|e| format!("http: {e:?}"))?;
        let status = r.status();
        Ok((status, r.body().map_err(|e| format!("body: {e:?}"))?))
    }

    pub fn post_json(url: &str, bearer: &str, body: &str) -> Result<(u16, Vec<u8>), String> {
        let r = Client::new()
            .post(url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {bearer}"))
            .connect_timeout(crate::budget::cap(Duration::from_secs(40))?)
            .body(body.as_bytes())
            .send()
            .map_err(|e| format!("http: {e:?}"))?;
        let status = r.status();
        Ok((status, r.body().map_err(|e| format!("body: {e:?}"))?))
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod http {
    pub fn get(_: &str) -> Result<(u16, Vec<u8>), String> {
        Err("outbound HTTP exists only in the wasm32-wasip2 build".into())
    }
    pub fn post_json(_: &str, _: &str, _: &str) -> Result<(u16, Vec<u8>), String> {
        Err("outbound HTTP exists only in the wasm32-wasip2 build".into())
    }
    pub fn post_plain(_: &str, _: &str) -> Result<(u16, Vec<u8>), String> {
        Err("outbound HTTP exists only in the wasm32-wasip2 build".into())
    }
}

fn rpc(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let (status, body) = http::post_plain(url, &serde_json::json!({"jsonrpc": "2.0", "id": "m", "method": method, "params": params}).to_string())?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("rpc json: {e}"))?;
    if !(200..300).contains(&status) || v.get("error").is_some() {
        return Err(format!("rpc {method}: {}", v["error"].to_string().chars().take(160).collect::<String>()));
    }
    Ok(v["result"].clone())
}

/// Generates the picture, stores it on FastFS with the function-call-only media key, and checks it is really served.
/// Returns (url, sha256 hex).
fn make_meme(key: &str, rpc_url: &str, image_prompt: &str, stage: Option<u8>) -> Result<(String, String), String> {
    let stop = |n: u8, what: String| -> Result<(), String> { if stage == Some(n) { Err(format!("stage {n} reached: {what}")) } else { Ok(()) } };
    use base64::Engine;
    // The image API is slower and flakier from the enclave than from a laptop: try up to three times.
    let (mut status, mut body) = (0u16, Vec::new());
    let mut last_err = String::new();
    for _ in 0..3 {
        if budget::left() < std::time::Duration::from_secs(30) && !last_err.is_empty() {
            break; // not enough time left to draw again and still upload
        }
        match http::post_json(&format!("{NEAR_AI_BASE}/images/generations"), key, &image_request(image_prompt)) {
            Ok((st, b)) if (200..300).contains(&st) => {
                status = st;
                body = b;
                break;
            }
            Ok((st, _)) => last_err = format!("image model returned HTTP {st}"),
            Err(e) => last_err = e,
        }
    }
    if !(200..300).contains(&status) {
        return Err(last_err);
    }
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("image json: {e}"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(v["data"][0]["b64_json"].as_str().ok_or("the image model returned no picture")?).map_err(|e| format!("bad base64: {e}"))?;
    if bytes.is_empty() || bytes.len() > chain::MAX_IMAGE_BYTES {
        return Err(format!("picture size {} is outside 1..{} bytes", bytes.len(), chain::MAX_IMAGE_BYTES));
    }
    stop(1, format!("picture bytes {}", bytes.len()))?;
    let (ext, mime) = match bytes.as_slice() {
        [0xff, 0xd8, ..] => ("jpg", "image/jpeg"),
        [0x89, 0x50, ..] => ("png", "image/png"),
        [0x52, 0x49, ..] => ("webp", "image/webp"),
        _ => return Err("the picture is not a JPEG, PNG or WebP".into()),
    };
    let hash = chain::sha256_hex(&bytes);
    let path = format!("{hash}.{ext}");
    stop(2, format!("hash {hash}"))?;
    let account = std::env::var("MEDIA_ACCOUNT").map_err(|_| "MEDIA_ACCOUNT is not set".to_string())?;
    let sk = chain::parse_secret(&std::env::var("MEDIA_KEY").map_err(|_| "MEDIA_KEY is not set".to_string())?)?;
    let pk = format!("ed25519:{}", bs58::encode(sk.verifying_key().to_bytes()).into_string());
    stop(3, format!("key parsed for {account}"))?;
    let block = rpc(rpc_url, "block", serde_json::json!({"finality": "final"}))?;
    let bh = chain::parse_block_hash(block["header"]["hash"].as_str().ok_or("no block hash")?)?;
    let ak = rpc(rpc_url, "query", serde_json::json!({"request_type": "view_access_key", "finality": "final", "account_id": account, "public_key": pk}))?;
    let nonce = ak["nonce"].as_u64().ok_or("no nonce")? + 1;
    stop(4, format!("nonce {nonce}"))?;
    let signed = chain::sign_call(&sk, &account, nonce, &bh, &chain::fastfs_payload(&path, mime, &bytes));
    stop(5, format!("signed {} bytes", signed.len()))?;
    // The call is meant to run out of gas (FastFS indexes the data), so only an RPC-level refusal is an error.
    let b64 = base64::engine::general_purpose::STANDARD.encode(&signed);
    // Some keyless RPCs (drpc) refuse a ~60 KB transaction with "Temporary internal error": try the next one. A repeat of the
    // same signed transaction is harmless (same nonce: at most one is included).
    let mut sent = Err(String::from("no rpc tried"));
    for url in upload_rpcs(rpc_url) {
        if budget::left() < std::time::Duration::from_secs(8) {
            break;
        }
        sent = rpc(&url, "send_tx", serde_json::json!({"signed_tx_base64": b64, "wait_until": "INCLUDED"}));
        if sent.is_ok() {
            break;
        }
    }
    sent?;
    stop(6, "sent".into())?;
    let url = chain::fastfs_url(&account, &path);
    // FastFS serves the file shortly after the transaction is included; each GET takes a moment, so just ask again.
    let mut last = String::from("no attempt");
    for i in 0..25 {
        if budget::left() < std::time::Duration::from_secs(5) {
            break;
        }
        // A fresh query string per try, so a cached 404 cannot be served back.
        match http::get(&format!("{url}?t={i}")) {
            Ok((200, got)) if chain::sha256_hex(&got) == hash => return Ok((url, hash)),
            Ok((st, got)) => last = format!("HTTP {st}, {} bytes, sha {}", got.len(), chain::sha256_hex(&got).chars().take(10).collect::<String>()),
            Err(e) => last = e.chars().take(120).collect(),
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    Err(format!("the picture was uploaded but FastFS does not serve it (last try: {last}; {url})"))
}

fn fail(msg: &str) -> ! {
    // One JSON line even on failure, so the contract can tell "worker said no" from "worker crashed".
    println!("{}", serde_json::json!({ "ok": false, "error": msg }));
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

fn ping() {
    let started = std::env::var("NEAR_BLOCK_HEIGHT").unwrap_or_default();
    match http::get(&format!("{NEAR_AI_BASE}/models")) {
        Ok((status, body)) => {
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            let n = v["data"].as_array().map(|a| a.len()).unwrap_or(0);
            println!(
                "{}",
                serde_json::json!({
                    "ok": status == 200, "mode": "ping", "http_status": status, "models": n,
                    "has_api_key": std::env::var("NEAR_AI_API_KEY").map(|k| !k.is_empty()).unwrap_or(false),
                    "sender": std::env::var("NEAR_SENDER_ID").unwrap_or_default(), "block": started,
                })
            );
        }
        Err(e) => fail(&e),
    }
}

#[cfg(target_arch = "wasm32")]
fn coin_vibe(rpc: &str, coin: &str) -> Option<String> {
    use std::time::Duration;
    let body = serde_json::json!({"jsonrpc":"2.0","id":"p","method":"query","params":{"request_type":"call_function","finality":"final","account_id":coin,"method_name":"get_config","args_base64":"e30="}}).to_string();
    let r = wasi_http_client::Client::new().post(rpc).header("Content-Type", "application/json").connect_timeout(crate::budget::cap(Duration::from_secs(8)).ok()?).body(body.as_bytes()).send().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&r.body().ok()?).ok()?;
    let bytes: Vec<u8> = v["result"]["result"].as_array()?.iter().filter_map(|x| x.as_u64().map(|n| n as u8)).collect();
    Some(vibe_from_config(&serde_json::from_slice(&bytes).ok()?))
}
#[cfg(not(target_arch = "wasm32"))]
fn coin_vibe(_: &str, _: &str) -> Option<String> {
    None
}

fn job(mut job: Job) {
    if let (Some(rpc), Some(coin)) = (job.rpc.clone(), job.coin.clone()) {
        if let Some(v) = coin_vibe(&rpc, &coin) {
            if !v.is_empty() {
                job.persona.vibe = v;
            }
        }
    }
    let Ok(key) = std::env::var("NEAR_AI_API_KEY") else { fail("NEAR_AI_API_KEY is not set") };
    if key.is_empty() {
        fail("NEAR_AI_API_KEY is empty");
    }
    let model = job.model.clone().unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let (status, body) = match http::post_json(&format!("{NEAR_AI_BASE}/chat/completions"), &key, &chat_request(&job)) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    if !(200..300).contains(&status) {
        fail(&format!("NEAR AI returned HTTP {status}"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let content = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    let reply = match parse_reply(content) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    let (mut media_url, mut media_hash) = (String::new(), String::new());
    if job.kind == "meme" && !reply.refuse {
        let prompt = if reply.image_prompt.is_empty() { reply.text.clone() } else { reply.image_prompt.clone() };
        match make_meme(&key, &job.rpc.clone().unwrap_or_else(|| "https://rpc.testnet.fastnear.com".into()), &prompt, job.debug_stage) {
            Ok((u, h)) => {
                media_url = u;
                media_hash = h;
            }
            Err(e) => fail(&e),
        }
    }
    let out = JobOutput {
        ok: true,
        job_id: job.job_id,
        refused: reply.refuse,
        reason: truncate_bytes(&reply.reason, 200),
        text: reply.text,
        model: v["model"].as_str().unwrap_or(&model).to_string(),
        chat_id: v["id"].as_str().unwrap_or("").to_string(),
        prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        media_url,
        media_hash,
    };
    println!("{}", serde_json::to_string(&out).unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialize\"}".into()));
}

fn defense(mut d: Defense) {
    let Ok(key) = std::env::var("NEAR_AI_API_KEY") else { fail("NEAR_AI_API_KEY is not set") };
    let mut vibe = String::new();
    if let (Some(rpc), Some(coin)) = (d.rpc.clone(), d.coin.clone()) {
        vibe = coin_vibe(&rpc, &coin).unwrap_or_default();
    }
    let model = d.model.clone().unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let (status, body) = match http::post_json(&format!("{NEAR_AI_BASE}/chat/completions"), &key, &defense_request(&d, &vibe)) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    if !(200..300).contains(&status) {
        fail(&format!("NEAR AI returned HTTP {status}"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
    let decision = match parse_decision(v["choices"][0]["message"]["content"].as_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => fail(&e),
    };
    d.treasury_near.clear();
    let out = DefenseOutput {
        ok: true,
        mode: "defense".into(),
        action: decision.action,
        bps: decision.bps as u32,
        reason: decision.reason,
        drop_pct_x100: drop_pct_x100(d.current_point, d.high_point),
        model: v["model"].as_str().unwrap_or(&model).to_string(),
        chat_id: v["id"].as_str().unwrap_or("").to_string(),
    };
    println!("{}", serde_json::to_string(&out).unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialize\"}".into()));
}

fn main() {
    budget::start();
    let mut s = String::new();
    if std::io::stdin().read_to_string(&mut s).is_err() {
        fail("cannot read input");
    }
    match serde_json::from_str::<Input>(&s) {
        Ok(Input::Ping {}) => ping(),
        Ok(Input::Job(j)) => job(j),
        Ok(Input::Defense(d)) => defense(d),
        Err(e) => fail(&format!("bad input: {e}")),
    }
}
