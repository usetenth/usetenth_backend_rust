use crate::app_state::ApplicationState;
use crate::models::schemas::InvestmentPlan;
use crate::services::bot::TelegramUser;

pub struct BotHolding {
    pub name: String,
    pub amount: String,
}

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

pub struct BotNetworkAddress {
    pub network: String,
    pub address: String,
}

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

// The user's portfolio, or None when they have no gateway account yet.
pub async fn get_portfolio(_state: &ApplicationState, _from: &TelegramUser) -> anyhow::Result<Option<BotPortfolio>> {
    Ok(None)
}

// The user's saved tenth (percent + mix), or None when not set.
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

// Deposit addresses, or None when deposits are not available to this user.
pub async fn get_deposit(_state: &ApplicationState, _from: &TelegramUser) -> anyhow::Result<Option<BotDeposit>> {
    Ok(None)
}

// What the user may sell, or None when selling is not available to them.
pub async fn get_sellable(_state: &ApplicationState, _from: &TelegramUser) -> anyhow::Result<Option<Vec<BotSellable>>> {
    Ok(None)
}
