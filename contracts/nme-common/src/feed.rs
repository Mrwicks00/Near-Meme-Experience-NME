//! On-chain feed ring buffer (D-07, I6): last CAP items, ids are monotonic from 1.
use near_sdk::json_types::U128;
use near_sdk::store::Vector;
use near_sdk::{near, IntoStorageKey};

pub const FEED_CAP: u64 = 512;
pub const MAX_EXTRA_LEN: usize = 256;
pub const MAX_VIEW_LIMIT: u64 = 100;

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct FeedItem {
    pub id: u64,
    pub ts_ms: u64,
    pub kind: String,
    pub ticker: String,
    pub actor: String,
    pub amount: U128,
    pub extra: String,
}

#[near(serializers = [borsh])]
pub struct Feed {
    items: Vector<FeedItem>,
    next_id: u64,
}

impl Feed {
    pub fn new<S: IntoStorageKey>(prefix: S) -> Self {
        Self { items: Vector::new(prefix), next_id: 1 }
    }

    /// Append an item; returns its id. `extra` is truncated at a char boundary to MAX_EXTRA_LEN bytes.
    pub fn push(&mut self, ts_ms: u64, kind: &str, ticker: &str, actor: &str, amount: u128, extra: &str) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut end = extra.len().min(MAX_EXTRA_LEN);
        while !extra.is_char_boundary(end) {
            end -= 1;
        }
        let item = FeedItem {
            id,
            ts_ms,
            kind: kind.to_string(),
            ticker: ticker.to_string(),
            actor: actor.to_string(),
            amount: U128(amount),
            extra: extra[..end].to_string(),
        };
        if (self.items.len() as u64) < FEED_CAP {
            self.items.push(item);
        } else {
            let slot = ((id - 1) % FEED_CAP) as u32;
            self.items.replace(slot, item);
        }
        id
    }

    /// Write pending changes now (the `store` collections are lazy) so storage deltas can be measured.
    pub fn flush(&mut self) {
        self.items.flush();
    }

    /// Id of the newest item (0 if empty).
    pub fn last_id(&self) -> u64 {
        self.next_id - 1
    }

    /// Items with `id > after_id`, oldest first, at most `limit` (capped at MAX_VIEW_LIMIT).
    /// Items already overwritten by the ring are skipped.
    pub fn since(&self, after_id: u64, limit: u64) -> Vec<FeedItem> {
        let limit = limit.min(MAX_VIEW_LIMIT);
        let last = self.last_id();
        let oldest = if last > FEED_CAP { last - FEED_CAP + 1 } else { 1 };
        let start = (after_id + 1).max(oldest);
        let mut out = Vec::new();
        let mut id = start;
        while id <= last && (out.len() as u64) < limit {
            if let Some(it) = self.items.get(((id - 1) % FEED_CAP) as u32) {
                if it.id == id {
                    out.push(it.clone());
                }
            }
            id += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed() -> Feed {
        Feed::new(b"f".to_vec())
    }

    #[test]
    fn ids_monotonic_and_since() {
        let mut f = feed();
        assert_eq!(f.last_id(), 0);
        for i in 0..5u64 {
            assert_eq!(f.push(i, "trade", "pepe", "a.testnet", 1, ""), i + 1);
        }
        let all = f.since(0, 100);
        assert_eq!(all.iter().map(|x| x.id).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
        assert_eq!(f.since(3, 100).len(), 2);
        assert_eq!(f.since(5, 100).len(), 0);
        assert_eq!(f.since(0, 2).len(), 2);
    }

    #[test]
    fn ring_overwrites_oldest() {
        let mut f = feed();
        for i in 0..(FEED_CAP + 10) {
            f.push(i, "k", "t", "a", i as u128, "");
        }
        assert_eq!(f.last_id(), FEED_CAP + 10);
        let first = f.since(0, 1);
        assert_eq!(first[0].id, 11); // ids 1..=10 were overwritten
        let tail = f.since(FEED_CAP + 7, 100);
        assert_eq!(tail.iter().map(|x| x.id).collect::<Vec<_>>(), vec![FEED_CAP + 8, FEED_CAP + 9, FEED_CAP + 10]);
    }

    #[test]
    fn view_limit_capped() {
        let mut f = feed();
        for i in 0..150 {
            f.push(i, "k", "t", "a", 0, "");
        }
        assert_eq!(f.since(0, 1000).len() as u64, MAX_VIEW_LIMIT);
    }

    #[test]
    fn extra_truncated_on_char_boundary() {
        let mut f = feed();
        let long = "é".repeat(300); // 600 bytes
        f.push(0, "k", "t", "a", 0, &long);
        let e = &f.since(0, 1)[0].extra;
        assert!(e.len() <= MAX_EXTRA_LEN);
        assert!(e.chars().all(|c| c == 'é'));
    }
}
