use super::{LocalKeyWrapper, SigningLimits, SigningPayload, SigningService, policy::MICROS_PER_USD};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub const ACTIVITY: &str = "ACTIVITY_TYPE_SIGN_TRANSACTION_V2";

pub fn limits(max_order_usd: i64, daily_usd: i64) -> SigningLimits {
    SigningLimits {
        max_order_micros: max_order_usd * MICROS_PER_USD,
        daily_micros: daily_usd * MICROS_PER_USD,
        intent_ttl_secs: 120,
        max_payloads: 2,
        allowed_activity_types: vec![ACTIVITY.to_string()],
        sell_max_order_micros: 500 * MICROS_PER_USD,
        sell_daily_micros: 2000 * MICROS_PER_USD,
        withdraw_max_order_micros: 100 * MICROS_PER_USD,
        withdraw_daily_micros: 200 * MICROS_PER_USD,
    }
}

pub fn wrapper() -> LocalKeyWrapper {
    LocalKeyWrapper::from_spec(&format!("1:{}", hex::encode([7u8; 32])), 1).unwrap()
}

pub async fn service(pool: &PgPool, limits: SigningLimits) -> SigningService<LocalKeyWrapper> {
    // `#[sqlx::test]` has already applied every migration to this database.
    SigningService::new(pool.clone(), wrapper(), limits)
}

pub async fn insert_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("INSERT INTO users (telegram_id) VALUES ($1) RETURNING id")
        .bind(rand::random::<i64>())
        .fetch_one(pool)
        .await
        .unwrap()
}

pub fn payload(nonce: &str) -> SigningPayload {
    payload_of_type(ACTIVITY, nonce)
}

pub fn payload_of_type(activity: &str, nonce: &str) -> SigningPayload {
    let payload = format!(r#"{{"type":"{activity}","nonce":"{nonce}"}}"#);
    SigningPayload { digest: hex::encode(Sha256::digest(payload.as_bytes())), payload }
}
