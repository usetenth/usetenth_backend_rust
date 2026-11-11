use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Type;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "verification_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum VerificationStatus { Unverified, Review, Verified }

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "investment_plan", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum InvestmentPlan { Steady, Growth, Tech, Custom }

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "event_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Pending,
    Processing,
    Submitted,
    AwaitingSettlement,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "signing_identity_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SigningIdentityStatus { Pending, Active, Revoked, Failed }

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "job_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum JobStatus { Pending, Processing, Completed, Failed, DeadLetter }

#[derive(sqlx::FromRow, Serialize, Deserialize, Clone, Debug)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: Option<String>,
    pub email: Option<String>,
    pub telegram_id: Option<String>,
    pub verified: VerificationStatus,
    pub gateway_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct TelegramSession {
    pub telegram_id: String,
    pub telegram_user_name: String,
    pub expires: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Deposit {
    pub id: Uuid,
    pub user_id: Uuid,
    pub tx_id: String,
    pub amount: i64,
    pub asset: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Rule {
    pub id: Uuid,
    pub plan: InvestmentPlan,
    pub user_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub token_type: Vec<String>,
    pub enabled: bool,
    /// Validate 0..=100 in the DB and application.
    pub percentage: i16,
    pub fixed_amount: i64,
    pub minimum_amount: i64,
    pub max_per_transaction: Option<i64>,
    pub max_daily_amount: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Order {
    pub id: Uuid,
    pub user_id: Uuid,
    pub gateway_order_id: String,
    pub tx_id: Option<String>,
    pub amount: i64,
    pub asset: String,
    pub status: EventStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Payment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub asset: String,
    pub amount: i64,
    pub status: EventStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Investment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub tx_id: Option<String>,
    pub gateway_order_id: Option<String>,
    pub asset: String,
    pub amount: i64,
    pub status: EventStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Job {
    pub id: Uuid,
    pub job_type: String,
    pub reference_id: String,
    pub status: JobStatus,
    pub attempt_count: i32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct InvestmentEvent {
    pub id: Uuid,
    pub user_id: Uuid,
    pub payment_id: Uuid,
    pub rule_id: Uuid,
    pub policy_version: i32,
    pub asset: String,
    pub paid_amount: i64,
    pub investment_amount: i64,
    pub idempotency_key: String,
    pub status: EventStatus,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct OrderEvent {
    pub id: Uuid,
    pub user_id: Uuid,
    pub gateway_order_id: String,
    pub asset: String,
    pub amount: i64,
    pub investment_id: Uuid,
    pub status: EventStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub asset_key: String,
    pub symbol: String,
    pub ticker: String,
    pub name: String,
    pub chain: String,
    pub address: String,
    pub venue: String,
    pub asset_class: String,
    pub networks: Vec<String>,
    pub provider_id: String,
    pub tradable: bool,
    pub stable: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct AutoInvestmentPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub enabled: bool,
    pub policy_version: i32,
    pub plan: InvestmentPlan,
    pub percentage: i16,
    pub fixed_amount: i64,
    pub minimum_amount: i64,
    pub max_per_transaction: Option<i64>,
    pub max_daily_amount: Option<i64>,
    pub allowed_assets: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub consent_reference: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct SigningIdentity {
    pub id: Uuid,
    pub user_id: Uuid,
    pub gateway_user_id: String,
    pub signer_public_key: String,
    pub signer_provider: String,
    /// Opaque provider key reference only
    pub key_reference: String,
    pub status: SigningIdentityStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct SigningAuthorization {
    pub id: Uuid,
    pub user_id: Uuid,
    pub policy_id: Uuid,
    pub policy_version: i32,
    pub gateway_order_id: String,
    pub payload_hash: String,
    pub status: EventStatus,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Wallet {
    pub user_id: Uuid,
    pub address: String,
    pub network: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TelegramUpdates {
    pub id: Uuid,
    pub update_id: i64,
    pub created_at: DateTime<Utc>,
}
