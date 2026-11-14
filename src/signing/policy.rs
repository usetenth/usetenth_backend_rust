use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MICROS_PER_USD: i64 = 1_000_000;

const FORBIDDEN_ACTIVITY_MARKERS: [&str; 9] = [
    "EXPORT",
    "API_KEY",
    "POLICY",
    "DELETE",
    "AUTHENTICATOR",
    "RECOVER",
    "USER",
    "ORGANIZATION",
    "OAUTH",
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("invalid amount: {0}")]
    InvalidAmount(String),
    #[error("order exceeds the per-order signing limit")]
    OrderTooLarge,
    #[error("daily signing limit would be exceeded")]
    DailyLimitExceeded,
    #[error("signing intent not found")]
    IntentNotFound,
    #[error("signing intent is not pending")]
    IntentNotPending,
    #[error("signing intent has expired")]
    IntentExpired,
    #[error("user has no active signer key")]
    NoActiveKey,
    #[error("expected between 1 and {0} payloads")]
    BadPayloadCount(usize),
    #[error("payload {0} does not match its digest")]
    DigestMismatch(usize),
    #[error("payload {0} is not a JSON object")]
    PayloadNotJson(usize),
    #[error("payload {0} has no activity type")]
    MissingActivityType(usize),
    #[error("activity type {0} is not allowed")]
    ActivityTypeNotAllowed(String),
    #[error("activity type {0} is forbidden")]
    ForbiddenActivityType(String),
}

#[derive(Debug, Clone)]
pub struct SigningLimits {
    pub max_order_micros: i64,
    pub daily_micros: i64,
    pub intent_ttl_secs: i64,
    pub max_payloads: usize,
    pub allowed_activity_types: Vec<String>,
    // Sells and withdrawals have their own ceilings; a day of buying never eats into them.
    pub sell_max_order_micros: i64,
    pub sell_daily_micros: i64,
    pub withdraw_max_order_micros: i64,
    pub withdraw_daily_micros: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    Buy,
    Sell,
    Withdraw,
}

impl IntentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
            Self::Withdraw => "withdraw",
        }
    }

    /// (largest single intent, largest total signed in 24 hours)
    pub fn limits(self, l: &SigningLimits) -> (i64, i64) {
        match self {
            Self::Buy => (l.max_order_micros, l.daily_micros),
            Self::Sell => (l.sell_max_order_micros, l.sell_daily_micros),
            Self::Withdraw => (l.withdraw_max_order_micros, l.withdraw_daily_micros),
        }
    }
}

impl SigningLimits {
    pub fn from_env() -> Result<Self, PolicyError> {
        let usd = |name: &str, default: &str| {
            parse_usd_micros(&std::env::var(name).unwrap_or_else(|_| default.to_string()))
        };
        let number = |name: &str, default: i64| {
            std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        };
        Ok(Self {
            max_order_micros: usd("SIGNING_MAX_ORDER_USD", "50")?,
            daily_micros: usd("SIGNING_DAILY_LIMIT_USD", "200")?,
            intent_ttl_secs: number("SIGNING_INTENT_TTL_SECS", 120),
            max_payloads: number("SIGNING_MAX_PAYLOADS", 4) as usize,
            allowed_activity_types: std::env::var("SIGNING_ALLOWED_ACTIVITY_TYPES")
                .unwrap_or_default()
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            sell_max_order_micros: usd("SIGNING_MAX_SELL_USD", "500")?,
            sell_daily_micros: usd("SIGNING_SELL_DAILY_LIMIT_USD", "2000")?,
            withdraw_max_order_micros: usd("SIGNING_MAX_WITHDRAW_USD", "100")?,
            withdraw_daily_micros: usd("SIGNING_WITHDRAW_DAILY_LIMIT_USD", "200")?,
        })
    }
}

pub struct PayloadRef<'a> {
    pub digest: &'a str,
    pub payload: &'a str,
}

pub fn parse_usd_micros(value: &str) -> Result<i64, PolicyError> {
    let bad = || PolicyError::InvalidAmount(value.to_string());
    let (whole, frac) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || whole.len() > 12 || frac.len() > 6 || !digits(whole) || !digits(frac) {
        return Err(bad());
    }
    let whole: i64 = whole.parse().map_err(|_| bad())?;
    let frac: i64 = format!("{frac:0<6}").parse().map_err(|_| bad())?;
    let micros = whole
        .checked_mul(MICROS_PER_USD)
        .and_then(|v| v.checked_add(frac))
        .ok_or_else(bad)?;
    if micros <= 0 {
        return Err(bad());
    }
    Ok(micros)
}

pub fn check_amount_for(
    kind: IntentKind,
    limits: &SigningLimits,
    amount_micros: i64,
    signed_last_day_micros: i64,
) -> Result<(), PolicyError> {
    let (max_order, daily) = kind.limits(limits);
    if amount_micros > max_order {
        return Err(PolicyError::OrderTooLarge);
    }
    match signed_last_day_micros.checked_add(amount_micros) {
        Some(total) if total <= daily => Ok(()),
        _ => Err(PolicyError::DailyLimitExceeded),
    }
}

pub fn check_payloads(
    limits: &SigningLimits,
    payloads: &[PayloadRef<'_>],
) -> Result<(), PolicyError> {
    if payloads.is_empty() || payloads.len() > limits.max_payloads {
        return Err(PolicyError::BadPayloadCount(limits.max_payloads));
    }
    for (index, item) in payloads.iter().enumerate() {
        let digest = hex::encode(Sha256::digest(item.payload.as_bytes()));
        if !digest.eq_ignore_ascii_case(item.digest) {
            return Err(PolicyError::DigestMismatch(index));
        }
        let value: Value =
            serde_json::from_str(item.payload).map_err(|_| PolicyError::PayloadNotJson(index))?;
        let activity_type = value
            .as_object()
            .ok_or(PolicyError::PayloadNotJson(index))?
            .get("type")
            .and_then(Value::as_str)
            .ok_or(PolicyError::MissingActivityType(index))?;
        check_activity_type(limits, activity_type)?;
    }
    Ok(())
}

fn check_activity_type(limits: &SigningLimits, activity_type: &str) -> Result<(), PolicyError> {
    let upper = activity_type.to_ascii_uppercase();
    if FORBIDDEN_ACTIVITY_MARKERS.iter().any(|m| upper.contains(m)) {
        return Err(PolicyError::ForbiddenActivityType(activity_type.to_string()));
    }
    if !limits.allowed_activity_types.iter().any(|t| t == activity_type) {
        return Err(PolicyError::ActivityTypeNotAllowed(activity_type.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(allowed: &[&str]) -> SigningLimits {
        SigningLimits {
            max_order_micros: 50 * MICROS_PER_USD,
            daily_micros: 100 * MICROS_PER_USD,
            intent_ttl_secs: 120,
            max_payloads: 2,
            allowed_activity_types: allowed.iter().map(|s| s.to_string()).collect(),
            sell_max_order_micros: 500 * MICROS_PER_USD,
            sell_daily_micros: 1000 * MICROS_PER_USD,
            withdraw_max_order_micros: 20 * MICROS_PER_USD,
            withdraw_daily_micros: 30 * MICROS_PER_USD,
        }
    }

    fn digest(payload: &str) -> String {
        hex::encode(Sha256::digest(payload.as_bytes()))
    }

    fn check(allowed: &[&str], payload: &str, digest_value: &str) -> Result<(), PolicyError> {
        check_payloads(&limits(allowed), &[PayloadRef { digest: digest_value, payload }])
    }

    #[test]
    fn parses_amounts() {
        assert_eq!(parse_usd_micros("1"), Ok(1_000_000));
        assert_eq!(parse_usd_micros("12.5"), Ok(12_500_000));
        assert_eq!(parse_usd_micros("0.000001"), Ok(1));
        for bad in ["", "0", "-1", "1e3", "1.1234567", ".5", "abc", "1,5", "9999999999999"] {
            assert!(parse_usd_micros(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn enforces_order_and_daily_limits() {
        let l = limits(&[]);
        assert!(check_amount_for(IntentKind::Buy, &l, 50 * MICROS_PER_USD, 50 * MICROS_PER_USD).is_ok());
        assert_eq!(check_amount_for(IntentKind::Buy, &l, 51 * MICROS_PER_USD, 0), Err(PolicyError::OrderTooLarge));
        assert_eq!(
            check_amount_for(IntentKind::Buy, &l, MICROS_PER_USD, 100 * MICROS_PER_USD),
            Err(PolicyError::DailyLimitExceeded)
        );
        assert_eq!(check_amount_for(IntentKind::Buy, &l, 1, i64::MAX), Err(PolicyError::DailyLimitExceeded));
    }

    #[test]
    fn each_kind_has_its_own_ceilings() {
        let l = limits(&[]);
        assert!(check_amount_for(IntentKind::Sell, &l, 500 * MICROS_PER_USD, 500 * MICROS_PER_USD).is_ok());
        assert_eq!(check_amount_for(IntentKind::Sell, &l, 501 * MICROS_PER_USD, 0), Err(PolicyError::OrderTooLarge));
        assert_eq!(check_amount_for(IntentKind::Withdraw, &l, 21 * MICROS_PER_USD, 0), Err(PolicyError::OrderTooLarge));
        assert!(check_amount_for(IntentKind::Withdraw, &l, 20 * MICROS_PER_USD, 10 * MICROS_PER_USD).is_ok());
        assert_eq!(
            check_amount_for(IntentKind::Withdraw, &l, 20 * MICROS_PER_USD, 11 * MICROS_PER_USD),
            Err(PolicyError::DailyLimitExceeded)
        );
    }

    #[test]
    fn allows_listed_activity_with_matching_digest() {
        let payload = r#"{"type":"ACTIVITY_TYPE_SIGN_TRANSACTION_V2"}"#;
        assert_eq!(check(&["ACTIVITY_TYPE_SIGN_TRANSACTION_V2"], payload, &digest(payload)), Ok(()));
    }

    #[test]
    fn digest_is_case_insensitive_but_must_match() {
        let payload = r#"{"type":"ACTIVITY_TYPE_SIGN_TRANSACTION_V2"}"#;
        let allowed = ["ACTIVITY_TYPE_SIGN_TRANSACTION_V2"];
        assert!(check(&allowed, payload, &digest(payload).to_uppercase()).is_ok());
        assert_eq!(check(&allowed, payload, &digest("other")), Err(PolicyError::DigestMismatch(0)));
    }

    #[test]
    fn fails_closed_when_nothing_is_allowed() {
        let payload = r#"{"type":"ACTIVITY_TYPE_SIGN_TRANSACTION_V2"}"#;
        assert!(matches!(
            check(&[], payload, &digest(payload)),
            Err(PolicyError::ActivityTypeNotAllowed(_))
        ));
    }

    #[test]
    fn forbidden_types_are_rejected_even_if_listed() {
        for activity in [
            "ACTIVITY_TYPE_EXPORT_SECRETS",
            "ACTIVITY_TYPE_CREATE_API_KEYS",
            "ACTIVITY_TYPE_CREATE_POLICY",
            "ACTIVITY_TYPE_DELETE_WALLETS",
        ] {
            let payload = format!(r#"{{"type":"{activity}"}}"#);
            assert!(matches!(
                check(&[activity], &payload, &digest(&payload)),
                Err(PolicyError::ForbiddenActivityType(_))
            ));
        }
    }

    #[test]
    fn rejects_malformed_payloads() {
        let allowed = ["A"];
        let not_json = "nope";
        assert_eq!(check(&allowed, not_json, &digest(not_json)), Err(PolicyError::PayloadNotJson(0)));
        let array = "[1]";
        assert_eq!(check(&allowed, array, &digest(array)), Err(PolicyError::PayloadNotJson(0)));
        let untyped = r#"{"a":1}"#;
        assert_eq!(check(&allowed, untyped, &digest(untyped)), Err(PolicyError::MissingActivityType(0)));
    }

    #[test]
    fn enforces_payload_count() {
        let l = limits(&["A"]);
        assert!(check_payloads(&l, &[]).is_err());
        let payload = r#"{"type":"A"}"#;
        let d = digest(payload);
        let three: Vec<_> = (0..3).map(|_| PayloadRef { digest: &d, payload }).collect();
        assert!(check_payloads(&l, &three).is_err());
    }
}
