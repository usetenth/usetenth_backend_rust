use super::auth::{ApiResult, AuthUser};
use crate::app_state::ApplicationState;
use crate::engine::money::{format_micros, parse_micros};
use crate::integrations::types::{OrderSide, QuantityUnit, QuoteRequest};
use crate::models::schemas::InvestmentPlan;
use crate::services::auth::AuthError;
use crate::services::bot_data::{self, BotDeposit, BotPortfolio};
use crate::services::onboarding::{self, Stage};
use crate::services::response::ApiResponse;
use axum::{
    Json,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Gateway id of a user who has finished onboarding; everyone else is told to finish first.
pub(super) async fn active_gateway_user(state: &ApplicationState, user_id: Uuid) -> Result<String, AuthError> {
    let row: Option<(Option<String>, bool)> =
        sqlx::query_as("SELECT gateway_user_id, verified = 'verified' FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?;
    match row {
        Some((Some(gateway_id), true)) => Ok(gateway_id),
        Some(_) => Err(AuthError::Conflict("Finish setting up your account first".into())),
        None => Err(AuthError::Unauthorized),
    }
}

fn internal(error: anyhow::Error) -> AuthError {
    AuthError::Internal(error)
}


#[derive(Serialize)]
pub struct StageView {
    stage: Stage,
}

pub async fn onboarding_status(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<StageView> {
    let stage = onboarding::stage_of(&state, user_id).await.map_err(internal)?;
    Ok(ApiResponse::ok("Onboarding", StageView { stage }))
}

/// Runs the automatic steps (gateway account, verification). Safe to call again to retry.
pub async fn onboarding_advance(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<StageView> {
    if !state.limiter.allow(&format!("advance:{user_id}"), 10, std::time::Duration::from_secs(60)) {
        return Err(AuthError::TooManyRequests);
    }
    match onboarding::advance(&state, user_id).await {
        Ok(stage) => Ok(ApiResponse::ok("Onboarding", StageView { stage })),
        Err(error) => {
            eprintln!("Onboarding advance failed for {user_id}: {error:#}");
            Err(AuthError::Conflict("We could not finish setting up your account. Please try again.".into()))
        }
    }
}


#[derive(Serialize)]
pub struct RuleView {
    percentage: i16,
    plan: InvestmentPlan,
    enabled: bool,
}

pub async fn get_rule(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Option<RuleView>> {
    let row: Option<(i16, InvestmentPlan, bool)> =
        sqlx::query_as("SELECT percentage, plan, enabled FROM rules WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?;
    Ok(ApiResponse::ok("Your tenth", row.map(|(percentage, plan, enabled)| RuleView { percentage, plan, enabled })))
}

#[derive(Deserialize)]
pub struct SetRule {
    percentage: i16,
    plan: InvestmentPlan,
}

pub async fn set_rule(
    State(state): State<ApplicationState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<SetRule>,
) -> ApiResult<RuleView> {
    if onboarding::parse_percentage(&body.percentage.to_string()).is_none() {
        return Err(AuthError::BadRequest(format!("Percentage must be one of {:?}", onboarding::PERCENTAGES)));
    }
    if body.plan == InvestmentPlan::Custom {
        let picks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM custom_selections WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&state.pool)
            .await?;
        if picks == 0 {
            return Err(AuthError::BadRequest("Choose your stocks first".into()));
        }
    }
    onboarding::save_rule(&state, user_id, body.percentage, &body.plan).await.map_err(internal)?;
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM rules WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;
    Ok(ApiResponse::ok("Your tenth is saved", RuleView { percentage: body.percentage, plan: body.plan, enabled }))
}

#[derive(Deserialize)]
pub struct Pick {
    asset_key: String,
    weight_bps: i32,
}

pub fn validate_weights(weights: &[(String, i32)]) -> Result<(), AuthError> {
    let bad = |m: &str| Err(AuthError::BadRequest(m.into()));
    if weights.is_empty() || weights.len() > 20 {
        return bad("Choose between 1 and 20 stocks");
    }
    let mut keys: Vec<&str> = weights.iter().map(|(k, _)| k.as_str()).collect();
    keys.sort();
    keys.dedup();
    if keys.len() != weights.len() {
        return bad("Each stock can only be listed once");
    }
    if weights.iter().any(|(_, w)| !(1..=10_000).contains(w)) {
        return bad("Each weight must be between 1 and 10000");
    }
    if weights.iter().map(|(_, w)| *w as i64).sum::<i64>() != 10_000 {
        return bad("Weights must add up to exactly 10000 (100%)");
    }
    Ok(())
}

const PLANS: [(&str, &str, &str); 3] = [
    ("steady", "Steady", "Spread across the largest companies."),
    ("growth", "Growth", "Fewer names, bigger swings."),
    ("tech", "Tech-heavy", "Software and chipmakers only."),
];

/// What each preset mix buys right now, so the app can show it before anyone picks it.
pub async fn list_plans(State(state): State<ApplicationState>, AuthUser(_): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<(InvestmentPlan, String, String, String, i32)> = sqlx::query_as(
        "SELECT p.plan, a.asset_key, a.symbol, a.name, p.weight_bps FROM plan_allocations p JOIN assets a ON a.asset_key = p.asset_key
         WHERE a.tradable AND NOT a.stable ORDER BY p.weight_bps DESC, a.symbol",
    )
    .fetch_all(&state.pool)
    .await?;
    let plans = PLANS
        .iter()
        .map(|(id, name, blurb)| {
            let stocks: Vec<Value> = rows
                .iter()
                .filter(|(plan, ..)| format!("{plan:?}").eq_ignore_ascii_case(id))
                .map(|(_, key, symbol, stock_name, bps)| json!({ "asset_key": key, "symbol": symbol, "name": stock_name, "weight_bps": bps }))
                .collect();
            json!({ "plan": id, "name": name, "blurb": blurb, "stocks": stocks })
        })
        .collect();
    Ok(ApiResponse::ok("Plans", plans))
}

pub async fn get_custom_picks(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<(String, String, String, i32)> = sqlx::query_as(
        "SELECT a.asset_key, a.symbol, a.name, c.weight_bps FROM custom_selections c JOIN assets a ON a.asset_key = c.asset_key
         WHERE c.user_id = $1 ORDER BY c.weight_bps DESC, a.symbol",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(ApiResponse::ok(
        "Your picks",
        rows.into_iter().map(|(key, symbol, name, bps)| json!({ "asset_key": key, "symbol": symbol, "name": name, "weight_bps": bps })).collect(),
    ))
}

pub async fn set_custom_picks(
    State(state): State<ApplicationState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<Vec<Pick>>,
) -> ApiResult<usize> {
    let weights: Vec<(String, i32)> = body.into_iter().map(|p| (p.asset_key, p.weight_bps)).collect();
    validate_weights(&weights)?;
    let keys: Vec<String> = weights.iter().map(|(k, _)| k.clone()).collect();
    let ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE asset_key = ANY($1) AND tradable AND NOT stable")
        .bind(&keys)
        .fetch_one(&state.pool)
        .await?;
    if ok as usize != keys.len() {
        return Err(AuthError::BadRequest("One of those stocks is not available".into()));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM custom_selections WHERE user_id = $1").bind(user_id).execute(&mut *tx).await?;
    for (key, bps) in &weights {
        sqlx::query("INSERT INTO custom_selections (user_id, asset_key, weight_bps) VALUES ($1, $2, $3)")
            .bind(user_id)
            .bind(key)
            .bind(bps)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(ApiResponse::ok("Your picks are saved", weights.len()))
}


pub async fn list_assets(State(state): State<ApplicationState>, AuthUser(_): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT asset_key, symbol, name, asset_class FROM assets WHERE tradable AND NOT stable ORDER BY symbol",
    )
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(|(asset_key, symbol, name, class)| json!({ "asset_key": asset_key, "symbol": symbol, "name": name, "asset_class": class }))
        .collect();
    Ok(ApiResponse::ok("Assets", items))
}

pub async fn portfolio(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<BotPortfolio> {
    let gateway_id = active_gateway_user(&state, user_id).await?;
    let portfolio = bot_data::portfolio_for_gateway(&state, &gateway_id).await.map_err(internal)?;
    Ok(ApiResponse::ok("Portfolio", portfolio))
}

pub async fn deposit(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<BotDeposit> {
    active_gateway_user(&state, user_id).await?;
    match bot_data::deposit_for_user(&state, user_id).await.map_err(internal)? {
        Some(deposit) => Ok(ApiResponse::ok("Deposit addresses", deposit)),
        None => Err(AuthError::NotFound("Your wallet is still being prepared".into())),
    }
}

// Simulated runs are shown as such; operator-only states look like ordinary progress.
pub(super) fn public_status(status: &str) -> &'static str {
    match status {
        "completed" => "completed",
        "failed" => "failed",
        "partial" => "partial",
        "dry_run" | "simulated" => "simulated",
        _ => "in_progress",
    }
}

pub(super) fn order_json(id: Uuid, symbol: Option<String>, kind: &str, micros: i64, status: &str, created: DateTime<Utc>, side: &str) -> Value {
    json!({
        "id": id, "asset": symbol, "kind": kind, "side": side, "amount_usd": format_micros(micros),
        "status": public_status(status), "created_at": created,
    })
}

type OrderTuple = (Uuid, Option<String>, String, i64, String, DateTime<Utc>, String);

const ORDER_COLUMNS: &str = "o.id, a.symbol, o.kind, o.amount_micros, o.status, o.created_at, o.side";

pub async fn list_orders(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<OrderTuple> = sqlx::query_as(&format!(
        "SELECT {ORDER_COLUMNS} FROM orders o LEFT JOIN assets a ON a.asset_key = o.asset_key
         WHERE o.user_id = $1 ORDER BY o.created_at DESC LIMIT 50"
    ))
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(ApiResponse::ok("Orders", rows.into_iter().map(|(a, b, c, d, e, f, g)| order_json(a, b, &c, d, &e, f, &g)).collect()))
}

pub async fn get_order(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Value> {
    let row: Option<OrderTuple> = sqlx::query_as(&format!(
        "SELECT {ORDER_COLUMNS} FROM orders o LEFT JOIN assets a ON a.asset_key = o.asset_key WHERE o.id = $1 AND o.user_id = $2"
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let (a, b, c, d, e, f, g) = row.ok_or_else(|| AuthError::NotFound("Order not found".into()))?;
    Ok(ApiResponse::ok("Order", order_json(a, b, &c, d, &e, f, &g)))
}

pub async fn list_investments(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Vec<Value>> {
    let investments: Vec<(Uuid, String, Option<String>, i64, i64, i16, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, status, skip_reason, deposit_micros, invest_micros, percentage, created_at
         FROM investments WHERE user_id = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;
    let ids: Vec<Uuid> = investments.iter().map(|i| i.0).collect();
    let orders: Vec<(Uuid, Option<String>, i64, String)> = sqlx::query_as(
        "SELECT o.investment_id, a.symbol, o.amount_micros, o.status FROM orders o
         LEFT JOIN assets a ON a.asset_key = o.asset_key WHERE o.investment_id = ANY($1) ORDER BY o.amount_micros DESC",
    )
    .bind(&ids)
    .fetch_all(&state.pool)
    .await?;
    let items = investments
        .into_iter()
        .map(|(id, status, skip_reason, deposit, invest, percentage, created)| {
            let legs: Vec<Value> = orders
                .iter()
                .filter(|o| o.0 == id)
                .map(|(_, symbol, micros, status)| json!({ "asset": symbol, "amount_usd": format_micros(*micros), "status": public_status(status) }))
                .collect();
            json!({
                "id": id,
                "status": if status == "skipped" { "skipped" } else { public_status(&status) },
                "reason": skip_reason, "deposit_usd": format_micros(deposit), "invested_usd": format_micros(invest),
                "percentage": percentage, "created_at": created, "orders": legs,
            })
        })
        .collect();
    Ok(ApiResponse::ok("Investments", items))
}


#[derive(Deserialize)]
pub struct TradeInput {
    asset_key: String,
    amount_usd: String,
    client_ref: Option<String>,
}

async fn checked_trade(state: &ApplicationState, body: &TradeInput) -> Result<i64, AuthError> {
    let cfg = &state.config.engine;
    let micros = parse_micros(&body.amount_usd).ok_or_else(|| AuthError::BadRequest("Enter a valid amount".into()))?;
    if micros < cfg.global_floor_micros || micros > cfg.max_order_micros {
        return Err(AuthError::BadRequest(format!(
            "Amount must be between ${} and ${}",
            format_micros(cfg.global_floor_micros),
            format_micros(cfg.max_order_micros)
        )));
    }
    let tradable: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE asset_key = $1 AND tradable AND NOT stable)")
        .bind(&body.asset_key)
        .fetch_one(&state.pool)
        .await?;
    if !tradable {
        return Err(AuthError::BadRequest("That stock is not available".into()));
    }
    Ok(micros)
}

pub async fn quote(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<TradeInput>) -> ApiResult<Value> {
    let gateway_id = active_gateway_user(&state, user_id).await?;
    let micros = checked_trade(&state, &body).await?;
    let settlement: Option<String> = sqlx::query_scalar("SELECT asset_key FROM assets WHERE stable AND upper(symbol) = ANY($1) LIMIT 1")
        .bind(&state.config.engine.settlement_symbols)
        .fetch_optional(&state.pool)
        .await?;
    let quote_asset = settlement.ok_or_else(|| AuthError::Conflict("Buying is not available right now".into()))?;
    let quote = state
        .tm_client
        .get_market_quote(
            &gateway_id,
            &QuoteRequest {
                base_asset: body.asset_key.clone(),
                quote_asset,
                qty: format_micros(micros),
                qty_unit: QuantityUnit::Quote,
                side: OrderSide::Buy,
            },
        )
        .await
        .map_err(|error| {
            eprintln!("Quote failed for {user_id}: {error:#}");
            AuthError::Conflict("We could not get a quote. Please try again.".into())
        })?;
    Ok(ApiResponse::ok(
        "Quote",
        json!({ "amount_usd": format_micros(micros), "qty": quote.qty, "price": quote.price, "fee": quote.fee }),
    ))
}

/// Queues a buy for the engine; the same `client_ref` returns the same order.
pub async fn buy(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<TradeInput>) -> ApiResult<Value> {
    active_gateway_user(&state, user_id).await?;
    let client_ref = body.client_ref.as_deref().map(str::trim).filter(|r| !r.is_empty() && r.len() <= 64);
    let Some(client_ref) = client_ref else {
        return Err(AuthError::BadRequest("client_ref is required (1 to 64 characters)".into()));
    };
    let micros = checked_trade(&state, &body).await?;

    sqlx::query(
        "INSERT INTO orders (user_id, kind, client_ref, asset_key, amount_micros) VALUES ($1, 'manual', $2, $3, $4)
         ON CONFLICT (user_id, client_ref) WHERE client_ref IS NOT NULL DO NOTHING",
    )
    .bind(user_id)
    .bind(client_ref)
    .bind(&body.asset_key)
    .bind(micros)
    .execute(&state.pool)
    .await?;
    let row: OrderTuple = sqlx::query_as(&format!(
        "SELECT {ORDER_COLUMNS} FROM orders o LEFT JOIN assets a ON a.asset_key = o.asset_key WHERE o.user_id = $1 AND o.client_ref = $2"
    ))
    .bind(user_id)
    .bind(client_ref)
    .fetch_one(&state.pool)
    .await?;
    if row.3 != micros || sqlx::query_scalar::<_, String>("SELECT asset_key FROM orders WHERE id = $1").bind(row.0).fetch_one(&state.pool).await? != body.asset_key {
        return Err(AuthError::Conflict("That client_ref was already used for a different order".into()));
    }
    Ok(ApiResponse::ok("Order queued", order_json(row.0, row.1, &row.2, row.3, &row.4, row.5, &row.6)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pairs: &[(&str, i32)]) -> Vec<(String, i32)> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn valid_mixes_pass() {
        assert!(validate_weights(&w(&[("a", 10_000)])).is_ok());
        assert!(validate_weights(&w(&[("a", 6000), ("b", 4000)])).is_ok());
    }

    #[test]
    fn invalid_mixes_fail() {
        assert!(validate_weights(&[]).is_err());
        assert!(validate_weights(&w(&[("a", 5000)])).is_err());
        assert!(validate_weights(&w(&[("a", 6000), ("b", 5000)])).is_err());
        assert!(validate_weights(&w(&[("a", 5000), ("a", 5000)])).is_err());
        assert!(validate_weights(&w(&[("a", 0), ("b", 10_000)])).is_err());
        assert!(validate_weights(&w(&[("a", -1), ("b", 10_001)])).is_err());
    }

    #[test]
    fn rolled_up_investment_statuses_keep_their_meaning() {
        assert_eq!(public_status("partial"), "partial");
        assert_eq!(public_status("simulated"), "simulated");
        assert_eq!(public_status("pending"), "in_progress");
    }

    #[test]
    fn operator_only_statuses_are_hidden_from_users() {
        assert_eq!(public_status("needs_review"), "in_progress");
        assert_eq!(public_status("awaiting_settlement"), "in_progress");
        assert_eq!(public_status("dry_run"), "simulated");
    }
}


// ---- Identity ----

#[derive(Deserialize)]
pub struct KycInput {
    full_name: String,
    nin: String,
}

pub async fn kyc_status(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<crate::services::kyc::KycView> {
    Ok(ApiResponse::ok("Identity check", crate::services::kyc::current(&state, user_id).await?))
}

/// Starts the identity check; the NIN is stored encrypted until a decision and wiped after.
pub async fn kyc_submit(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<KycInput>) -> ApiResult<crate::services::kyc::KycView> {
    if !state.limiter.allow(&format!("kyc:{user_id}"), 10, std::time::Duration::from_secs(3600)) {
        return Err(AuthError::TooManyRequests);
    }
    Ok(ApiResponse::ok("Identity check", crate::services::kyc::submit(&state, user_id, &body.full_name, &body.nin).await?))
}
