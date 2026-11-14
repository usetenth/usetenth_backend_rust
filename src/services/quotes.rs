use crate::app_state::ApplicationState;
use crate::engine::money::{format_micros, unit_price};
use crate::integrations::types::{CreateOrderRequest, DefiQuoteRequest, OrderSide, OrderType, QuantityUnit, QuoteRequest};
use anyhow::{Result, bail};

pub struct TradeQuote {
    pub qty: Option<String>,
    pub notional: Option<String>,
    pub price: Option<String>,
    pub fee: Option<String>,
}

// Tokenized stocks live on a chain and are priced by contract address; anything else goes through the exchange quote.
async fn listing(state: &ApplicationState, asset_key: &str) -> Result<(Option<String>, Option<String>)> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as("SELECT chain, address FROM assets WHERE asset_key = $1")
        .bind(asset_key)
        .fetch_optional(&state.pool)
        .await?;
    match row {
        Some(r) => Ok(r),
        None => bail!("unknown asset {asset_key}"),
    }
}

async fn exchange_quote(state: &ApplicationState, gateway_id: &str, asset_key: &str, side: OrderSide, qty: String, unit: QuantityUnit) -> Result<TradeQuote> {
    let preferred = &state.config.engine.settlement_symbols;
    let Some(quote_asset) = crate::services::catalog::quote_asset_for(&state.pool, asset_key, preferred).await? else {
        bail!("no settlement stablecoin is listed");
    };
    let q = state.tm_client.get_market_quote(gateway_id, &QuoteRequest { base_asset: asset_key.to_string(), quote_asset, qty, qty_unit: unit, side }).await?;
    Ok(TradeQuote { qty: q.qty, notional: q.notional, price: q.price, fee: q.fee })
}

// The DeFi quote endpoint, asked on the user's behalf.
async fn defi_quote(state: &ApplicationState, gateway_id: &str, chain: String, address: String, side: OrderSide, qty: String, unit: QuantityUnit) -> Result<(Option<String>, Option<String>)> {
    let q = state.tm_client.defi_quote(gateway_id, &DefiQuoteRequest { chain, base_asset: address, order_side: side, qty, qty_unit: unit }).await?;
    if q.qty_out.is_none() {
        let reasons = q.issues.map(|i| serde_json::Value::Array(i).to_string()).unwrap_or_default();
        bail!("the quote came back without an amount {reasons}");
    }
    Ok((q.qty_out, q.fee))
}

// The gateway also prices a tokenized stock when an order is created, and nothing moves until that order is executed.
async fn order_quote(state: &ApplicationState, gateway_id: &str, asset_key: &str, side: OrderSide, qty: String, unit: QuantityUnit) -> Result<(Option<String>, Option<String>)> {
    let created = state
        .tm_client
        .create_order(gateway_id, &CreateOrderRequest { asset_id: asset_key.to_string(), qty, qty_unit: unit, side, order_type: OrderType::Market })
        .await?;
    match created.quote {
        Some(q) if q.qty_out.is_some() => Ok((q.qty_out, q.fee)),
        Some(q) => bail!("the order came back without an amount {:?}", q.issues),
        None => bail!("the order came back without a quote"),
    }
}

// Tries the DeFi quote first and the order quote if that is refused, keeping both reasons when neither works.
async fn token_quote(state: &ApplicationState, gateway_id: &str, asset_key: &str, chain: String, address: String, side: OrderSide, qty: String, unit: QuantityUnit) -> Result<(Option<String>, Option<String>)> {
    match defi_quote(state, gateway_id, chain, address, side, qty.clone(), unit).await {
        Ok(found) => Ok(found),
        Err(first) => {
            eprintln!("DeFi quote refused, pricing through an order instead: {first:#}");
            order_quote(state, gateway_id, asset_key, side, qty, unit).await.map_err(|second| anyhow::anyhow!("{second:#} (the DeFi quote said: {first:#})"))
        }
    }
}

/// What spending `amount_micros` on a stock gets: tokens out, the price each and the fee.
pub async fn buy(state: &ApplicationState, gateway_id: &str, asset_key: &str, amount_micros: i64) -> Result<TradeQuote> {
    let amount = format_micros(amount_micros);
    match listing(state, asset_key).await? {
        (Some(chain), Some(address)) => {
            let (out, fee) = token_quote(state, gateway_id, asset_key, chain, address, OrderSide::Buy, amount.clone(), QuantityUnit::Quote).await?;
            let price = out.as_deref().and_then(|tokens| unit_price(&amount, tokens));
            Ok(TradeQuote { qty: out, notional: None, price, fee })
        }
        _ => exchange_quote(state, gateway_id, asset_key, OrderSide::Buy, amount, QuantityUnit::Quote).await,
    }
}

/// What selling `qty` tokens pays: the proceeds in stablecoin, the price each and the fee.
pub async fn sell(state: &ApplicationState, gateway_id: &str, asset_key: &str, qty: &str) -> Result<TradeQuote> {
    match listing(state, asset_key).await? {
        (Some(chain), Some(address)) => {
            let (out, fee) = token_quote(state, gateway_id, asset_key, chain, address, OrderSide::Sell, qty.to_string(), QuantityUnit::Base).await?;
            let price = out.as_deref().and_then(|proceeds| unit_price(proceeds, qty));
            Ok(TradeQuote { qty: out.clone(), notional: out, price, fee })
        }
        _ => exchange_quote(state, gateway_id, asset_key, OrderSide::Sell, qty.to_string(), QuantityUnit::Base).await,
    }
}
