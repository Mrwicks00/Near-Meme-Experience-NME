//! S4 spike: call OutLayer `request_execution` and record exactly what the callback receives.
use near_sdk::serde_json::json;
use near_sdk::{env, near, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseResult};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    outlayer: AccountId,
    last_request_block: u64,
    last_result_block: u64,
    last_ok: bool,
    last_raw: String,
    calls: u32,
}

#[near]
impl Contract {
    #[init]
    pub fn new(outlayer: AccountId) -> Self {
        Self { outlayer, last_request_block: 0, last_result_block: 0, last_ok: false, last_raw: String::new(), calls: 0 }
    }

    /// `source` is passed through verbatim (ExecutionSource JSON) so the spike can try all shapes.
    #[payable]
    pub fn ask(&mut self, source: near_sdk::serde_json::Value, input: String, format: String, secrets_ref: Option<near_sdk::serde_json::Value>, max_seconds: Option<u64>, max_instructions: Option<u64>, max_memory_mb: Option<u32>) -> Promise {
        self.last_request_block = env::block_height();
        self.calls += 1;
        let args = json!({
            "source": source,
            "resource_limits": { "max_instructions": max_instructions.unwrap_or(1_000_000_000u64), "max_memory_mb": max_memory_mb.unwrap_or(64), "max_execution_seconds": max_seconds.unwrap_or(30) },
            "input_data": input,
            "secrets_ref": secrets_ref,
            "response_format": format,
            "payer_account_id": env::predecessor_account_id(),
        });
        Promise::new(self.outlayer.clone())
            .function_call("request_execution".to_string(), args.to_string().into_bytes(), env::attached_deposit(), Gas::from_tgas(100))
            .then(Promise::new(env::current_account_id()).function_call(
                "on_result".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(15)))
    }

    #[private]
    pub fn on_result(&mut self) {
        self.last_result_block = env::block_height();
        match env::promise_result(0) {
            PromiseResult::Successful(bytes) => { self.last_ok = true; self.last_raw = String::from_utf8_lossy(&bytes).into_owned(); }
            PromiseResult::Failed => { self.last_ok = false; self.last_raw = "PROMISE_FAILED".into(); }
        }
        env::log_str(&format!("on_result ok={} raw_len={}", self.last_ok, self.last_raw.len()));
    }

    pub fn last(&self) -> (bool, u64, u64, String, u32) {
        (self.last_ok, self.last_request_block, self.last_result_block, self.last_raw.clone(), self.calls)
    }
}
