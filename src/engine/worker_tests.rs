use super::worker::*;
use crate::app_state::ApplicationState;
use crate::config::{AppConfig, EngineConfig};
use crate::integrations::tm_client::TrueMarketClient;
use crate::signing::{SigningService, test_support};
use axum::{Json, Router, extract::{Path, State}, http::StatusCode, routing::{get, post}};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use uuid::Uuid;


pub(crate) const SPY: &str = "11111111-1111-4111-8111-111111111111";
pub(crate) const USDC_SOL: &str = "22222222-2222-4222-8222-222222222222";
pub(crate) const USDC_BASE: &str = "33333333-3333-4333-8333-333333333333";

#[derive(Clone)]
pub(crate) struct Mock {
    pub(crate) creates: Arc<AtomicUsize>,
    pub(crate) executes: Arc<AtomicUsize>,
    pub(crate) execute_fails: Arc<AtomicBool>,
    pub(crate) status: Arc<Mutex<String>>,
    pub(crate) signatures: Arc<Mutex<Vec<Value>>>,
    // Sells come back already pending with nothing to sign, like an exchange-venue sell.
    pub(crate) sells_without_payloads: Arc<AtomicBool>,
    pub(crate) last_order: Arc<Mutex<Value>>,
    pub(crate) transfers: Arc<AtomicUsize>,
    pub(crate) transfer_executes: Arc<AtomicUsize>,
    pub(crate) transfer_execute_fails: Arc<AtomicBool>,
    pub(crate) transfer_tamper_to: Arc<AtomicBool>,
    pub(crate) transfer_status: Arc<Mutex<String>>,
    pub(crate) last_transfer: Arc<Mutex<Value>>,
    pub(crate) usdc_available: Arc<Mutex<String>>,
    pub(crate) spy_available: Arc<Mutex<String>>,
    pub(crate) telegram: Arc<Mutex<Vec<Value>>>,
    pub(crate) telegram_down: Arc<AtomicBool>,
    pub(crate) prembly: Arc<Mutex<String>>,
    pub(crate) prembly_calls: Arc<AtomicUsize>,
    pub(crate) dojah_calls: Arc<AtomicUsize>,
    pub(crate) nin_name: Arc<Mutex<(String, String, String)>>,
}

impl Mock {
    pub(crate) fn new(status: &str) -> Self {
        Self {
            creates: Default::default(),
            executes: Default::default(),
            execute_fails: Default::default(),
            status: Arc::new(Mutex::new(status.into())),
            signatures: Default::default(),
            sells_without_payloads: Default::default(),
            last_order: Arc::new(Mutex::new(Value::Null)),
            transfers: Default::default(),
            transfer_executes: Default::default(),
            transfer_execute_fails: Default::default(),
            transfer_tamper_to: Default::default(),
            transfer_status: Arc::new(Mutex::new("completed".into())),
            last_transfer: Arc::new(Mutex::new(Value::Null)),
            usdc_available: Arc::new(Mutex::new("100".into())),
            spy_available: Arc::new(Mutex::new("2".into())),
            telegram: Default::default(),
            telegram_down: Default::default(),
            prembly: Arc::new(Mutex::new("ok".into())),
            prembly_calls: Default::default(),
            dojah_calls: Default::default(),
            nin_name: Arc::new(Mutex::new(("Adaeze".into(), "".into(), "Obi".into()))),
        }
    }
    pub(crate) fn creates(&self) -> usize {
        self.creates.load(Ordering::SeqCst)
    }
    pub(crate) fn executes(&self) -> usize {
        self.executes.load(Ordering::SeqCst)
    }
    pub(crate) fn transfers(&self) -> usize {
        self.transfers.load(Ordering::SeqCst)
    }
    pub(crate) fn transfer_executes(&self) -> usize {
        self.transfer_executes.load(Ordering::SeqCst)
    }
    pub(crate) fn set_transfer_status(&self, status: &str) {
        *self.transfer_status.lock().unwrap() = status.into();
    }
}

fn transfer_json(id: &str, status: &str, body: &Value, to: &str, with_payload: bool, tx_hash: &str) -> Value {
    let payloads = if with_payload {
        let p = test_support::payload(&format!("transfer-{id}"));
        json!([{ "digest": p.digest, "payload": p.payload }])
    } else {
        json!([])
    };
    json!({
        "id": id, "status": status, "venue": "defi", "asset_id": body["asset_id"], "asset_symbol": "USDC",
        "chain": "solana", "network": null, "to": to, "qty": body["qty"], "qty_unit": "base",
        "sent": "", "fee": "0", "received": "0", "tx_hash": tx_hash, "explorer_url": null, "payloads": payloads,
        "created_at": "2026-11-13T00:00:00Z", "updated_at": "2026-11-13T00:00:00Z"
    })
}

async fn serve(mock: Mock) -> String {
    async fn token() -> Json<Value> {
        Json(json!({ "access_token": "test-token" }))
    }
    async fn create(State(m): State<Mock>, Json(body): Json<Value>) -> Json<Value> {
        let n = m.creates.fetch_add(1, Ordering::SeqCst);
        let is_sell = body["side"] == "sell";
        *m.last_order.lock().unwrap() = body;
        let order_id = format!("go-{n}-{}", Uuid::new_v4());
        if is_sell && m.sells_without_payloads.load(Ordering::SeqCst) {
            return Json(json!({ "order_id": order_id, "status": "pending", "payloads": [] }));
        }
        let p = test_support::payload(&format!("order-{n}"));
        Json(json!({ "order_id": order_id, "status": "initialized", "payloads": [{ "digest": p.digest, "payload": p.payload }] }))
    }
    async fn execute(State(m): State<Mock>, Path(_id): Path<String>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        m.executes.fetch_add(1, Ordering::SeqCst);
        m.signatures.lock().unwrap().push(body);
        if m.execute_fails.load(Ordering::SeqCst) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "message": "boom" })));
        }
        (StatusCode::OK, Json(json!({ "status": "pending" })))
    }
    async fn status(State(m): State<Mock>, Path(_id): Path<String>) -> Json<Value> {
        Json(json!({ "status": *m.status.lock().unwrap() }))
    }
    async fn detail(State(m): State<Mock>, Path(_id): Path<String>) -> Json<Value> {
        Json(json!({ "status": *m.status.lock().unwrap(), "tx_hash": "0xabc" }))
    }
    async fn create_transfer(State(m): State<Mock>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        m.transfers.fetch_add(1, Ordering::SeqCst);
        let to = if m.transfer_tamper_to.load(Ordering::SeqCst) {
            "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin".to_string()
        } else {
            body["to"].as_str().unwrap_or_default().to_string()
        };
        let id = Uuid::new_v4().to_string();
        let json = transfer_json(&id, "awaiting_signature", &body, &to, true, "");
        *m.last_transfer.lock().unwrap() = body;
        (StatusCode::CREATED, Json(json))
    }
    async fn execute_transfer(State(m): State<Mock>, Path(id): Path<String>, Json(_): Json<Value>) -> (StatusCode, Json<Value>) {
        m.transfer_executes.fetch_add(1, Ordering::SeqCst);
        if m.transfer_execute_fails.load(Ordering::SeqCst) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "message": "boom" })));
        }
        let body = m.last_transfer.lock().unwrap().clone();
        let to = body["to"].as_str().unwrap_or_default().to_string();
        let status = m.transfer_status.lock().unwrap().clone();
        (StatusCode::OK, Json(transfer_json(&id, &status, &body, &to, false, "0xtransfer")))
    }
    async fn get_transfer(State(m): State<Mock>, Path(id): Path<String>) -> Json<Value> {
        let body = m.last_transfer.lock().unwrap().clone();
        let to = body["to"].as_str().unwrap_or_default().to_string();
        let status = m.transfer_status.lock().unwrap().clone();
        Json(transfer_json(&id, &status, &body, &to, status == "awaiting_signature", "0xtransfer"))
    }
    async fn balances(State(m): State<Mock>) -> Json<Value> {
        Json(json!({ "data": [
            { "asset_id": USDC_SOL, "available": *m.usdc_available.lock().unwrap() },
            { "asset_id": USDC_BASE, "available": *m.usdc_available.lock().unwrap() },
        ] }))
    }
    async fn portfolio(State(m): State<Mock>) -> Json<Value> {
        let spy = m.spy_available.lock().unwrap().clone();
        Json(json!({
            "balances": [
                { "asset_id": SPY, "symbol": "SPY", "name": "SPDR S&P 500", "venue": "defi", "decimals": 6, "stable": false,
                  "tradeable": true, "total": spy, "available": spy, "held": "0", "price": "500" },
                { "asset_id": USDC_SOL, "symbol": "USDC", "venue": "defi", "decimals": 6, "stable": true,
                  "tradeable": true, "total": "100", "available": "100", "held": "0", "price": "1" },
            ],
            "positions": []
        }))
    }
    async fn send_message(State(m): State<Mock>, Path(_bot): Path<String>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        if m.telegram_down.load(Ordering::SeqCst) {
            return (StatusCode::BAD_GATEWAY, Json(json!({ "ok": false, "description": "down" })));
        }
        m.telegram.lock().unwrap().push(body);
        (StatusCode::OK, Json(json!({ "ok": true })))
    }
    async fn prembly_nin(State(m): State<Mock>, Json(_): Json<Value>) -> (StatusCode, Json<Value>) {
        m.prembly_calls.fetch_add(1, Ordering::SeqCst);
        let (first, middle, last) = m.nin_name.lock().unwrap().clone();
        let mode = m.prembly.lock().unwrap().clone();
        match mode.as_str() {
            "not_found" => (StatusCode::NOT_FOUND, Json(json!({ "detail": "No record found" }))),
            "outage" => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))),
            "account" => (StatusCode::PAYMENT_REQUIRED, Json(json!({ "detail": "Insufficient wallet balance" }))),
            _ => (
                StatusCode::OK,
                Json(json!({ "status": true, "data": { "firstname": first, "middlename": middle, "surname": last, "nin_suspension_status": false } })),
            ),
        }
    }
    async fn dojah_nin(State(m): State<Mock>) -> Json<Value> {
        m.dojah_calls.fetch_add(1, Ordering::SeqCst);
        let (first, middle, last) = m.nin_name.lock().unwrap().clone();
        Json(json!({ "entity": { "first_name": first, "middle_name": middle, "last_name": last } }))
    }
    async fn quote(Json(_): Json<Value>) -> Json<Value> {
        Json(json!({ "qty": "1", "price": "500", "notional": "500", "fee": "0.5" }))
    }
    let app = Router::new()
        .route("/v1/auth/api-key/token", post(token))
        .route("/v1/gateway/orders", post(create))
        .route("/v1/gateway/orders/{id}/execute", post(execute))
        .route("/v1/gateway/orders/{id}/status", get(status))
        .route("/v1/gateway/orders/{id}", get(detail))
        .route("/v1/gateway/transfers", post(create_transfer))
        .route("/v1/gateway/transfers/{id}/execute", post(execute_transfer))
        .route("/v1/gateway/transfers/{id}", get(get_transfer))
        .route("/v1/gateway/balances", get(balances))
        .route("/v1/gateway/portfolio", get(portfolio))
        .route("/v1/gateway/quotes", post(quote))
        .route("/telegram/{bot}/sendMessage", post(send_message))
        .route("/verification/vnin-basic", post(prembly_nin))
        .route("/api/v1/kyc/nin", get(dojah_nin))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1/gateway")
}

pub(crate) async fn state(pool: &PgPool, mock: &Mock, dry_run: bool) -> ApplicationState {
    let base = serve(mock.clone()).await;
    let engine = EngineConfig::for_test(dry_run);
    let mut config = AppConfig::for_test(&base, engine);
    let origin = base.trim_end_matches("/v1/gateway").to_string();
    config.telegram_api_base = format!("{origin}/telegram");
    config.kyc = crate::config::KycConfig::for_test(&origin);
    ApplicationState::for_test(
        pool.clone(),
        config,
        TrueMarketClient::for_test(&base),
        SigningService::new(pool.clone(), test_support::wrapper(), test_support::limits(50, 200)),
    )
}


pub(crate) async fn seed_assets(pool: &PgPool) {
    for (key, symbol) in [("spy-id", "SPY"), ("qqq-id", "QQQ")] {
        sqlx::query("INSERT INTO assets (asset_key, symbol, name, tradable) VALUES ($1, $2, $2, true)")
            .bind(key)
            .bind(symbol)
            .execute(pool)
            .await
            .unwrap();
    }
    for (key, bps) in [("spy-id", 6000), ("qqq-id", 4000)] {
        sqlx::query("INSERT INTO plan_allocations (plan, asset_key, weight_bps) VALUES ('steady', $1, $2)")
            .bind(key)
            .bind(bps)
            .execute(pool)
            .await
            .unwrap();
    }
}

pub(crate) async fn seed_user(pool: &PgPool) -> Uuid {
    let user: Uuid = sqlx::query_scalar(
        "INSERT INTO users (telegram_id, gateway_user_id, verified) VALUES ($1, $2, 'verified') RETURNING id",
    )
    .bind(rand::random::<i64>())
    .bind(format!("gw-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO rules (user_id, plan, percentage, enabled, enabled_at) VALUES ($1, 'steady', 10, true, now() - interval '1 day')",
    )
    .bind(user)
    .execute(pool)
    .await
    .unwrap();
    user
}

pub(crate) async fn seed_deposit(pool: &PgPool, user: Uuid, micros: i64, observed: &str) -> Uuid {
    sqlx::query_scalar(&format!(
        "INSERT INTO deposits (user_id, source_id, asset, amount_micros, observed_at)
         VALUES ($1, $2, 'USDC', $3, now() - interval '{observed}') RETURNING id"
    ))
    .bind(user)
    .bind(Uuid::new_v4().to_string())
    .bind(micros)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn order_statuses(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM orders ORDER BY amount_micros DESC, asset_key")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn investment_status(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT status FROM investments").fetch_one(pool).await.unwrap()
}


#[sqlx::test]
async fn a_deposit_becomes_one_investment_split_across_the_mix(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let cfg = EngineConfig::for_test(true);

    plan_investments(&pool, &cfg).await.unwrap();
    plan_investments(&pool, &cfg).await.unwrap();

    let (status, invested): (String, i64) =
        sqlx::query_as("SELECT status, invest_micros FROM investments").fetch_one(&pool).await.unwrap();
    assert_eq!((status.as_str(), invested), ("pending", 10_000_000));
    let legs: Vec<(String, i64)> =
        sqlx::query_as("SELECT asset_key, amount_micros FROM orders ORDER BY amount_micros DESC").fetch_all(&pool).await.unwrap();
    assert_eq!(legs, vec![("spy-id".to_string(), 6_000_000), ("qqq-id".to_string(), 4_000_000)]);
}

#[sqlx::test]
async fn money_that_arrived_before_activation_is_never_invested(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "10 days").await;

    plan_investments(&pool, &EngineConfig::for_test(true)).await.unwrap();

    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, skip_reason FROM investments").fetch_one(&pool).await.unwrap();
    assert_eq!((status.as_str(), reason.as_deref()), ("skipped", Some("before_activation")));
    assert!(order_statuses(&pool).await.is_empty());
}

#[sqlx::test]
async fn disabled_rules_and_missing_mixes_skip_with_a_reason(pool: PgPool) {
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let cfg = EngineConfig::for_test(true);

    plan_investments(&pool, &cfg).await.unwrap(); // no assets or allocations exist
    let reason: Option<String> = sqlx::query_scalar("SELECT skip_reason FROM investments").fetch_one(&pool).await.unwrap();
    assert_eq!(reason.as_deref(), Some("allocation_unavailable"));

    sqlx::query("DELETE FROM investments").execute(&pool).await.unwrap();
    sqlx::query("UPDATE rules SET enabled = false").execute(&pool).await.unwrap();
    plan_investments(&pool, &cfg).await.unwrap();
    let reason: Option<String> = sqlx::query_scalar("SELECT skip_reason FROM investments").fetch_one(&pool).await.unwrap();
    assert_eq!(reason.as_deref(), Some("rule_disabled"));
}

#[sqlx::test]
async fn the_daily_limit_counts_earlier_investments(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    let mut cfg = EngineConfig::for_test(true);
    cfg.max_user_daily_micros = Some(12_000_000);
    seed_deposit(&pool, user, 100_000_000, "2 hours").await;
    plan_investments(&pool, &cfg).await.unwrap();
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    plan_investments(&pool, &cfg).await.unwrap();

    let mut invested: Vec<i64> = sqlx::query_scalar("SELECT invest_micros FROM investments").fetch_all(&pool).await.unwrap();
    invested.sort();
    // 2 USD remain under the cap, split 60/40: the 0.80 USD leg is under the 1 USD floor, so 1.20 is invested.
    assert_eq!(invested, vec![1_200_000, 10_000_000]);
}


#[sqlx::test]
async fn dry_run_never_touches_the_gateway(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let mock = Mock::new("complete");
    let state = state(&pool, &mock, true).await;

    plan_investments(&pool, &state.config.engine).await.unwrap();
    execute_pending(&state).await.unwrap();
    finalize_investments(&pool).await.unwrap();

    assert_eq!(order_statuses(&pool).await, vec!["dry_run", "dry_run"]);
    assert_eq!(investment_status(&pool).await, "simulated");
    assert_eq!((mock.creates(), mock.executes()), (0, 0));
}

#[sqlx::test]
async fn live_orders_are_signed_executed_and_settled(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let mock = Mock::new("complete");
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();

    plan_investments(&pool, &state.config.engine).await.unwrap();
    execute_pending(&state).await.unwrap();
    assert_eq!(order_statuses(&pool).await, vec!["awaiting_settlement", "awaiting_settlement"]);
    assert_eq!((mock.creates(), mock.executes()), (2, 2));
    let bodies = mock.signatures.lock().unwrap().clone();
    assert!(bodies.iter().all(|b| b["auth_type"] == "test_auth" && b["signatures"].as_array().is_some_and(|s| s.len() == 1)));

    reconcile(&state).await.unwrap();
    finalize_investments(&pool).await.unwrap();
    assert_eq!(order_statuses(&pool).await, vec!["completed", "completed"]);
    assert_eq!(investment_status(&pool).await, "completed");
    let hash: Option<String> = sqlx::query_scalar("SELECT tx_hash FROM orders LIMIT 1").fetch_one(&pool).await.unwrap();
    assert_eq!(hash.as_deref(), Some("0xabc"));

    execute_pending(&state).await.unwrap();
    reconcile(&state).await.unwrap();
    assert_eq!((mock.creates(), mock.executes()), (2, 2));
}

#[sqlx::test]
async fn orders_over_the_signing_limit_are_rejected_before_execute(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    // 1000 USD deposit at 10% is 100 USD, split 60/40: the 60 USD leg exceeds the 50 USD order limit.
    seed_deposit(&pool, user, 1_000_000_000, "1 hour").await;
    let mock = Mock::new("complete");
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();

    plan_investments(&pool, &state.config.engine).await.unwrap();
    execute_pending(&state).await.unwrap();

    let rows: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT amount_micros, status, failure_reason FROM orders ORDER BY amount_micros DESC")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows[0].1, "failed");
    assert!(rows[0].2.as_deref().unwrap().contains("signing policy"));
    assert_eq!(rows[1].1, "awaiting_settlement");
    assert_eq!(mock.executes(), 1);

    reconcile(&state).await.unwrap();
    finalize_investments(&pool).await.unwrap();
    assert_eq!(investment_status(&pool).await, "partial");
}

#[sqlx::test]
async fn an_ambiguous_execute_is_never_blindly_retried(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let mock = Mock::new("pending"); // the gateway did accept the execute
    mock.execute_fails.store(true, Ordering::SeqCst);
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();

    plan_investments(&pool, &state.config.engine).await.unwrap();
    execute_pending(&state).await.unwrap();
    assert_eq!(order_statuses(&pool).await, vec!["submitted", "submitted"]);

    execute_pending(&state).await.unwrap();
    recover_stale(&state).await.unwrap();
    assert_eq!(order_statuses(&pool).await, vec!["submitted", "submitted"]);
    assert_eq!(mock.executes(), 2);

    sqlx::query("UPDATE orders SET updated_at = now() - interval '10 minutes'").execute(&pool).await.unwrap();
    recover_stale(&state).await.unwrap();
    assert_eq!(order_statuses(&pool).await, vec!["awaiting_settlement", "awaiting_settlement"]);
    assert_eq!((mock.creates(), mock.executes()), (2, 2));
}

#[sqlx::test]
async fn an_order_the_gateway_never_executed_is_placed_again_once(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let mock = Mock::new("initialized"); // the execute never took effect
    mock.execute_fails.store(true, Ordering::SeqCst);
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();

    plan_investments(&pool, &state.config.engine).await.unwrap();
    execute_pending(&state).await.unwrap();
    sqlx::query("UPDATE orders SET updated_at = now() - interval '10 minutes'").execute(&pool).await.unwrap();
    recover_stale(&state).await.unwrap();

    assert_eq!(order_statuses(&pool).await, vec!["pending", "pending"]);
    let ids: i64 = sqlx::query_scalar("SELECT COUNT(gateway_order_id) FROM orders").fetch_one(&pool).await.unwrap();
    assert_eq!(ids, 0);

    mock.execute_fails.store(false, Ordering::SeqCst);
    execute_pending(&state).await.unwrap();
    assert_eq!(order_statuses(&pool).await, vec!["awaiting_settlement", "awaiting_settlement"]);
}

#[sqlx::test]
async fn failed_gateway_orders_fail_the_investment(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    let mock = Mock::new("failed");
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();

    plan_investments(&pool, &state.config.engine).await.unwrap();
    execute_pending(&state).await.unwrap();
    reconcile(&state).await.unwrap();
    finalize_investments(&pool).await.unwrap();

    assert_eq!(order_statuses(&pool).await, vec!["failed", "failed"]);
    assert_eq!(investment_status(&pool).await, "failed");
}

#[sqlx::test]
async fn concurrent_workers_never_place_an_order_twice(pool: PgPool) {
    seed_assets(&pool).await;
    for _ in 0..3 {
        let user = seed_user(&pool).await;
        seed_deposit(&pool, user, 100_000_000, "1 hour").await;
    }
    let mock = Mock::new("pending");
    let state = state(&pool, &mock, false).await;
    for user in sqlx::query_scalar::<_, Uuid>("SELECT id FROM users").fetch_all(&pool).await.unwrap() {
        state.signing.ensure_user_key(user).await.unwrap();
    }
    plan_investments(&pool, &state.config.engine).await.unwrap();

    let (a, b, c) = (state.clone(), state.clone(), state.clone());
    let _ = tokio::join!(
        tokio::spawn(async move { execute_pending(&a).await }),
        tokio::spawn(async move { execute_pending(&b).await }),
        tokio::spawn(async move { execute_pending(&c).await }),
    );
    assert_eq!((mock.creates(), mock.executes()), (6, 6));
    assert!(order_statuses(&pool).await.iter().all(|s| s == "awaiting_settlement"));
}
