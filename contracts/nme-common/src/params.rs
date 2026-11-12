//! Per-coin immutable parameters and the global bounds the factory enforces (docs/11 §3, docs/spikes/S12.md).
//! The launch is a minimal single-sided position on Rhea DCL: fixed supply minted once, 95% into the pool,
//! 5% into the Crown vault. There is no batch, no auction and no anti-snipe logic (sniping is allowed by design).
use near_sdk::json_types::U128;
use near_sdk::{near, AccountId};

pub const DECIMALS: u8 = 18;
/// 1,000,000,000 tokens × 10^18
pub const TOTAL_SUPPLY: u128 = 1_000_000_000 * 10u128.pow(18);
pub const BPS: u16 = 10_000;

pub const MS_MIN: u64 = 60_000;
pub const MS_HOUR: u64 = 3_600_000;
pub const MS_DAY: u64 = 24 * MS_HOUR;

// ---- supply split (fixed for every coin) ----
pub const POOL_SUPPLY_BPS: u16 = 9_500;
pub const CROWN_SUPPLY_BPS: u16 = 500;
/// Creator first buy is capped at this share of supply (bounds dev dumps; no other anti-snipe logic).
pub const FIRST_BUY_MAX_BPS: u16 = 500;

// ---- Rhea DCL (S12) ----
/// Fee tiers in the order a launch tries them (1%, 0.2%, 0.04%, 0.01%).
pub const DCL_FEE_TIERS: [u32; 4] = [10_000, 2_000, 400, 100];
/// The 1% tier has point_delta 200 and every other tier divides it, so a start point on the 200 grid is valid in all.
pub const DCL_GRID: i32 = 200;
pub const POINT_LIMIT: i32 = 700_000; // DCL's hard limit is 800_000; keep clear of it
pub const RANGE_MIN_POINTS: i32 = 2_000;
pub const RANGE_MAX_POINTS: i32 = 300_000;

// ---- crown bounds ----
pub const CLIFF_MAX_MS: u64 = 30 * MS_DAY;
pub const VEST_MIN_MS: u64 = MS_MIN;
pub const VEST_MAX_MS: u64 = 4 * 365 * MS_DAY;
pub const INACTIVITY_MIN_MS: u64 = 5 * MS_MIN;
pub const INACTIVITY_MAX_MS: u64 = 90 * MS_DAY;

pub const MAX_NAME_LEN: usize = 64;
pub const MAX_DESCRIPTION_LEN: usize = 280;
pub const MAX_SOCIALS_LEN: usize = 300;
pub const MAX_PERSONA_LEN: usize = 280;
pub const MAX_ICON_BYTES: usize = 24 * 1024;

/// Optional creator first buy: exactly `tokens` are bought at launch (SwapByOutput), paying at most `max_near`;
/// the unused NEAR is returned to the creator.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct FirstBuy {
    pub tokens: U128,
    pub max_near: U128,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchParams {
    /// DCL start point (price: raw Y/X = 1.0001^point, X/Y being the lexicographic order of the pool tokens).
    /// Must be a multiple of `DCL_GRID`.
    pub start_point: i32,
    /// Width of the single-sided range in points (multiple of `DCL_GRID`). The range sits above the start point
    /// when the coin is token X of the pool, below it when the coin is token Y.
    pub range_points: i32,
    pub first_buy: Option<FirstBuy>,
}

#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct CrownParams {
    pub cliff_ms: u64,
    /// Total linear vesting duration counted from launch (nothing before the cliff).
    pub vest_ms: u64,
    /// Crown is forfeitable after this long without a heartbeat.
    pub inactivity_ms: u64,
}

/// Immutable persona fed to the coin's brain (docs/12): never changes after creation.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct Persona {
    pub vibe: String,
    pub rivals: Vec<String>,
}

impl LaunchParams {
    pub fn defaults() -> Self {
        Self { start_point: 92_000, range_points: 40_000, first_buy: None }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.start_point % DCL_GRID != 0 || self.start_point.abs() > POINT_LIMIT {
            return Err(format!("start_point must be a multiple of {DCL_GRID} within ±{POINT_LIMIT}"));
        }
        if self.range_points % DCL_GRID != 0 || !(RANGE_MIN_POINTS..=RANGE_MAX_POINTS).contains(&self.range_points) {
            return Err(format!("range_points must be a multiple of {DCL_GRID} in [{RANGE_MIN_POINTS}, {RANGE_MAX_POINTS}]"));
        }
        if (self.start_point.abs() + self.range_points + DCL_GRID) > POINT_LIMIT + RANGE_MAX_POINTS {
            return Err("range leaves DCL's point limits".into());
        }
        if let Some(fb) = &self.first_buy {
            if fb.tokens.0 == 0 || fb.max_near.0 == 0 {
                return Err("first_buy needs positive tokens and max_near".into());
            }
            if fb.tokens.0 > crate::math::bps_of(TOTAL_SUPPLY, FIRST_BUY_MAX_BPS) {
                return Err("first_buy exceeds 5% of supply".into());
            }
        }
        Ok(())
    }

    /// The single-sided range for a coin that is token X (`true`) or token Y (`false`) of its pool.
    /// X: [start + grid, start + grid + range]; Y: [start - grid - range, start - grid].
    pub fn range(&self, coin_is_x: bool) -> (i32, i32) {
        if coin_is_x {
            (self.start_point + DCL_GRID, self.start_point + DCL_GRID + self.range_points)
        } else {
            (self.start_point - DCL_GRID - self.range_points, self.start_point - DCL_GRID)
        }
    }
}

impl CrownParams {
    pub fn defaults() -> Self {
        Self { cliff_ms: MS_DAY, vest_ms: 30 * MS_DAY, inactivity_ms: 7 * MS_DAY }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.cliff_ms > CLIFF_MAX_MS {
            return Err("cliff_ms too long".into());
        }
        in_range("vest_ms", self.vest_ms, VEST_MIN_MS, VEST_MAX_MS)?;
        if self.vest_ms < self.cliff_ms {
            return Err("vest_ms must be >= cliff_ms".into());
        }
        in_range("inactivity_ms", self.inactivity_ms, INACTIVITY_MIN_MS, INACTIVITY_MAX_MS)
    }
}

impl Persona {
    pub fn validate(&self) -> Result<(), String> {
        let n = self.vibe.len() + self.rivals.iter().map(|r| r.len()).sum::<usize>();
        if self.vibe.trim().is_empty() || n > MAX_PERSONA_LEN || self.rivals.len() > 8 {
            return Err(format!("persona must have a vibe and at most {MAX_PERSONA_LEN} bytes / 8 rivals"));
        }
        Ok(())
    }
}

fn in_range(name: &str, v: u64, lo: u64, hi: u64) -> Result<(), String> {
    if v < lo || v > hi {
        return Err(format!("{name} must be in [{lo}, {hi}]"));
    }
    Ok(())
}

/// How the fixed supply is split at launch. Exact: parts sum to `TOTAL_SUPPLY`.
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug, PartialEq)]
pub struct SupplySplit {
    pub pool: U128,
    pub crown: U128,
}

pub fn supply_split() -> SupplySplit {
    let crown = crate::math::bps_of(TOTAL_SUPPLY, CROWN_SUPPLY_BPS);
    SupplySplit { pool: U128(TOTAL_SUPPLY - crown), crown: U128(crown) }
}

/// Pool id on DCL: the two token accounts sorted lexicographically, then the fee tier.
pub fn dcl_pool_id(token: &str, wrap: &str, fee: u32) -> String {
    let (x, y) = if token < wrap { (token, wrap) } else { (wrap, token) };
    format!("{x}|{y}|{fee}")
}

/// Splits a DCL pool id `x|y|fee` into its parts. `None` unless it has exactly three parts and a numeric fee.
pub fn parse_dcl_pool_id(pool_id: &str) -> Option<(&str, &str, u32)> {
    let mut it = pool_id.split('|');
    let (x, y, f) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || x.is_empty() || y.is_empty() {
        return None;
    }
    Some((x, y, f.parse().ok()?))
}

/// Immutable per-coin config (stored by the coin, readable via `get_config`).
#[near(serializers = [json, borsh])]
#[derive(Clone, Debug)]
pub struct CoinConfig {
    pub ticker: String,
    pub creator: AccountId,
    pub launch: LaunchParams,
    pub crown: CrownParams,
    pub persona: Persona,
    pub colosseum_opt_in: bool,
    pub arena: AccountId,
    pub crown_collection: AccountId,
    pub wrap: AccountId,
    pub dcl: AccountId,
    pub vault_code_hash: near_sdk::json_types::Base58CryptoHash,
    pub created_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_valid() {
        assert!(LaunchParams::defaults().validate().is_ok());
        assert!(CrownParams::defaults().validate().is_ok());
    }

    #[test]
    fn launch_bounds() {
        let mut p = LaunchParams::defaults();
        p.start_point = 92_100; // off the 200 grid
        assert!(p.validate().is_err());
        let mut p = LaunchParams::defaults();
        p.range_points = 100;
        assert!(p.validate().is_err());
        let mut p = LaunchParams::defaults();
        p.range_points = RANGE_MAX_POINTS + 200;
        assert!(p.validate().is_err());
        let mut p = LaunchParams::defaults();
        p.start_point = POINT_LIMIT + 200;
        assert!(p.validate().is_err());
        let mut p = LaunchParams::defaults();
        p.first_buy = Some(FirstBuy { tokens: U128(TOTAL_SUPPLY / 10), max_near: U128(1) }); // 10% > 5%
        assert!(p.validate().is_err());
        p.first_buy = Some(FirstBuy { tokens: U128(TOTAL_SUPPLY / 20), max_near: U128(1) }); // exactly 5%
        assert!(p.validate().is_ok());
        p.first_buy = Some(FirstBuy { tokens: U128(0), max_near: U128(1) });
        assert!(p.validate().is_err());
    }

    #[test]
    fn range_sits_on_the_right_side_of_the_start_point() {
        let p = LaunchParams { start_point: 92_000, range_points: 40_000, first_buy: None };
        assert_eq!(p.range(true), (92_200, 132_200)); // coin is X: above
        assert_eq!(p.range(false), (51_800, 91_800)); // coin is Y: below
        let n = LaunchParams { start_point: -92_000, range_points: 40_000, first_buy: None };
        assert_eq!(n.range(false), (-132_200, -92_200)); // matches the S12 sandbox run
    }

    #[test]
    fn crown_bounds() {
        let mut c = CrownParams::defaults();
        c.vest_ms = c.cliff_ms - 1;
        assert!(c.validate().is_err());
        c = CrownParams::defaults();
        c.inactivity_ms = 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn persona_bounds() {
        assert!(Persona { vibe: "degen frog".into(), rivals: vec!["doge".into()] }.validate().is_ok());
        assert!(Persona { vibe: " ".into(), rivals: vec![] }.validate().is_err());
        assert!(Persona { vibe: "x".repeat(MAX_PERSONA_LEN + 1), rivals: vec![] }.validate().is_err());
        assert!(Persona { vibe: "ok".into(), rivals: vec!["r".into(); 9] }.validate().is_err());
    }

    #[test]
    fn supply_split_is_exact_95_5() {
        let s = supply_split();
        assert_eq!(s.pool.0 + s.crown.0, TOTAL_SUPPLY);
        assert_eq!(s.crown.0, TOTAL_SUPPLY / 20);
    }

    #[test]
    fn pool_id_parses_back() {
        assert_eq!(parse_dcl_pool_id("a.testnet|wrap.testnet|10000"), Some(("a.testnet", "wrap.testnet", 10_000)));
        assert_eq!(parse_dcl_pool_id("a|b"), None);
        assert_eq!(parse_dcl_pool_id("a|b|c"), None);
        assert_eq!(parse_dcl_pool_id("a|b|1|2"), None);
        assert_eq!(parse_dcl_pool_id("|b|1"), None);
    }

    #[test]
    fn pool_id_is_sorted() {
        assert_eq!(dcl_pool_id("pepe.nme.testnet", "wrap.testnet", 10_000), "pepe.nme.testnet|wrap.testnet|10000");
        assert_eq!(dcl_pool_id("zeta.nme.testnet", "wrap.testnet", 2_000), "wrap.testnet|zeta.nme.testnet|2000");
    }
}
