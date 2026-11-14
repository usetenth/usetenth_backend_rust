// Mirrors the gateway API; not every field or call is used yet.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use uuid::Uuid;

pub type ApiObject = Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuantityUnit {
    Base,
    Quote,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayOrderStatus {
    Initialized,
    Pending,
    Complete,
    Active,
    Canceled,
    CancelPending,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Venue {
    Defi,
    Cefi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Chain {
    Solana,
    Base,
    Robinhood,
    HypercoreSpot,
    HypercorePerp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProductType {
    Spot,
    Perp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetClass {
    Crypto,
    Stock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    Active,
    Preview,
    SellOnly,
    TransferOnly,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Settlement {
    Paxos,
    Defi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PositionSide {
    Long,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    TakeProfit,
    StopLoss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteReason {
    LimitOrder,
    BestPrice,
    CefiNoMarket,
    CefiUnavailable,
    DefiUnavailable,
    NoMarket,
    OnlyVenue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    AwaitingSignature,
    Pending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RampPaymentMethod {
    Coinbase,
    Paypal,
    ApplePay,
    GooglePay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RampDirection {
    Onramp,
    Offramp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RampStatus {
    Created,
    Pending,
    Completed,
    Failed,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionType {
    Order,
    Transfer,
    Ramp,
    ReceivedTransfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransactionStatus {
    Pending,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetFlow {
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChainFamily {
    Evm,
    Solana,
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

#[derive(Debug, Clone, Serialize)]
pub struct CreateOrderRequest {
    pub asset_id: String,
    pub qty: String,
    pub qty_unit: QuantityUnit,
    pub side: OrderSide,
    #[serde(rename = "type")]
    pub order_type: OrderType,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecuteRequest {
    /// Gateway signature envelopes as documented by True Markets.
    pub signatures: Vec<String>,
    pub auth_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CancelOrderRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGatewayUserRequest {
    pub external_ref_id: String,
    pub signer_public_key: String,
}

/// POST /transfers; `network` is omitted because DeFi transfers use the asset's own chain.
#[derive(Debug, Clone, Serialize)]
pub struct TransferRequest {
    pub asset_id: String,
    pub qty: String,
    pub qty_unit: QuantityUnit,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampRequest {
    #[serde(flatten)]
    pub fields: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayUserResponse {
    pub user_id: String,
    pub external_ref_id: String,
    pub created_at: DateTime<Utc>,
    pub wallets: Vec<GatewayUserWallet>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayUserWallet {
    pub address: String,
    pub chain_family: ChainFamily,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pagination {
    pub next_cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnsignedPayload {
    pub digest: String,
    pub payload: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuoteResponse {
    pub qty: Option<String>,
    pub price: Option<String>,
    pub notional: Option<String>,
    pub fee: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Trigger {
    #[serde(rename = "type")]
    pub trigger_type: TriggerType,
    pub price: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuoteDetails {
    pub base_asset: Option<String>,
    pub quote_asset: Option<String>,
    pub qty: Option<String>,
    pub qty_out: Option<String>,
    pub fee: Option<String>,
    pub fee_asset: Option<String>,
    #[serde(default)]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateOrderResponse {
    pub order_id: Option<String>,
    pub status: Option<GatewayOrderStatus>,
    pub venue: Option<Venue>,
    pub route_reason: Option<RouteReason>,
    #[serde(default)]
    pub payloads: Vec<UnsignedPayload>,
    pub quote: Option<QuoteDetails>,
    pub web_authn_payload: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecuteOrderResponse {
    pub status: Option<GatewayOrderStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelOrderResponse {
    #[serde(default)]
    pub payloads: Vec<UnsignedPayload>,
}

pub type ModifyOrderResponse = Map<String, Value>;

#[derive(Debug, Clone, Deserialize)]
pub struct GetOrderStatusResponse {
    pub status: Option<GatewayOrderStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderDetail {
    pub order_id: Option<String>,
    pub asset_id: Option<Uuid>,
    pub economic_asset_id: Option<Uuid>,
    pub asset_symbol: Option<String>,
    pub base_asset: Option<String>,
    pub quote_asset: Option<String>,
    #[serde(rename = "type")]
    pub order_type: Option<OrderType>,
    pub side: Option<OrderSide>,
    pub price: Option<String>,
    pub qty: Option<String>,
    pub qty_unit: Option<QuantityUnit>,
    pub executed_qty: Option<String>,
    pub leaves_qty: Option<String>,
    pub executed_vwap: Option<String>,
    pub fee: Option<String>,
    pub tx_hash: Option<String>,
    pub explorer_url: Option<String>,
    pub failure_reason: Option<String>,
    pub venue: Option<Venue>,
    pub chain: Option<Chain>,
    pub status: Option<GatewayOrderStatus>,
    pub created_at: Option<DateTime<Utc>>,
    pub asset_type: Option<ProductType>,
    pub trigger: Option<Trigger>,
    pub strategy_managed: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetOrdersResponse {
    #[serde(default)]
    pub data: Vec<OrderDetail>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BalanceItem {
    pub asset_id: Option<String>,
    pub economic_asset_id: Option<String>,
    pub venue: Option<Venue>,
    pub symbol: Option<String>,
    pub name: Option<String>,
    pub chain: Option<String>,
    pub address: Option<String>,
    pub decimals: Option<u32>,
    pub total: Option<String>,
    pub available: Option<String>,
    pub held: Option<String>,
    pub allocated: Option<String>,
    pub stable: Option<bool>,
    pub tradeable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListBalancesResponse {
    #[serde(default)]
    pub data: Vec<BalanceItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    pub asset_id: Option<String>,
    pub venue: Option<Venue>,
    pub symbol: Option<String>,
    pub side: Option<PositionSide>,
    pub size: Option<String>,
    pub entry_price: Option<String>,
    pub mark_price: Option<String>,
    pub liquidation_price: Option<String>,
    pub margin_used: Option<String>,
    pub unrealized_pnl: Option<String>,
    pub return_on_equity: Option<String>,
    pub leverage: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListPositionsResponse {
    pub positions: Vec<Position>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioCostBasis {
    pub avg_entry_price: String,
    pub cost_usd: String,
    pub qty: String,
    pub realized_pnl: String,
    pub unrealized_pnl: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioBalance {
    pub asset_id: Option<Uuid>,
    pub economic_asset_id: Option<Uuid>,
    pub symbol: String,
    pub name: Option<String>,
    pub venue: Venue,
    pub chain: Option<String>,
    pub address: Option<String>,
    pub decimals: u32,
    pub icon: Option<String>,
    pub status: Option<AssetStatus>,
    pub stable: bool,
    pub tradeable: bool,
    pub total: String,
    pub available: String,
    pub held: String,
    pub allocated: Option<String>,
    pub in_transit: Option<String>,
    pub price: Option<String>,
    pub value: Option<String>,
    pub change_24h_pct: Option<String>,
    pub cost_basis: Option<PortfolioCostBasis>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioStrategies {
    pub value_usd: Option<String>,
    pub cash_usd: String,
    pub free_usd: Option<String>,
    pub allocated_usd: String,
    pub pnl_usd: Option<String>,
    pub as_of: DateTime<Utc>,
    pub perp_data_unavailable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioUnavailableChain {
    pub chain: String,
    pub owner: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioAssetRef {
    pub asset_id: Uuid,
    pub symbol: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioIncomplete {
    #[serde(default)]
    pub unavailable_chains: Vec<PortfolioUnavailableChain>,
    #[serde(default)]
    pub unpriced: Vec<String>,
    #[serde(default)]
    pub over_allocated: Vec<PortfolioAssetRef>,
    #[serde(default)]
    pub stranded: Vec<PortfolioAssetRef>,
    #[serde(default)]
    pub unrecognized_cefi_assets: Vec<String>,
    pub positions_unavailable: Option<bool>,
    pub defi_unavailable: Option<bool>,
    pub cefi_unavailable: Option<bool>,
    pub strategies_unavailable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PortfolioResponse {
    pub total_usd: Option<String>,
    pub account_total_usd: Option<String>,
    pub balances_usd: Option<String>,
    pub cash_usd: Option<String>,
    pub change_24h_usd: Option<String>,
    pub change_24h_pct: Option<String>,
    pub unrealized_pnl_usd: Option<String>,
    pub realized_pnl_usd: Option<String>,
    pub balances: Vec<PortfolioBalance>,
    pub positions: Vec<Position>,
    pub strategies: Option<PortfolioStrategies>,
    pub incomplete: Option<PortfolioIncomplete>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetImage {
    pub thumb: Option<String>,
    pub small: Option<String>,
    pub large: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetMarketData {
    pub circulating_supply: Option<i64>,
    pub total_supply: Option<i64>,
    pub max_supply: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetPerpData {
    pub funding_rate: Option<String>,
    pub open_interest: Option<String>,
    pub volume_24h: Option<String>,
    pub max_leverage: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetSocials {
    pub x_username: Option<String>,
    pub facebook_username: Option<String>,
    pub subreddit_url: Option<String>,
    pub official_forum_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetNetwork {
    pub network: String,
    pub compatible_networks: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetItem {
    pub id: Option<Uuid>,
    pub economic_asset_id: Option<Uuid>,
    pub chain: Option<String>,
    pub address: Option<String>,
    pub symbol: Option<String>,
    pub name: Option<String>,
    pub decimals: Option<u8>,
    pub slug: Option<String>,
    pub is_active: Option<bool>,
    pub status: Option<AssetStatus>,
    pub description: Option<String>,
    pub website: Option<String>,
    pub icon: Option<String>,
    pub tradeable: Option<bool>,
    pub stable: Option<bool>,
    pub venue: Option<Venue>,
    #[serde(rename = "type")]
    pub product_type: Option<ProductType>,
    pub asset_class: Option<AssetClass>,
    pub image: Option<AssetImage>,
    pub market_data: Option<AssetMarketData>,
    pub perp: Option<AssetPerpData>,
    pub socials: Option<AssetSocials>,
    pub networks: Vec<AssetNetwork>,
    pub settlement: Option<Settlement>,
    pub cefi_instrument: Option<String>,
    pub order_types: Vec<OrderType>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListAssetsResponse {
    #[serde(default)]
    pub data: Vec<AssetItem>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransferDetail {
    pub id: Uuid,
    pub status: TransferStatus,
    pub venue: Venue,
    pub asset_id: Uuid,
    pub asset_symbol: String,
    pub chain: Option<String>,
    pub network: Option<String>,
    pub to: String,
    pub qty: String,
    pub qty_unit: QuantityUnit,
    // Empty until the transfer is broadcast; tolerate absence rather than fail a transfer that moved.
    #[serde(default)]
    pub sent: String,
    #[serde(default)]
    pub fee: String,
    #[serde(default)]
    pub received: String,
    #[serde(default)]
    pub tx_hash: String,
    pub explorer_url: Option<String>,
    #[serde(default)]
    pub payloads: Vec<UnsignedPayload>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListTransfersResponse {
    #[serde(default)]
    pub data: Vec<TransferDetail>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RampBridgeStep {
    pub kind: String,
    pub amount: String,
    pub asset_symbol: String,
    pub dst_asset_symbol: Option<String>,
    pub src_chain: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RampBridgeShortfall {
    pub amount: String,
    pub asset_symbol: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RampBridgePlan {
    pub id: String,
    pub payloads: Vec<UnsignedPayload>,
    pub steps: Vec<RampBridgeStep>,
    pub shortfall: RampBridgeShortfall,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRampResponse {
    pub id: Uuid,
    pub redirect_url: String,
    pub bridge_plan: Option<RampBridgePlan>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RampDetail {
    pub id: Uuid,
    pub payment_method: RampPaymentMethod,
    pub direction: RampDirection,
    pub status: RampStatus,
    pub amount: String,
    pub chain: Chain,
    pub asset_symbol: Option<String>,
    pub asset_amount: Option<String>,
    pub fiat_currency: Option<String>,
    pub fiat_amount: Option<String>,
    pub fee_amount: Option<String>,
    pub fee_currency: Option<String>,
    pub exchange_rate: Option<String>,
    pub wallet_address: Option<String>,
    pub provider_wallet_address: Option<String>,
    pub tx_hash: Option<String>,
    pub redirect_url: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReceivedTransferDetail {
    pub id: Uuid,
    pub chain: String,
    pub tx_hash: String,
    pub from_address: String,
    pub to_address: String,
    pub token_address: String,
    pub token_symbol: String,
    pub amount: String,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransactionDetail {
    pub id: Option<Uuid>,
    #[serde(rename = "type")]
    pub transaction_type: Option<TransactionType>,
    pub asset_flow: Option<AssetFlow>,
    pub status: Option<TransactionStatus>,
    pub created_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub qty: Option<String>,
    pub asset_id: Option<Uuid>,
    pub asset_symbol: Option<String>,
    pub tx_hash: Option<String>,
    pub explorer_url: Option<String>,
    pub failure_reason: Option<String>,
    pub order: Option<OrderDetail>,
    pub transfer: Option<TransferDetail>,
    pub ramp: Option<RampDetail>,
    pub received_transfer: Option<ReceivedTransferDetail>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListTransactionsResponse {
    #[serde(default)]
    pub data: Vec<TransactionDetail>,
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FieldViolation {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorResponse {
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: Option<String>,
    pub message: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub field_violations: Vec<FieldViolation>,
    pub request_id: String,
}
