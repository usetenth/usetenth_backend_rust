use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use sqlx::Type;

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
#[sqlx(type_name = "verification_status", rename_all = "lowercase")]
pub enum VerificationStatus {
    UNVERIFIED,
    REVIEW,
    VERIFIED,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
#[sqlx(type_name = "investment_plan", rename_all = "lowercase")]
pub enum InvestmentPlan {
    STEADY,
    GROWTH,
    TECH,
    CUSTOM
}


#[derive(Serialize, Deserialize, Clone, Debug, Type)]
#[sqlx(type_name = "event_status", rename_all = "lowercase")]
pub enum EventStatus {
    PENDING,
    COMPLETED,
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Clone, Debug)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password: Option<String>,
    pub email: Option<String>,
    pub telegram_id: Option<String>, //Foreign key for user telegram session
    pub verified: VerificationStatus,
    pub external_ref_id: String,
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
pub struct Deposits {
    id: Uuid, //Primary Key
    user_id : String,
    tx_id: String,
    amount: i64,
    asset: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Rule {
    id: Uuid, //Primary Key
    pub plan: InvestmentPlan,
    pub user_id: String,
    pub expiry: DateTime<Utc>,
    pub token_type: Vec<String>,
    pub enabled: bool,
    pub percentage: i8,
    pub fixed_amount: i64,
    pub minimum_amount: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Order {
    id: Uuid,
    user_id : String,
    tx_id: String,
    amount: i64,
    asset: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Payment {
    id : Uuid,
    user_id : String,
    asset : String,
    amount: i64,
    status: EventStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Investment {
    id: Uuid,
    user_id: String,
    tx_id: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Job {
    pub id: Uuid,
    status: EventStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct InvestmentEvent {
    id: Uuid, //Primary Key
    user_id: String,
    payment_id: String,
    rule_id: String,
    asset: String,
    paid_amount: i64,
    investment_amount: i64,
    status: EventStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct OrderEvent {
    id : Uuid,
    user_id : String,
    order_id : String,
    asset : String,
    amount: i64,
    investment_id : String,
    status: EventStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
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
    pub updated_at: DateTime<Utc>
}

#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct AutoInvestmentPolicy {
    id: Uuid,
    created_at: DateTime<Utc>,
    expires: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}


#[derive(Debug, Clone, sqlx::FromRow, Deserialize, Serialize)]
pub struct Wallet {
    user_id: String, //Primary Key
    address: String,
    network: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TelegramUpdates {
    id: Uuid,
    update_id: i64,
    created_at: DateTime<Utc>,
}
