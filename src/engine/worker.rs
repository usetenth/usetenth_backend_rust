use super::money::{format_micros, parse_micros};
use super::planner::{self, Context, Decision, RuleInput};
use crate::app_state::ApplicationState;
use crate::config::EngineConfig;
use crate::integrations::types::{
    CreateOrderRequest, ExecuteRequest, GatewayOrderStatus, OrderSide, OrderType, QuantityUnit,
    TransactionStatus, TransactionType,
};
use crate::services::telegram_service::TelegramService;
use crate::signing::{IntentKind, PolicyError, SigningPayload};
use crate::utils::usd;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

const MAX_ATTEMPTS: i32 = 5;
// An order stuck mid-flight this long is resolved by asking the gateway what happened.
const STALE_SECS: i64 = 300;
const MAX_ORDERS_PER_TICK: usize = 20;

pub async fn run(state: ApplicationState) {
    let cfg = state.config.engine.clone();
    println!("Engine started (dry_run={}, poll={}s, deposits in {})", cfg.dry_run, cfg.poll_secs, cfg.settlement_symbols.join("/"));
    if cfg.global_floor_micros > cfg.max_order_micros {
        eprintln!("GLOBAL_FLOOR_USD is above MAX_ORDER_USD, so manual buys can never be in range");
    }
    loop {
        tick(&state).await;
        tokio::time::sleep(Duration::from_secs(cfg.poll_secs)).await;
    }
}

fn log<T>(result: anyhow::Result<T>, stage: &str) {
    if let Err(error) = result {
        eprintln!("Engine stage '{stage}' failed: {error:#}");
    }
}

/// One pass over every stage. Stages are independent: one failing never blocks the rest.
pub async fn tick(state: &ApplicationState) {
    let cfg = &state.config.engine;
    log(crate::services::kyc::process_due(state).await, "identity checks");
    log(detect_deposits(state).await, "detect deposits");
    log(plan_investments(&state.pool, cfg).await, "plan investments");
    log(recover_stale(state).await, "recover stale orders");
    log(execute_pending(state).await, "execute orders");
    log(super::withdrawals::recover_stale(state).await, "recover stale withdrawals");
    log(super::withdrawals::execute_pending(state).await, "execute withdrawals");
    log(super::withdrawals::reconcile(state).await, "reconcile withdrawals");
    log(super::withdrawals::notify(state).await, "notify withdrawals");
    log(reconcile(state).await, "reconcile orders");
    log(finalize_investments(&state.pool).await, "finalize investments");
    log(notify_deposits(state).await, "notify deposits");
    log(notify_users(state).await, "notify users");
}


#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    InFlight,
    NotExecuted,
    Failed,
}

pub fn outcome(status: Option<GatewayOrderStatus>) -> Outcome {
    match status {
        Some(GatewayOrderStatus::Complete) => Outcome::Done,
        Some(GatewayOrderStatus::Failed | GatewayOrderStatus::Canceled) => Outcome::Failed,
        Some(GatewayOrderStatus::Initialized) => Outcome::NotExecuted,
        // Unknown is treated as still running: never act on a status we do not understand.
        Some(GatewayOrderStatus::Pending | GatewayOrderStatus::Active | GatewayOrderStatus::CancelPending) | None => {
            Outcome::InFlight
        }
    }
}


pub async fn detect_deposits(state: &ApplicationState) -> anyhow::Result<()> {
    let users: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, gateway_user_id FROM users WHERE gateway_user_id IS NOT NULL AND verified = 'verified'",
    )
    .fetch_all(&state.pool)
    .await?;

    for (user_id, gateway_id) in users {
        if let Err(error) = detect_for_user(state, user_id, &gateway_id).await {
            eprintln!("Deposit detection failed for user {user_id}: {error:#}");
        }
    }
    Ok(())
}

// A payment in a token that is not a settlement asset is skipped, so say so once instead of every poll.
fn warn_once_ignored(source_id: &str, symbol: &str, settlement: &[String]) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = SEEN.get_or_init(Default::default).lock().unwrap().insert(source_id.to_string());
    if first {
        eprintln!("Ignored a received transfer of {symbol}: only {} count as deposits (set TM_SETTLEMENT_ASSET)", settlement.join(", "));
    }
}

async fn detect_for_user(state: &ApplicationState, user_id: Uuid, gateway_id: &str) -> anyhow::Result<()> {
    let symbols = &state.config.engine.settlement_symbols;
    let response = state.tm_client.list_transactions(gateway_id).await?;
    for tx in response.data {
        let (Some(TransactionType::ReceivedTransfer), Some(TransactionStatus::Completed), Some(received)) =
            (tx.transaction_type, tx.status, tx.received_transfer)
        else {
            continue;
        };
        let symbol = received.token_symbol.to_uppercase();
        let Some(micros) = parse_micros(&received.amount).filter(|m| *m > 0) else { continue };
        if !symbols.contains(&symbol) {
            warn_once_ignored(&received.id.to_string(), &symbol, symbols);
            continue;
        }
        sqlx::query(
            "INSERT INTO deposits (user_id, source_id, tx_hash, chain, asset, amount_micros, observed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (source_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(received.id.to_string())
        .bind(&received.tx_hash)
        .bind(&received.chain)
        .bind(&symbol)
        .bind(micros)
        .bind(received.observed_at)
        .execute(&state.pool)
        .await?;
    }
    Ok(())
}


#[derive(sqlx::FromRow)]
struct DepositRow {
    id: Uuid,
    user_id: Uuid,
    amount_micros: i64,
    observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow)]
struct RuleRow {
    plan: crate::models::schemas::InvestmentPlan,
    percentage: i16,
    enabled: bool,
    enabled_at: Option<chrono::DateTime<chrono::Utc>>,
    expired: bool,
    minimum_amount: i64,
    max_per_transaction: Option<i64>,
    max_daily_amount: Option<i64>,
}

/// Gives every new deposit exactly one investment record (invested or skipped, with a reason).
pub async fn plan_investments(pool: &PgPool, cfg: &EngineConfig) -> anyhow::Result<()> {
    let deposits: Vec<DepositRow> = sqlx::query_as(
        "SELECT d.id, d.user_id, d.amount_micros, d.observed_at FROM deposits d
         LEFT JOIN investments i ON i.deposit_id = d.id
         WHERE i.id IS NULL ORDER BY d.created_at LIMIT 100",
    )
    .fetch_all(pool)
    .await?;
    for deposit in deposits {
        let id = deposit.id;
        if let Err(error) = plan_one(pool, cfg, deposit).await {
            eprintln!("Planning deposit {id} failed: {error:#}");
        }
    }
    Ok(())
}

async fn plan_one(pool: &PgPool, cfg: &EngineConfig, deposit: DepositRow) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    // Serializes planning per user so the 24h spend figure cannot be raced.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE").bind(deposit.user_id).execute(&mut *tx).await?;

    let rule: Option<RuleRow> = sqlx::query_as(
        "SELECT plan, percentage, enabled, enabled_at, COALESCE(expires_at < now(), false) AS expired,
                minimum_amount, max_per_transaction, max_daily_amount
         FROM rules WHERE user_id = $1",
    )
    .bind(deposit.user_id)
    .fetch_optional(&mut *tx)
    .await?;

    let mut percentage = 0i16;
    let mut decision = Decision::Skip("rule_disabled");
    if let Some(rule) = rule {
        percentage = rule.percentage;
        decision = if !rule.enabled {
            Decision::Skip("rule_disabled")
        } else if rule.expired {
            Decision::Skip("rule_expired")
        } else if rule.enabled_at.is_none_or(|at| deposit.observed_at < at) {
            // Money that arrived before the tenth was switched on is never invested retroactively.
            Decision::Skip("before_activation")
        } else {
            let allocations: Vec<(String, i32)> = if rule.plan == crate::models::schemas::InvestmentPlan::Custom {
                sqlx::query_as(
                    "SELECT c.asset_key, c.weight_bps FROM custom_selections c JOIN assets a ON a.asset_key = c.asset_key
                     WHERE c.user_id = $1 AND a.tradable AND NOT a.stable",
                )
                .bind(deposit.user_id)
                .fetch_all(&mut *tx)
                .await?
            } else {
                sqlx::query_as(
                    "SELECT p.asset_key, p.weight_bps FROM plan_allocations p JOIN assets a ON a.asset_key = p.asset_key
                     WHERE p.plan = $1 AND a.tradable AND NOT a.stable",
                )
                .bind(&rule.plan)
                .fetch_all(&mut *tx)
                .await?
            };
            let spent: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(invest_micros), 0)::BIGINT FROM investments
                 WHERE user_id = $1 AND created_at > now() - interval '24 hours' AND status NOT IN ('skipped', 'failed')",
            )
            .bind(deposit.user_id)
            .fetch_one(&mut *tx)
            .await?;
            planner::decide(
                deposit.amount_micros,
                &RuleInput {
                    percentage: rule.percentage,
                    minimum_micros: rule.minimum_amount,
                    max_per_transaction: rule.max_per_transaction,
                    max_daily: rule.max_daily_amount,
                },
                &Context {
                    global_floor_micros: cfg.global_floor_micros,
                    max_user_daily_micros: cfg.max_user_daily_micros,
                    spent_last_day: spent,
                    allocations: &allocations,
                },
            )
        };
    }

    let (status, invest_micros, skip_reason, legs) = match decision {
        Decision::Skip(reason) => ("skipped", 0, Some(reason), vec![]),
        Decision::Invest { invest_micros, legs } => ("pending", invest_micros, None, legs),
    };
    let investment_id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO investments (user_id, deposit_id, deposit_micros, invest_micros, percentage, status, skip_reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (deposit_id) DO NOTHING RETURNING id",
    )
    .bind(deposit.user_id)
    .bind(deposit.id)
    .bind(deposit.amount_micros)
    .bind(invest_micros)
    .bind(percentage)
    .bind(status)
    .bind(skip_reason)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(investment_id) = investment_id {
        for (asset_key, micros) in legs {
            sqlx::query(
                "INSERT INTO orders (user_id, investment_id, kind, asset_key, amount_micros) VALUES ($1, $2, 'auto', $3, $4)",
            )
            .bind(deposit.user_id)
            .bind(investment_id)
            .bind(asset_key)
            .bind(micros)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}


#[derive(sqlx::FromRow)]
struct OrderRow {
    id: Uuid,
    user_id: Uuid,
    asset_key: String,
    amount_micros: i64,
    attempt_count: i32,
    side: String,
    qty_base: Option<String>,
}

async fn claim_next(pool: &PgPool) -> anyhow::Result<Option<OrderRow>> {
    Ok(sqlx::query_as(
        "UPDATE orders SET status = 'processing', attempt_count = attempt_count + 1, updated_at = now()
         WHERE id = (
             SELECT id FROM orders
             WHERE status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= now())
             ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1)
         RETURNING id, user_id, asset_key, amount_micros, attempt_count, side, qty_base",
    )
    .fetch_optional(pool)
    .await?)
}

async fn set_status(pool: &PgPool, id: Uuid, status: &str, reason: Option<&str>) -> anyhow::Result<()> {
    sqlx::query("UPDATE orders SET status = $2, failure_reason = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(status)
        .bind(reason)
        .execute(pool)
        .await?;
    Ok(())
}

// Nothing has been placed yet, so trying again later is safe.
async fn retry_later(pool: &PgPool, order: &OrderRow, reason: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE orders SET status = 'pending', failure_reason = $2, gateway_order_id = NULL,
                next_attempt_at = now() + ($3::BIGINT * interval '1 second'), updated_at = now()
         WHERE id = $1",
    )
    .bind(order.id)
    .bind(reason)
    .bind(60 * order.attempt_count as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn execute_pending(state: &ApplicationState) -> anyhow::Result<()> {
    for _ in 0..MAX_ORDERS_PER_TICK {
        let Some(order) = claim_next(&state.pool).await? else { break };
        if order.attempt_count > MAX_ATTEMPTS {
            set_status(&state.pool, order.id, "failed", Some("too many attempts")).await?;
            continue;
        }
        let id = order.id;
        if let Err(error) = process_order(state, order).await {
            // Left in 'processing' or 'submitted': recover_stale asks the gateway what happened.
            eprintln!("Order {id} did not finish: {error:#}");
        }
    }
    Ok(())
}

fn is_policy_rejection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<PolicyError>().is_some()
}

async fn process_order(state: &ApplicationState, order: OrderRow) -> anyhow::Result<()> {
    let cfg = &state.config.engine;
    let pool = &state.pool;
    if cfg.dry_run {
        return set_status(pool, order.id, "dry_run", None).await;
    }
    let auth_type = cfg
        .execute_auth_type
        .clone()
        .ok_or_else(|| anyhow::anyhow!("TM_EXECUTE_AUTH_TYPE is not configured"))?;
    let amount = format_micros(order.amount_micros);

    let gateway_user: Option<String> =
        sqlx::query_scalar("SELECT gateway_user_id FROM users WHERE id = $1").bind(order.user_id).fetch_one(pool).await?;
    let Some(gateway_user) = gateway_user else {
        return set_status(pool, order.id, "failed", Some("user has no gateway account")).await;
    };

    // Buys are sized in dollars, sells in the asset sold; nothing moves until execute, so create failures retry.
    let sell = order.side == "sell";
    let request = if sell {
        let qty = order.qty_base.clone().ok_or_else(|| anyhow::anyhow!("sell order has no quantity"))?;
        CreateOrderRequest {
            asset_id: order.asset_key.clone(),
            qty,
            qty_unit: QuantityUnit::Base,
            side: OrderSide::Sell,
            order_type: OrderType::Market,
        }
    } else {
        CreateOrderRequest {
            asset_id: order.asset_key.clone(),
            qty: amount.clone(),
            qty_unit: QuantityUnit::Quote,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
        }
    };
    let created = match state.tm_client.create_order(&gateway_user, &request).await {
        Ok(created) => created,
        Err(error) => return retry_later(pool, &order, &format!("create order failed: {error:#}")).await,
    };
    let Some(gateway_order_id) = created.order_id else {
        return retry_later(pool, &order, "gateway returned no order id").await;
    };
    if created.payloads.is_empty() {
        // Exchange-venue sells arrive pending with nothing to sign; anything else unsigned is unusable.
        let already_live = sell && matches!(created.status, Some(GatewayOrderStatus::Pending | GatewayOrderStatus::Complete));
        if !already_live {
            return set_status(pool, order.id, "failed", Some("gateway returned nothing to sign")).await;
        }
        sqlx::query("UPDATE orders SET gateway_order_id = $2, status = 'awaiting_settlement', updated_at = now() WHERE id = $1")
            .bind(order.id)
            .bind(&gateway_order_id)
            .execute(pool)
            .await?;
        return Ok(());
    }
    sqlx::query("UPDATE orders SET gateway_order_id = $2, status = 'submitted', updated_at = now() WHERE id = $1")
        .bind(order.id)
        .bind(&gateway_order_id)
        .execute(pool)
        .await?;

    let payloads: Vec<SigningPayload> = created
        .payloads
        .into_iter()
        .map(|p| SigningPayload { digest: p.digest, payload: p.payload })
        .collect();
    let signed = async {
        let kind = if sell { IntentKind::Sell } else { IntentKind::Buy };
        let intent = state.signing.create_intent(order.user_id, &gateway_order_id, kind, &amount).await?;
        state.signing.sign_intent(intent, &payloads).await
    }
    .await;
    let stamps = match signed {
        Ok(stamps) => stamps,
        Err(error) if is_policy_rejection(&error) => {
            return set_status(pool, order.id, "failed", Some(&format!("rejected by signing policy: {error}"))).await;
        }
        Err(error) => return Err(error),
    };

    // A failed execute is ambiguous, so the order stays 'submitted' for recover_stale to resolve.
    state
        .tm_client
        .execute_order(&gateway_user, &gateway_order_id, &ExecuteRequest { signatures: stamps, auth_type })
        .await?;
    set_status(pool, order.id, "awaiting_settlement", None).await
}

/// Resolves stuck orders by asking the gateway; the only place a not-executed order is re-placed.
pub async fn recover_stale(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    let stuck: Vec<(Uuid, Uuid, Option<String>, i32)> = sqlx::query_as(
        "SELECT id, user_id, gateway_order_id, attempt_count FROM orders
         WHERE status IN ('processing', 'submitted') AND updated_at < now() - ($1::BIGINT * interval '1 second')
         ORDER BY updated_at LIMIT 20",
    )
    .bind(STALE_SECS)
    .fetch_all(pool)
    .await?;

    for (id, user_id, gateway_order_id, attempts) in stuck {
        let result: anyhow::Result<()> = async {
            let Some(gateway_order_id) = gateway_order_id else {
                sqlx::query("UPDATE orders SET status = 'pending', updated_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(pool)
                    .await?;
                return Ok(());
            };
            let gateway_user: String = sqlx::query_scalar("SELECT gateway_user_id FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(pool)
                .await?;
            let status = state.tm_client.get_order_status(&gateway_user, &gateway_order_id).await?.status;
            match outcome(status) {
                Outcome::Done | Outcome::InFlight => set_status(pool, id, "awaiting_settlement", None).await?,
                Outcome::Failed => set_status(pool, id, "failed", Some("gateway reported the order failed")).await?,
                Outcome::NotExecuted if attempts >= MAX_ATTEMPTS => {
                    set_status(pool, id, "needs_review", Some("order was never executed and retries are exhausted")).await?
                }
                Outcome::NotExecuted => {
                    sqlx::query(
                        "UPDATE orders SET status = 'pending', gateway_order_id = NULL, updated_at = now() WHERE id = $1",
                    )
                    .bind(id)
                    .execute(pool)
                    .await?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("Could not resolve stuck order {id}: {error:#}");
        }
    }
    Ok(())
}

pub async fn reconcile(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    let open: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id, gateway_order_id FROM orders
         WHERE status = 'awaiting_settlement' AND gateway_order_id IS NOT NULL ORDER BY updated_at LIMIT 50",
    )
    .fetch_all(pool)
    .await?;

    for (id, user_id, gateway_order_id) in open {
        let result: anyhow::Result<()> = async {
            let gateway_user: String = sqlx::query_scalar("SELECT gateway_user_id FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(pool)
                .await?;
            let detail = state.tm_client.get_order(&gateway_user, &gateway_order_id).await?;
            match outcome(detail.status) {
                Outcome::Done => {
                    sqlx::query("UPDATE orders SET status = 'completed', tx_hash = $2, updated_at = now() WHERE id = $1")
                        .bind(id)
                        .bind(detail.tx_hash)
                        .execute(pool)
                        .await?;
                }
                Outcome::Failed => {
                    let reason = detail.failure_reason.unwrap_or_else(|| "gateway reported the order failed".into());
                    set_status(pool, id, "failed", Some(&reason)).await?;
                }
                Outcome::InFlight | Outcome::NotExecuted => {}
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("Could not reconcile order {id}: {error:#}");
        }
    }

    // Anything still unresolved after a day needs a human, not another poll.
    sqlx::query(
        "UPDATE orders SET status = 'needs_review', failure_reason = 'not settled after 24 hours', updated_at = now()
         WHERE status = 'awaiting_settlement' AND updated_at < now() - interval '24 hours'",
    )
    .execute(pool)
    .await?;
    Ok(())
}


pub async fn finalize_investments(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE investments i SET status = s.new_status, updated_at = now()
         FROM (
             SELECT investment_id,
                    CASE WHEN bool_and(status = 'dry_run') THEN 'simulated'
                         WHEN bool_and(status IN ('completed', 'dry_run')) THEN 'completed'
                         WHEN bool_or(status IN ('completed', 'dry_run')) THEN 'partial'
                         ELSE 'failed' END AS new_status
             FROM orders WHERE investment_id IS NOT NULL
             GROUP BY investment_id
             HAVING bool_and(status IN ('completed', 'dry_run', 'failed', 'needs_review'))
         ) s
         WHERE i.id = s.investment_id AND i.status <> s.new_status",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// What a deposit notice says: the payment first, then what happens to the tenth.
pub fn deposit_summary(amount_micros: i64, asset: &str, invest_micros: Option<i64>, status: Option<&str>, skip_reason: Option<&str>) -> String {
    let arrived = format!("{} {asset} just arrived in your wallet.", usd(&format_micros(amount_micros)));
    let next = match (invest_micros, status, skip_reason) {
        (_, Some("simulated"), _) => " Test mode is on, so nothing was invested and it stays in your balance.".to_string(),
        (Some(micros), Some(s), _) if micros > 0 && s != "skipped" && s != "failed" => {
            format!(" We are investing your tenth, {}.", usd(&format_micros(micros)))
        }
        (_, Some("skipped"), Some("below_minimum")) => " It was too small to invest from, so it stays in your balance.".to_string(),
        (_, Some("skipped"), Some("daily_limit")) => " You have reached today's investing limit, so it stays in your balance.".to_string(),
        (_, Some("skipped"), _) => " Nothing was invested from this one, so it stays in your balance.".to_string(),
        _ => String::new(),
    };
    format!("{arrived}{next}")
}

/// Announces every new deposit once, in the app's list and on Telegram, even when nothing is invested from it.
pub async fn notify_deposits(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    let due: Vec<(Uuid, Uuid, i64, String, Option<i64>, Option<i64>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT d.id, d.user_id, d.amount_micros, d.asset, u.telegram_id, i.invest_micros, i.status, i.skip_reason
         FROM deposits d JOIN users u ON u.id = d.user_id LEFT JOIN investments i ON i.deposit_id = d.id
         WHERE d.notified_at IS NULL AND (i.id IS NOT NULL OR d.created_at < now() - interval '2 minutes')
         ORDER BY d.created_at LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    for (id, user_id, amount, asset, chat, invest, status, skip) in due {
        let body = deposit_summary(amount, &asset, invest, status.as_deref(), skip.as_deref());
        sqlx::query(
            "INSERT INTO notifications (user_id, kind, title, body, deposit_id) VALUES ($1, 'deposit', 'Payment received', $2, $3)
             ON CONFLICT (deposit_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(&body)
        .bind(id)
        .execute(pool)
        .await?;
        if let Some(chat_id) = chat {
            let text = format!("<b>Payment received</b>\n\n{body}");
            if let Err(error) = TelegramService::send_message(state, chat_id, &text, &[]).await {
                eprintln!("Could not notify about deposit {id}: {error:#}");
                continue;
            }
        }
        sqlx::query("UPDATE deposits SET notified_at = now() WHERE id = $1").bind(id).execute(pool).await?;
    }
    Ok(())
}

pub async fn notify_users(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    // Nothing real happened (simulated), so there is nothing to tell anyone.
    sqlx::query("UPDATE investments SET notified_at = now() WHERE notified_at IS NULL AND status = 'simulated'").execute(pool).await?;

    let due: Vec<(Uuid, Uuid, Option<i64>, String, i64)> = sqlx::query_as(
        "SELECT i.id, i.user_id, u.telegram_id, i.status, i.invest_micros FROM investments i JOIN users u ON u.id = i.user_id
         WHERE i.notified_at IS NULL AND i.status IN ('completed', 'partial', 'failed') LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    for (id, user_id, chat, status, invest_micros) in due {
        let amount = usd(&format_micros(invest_micros));
        let (title, body) = match status.as_str() {
            "completed" => ("Your tenth is invested", format!("We invested {amount} from your latest payment.")),
            "partial" => ("Part of your tenth is invested", format!("Some of your {amount} was invested. The rest stays in your balance and we are looking into it.")),
            _ => ("We could not invest your latest tenth", "Your money is safe in your balance. We are looking into it.".to_string()),
        };
        crate::services::notify::record(pool, user_id, "investment", id, title, &body).await?;
        if let Some(chat_id) = chat {
            if let Err(error) = TelegramService::send_message(state, chat_id, &format!("<b>{title}</b>\n\n{body}"), &[]).await {
                eprintln!("Could not notify about investment {id}: {error:#}");
                continue;
            }
        }
        sqlx::query("UPDATE investments SET notified_at = now() WHERE id = $1").bind(id).execute(pool).await?;
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn gateway_statuses_map_conservatively() {
        assert_eq!(outcome(Some(GatewayOrderStatus::Complete)), Outcome::Done);
        assert_eq!(outcome(Some(GatewayOrderStatus::Failed)), Outcome::Failed);
        assert_eq!(outcome(Some(GatewayOrderStatus::Canceled)), Outcome::Failed);
        assert_eq!(outcome(Some(GatewayOrderStatus::Initialized)), Outcome::NotExecuted);
        assert_eq!(outcome(Some(GatewayOrderStatus::Pending)), Outcome::InFlight);
        assert_eq!(outcome(Some(GatewayOrderStatus::CancelPending)), Outcome::InFlight);
        assert_eq!(outcome(None), Outcome::InFlight);
    }
}
