//! NEP-297 events, standard "nme" (I6). Every contract logs through `emit`.
use near_sdk::{env, serde_json::{json, Value}};

pub const STANDARD: &str = "nme";
pub const VERSION: &str = "1.0.0";

/// Event names (docs/02 §6).
pub mod name {
    pub const COIN_CREATED: &str = "coin_created";
    pub const LAUNCH_STEP: &str = "launch_step";
    pub const LAUNCH_FINALIZED: &str = "launch_finalized";
    pub const LAUNCH_FAILED: &str = "launch_failed";
    pub const FEES_CLAIMED: &str = "fees_claimed";
    pub const TREASURY_FUNDED: &str = "treasury_funded";
    pub const BURN: &str = "burn";
    pub const CROWN_MINTED: &str = "crown_minted";
    pub const VAULT_RELEASE: &str = "vault_release";
    pub const VAULT_FORFEIT: &str = "vault_forfeit";
    pub const TRADE: &str = "trade";
    pub const CHEST_FUNDED: &str = "chest_funded";
    pub const COIN_ENLISTED: &str = "coin_enlisted";
    pub const TREASURY_BUYBACK: &str = "treasury_buyback";
}

pub fn format_event(event: &str, data: Value) -> String {
    let body = json!({ "standard": STANDARD, "version": VERSION, "event": event, "data": [data] });
    format!("EVENT_JSON:{}", body)
}

/// Same log as `emit`, from an already-serialized JSON object (no `serde_json::Value` machinery in the wasm).
pub fn emit_raw(event: &str, data_json: &str) {
    env::log_str(&format!(r#"EVENT_JSON:{{"standard":"{STANDARD}","version":"{VERSION}","event":"{event}","data":[{data_json}]}}"#));
}

pub fn emit(event: &str, data: Value) {
    env::log_str(&format_event(event, data));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_and_value_emitters_agree() {
        let a = format_event("burn", json!({"ticker":"pepe","amount":"5"}));
        let raw = format!(r#"EVENT_JSON:{{"standard":"{STANDARD}","version":"{VERSION}","event":"burn","data":[{{"ticker":"pepe","amount":"5"}}]}}"#);
        let (x, y): (Value, Value) = (near_sdk::serde_json::from_str(&a["EVENT_JSON:".len()..]).unwrap(), near_sdk::serde_json::from_str(&raw["EVENT_JSON:".len()..]).unwrap());
        assert_eq!(x, y);
    }

    #[test]
    fn nep297_shape() {
        let s = format_event("burn", json!({"ticker":"pepe","amount":"5"}));
        assert!(s.starts_with("EVENT_JSON:"));
        let v: Value = near_sdk::serde_json::from_str(&s["EVENT_JSON:".len()..]).unwrap();
        assert_eq!(v["standard"], "nme");
        assert_eq!(v["version"], "1.0.0");
        assert_eq!(v["event"], "burn");
        assert_eq!(v["data"][0]["ticker"], "pepe");
    }
}
