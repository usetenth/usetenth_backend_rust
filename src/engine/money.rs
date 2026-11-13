//! Fixed-point USD in whole micros (1e-6); floats never touch money.

pub const MICROS_PER_USD: i64 = 1_000_000;

/// Parses a non-negative decimal; digits past six places are floored, never rounded up.
pub fn parse_micros(value: &str) -> Option<i64> {
    let value = value.trim();
    let (whole, frac) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || whole.len() > 12 || !digits(whole) || !digits(frac) {
        return None;
    }
    let frac: String = frac.chars().take(6).collect();
    let frac = format!("{frac:0<6}");
    whole.parse::<i64>().ok()?.checked_mul(MICROS_PER_USD)?.checked_add(frac.parse().ok()?)
}

pub fn format_micros(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let abs = micros.unsigned_abs();
    let (whole, frac) = (abs / MICROS_PER_USD as u64, abs % MICROS_PER_USD as u64);
    let mut frac = format!("{frac:06}");
    while frac.len() > 2 && frac.ends_with('0') {
        frac.pop();
    }
    format!("{sign}{whole}.{frac}")
}

pub fn parse_decimal(value: &str) -> Option<(i128, u32)> {
    let value = value.trim();
    let (whole, frac) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || whole.len() + frac.len() > 30 || frac.len() > 18 {
        return None;
    }
    Some((format!("{whole}{frac}").parse().ok()?, frac.len() as u32))
}

/// floor(qty * price) in micros of USD, exactly. None on bad input or overflow.
pub fn mul_to_micros(qty: &str, price: &str) -> Option<i64> {
    let ((a, sa), (b, sb)) = (parse_decimal(qty)?, parse_decimal(price)?);
    let product = a.checked_mul(b)?;
    let scale = sa + sb;
    let micros = if scale >= 6 {
        product / 10i128.checked_pow(scale - 6)?
    } else {
        product.checked_mul(10i128.checked_pow(6 - scale)?)?
    };
    i64::try_from(micros).ok()
}

pub fn cmp_decimal(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let ((x, sx), (y, sy)) = (parse_decimal(a)?, parse_decimal(b)?);
    let scale = sx.max(sy);
    Some(x.checked_mul(10i128.checked_pow(scale - sx)?)?.cmp(&y.checked_mul(10i128.checked_pow(scale - sy)?)?))
}

pub fn percent_of(micros: i64, percent: i16) -> i64 {
    (micros as i128 * percent as i128 / 100) as i64
}

/// Splits `total` by basis-point weights; the floored leftover goes to the heaviest leg so legs sum to `total`.
pub fn split_bps(total: i64, weights: &[(String, i32)]) -> Vec<(String, i64)> {
    let mut sorted: Vec<&(String, i32)> = weights.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut legs: Vec<(String, i64)> = sorted
        .iter()
        .map(|(key, bps)| (key.clone(), (total as i128 * *bps as i128 / 10_000) as i64))
        .collect();
    let leftover = total - legs.iter().map(|(_, v)| v).sum::<i64>();
    if let Some(first) = legs.first_mut() {
        first.1 += leftover;
    }
    legs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimals() {
        assert_eq!(parse_micros("12"), Some(12_000_000));
        assert_eq!(parse_micros("12.5"), Some(12_500_000));
        assert_eq!(parse_micros("0.000001"), Some(1));
        assert_eq!(parse_micros("1.23456789"), Some(1_234_567));
        assert_eq!(parse_micros(" 3 "), Some(3_000_000));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "-1", "1e3", "abc", ".5", "1.2.3", "9999999999999"] {
            assert_eq!(parse_micros(bad), None, "{bad}");
        }
    }

    #[test]
    fn formats_amounts() {
        assert_eq!(format_micros(12_500_000), "12.50");
        assert_eq!(format_micros(1), "0.000001");
        assert_eq!(format_micros(0), "0.00");
        assert_eq!(format_micros(1_234_567), "1.234567");
        assert_eq!(parse_micros(&format_micros(987_654_321)), Some(987_654_321));
    }

    #[test]
    fn multiplies_exactly() {
        assert_eq!(mul_to_micros("0.5", "200"), Some(100_000_000));
        assert_eq!(mul_to_micros("0.123456789", "10"), Some(1_234_567));
        assert_eq!(mul_to_micros("3", "0.000001"), Some(3));
        assert_eq!(mul_to_micros("1.0000001", "1"), Some(1_000_000));
        assert_eq!(mul_to_micros("abc", "1"), None);
        assert_eq!(mul_to_micros("99999999999999999999999999999", "99999999999999999999999999999"), None);
    }

    #[test]
    fn compares_decimals_exactly() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_decimal("1.50", "1.5"), Some(Equal));
        assert_eq!(cmp_decimal("0.1", "0.09999999"), Some(Greater));
        assert_eq!(cmp_decimal("2", "10"), Some(Less));
        assert_eq!(cmp_decimal("x", "1"), None);
    }

    #[test]
    fn percent_never_overflows() {
        assert_eq!(percent_of(100_000_000, 10), 10_000_000);
        assert_eq!(percent_of(i64::MAX, 20), (i64::MAX as i128 * 20 / 100) as i64);
    }

    #[test]
    fn split_conserves_the_total() {
        let weights = vec![("a".to_string(), 3333), ("b".to_string(), 3333), ("c".to_string(), 3334)];
        for total in [1, 7, 999_999, 10_000_001, 123_456_789] {
            let legs = split_bps(total, &weights);
            assert_eq!(legs.iter().map(|(_, v)| v).sum::<i64>(), total);
            assert!(legs.iter().all(|(_, v)| *v >= 0));
        }
        assert_eq!(split_bps(100, &weights)[0].0, "c");
    }
}
