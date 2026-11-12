//! Ticker normalization and validation. A ticker becomes the NEAR sub-account `<ticker>.<root>`.

pub const MIN_TICKER_LEN: usize = 2;
pub const MAX_TICKER_LEN: usize = 32;
pub const MAX_ACCOUNT_ID_LEN: usize = 64;
/// `burn` is the keyless graveyard account (`burn.<root>`): no coin may ever take that name.
pub const RESERVED: &[&str] = &["arena", "crown", "gateway", "factory", "admin", "nme", "treasury", "burn"];

/// Lowercase + trim, then validate `[a-z0-9_-]`, 2–32 chars, no leading/trailing/double separators,
/// not reserved. Returns the normalized ticker.
pub fn normalize_ticker(raw: &str) -> Result<String, String> {
    let t = raw.trim().to_ascii_lowercase();
    if t.len() < MIN_TICKER_LEN || t.len() > MAX_TICKER_LEN {
        return Err(format!("ticker must be {}-{} chars", MIN_TICKER_LEN, MAX_TICKER_LEN));
    }
    let b = t.as_bytes();
    let is_sep = |c: u8| c == b'-' || c == b'_';
    if !b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || is_sep(c)) {
        return Err("ticker may only contain a-z, 0-9, '-' and '_'".into());
    }
    if is_sep(b[0]) || is_sep(b[b.len() - 1]) || b.windows(2).any(|w| is_sep(w[0]) && is_sep(w[1])) {
        return Err("ticker may not start/end with or repeat '-' / '_'".into());
    }
    if RESERVED.contains(&t.as_str()) {
        return Err("ticker is reserved".into());
    }
    Ok(t)
}

/// `<ticker>.<root>` (and `crown.<ticker>.<root>`) must stay within NEAR's 64-char limit.
pub fn validate_account_len(ticker: &str, root: &str) -> Result<(), String> {
    let longest = "crown.".len() + ticker.len() + 1 + root.len();
    if longest > MAX_ACCOUNT_ID_LEN {
        return Err(format!("account id would be {} chars (max {})", longest, MAX_ACCOUNT_ID_LEN));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_accepts() {
        assert_eq!(normalize_ticker(" PePe "), Ok("pepe".into()));
        assert_eq!(normalize_ticker("a-b_c9"), Ok("a-b_c9".into()));
        assert_eq!(normalize_ticker("ab"), Ok("ab".into()));
    }

    #[test]
    fn rejects_bad() {
        for bad in ["a", "", "-ab", "ab-", "a--b", "a_-b", "pe.pe", "pé", "pe pe", "ARENA", "crown", "nme", "burn", "BURN"] {
            assert!(normalize_ticker(bad).is_err(), "{bad} should fail");
        }
        assert!(normalize_ticker(&"a".repeat(33)).is_err());
        assert!(normalize_ticker(&"a".repeat(32)).is_ok());
    }

    #[test]
    fn account_length() {
        assert!(validate_account_len("pepe", "nme.testnet").is_ok());
        assert!(validate_account_len(&"a".repeat(32), "nme.testnet").is_ok());
        assert!(validate_account_len(&"a".repeat(32), &"r".repeat(30)).is_err());
    }
}
