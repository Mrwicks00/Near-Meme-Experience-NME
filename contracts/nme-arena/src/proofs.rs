//! Proof of Thought. The brain worker returns sha256(exact request bytes) and sha256(exact response bytes) of its NEAR AI
//! call; NEAR AI's model TEE signs `{model}:{request_hash}:{response_hash}` (GET /v1/signature/{chat_id}) with a key that
//! appears in the model's hardware attestation. Keeping the two hashes next to each delivery and commentary line lets
//! anyone match what the coin said to that signature. Stored per ring slot (bounded: 32 deliveries, 64 lines) under its
//! own prefixes (`Pd`, `Pc`), outside the main struct, so no migration.
use crate::*;
use near_sdk::store::LookupMap as Map;

fn delivery_proofs() -> Map<u64, (String, String)> {
    Map::new(b"Pd".to_vec())
}
fn commentary_proofs() -> Map<u64, (u64, String, String)> {
    Map::new(b"Pc".to_vec())
}

pub(crate) fn is_hash(h: &str) -> bool {
    h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// Records (or clears) the proof of the delivery in ring `slot`.
pub(crate) fn set_delivery_proof(slot: u64, request_hash: &str, response_hash: &str) {
    let mut m = delivery_proofs();
    if is_hash(request_hash) && is_hash(response_hash) {
        m.insert(slot, (request_hash.to_ascii_lowercase(), response_hash.to_ascii_lowercase()));
    } else {
        m.remove(&slot);
    }
    m.flush();
}

pub(crate) fn delivery_proof(slot: u64) -> (String, String) {
    delivery_proofs().get(&slot).cloned().unwrap_or_default()
}

/// Records (or clears) the proof of commentary line `seq`.
pub(crate) fn set_commentary_proof(slot: u64, seq: u64, request_hash: &str, response_hash: &str) {
    let mut m = commentary_proofs();
    if is_hash(request_hash) && is_hash(response_hash) {
        m.insert(slot, (seq, request_hash.to_ascii_lowercase(), response_hash.to_ascii_lowercase()));
    } else {
        m.remove(&slot);
    }
    m.flush();
}

#[near]
impl Contract {
    /// The proof hashes of commentary line `seq` (null when the line predates proofs or left the ring).
    pub fn commentary_proof(&self, seq: u64) -> Option<serde_json::Value> {
        commentary_proofs()
            .get(&(seq % COMMENTARY_RING))
            .filter(|(s, _, _)| *s == seq)
            .map(|(_, rq, rs)| json!({ "request_hash": rq, "response_hash": rs }))
    }
}
