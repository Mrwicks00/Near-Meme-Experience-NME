//! Mercenaries (owner decision 2026-11-12, changes I3): coins are customers of each other. When a war starts, each
//! fighting NME coin hires a third NME coin's brain to roast its rival, paid from its own treasury. The payment is an
//! ordinary job payment, so on delivery it buys and burns the hired coin (Working Memes); a failed job refunds the
//! escrow to the hiring coin's treasury, never to a wallet.
//!
//! The decision is a fixed contract rule (no AI chooses to spend): at most one mercenary per coin per war, only if the
//! mercenary's job price fits within `MERC_CAP_BPS` of the hiring coin's treasury. The mercenary is picked
//! deterministically from the other launched NME coins (a hash of round and coin). The AI only does the work, inside
//! the attested TEE, like any hire. State: one side map job -> sponsoring coin (prefix `Mj`), no migration.
use crate::*;
use near_sdk::store::LookupMap as Map;

/// A mercenary may cost at most 0.5% of the hiring coin's treasury.
pub const MERC_CAP_BPS: u16 = 50;

fn sponsors() -> Map<u64, String> {
    Map::new(b"Mj".to_vec())
}

/// The coin whose treasury paid job `job_id`, when it was a mercenary hire.
pub(crate) fn sponsor_of(job_id: u64) -> Option<String> {
    sponsors().get(&job_id).cloned()
}

/// Deterministic pick: a hash of (round, coin) indexes the sorted candidates.
pub(crate) fn pick(candidates: &[String], round_id: u64, coin: &str) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let mut sorted = candidates.to_vec();
    sorted.sort();
    let h = env::sha256(format!("{round_id}:{coin}").as_bytes());
    let n = u64::from_le_bytes(h[..8].try_into().unwrap());
    Some(sorted[(n % sorted.len() as u64) as usize].clone())
}

impl Contract {
    /// Run by `tick` right after a war starts. Returns whether a mercenary was hired.
    pub(crate) fn run_mercenary(&mut self, round_id: u64, coin: String, rival: String, now: u64) -> bool {
        let Some(round) = self.rounds.get(&round_id).cloned() else { return false };
        if now >= round.end_ms {
            return false;
        }
        let Some(me) = self.coins.get(&coin).cloned() else { return false };
        if me.kind != Kind::Native || !me.launched {
            return false;
        }
        let fighting: Vec<String> = round.pairs.iter().flat_map(|p| [p.a.clone(), p.b.clone()]).collect();
        let mut candidates = Vec::new();
        for i in 0..self.roster.len() {
            let k = self.roster.get(i).cloned().unwrap_or_default();
            if k == coin || k == rival || fighting.contains(&k) {
                continue;
            }
            if let Some(e) = self.coins.get(&k) {
                if e.kind == Kind::Native && e.launched && e.pool_id.is_some() {
                    candidates.push(k);
                }
            }
        }
        let Some(merc) = pick(&candidates, round_id, &coin) else {
            events::emit("mercenary_skipped", json!({ "round": round_id, "coin": coin, "reason": "no other NME coin to hire" }));
            return false;
        };
        let price = self.job_price.get(&merc).map(|p| p.0).unwrap_or(0).max(self.brain.min_price.0);
        let cap = bps_of(me.treasury_near.0, MERC_CAP_BPS);
        if price > cap || price > me.treasury_near.0 {
            events::emit("mercenary_skipped", json!({ "round": round_id, "coin": coin, "mercenary": merc, "reason": "treasury too small for the 0.5% cap", "price": U128(price), "cap": U128(cap) }));
            return false;
        }
        // Debit first: the escrow now lives in the job; a refund returns it to this treasury (refund_job).
        let mut e = me.clone();
        e.treasury_near = U128(e.treasury_near.0 - price);
        self.coins.insert(coin.clone(), e);
        let target = self.coins.get(&rival).map(|_| rival.split('.').next().unwrap_or(&rival).to_uppercase()).unwrap_or_default();
        let prompt = format!(
            "You were hired as a mercenary by ${} in its Colosseum war against ${}. Roast ${} so hard its holders switch sides. Keep it about the coins.",
            coin.to_uppercase(),
            target,
            target
        );
        let merc_entry = self.coins.get(&merc).cloned().unwrap();
        let job_id = self.open_job(&merc_entry, &merc, "roast", &target, &prompt, env::current_account_id(), price);
        let mut m = sponsors();
        m.insert(job_id, coin.clone());
        m.flush();
        self.push_feed("mercenary_hired", &coin, &merc, price, &format!("round={round_id} vs={rival} job={job_id}"));
        events::emit("mercenary_hired", json!({ "round": round_id, "coin": coin, "mercenary": merc, "rival": rival, "job_id": job_id, "price": U128(price) }));
        true
    }
}

#[near]
impl Contract {
    /// The coin whose treasury paid this job, if it was a mercenary hire.
    pub fn mercenary_sponsor(&self, job_id: u64) -> Option<String> {
        sponsor_of(job_id)
    }
}
