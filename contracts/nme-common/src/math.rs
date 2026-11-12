//! Exact 256-bit-intermediate arithmetic. Supply (1e27) × deposit (up to 1e26) overflows u128.

/// 128×128 → 256 bit product as (hi, lo).
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    let (a0, a1) = (a & u64::MAX as u128, a >> 64);
    let (b0, b1) = (b & u64::MAX as u128, b >> 64);
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;
    let mid = (p00 >> 64) + (p01 & u64::MAX as u128) + (p10 & u64::MAX as u128);
    let lo = (p00 & u64::MAX as u128) | (mid << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    (hi, lo)
}

/// floor(a × b / c). `None` if c == 0 or the result does not fit in u128.
pub fn mul_div(a: u128, b: u128, c: u128) -> Option<u128> {
    if c == 0 {
        return None;
    }
    let (hi, lo) = mul_wide(a, b);
    if hi >= c {
        return None;
    }
    let (mut rem, mut q) = (hi, 0u128);
    for i in (0..128).rev() {
        let carry = rem >> 127;
        rem = (rem << 1) | ((lo >> i) & 1);
        if carry == 1 || rem >= c {
            rem = rem.wrapping_sub(c);
            q |= 1u128 << i;
        }
    }
    Some(q)
}

/// amount × bps / 10_000 (rounded down).
pub fn bps_of(amount: u128, bps: u16) -> u128 {
    mul_div(amount, bps as u128, 10_000).expect("bps_of cannot overflow for bps <= 10000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_match_u128() {
        assert_eq!(mul_div(10, 20, 3), Some(66));
        assert_eq!(mul_div(0, 5, 7), Some(0));
        assert_eq!(mul_div(5, 5, 0), None);
    }

    #[test]
    fn product_larger_than_u128() {
        // 1e27 * 1e25 / 1e26 = 1e26 (product 1e52 does not fit in u128)
        let (a, b, c) = (10u128.pow(27), 10u128.pow(25), 10u128.pow(26));
        assert_eq!(mul_div(a, b, c), Some(10u128.pow(26)));
        // buyers_pool * share: 5.5e26 * 3e24 / 7e24 = 2.357142857142857142857142857e26 (floor)
        assert_eq!(mul_div(55 * 10u128.pow(25), 3 * 10u128.pow(24), 7 * 10u128.pow(24)), Some(235714285714285714285714285));
    }

    #[test]
    fn overflow_detected() {
        assert_eq!(mul_div(u128::MAX, u128::MAX, 1), None);
        assert_eq!(mul_div(u128::MAX, 2, 1), None);
        assert_eq!(mul_div(u128::MAX, u128::MAX, u128::MAX), Some(u128::MAX));
    }

    #[test]
    fn huge_divisor_with_carry() {
        // exercises the rem carry branch (c > 2^127)
        let c = u128::MAX - 5;
        assert_eq!(mul_div(c, 3, c), Some(3));
        assert_eq!(mul_div(c, c, c), Some(c));
    }

    #[test]
    fn bps() {
        assert_eq!(bps_of(1_000_000, 5500), 550_000);
        assert_eq!(bps_of(u128::MAX, 10_000), u128::MAX);
    }
}
