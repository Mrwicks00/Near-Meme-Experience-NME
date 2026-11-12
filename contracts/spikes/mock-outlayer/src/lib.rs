//! Sandbox stand-in for `outlayer.testnet`: `request_execution` answers at once with the same result shape the real one
//! gives a callback (the worker's stdout as a JSON string; a failed run is JSON `null`; a crash is a failed receipt).
//! The input text picks the behaviour: MOCK_REFUSE, MOCK_NULL, MOCK_FAIL, MOCK_GARBAGE, MOCK_SLOW (never answers: panics late), else success.
use near_sdk::serde_json::{self, json, Value};
use near_sdk::{env, near, PanicOnDefault};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    pub calls: u32,
    /// What a `defense` run answers: buyback (300 bps), over_cap (900 bps), hold, null, garbage, fail.
    pub defense_mode: String,
    pub last_defense_input: String,
    /// Solana deposits the mock "verifier" knows: tx_id -> near_yocto (set by a test).
    pub deposits: std::collections::HashMap<String, String>,
}

#[near]
impl Contract {
    #[init]
    pub fn new() -> Self {
        Self { calls: 0, defense_mode: "buyback".into(), last_defense_input: String::new(), deposits: Default::default() }
    }

    #[payable]
    pub fn request_execution(&mut self, source: Value, resource_limits: Option<Value>, input_data: Option<String>, secrets_ref: Option<Value>, response_format: Option<String>, payer_account_id: Option<String>) -> Value {
        let _ = (resource_limits, response_format, payer_account_id);
        self.calls += 1;
        assert!(source["WasmUrl"]["hash"].is_string(), "the arena must pin the brain by hash");
        let input = input_data.unwrap_or_default();
        let v: Value = serde_json::from_str(&input).expect("input is JSON");
        if v["chain"] != "solana" && v["chain"] != "evm" {
            assert!(secrets_ref.is_some(), "the arena must reference the brain's secrets");
        }
        if v["chain"] == "evm" {
            // the EVM adapter of the deposit verifier: same shape, the amount is in wei (1 ETH here)
            let tx = v["tx_id"].as_str().unwrap_or("").to_string();
            let to = v["expected_to"].as_str().unwrap_or("").to_string();
            return match self.deposits.get(&tx) {
                None => Value::String(json!({"ok": false, "error": "transaction not found"}).to_string()),
                Some(near) => Value::String(json!({"ok": true, "chain": "evm", "tx_id": tx, "expected_to": to, "raw_amount": "1000000000000000000", "near_yocto": near}).to_string()),
            };
        }
        if v["chain"] == "solana" {
            // the deposit verifier: answers with what a test registered for this transaction
            let tx = v["tx_id"].as_str().unwrap_or("").to_string();
            let to = v["expected_to"].as_str().unwrap_or("").to_string();
            return match self.deposits.get(&tx) {
                None => Value::String(json!({"ok": false, "error": "transaction not found (or not finalized yet)"}).to_string()),
                Some(near) => {
                    let echoed_to = if tx.starts_with("wrongto") { "SomeOtherAddress".to_string() } else { to };
                    Value::String(json!({"ok": true, "chain": "solana", "tx_id": tx, "expected_to": echoed_to, "lamports": 5_000_000_000u64, "near_yocto": near}).to_string())
                }
            };
        }
        if v["mode"] == "defense" {
            self.last_defense_input = input.clone();
            return match self.defense_mode.as_str() {
                "fail" => env::panic_str("worker crashed"),
                "null" => Value::Null,
                "garbage" => Value::String("not json".into()),
                "hold" => Value::String(json!({"ok": true, "mode": "defense", "action": "hold", "bps": 0, "reason": "just a rotation"}).to_string()),
                "over_cap" => Value::String(json!({"ok": true, "mode": "defense", "action": "buyback", "bps": 900, "reason": "all in"}).to_string()),
                _ => Value::String(json!({"ok": true, "mode": "defense", "action": "buyback", "bps": 300, "reason": "paper hands detected", "model": "mock/qwen", "chat_id": "defense-chat"}).to_string()),
            };
        }
        let job_id = v["job_id"].as_u64().unwrap_or(0);
        let prompt = v["prompt"].as_str().unwrap_or("");
        if prompt.contains("MOCK_FAIL") {
            env::panic_str("worker crashed");
        }
        if prompt.contains("MOCK_NULL") {
            return Value::Null;
        }
        if prompt.contains("MOCK_GARBAGE") {
            return Value::String("this is not json".into());
        }
        if prompt.contains("MOCK_REFUSE") {
            return Value::String(json!({"ok": true, "job_id": job_id, "refused": true, "reason": "disallowed", "text": ""}).to_string());
        }
        if v["kind"] == "meme" {
            let h: String = env::sha256(prompt.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
            if prompt.contains("MOCK_NOPIC") {
                return Value::String(json!({"ok": true, "job_id": job_id, "refused": false, "reason": "", "text": "a caption without a picture", "model": "mock/qwen", "chat_id": "c"}).to_string());
            }
            return Value::String(json!({"ok": true, "job_id": job_id, "refused": false, "reason": "", "text": "mock caption", "model": "mock/qwen", "chat_id": format!("chat-{job_id}"),
                "media_url": format!("https://test.fastfs.io/nme-media.testnet/outlayer.testnet/{h}.jpg"), "media_hash": h}).to_string());
        }
        let ticker = v["persona"]["ticker"].as_str().unwrap_or("?");
        Value::String(
            json!({"ok": true, "job_id": job_id, "refused": false, "reason": "", "text": format!("{ticker} says: {} is ngmi (job {job_id})", v["target"].as_str().unwrap_or("them")),
                   "model": "mock/qwen", "chat_id": format!("chat-{job_id}"), "prompt_tokens": 10, "completion_tokens": 10})
            .to_string(),
        )
    }

    pub fn set_deposit(&mut self, tx_id: String, near_yocto: String) {
        self.deposits.insert(tx_id, near_yocto);
    }

    /// Stand-in for the MPC contract's `derived_public_key` (the real one returns `ed25519:<base58>`).
    pub fn derived_public_key(&self, path: String, predecessor: String, domain_id: u64) -> String {
        let h = env::sha256(format!("{path}|{predecessor}|{domain_id}").as_bytes());
        if domain_id == 0 {
            // secp256k1 domain: 64 bytes, base58, like the real contract
            let mut b = h.clone();
            b.extend(env::sha256(&h));
            return format!("secp256k1:{}", near_sdk::bs58::encode(b).into_string());
        }
        format!("ed25519:{}", h.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    pub fn set_defense_mode(&mut self, mode: String) {
        self.defense_mode = mode;
    }

    pub fn last_defense_input(&self) -> String {
        self.last_defense_input.clone()
    }

    pub fn calls(&self) -> u32 {
        self.calls
    }
}
