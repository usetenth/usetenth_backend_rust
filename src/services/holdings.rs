
use crate::app_state::ApplicationState;
use crate::engine::money::{format_micros, mul_to_micros, parse_decimal};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Holding {
    pub asset_key: String,
    pub symbol: String,
    pub name: String,
    pub available: String,
    pub price: Option<String>,
    pub value_usd: Option<String>,
}

pub async fn sellable(state: &ApplicationState, gateway_user: &str) -> anyhow::Result<Vec<Holding>> {
    let portfolio = state.tm_client.get_portfolio(gateway_user).await?;
    Ok(portfolio
        .balances
        .into_iter()
        .filter(|b| !b.stable && b.tradeable)
        .filter(|b| parse_decimal(&b.available).is_some_and(|(n, _)| n > 0))
        .filter_map(|b| {
            let asset_key = b.asset_id?.to_string();
            let value = b.price.as_deref().and_then(|p| mul_to_micros(&b.available, p)).map(format_micros);
            Some(Holding {
                asset_key,
                name: b.name.clone().unwrap_or_else(|| b.symbol.clone()),
                symbol: b.symbol,
                available: b.available,
                price: b.price,
                value_usd: value,
            })
        })
        .collect())
}
