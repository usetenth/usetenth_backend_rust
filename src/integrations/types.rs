use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ApiObject = Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuantityUnit { Base, Quote }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide { Buy, Sell }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType { Market, Limit }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayOrderStatus {
    Initialized, Pending, Complete, Active, Canceled, CancelPending, Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuoteRequest {
    pub base_asset: String,
    pub quote_asset: String,
    /// Decimal quantity represented as a string to avoid floating-point rounding.
    pub qty: String,
    pub qty_unit: QuantityUnit,
    pub side: OrderSide,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuoteResponse {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateOrderRequest {
    pub asset_id: String,
    pub qty: String,
    pub qty_unit: QuantityUnit,
    pub side: OrderSide,
    #[serde(rename = "type")]
    pub order_type: OrderType,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnsignedPayload {
    /// Sign this exact string; do not parse and serialize it again.
    pub payload: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<GatewayOrderStatus>,
    #[serde(default)]
    pub payloads: Vec<UnsignedPayload>,
    #[serde(default)]
    pub quote: Option<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecuteRequest {
    /// Gateway signature envelopes as documented by True Markets.
    pub signatures: Vec<String>,
    pub auth_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecuteResponse {
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub transfer_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<GatewayOrderStatus>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelOrderRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGatewayUserRequest {
    /// Use only fields required by the current Accounts API schema.
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayUserResponse {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetListResponse {
    #[serde(default)]
    pub assets: Vec<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetResponse {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BalanceResponse {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioResponse {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PositionsResponse {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderListResponse {
    #[serde(default)]
    pub orders: Vec<OrderResponse>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderStatusResponse {
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<GatewayOrderStatus>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRequest {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransferResponse {
    #[serde(default)]
    pub transfer_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub payloads: Vec<UnsignedPayload>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransferListResponse {
    #[serde(default)]
    pub transfers: Vec<TransferResponse>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransactionsResponse {
    #[serde(default)]
    pub transactions: Vec<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampRequest {
    #[serde(flatten)]
    pub fields: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RampResponse {
    #[serde(default)]
    pub ramp_id: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorResponse {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}
