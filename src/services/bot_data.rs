use crate::app_state::ApplicationState;
use crate::models::schemas::InvestmentPlan;
use crate::services::bot::TelegramUser;

#[derive(serde::Serialize)]
pub struct BotHolding {
    pub name: String,
    pub amount: String,
}

#[derive(serde::Serialize)]
pub struct BotPortfolio {
    pub cash: String,
    pub value: String,
    pub invested: String,
    pub gain: Option<String>,
    pub holdings: Vec<BotHolding>,
}

pub struct BotRule {
    pub percent: i16,
    pub mix_name: String,
}

#[derive(serde::Serialize)]
pub struct BotNetworkAddress {
    pub network: String,
    pub address: String,
}

#[derive(serde::Serialize)]
pub struct BotDeposit {
    pub asset: String,
    pub default_network: String,
    pub networks: Vec<BotNetworkAddress>,
}

pub struct BotSellable {
    pub asset_key: String,
    pub name: String,
    pub value: Option<String>,
}

async fn gateway_user_id(state: &ApplicationState, from: &TelegramUser) -> anyhow::Result<Option<String>> {
    let id: Option<Option<String>> = sqlx::query_scalar("SELECT gateway_user_id FROM users WHERE telegram_id = $1")
        .bind(from.id)
        .fetch_optional(&state.pool)
        .await?;
    Ok(id.flatten())
}

fn money(value: f64) -> String {
    format!("{value:.2}")
}

fn parse_amount(value: Option<&str>) -> f64 {
    value.and_then(|v| v.parse().ok()).unwrap_or(0.0)
}

pub async fn get_portfolio(state: &ApplicationState, from: &TelegramUser) -> anyhow::Result<Option<BotPortfolio>> {
    match gateway_user_id(state, from).await? {
        Some(gateway_id) => portfolio_for_gateway(state, &gateway_id).await.map(Some),
        None => Ok(None),
    }
}

pub async fn portfolio_for_gateway(state: &ApplicationState, gateway_id: &str) -> anyhow::Result<BotPortfolio> {
    let portfolio = state.tm_client.get_portfolio(gateway_id).await?;

    // Stablecoins are cash; everything else the user holds is an investment.
    let holdings: Vec<_> = portfolio
        .balances
        .iter()
        .filter(|b| !b.stable && parse_amount(Some(&b.total)) > 0.0)
        .collect();
    let value: f64 = holdings.iter().map(|b| parse_amount(b.value.as_deref())).sum();
    let invested: f64 = holdings
        .iter()
        .map(|b| parse_amount(b.cost_basis.as_ref().map(|c| c.cost_usd.as_str())))
        .sum();

    Ok(BotPortfolio {
        cash: money(parse_amount(portfolio.cash_usd.as_deref())),
        value: money(value),
        invested: money(invested),
        gain: portfolio.unrealized_pnl_usd.as_deref().map(|g| money(parse_amount(Some(g)))),
        holdings: holdings
            .iter()
            .map(|b| BotHolding {
                name: b.name.clone().unwrap_or_else(|| b.symbol.clone()),
                amount: b.total.clone(),
            })
            .collect(),
    })
}

pub async fn get_rule(state: &ApplicationState, from: &TelegramUser) -> anyhow::Result<Option<BotRule>> {
    let row: Option<(i16, InvestmentPlan)> = sqlx::query_as(
        "SELECT r.percentage, r.plan FROM rules r JOIN users u ON u.id = r.user_id WHERE u.telegram_id = $1",
    )
    .bind(from.id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.map(|(percent, plan)| BotRule {
        percent,
        mix_name: match plan {
            InvestmentPlan::Steady => "Steady",
            InvestmentPlan::Growth => "Growth",
            InvestmentPlan::Tech => "Tech-heavy",
            InvestmentPlan::Custom => "Your picks",
        }
        .into(),
    }))
}

pub async fn get_deposit(state: &ApplicationState, from: &TelegramUser) -> anyhow::Result<Option<BotDeposit>> {
    let user_id: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE telegram_id = $1")
        .bind(from.id)
        .fetch_optional(&state.pool)
        .await?;
    match user_id {
        Some(user_id) => deposit_for_user(state, user_id).await,
        None => Ok(None),
    }
}

pub async fn deposit_for_user(state: &ApplicationState, user_id: uuid::Uuid) -> anyhow::Result<Option<BotDeposit>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT network, address FROM wallets WHERE user_id = $1 ORDER BY network")
            .bind(user_id)
            .fetch_all(&state.pool)
            .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let networks: Vec<BotNetworkAddress> = rows
        .into_iter()
        .map(|(network, address)| BotNetworkAddress { network, address })
        .collect();
    // Prefer Solana when the user has it: lower fees for small deposits.
    let default_network = networks
        .iter()
        .find(|n| n.network == "solana")
        .unwrap_or(&networks[0])
        .network
        .clone();
    Ok(Some(BotDeposit { asset: "USDC".into(), default_network, networks }))
}

pub async fn get_sellable(state: &ApplicationState, from: &TelegramUser) -> anyhow::Result<Option<Vec<BotSellable>>> {
    let Some(gateway_id) = gateway_user_id(state, from).await? else {
        return Ok(None);
    };
    let holdings = crate::services::holdings::sellable(state, &gateway_id).await?;
    Ok(Some(
        holdings
            .into_iter()
            .map(|h| BotSellable { asset_key: h.asset_key, name: h.name, value: h.value_usd })
            .collect(),
    ))
}
