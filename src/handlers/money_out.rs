
use super::auth::{ApiResult, AuthUser};
use super::mobile::{active_gateway_user, order_json, public_status};
use crate::app_state::ApplicationState;
use crate::engine::address;
use crate::engine::money::{cmp_decimal, format_micros, mul_to_micros, parse_decimal, parse_micros};
use crate::integrations::types::{OrderSide, QuantityUnit, QuoteRequest};
use crate::services::auth::AuthError;
use crate::services::holdings::{self, Holding};
use crate::services::notify;
use crate::services::response::ApiResponse;
use crate::services::step_up;
use axum::{
    Json,
    extract::{Path, State},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::time::Duration;
use uuid::Uuid;

fn client_ref(raw: &Option<String>) -> Result<&str, AuthError> {
    raw.as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= 64)
        .ok_or_else(|| AuthError::BadRequest("client_ref is required (1 to 64 characters)".into()))
}


#[derive(Deserialize)]
pub struct SellInput {
    asset_key: String,
    // Amount of the stock to sell, as a decimal string. Ignored when `all` is true.
    qty: Option<String>,
    all: Option<bool>,
    client_ref: Option<String>,
}

struct SellPlan {
    qty: String,
    est_micros: i64,
}

pub async fn holdings(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Vec<Holding>> {
    let gateway_id = active_gateway_user(&state, user_id).await?;
    let list = holdings::sellable(&state, &gateway_id).await.map_err(|error| {
        eprintln!("Holdings failed for {user_id}: {error:#}");
        AuthError::Conflict("We could not load your holdings. Please try again.".into())
    })?;
    Ok(ApiResponse::ok("Holdings", list))
}

async fn plan_sell(state: &ApplicationState, gateway_id: &str, user_id: Uuid, body: &SellInput) -> Result<SellPlan, AuthError> {
    let bad = |m: &str| AuthError::BadRequest(m.into());
    let list = holdings::sellable(state, gateway_id).await.map_err(|error| {
        eprintln!("Holdings failed for {user_id}: {error:#}");
        AuthError::Conflict("We could not check your holdings. Please try again.".into())
    })?;
    let holding = list.iter().find(|h| h.asset_key == body.asset_key).ok_or_else(|| bad("You do not hold that stock"))?;
    let price = holding.price.as_deref().ok_or_else(|| AuthError::Conflict("A price is not available right now".into()))?;

    let qty = if body.all == Some(true) {
        holding.available.clone()
    } else {
        body.qty.as_deref().map(str::trim).ok_or_else(|| bad("Give a quantity, or set all to true"))?.to_string()
    };
    if parse_decimal(&qty).is_none_or(|(n, _)| n == 0) {
        return Err(bad("Enter a valid quantity"));
    }
    if cmp_decimal(&qty, &holding.available) == Some(Ordering::Greater) {
        return Err(bad("You do not hold that much"));
    }
    let est_micros = mul_to_micros(&qty, price).ok_or_else(|| bad("Enter a valid quantity"))?;

    let floor = state.config.engine.global_floor_micros;
    if est_micros < floor {
        return Err(AuthError::BadRequest(format!("That is too small to sell. The minimum is ${}", format_micros(floor))));
    }
    let limits = state.signing.limits();
    if est_micros > limits.sell_max_order_micros {
        return Err(AuthError::BadRequest(format!(
            "A single sale can be at most ${}",
            format_micros(limits.sell_max_order_micros)
        )));
    }
    let sold_today: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_micros), 0)::BIGINT FROM orders
         WHERE user_id = $1 AND side = 'sell' AND status <> 'failed' AND created_at > now() - interval '24 hours'",
    )
    .bind(user_id)
    .fetch_one(&state.pool)
    .await?;
    if sold_today.saturating_add(est_micros) > limits.sell_daily_micros {
        return Err(AuthError::Conflict("You have reached today's selling limit. Try again tomorrow.".into()));
    }
    Ok(SellPlan { qty, est_micros })
}

async fn settlement_asset(state: &ApplicationState) -> Result<String, AuthError> {
    let key: Option<String> = sqlx::query_scalar("SELECT asset_key FROM assets WHERE stable AND upper(symbol) = ANY($1) LIMIT 1")
        .bind(&state.config.engine.settlement_symbols)
        .fetch_optional(&state.pool)
        .await?;
    key.ok_or_else(|| AuthError::Conflict("Selling is not available right now".into()))
}

pub async fn sell_quote(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<SellInput>) -> ApiResult<Value> {
    let gateway_id = active_gateway_user(&state, user_id).await?;
    let plan = plan_sell(&state, &gateway_id, user_id, &body).await?;
    let quote_asset = settlement_asset(&state).await?;
    let quote = state
        .tm_client
        .get_market_quote(
            &gateway_id,
            &QuoteRequest { base_asset: body.asset_key.clone(), quote_asset, qty: plan.qty.clone(), qty_unit: QuantityUnit::Base, side: OrderSide::Sell },
        )
        .await
        .map_err(|error| {
            eprintln!("Sell quote failed for {user_id}: {error:#}");
            AuthError::Conflict("We could not get a quote. Please try again.".into())
        })?;
    Ok(ApiResponse::ok(
        "Quote",
        json!({ "qty": plan.qty, "estimated_usd": format_micros(plan.est_micros), "proceeds": quote.notional.or(quote.qty), "price": quote.price, "fee": quote.fee }),
    ))
}

/// Queues a sale for the engine; the same `client_ref` returns the original order.
pub async fn sell(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<SellInput>) -> ApiResult<Value> {
    let gateway_id = active_gateway_user(&state, user_id).await?;
    let reference = client_ref(&body.client_ref)?.to_string();
    let plan = plan_sell(&state, &gateway_id, user_id, &body).await?;

    sqlx::query(
        "INSERT INTO orders (user_id, kind, side, client_ref, asset_key, amount_micros, qty_base)
         VALUES ($1, 'manual', 'sell', $2, $3, $4, $5)
         ON CONFLICT (user_id, client_ref) WHERE client_ref IS NOT NULL DO NOTHING",
    )
    .bind(user_id)
    .bind(&reference)
    .bind(&body.asset_key)
    .bind(plan.est_micros)
    .bind(&plan.qty)
    .execute(&state.pool)
    .await?;

    let row: (Uuid, Option<String>, String, i64, String, DateTime<Utc>, String, String, Option<String>) = sqlx::query_as(
        "SELECT o.id, a.symbol, o.kind, o.amount_micros, o.status, o.created_at, o.side, o.asset_key, o.qty_base
         FROM orders o LEFT JOIN assets a ON a.asset_key = o.asset_key WHERE o.user_id = $1 AND o.client_ref = $2",
    )
    .bind(user_id)
    .bind(&reference)
    .fetch_one(&state.pool)
    .await?;
    // A "sell everything" retry legitimately computes a different quantity if prices moved.
    let same_qty = body.all == Some(true) || row.8.as_deref() == Some(plan.qty.as_str());
    if row.6 != "sell" || row.7 != body.asset_key || !same_qty {
        return Err(AuthError::Conflict("That client_ref was already used for a different order".into()));
    }
    Ok(ApiResponse::ok("Sale queued", order_json(row.0, row.1, &row.2, row.3, &row.4, row.5, &row.6)))
}


#[derive(Deserialize)]
pub struct NewAddress {
    address: String,
    label: Option<String>,
    password: Option<String>,
    code: Option<String>,
}

const MAX_ADDRESSES: i64 = 10;

fn address_json(id: Uuid, family: &str, address: &str, label: Option<&str>, usable_at: DateTime<Utc>) -> Value {
    json!({ "id": id, "chain_family": family, "address": address, "label": label, "usable_at": usable_at, "usable": usable_at <= Utc::now() })
}

/// Saves a destination that can't receive money until the cooldown passes, so a stolen session can't drain at once.
pub async fn add_address(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<NewAddress>) -> ApiResult<Value> {
    active_gateway_user(&state, user_id).await?;

    let family = address::detect(&body.address);
    let canonical = address::validate(family, &body.address).map_err(|m| AuthError::BadRequest(m.into()))?;
    step_up::confirm(&state, user_id, &step_up::add_address_purpose(&canonical), body.password.as_deref(), body.code.as_deref()).await?;
    let own: Vec<String> = sqlx::query_scalar("SELECT address FROM wallets WHERE user_id = $1").bind(user_id).fetch_all(&state.pool).await?;
    if own.iter().any(|a| address::same_address(family, a, &canonical)) {
        return Err(AuthError::BadRequest("That is your own deposit address".into()));
    }
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM withdrawal_addresses WHERE user_id = $1 AND removed_at IS NULL")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;
    if active >= MAX_ADDRESSES {
        return Err(AuthError::Conflict("Remove an address before adding another".into()));
    }
    let label = body.label.as_deref().map(str::trim).filter(|l| !l.is_empty()).map(|l| l.chars().take(40).collect::<String>());

    let inserted: Result<(Uuid, DateTime<Utc>), sqlx::Error> = sqlx::query_as(
        "INSERT INTO withdrawal_addresses (user_id, chain_family, address, label, usable_at)
         VALUES ($1, $2, $3, $4, now() + ($5::BIGINT * interval '1 second')) RETURNING id, usable_at",
    )
    .bind(user_id)
    .bind(family.as_str())
    .bind(&canonical)
    .bind(&label)
    .bind(state.config.engine.withdraw_address_cooldown_secs)
    .fetch_one(&state.pool)
    .await;
    let (id, usable_at) = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(AuthError::Conflict("You already saved that address".into()));
        }
        Err(e) => return Err(e.into()),
    };

    notify::tell_user(
        &state,
        user_id,
        format!(
            "<b>New withdrawal address added</b>\n\n<code>{}</code>\n\nIt can receive withdrawals from {}. If this was not you, remove it in the app and change your password now.",
            crate::utils::escape_html(&canonical),
            usable_at.format("%Y-%m-%d %H:%M UTC")
        ),
    );
    Ok(ApiResponse::ok("Address saved", address_json(id, family.as_str(), &canonical, label.as_deref(), usable_at)))
}

pub async fn list_addresses(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<(Uuid, String, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, chain_family, address, label, usable_at FROM withdrawal_addresses
         WHERE user_id = $1 AND removed_at IS NULL ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(ApiResponse::ok(
        "Addresses",
        rows.iter().map(|(id, f, a, l, u)| address_json(*id, f, a, l.as_deref(), *u)).collect(),
    ))
}

pub async fn remove_address(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Path(id): Path<Uuid>) -> ApiResult<()> {
    let result = sqlx::query("UPDATE withdrawal_addresses SET removed_at = now() WHERE id = $1 AND user_id = $2 AND removed_at IS NULL")
        .bind(id)
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AuthError::NotFound("Address not found".into()));
    }
    Ok(ApiResponse::ok("Address removed", ()))
}


pub async fn withdraw_assets(State(state): State<ApplicationState>, AuthUser(_): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT asset_key, symbol, chain FROM assets WHERE stable AND tradable AND upper(symbol) = ANY($1) ORDER BY symbol, chain")
            .bind(&state.config.engine.settlement_symbols)
            .fetch_all(&state.pool)
            .await?;
    Ok(ApiResponse::ok(
        "Withdrawable assets",
        rows.into_iter()
            .map(|(key, symbol, chain)| {
                let family = address::family_of_chain(chain.as_deref().unwrap_or(""));
                json!({ "asset_key": key, "symbol": symbol, "chain": chain, "chain_family": family.as_str() })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct WithdrawInput {
    address_id: Uuid,
    asset_key: String,
    amount_usd: String,
    password: Option<String>,
    code: Option<String>,
    client_ref: Option<String>,
}

type WithdrawalTuple = (Uuid, String, i64, String, Option<String>, DateTime<Utc>);

fn withdrawal_json(w: &WithdrawalTuple) -> Value {
    json!({ "id": w.0, "status": public_status(&w.1), "amount_usd": format_micros(w.2), "to": w.3, "asset": w.4, "created_at": w.5 })
}

const WITHDRAWAL_COLUMNS: &str = "w.id, w.status, w.amount_micros, w.to_address, a.symbol, w.created_at";

pub async fn withdraw(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<WithdrawInput>) -> ApiResult<Value> {
    let gateway_id = active_gateway_user(&state, user_id).await?;
    let reference = client_ref(&body.client_ref)?.to_string();
    // Only queued withdrawals count toward the hourly cap, so typos never lock anyone out.
    let hour = Duration::from_secs(3600);
    let rate_key = format!("withdraw:{user_id}");
    if state.limiter.is_blocked(&rate_key, 5, hour) {
        return Err(AuthError::TooManyRequests);
    }
    let micros = parse_micros(&body.amount_usd).ok_or_else(|| AuthError::BadRequest("Enter a valid amount".into()))?;

    // A repeat of an already-authorized request returns the original and creates nothing, so it needs no new confirmation.
    let earlier: Option<(Uuid, String, i64, String, Option<String>, DateTime<Utc>, Uuid, String)> = sqlx::query_as(&format!(
        "SELECT {WITHDRAWAL_COLUMNS}, w.address_id, w.asset_key FROM withdrawals w LEFT JOIN assets a ON a.asset_key = w.asset_key
         WHERE w.user_id = $1 AND w.client_ref = $2"
    ))
    .bind(user_id)
    .bind(&reference)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(row) = earlier {
        if row.2 != micros || row.6 != body.address_id || row.7 != body.asset_key {
            return Err(AuthError::Conflict("That client_ref was already used for a different withdrawal".into()));
        }
        return Ok(ApiResponse::ok("Withdrawal queued", withdrawal_json(&(row.0, row.1, row.2, row.3, row.4, row.5))));
    }

    let purpose = step_up::withdraw_purpose(body.address_id, &body.asset_key, micros, &reference);
    step_up::confirm(&state, user_id, &purpose, body.password.as_deref(), body.code.as_deref()).await?;

    let dest: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT address, chain_family, usable_at FROM withdrawal_addresses WHERE id = $1 AND user_id = $2 AND removed_at IS NULL",
    )
    .bind(body.address_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let (to_address, family, usable_at) = dest.ok_or_else(|| AuthError::NotFound("Address not found".into()))?;
    if usable_at > Utc::now() {
        return Err(AuthError::Conflict(format!("That address can be used from {}", usable_at.format("%Y-%m-%d %H:%M UTC"))));
    }

    let chain: Option<Option<String>> =
        sqlx::query_scalar("SELECT chain FROM assets WHERE asset_key = $1 AND stable AND tradable AND upper(symbol) = ANY($2)")
            .bind(&body.asset_key)
            .bind(&state.config.engine.settlement_symbols)
            .fetch_optional(&state.pool)
            .await?;
    let chain = chain.ok_or_else(|| AuthError::BadRequest("That asset cannot be withdrawn".into()))?.unwrap_or_default();
    if address::family_of_chain(&chain).as_str() != family {
        return Err(AuthError::BadRequest("That address is for a different network than this asset".into()));
    }

    let cfg = &state.config.engine;
    let limits = state.signing.limits();
    if micros < cfg.global_floor_micros || micros > limits.withdraw_max_order_micros {
        return Err(AuthError::BadRequest(format!(
            "Amount must be between ${} and ${}",
            format_micros(cfg.global_floor_micros),
            format_micros(limits.withdraw_max_order_micros)
        )));
    }
    let drawn: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_micros), 0)::BIGINT FROM withdrawals
         WHERE user_id = $1 AND status <> 'failed' AND created_at > now() - interval '24 hours'",
    )
    .bind(user_id)
    .fetch_one(&state.pool)
    .await?;
    if drawn.saturating_add(micros) > limits.withdraw_daily_micros {
        return Err(AuthError::Conflict("You have reached today's withdrawal limit. Try again tomorrow.".into()));
    }

    let balances = state.tm_client.get_user_balance(&gateway_id).await.map_err(|error| {
        eprintln!("Balance check failed for {user_id}: {error:#}");
        AuthError::Conflict("We could not check your balance. Please try again.".into())
    })?;
    let available = balances
        .data
        .iter()
        .find(|b| b.asset_id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(&body.asset_key)))
        .and_then(|b| b.available.clone())
        .unwrap_or_else(|| "0".into());
    if cmp_decimal(&available, &format_micros(micros)) == Some(Ordering::Less) {
        return Err(AuthError::BadRequest("You do not have enough available to withdraw that".into()));
    }

    let queued = sqlx::query(
        "INSERT INTO withdrawals (user_id, address_id, to_address, asset_key, amount_micros, client_ref)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (user_id, client_ref) WHERE client_ref IS NOT NULL DO NOTHING",
    )
    .bind(user_id)
    .bind(body.address_id)
    .bind(&to_address)
    .bind(&body.asset_key)
    .bind(micros)
    .bind(&reference)
    .execute(&state.pool)
    .await?;
    if queued.rows_affected() == 1 {
        state.limiter.record(&rate_key, hour);
    }
    let row: (Uuid, String, i64, String, Option<String>, DateTime<Utc>, Uuid, String) = sqlx::query_as(&format!(
        "SELECT {WITHDRAWAL_COLUMNS}, w.address_id, w.asset_key FROM withdrawals w LEFT JOIN assets a ON a.asset_key = w.asset_key
         WHERE w.user_id = $1 AND w.client_ref = $2"
    ))
    .bind(user_id)
    .bind(&reference)
    .fetch_one(&state.pool)
    .await?;
    if row.2 != micros || row.6 != body.address_id || row.7 != body.asset_key {
        return Err(AuthError::Conflict("That client_ref was already used for a different withdrawal".into()));
    }
    Ok(ApiResponse::ok("Withdrawal queued", withdrawal_json(&(row.0, row.1, row.2, row.3, row.4, row.5))))
}

pub async fn list_withdrawals(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Vec<Value>> {
    let rows: Vec<WithdrawalTuple> = sqlx::query_as(&format!(
        "SELECT {WITHDRAWAL_COLUMNS} FROM withdrawals w LEFT JOIN assets a ON a.asset_key = w.asset_key
         WHERE w.user_id = $1 ORDER BY w.created_at DESC LIMIT 50"
    ))
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(ApiResponse::ok("Withdrawals", rows.iter().map(withdrawal_json).collect()))
}

pub async fn get_withdrawal(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Path(id): Path<Uuid>) -> ApiResult<Value> {
    let row: Option<WithdrawalTuple> = sqlx::query_as(&format!(
        "SELECT {WITHDRAWAL_COLUMNS} FROM withdrawals w LEFT JOIN assets a ON a.asset_key = w.asset_key WHERE w.id = $1 AND w.user_id = $2"
    ))
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let row = row.ok_or_else(|| AuthError::NotFound("Withdrawal not found".into()))?;
    Ok(ApiResponse::ok("Withdrawal", withdrawal_json(&row)))
}


#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ChallengeInput {
    AddAddress { address: String },
    Withdraw { address_id: Uuid, asset_key: String, amount_usd: String, client_ref: String },
}

/// Starts confirming an action: tells the client to use the password, or sends a code to Telegram bound to that exact action.
pub async fn challenge(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser, Json(body): Json<ChallengeInput>) -> ApiResult<Value> {
    active_gateway_user(&state, user_id).await?;
    let (purpose, summary) = match body {
        ChallengeInput::AddAddress { address } => {
            let canonical = address::validate(address::detect(&address), &address).map_err(|m| AuthError::BadRequest(m.into()))?;
            let summary = format!("<b>Confirm a new withdrawal address</b>\n\n<code>{}</code>", crate::utils::escape_html(&canonical));
            (step_up::add_address_purpose(&canonical), summary)
        }
        ChallengeInput::Withdraw { address_id, asset_key, amount_usd, client_ref } => {
            let reference = client_ref.trim().to_string();
            if reference.is_empty() || reference.len() > 64 {
                return Err(AuthError::BadRequest("client_ref is required (1 to 64 characters)".into()));
            }
            let micros = parse_micros(&amount_usd).ok_or_else(|| AuthError::BadRequest("Enter a valid amount".into()))?;
            let to: Option<String> =
                sqlx::query_scalar("SELECT address FROM withdrawal_addresses WHERE id = $1 AND user_id = $2 AND removed_at IS NULL")
                    .bind(address_id)
                    .bind(user_id)
                    .fetch_optional(&state.pool)
                    .await?;
            let to = to.ok_or_else(|| AuthError::NotFound("Address not found".into()))?;
            let summary = format!(
                "<b>Confirm a withdrawal</b>\n\n{} to\n<code>{}</code>",
                crate::utils::usd(&format_micros(micros)),
                crate::utils::escape_html(&to)
            );
            (step_up::withdraw_purpose(address_id, &asset_key, micros, &reference), summary)
        }
    };
    let method = step_up::challenge(&state, user_id, &purpose, &summary).await?;
    let expires_in = (method == step_up::Method::TelegramCode).then_some(step_up::CODE_TTL_SECS);
    Ok(ApiResponse::ok("Confirmation", json!({ "method": method, "expires_in": expires_in })))
}
