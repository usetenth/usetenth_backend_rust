
use super::withdrawals;
use super::worker::*;
use crate::engine::worker_tests::{Mock, SPY, USDC_BASE, USDC_SOL, seed_user, state};
use sqlx::PgPool;
use std::sync::atomic::Ordering;
use uuid::Uuid;

const DEST: &str = "So11111111111111111111111111111111111111112";

async fn seed_assets(pool: &PgPool) {
    sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'SPY', 'SPY', 'base', true, false)")
        .bind(SPY)
        .execute(pool)
        .await
        .unwrap();
    for (key, chain) in [(USDC_SOL, "solana"), (USDC_BASE, "base")] {
        sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'USDC', 'USDC', $2, true, true)")
            .bind(key)
            .bind(chain)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn sell_order(pool: &PgPool, user: Uuid, qty: &str, est_micros: i64) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO orders (user_id, kind, side, asset_key, amount_micros, qty_base) VALUES ($1, 'manual', 'sell', $2, $3, $4) RETURNING id",
    )
    .bind(user)
    .bind(SPY)
    .bind(est_micros)
    .bind(qty)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn status_of(pool: &PgPool, table: &str, id: Uuid) -> (String, Option<String>) {
    sqlx::query_as(&format!("SELECT status, failure_reason FROM {table} WHERE id = $1")).bind(id).fetch_one(pool).await.unwrap()
}

async fn intents(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM signing_intents WHERE kind = $1").bind(kind).fetch_one(pool).await.unwrap()
}


#[sqlx::test]
async fn a_sell_is_sized_in_the_asset_signed_as_a_sell_and_executed(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    let mock = Mock::new("complete");
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();
    let order = sell_order(&pool, user, "0.2", 100_000_000).await;

    execute_pending(&state).await.unwrap();

    let body = mock.last_order.lock().unwrap().clone();
    assert_eq!((body["side"].as_str(), body["qty_unit"].as_str(), body["qty"].as_str()), (Some("sell"), Some("base"), Some("0.2")));
    assert_eq!(status_of(&pool, "orders", order).await.0, "awaiting_settlement");
    assert_eq!((intents(&pool, "sell").await, intents(&pool, "buy").await), (1, 0));
    assert_eq!(mock.executes(), 1);

    reconcile(&state).await.unwrap();
    assert_eq!(status_of(&pool, "orders", order).await.0, "completed");
}

#[sqlx::test]
async fn exchange_venue_sells_skip_signing_and_execute(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    let mock = Mock::new("complete");
    mock.sells_without_payloads.store(true, Ordering::SeqCst);
    let state = state(&pool, &mock, false).await;
    let order = sell_order(&pool, user, "1", 500_000_000).await;

    execute_pending(&state).await.unwrap();

    assert_eq!(status_of(&pool, "orders", order).await.0, "awaiting_settlement");
    assert_eq!((mock.creates(), mock.executes(), intents(&pool, "sell").await), (1, 0, 0));
    reconcile(&state).await.unwrap();
    assert_eq!(status_of(&pool, "orders", order).await.0, "completed");
}

#[sqlx::test]
async fn a_sell_over_the_signing_limit_is_refused(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    let mock = Mock::new("complete");
    let state = state(&pool, &mock, false).await;
    state.signing.ensure_user_key(user).await.unwrap();
    let order = sell_order(&pool, user, "2", 600_000_000).await; // the sell limit is 500 USD

    execute_pending(&state).await.unwrap();

    let (status, reason) = status_of(&pool, "orders", order).await;
    assert_eq!(status, "failed");
    assert!(reason.unwrap().contains("signing policy"));
    assert_eq!(mock.executes(), 0);
}


async fn address(pool: &PgPool, user: Uuid, family: &str, to: &str, usable: &str) -> Uuid {
    sqlx::query_scalar(&format!(
        "INSERT INTO withdrawal_addresses (user_id, chain_family, address, usable_at) VALUES ($1, $2, $3, now() + interval '{usable}') RETURNING id"
    ))
    .bind(user)
    .bind(family)
    .bind(to)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn withdrawal(pool: &PgPool, user: Uuid, address: Uuid, to: &str, asset: &str, micros: i64) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO withdrawals (user_id, address_id, to_address, asset_key, amount_micros) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user)
    .bind(address)
    .bind(to)
    .bind(asset)
    .bind(micros)
    .fetch_one(pool)
    .await
    .unwrap()
}

struct Setup {
    state: crate::app_state::ApplicationState,
    mock: Mock,
    user: Uuid,
    id: Uuid,
}

async fn setup(pool: &PgPool, micros: i64, dry_run: bool) -> Setup {
    seed_assets(pool).await;
    let user = seed_user(pool).await;
    let mock = Mock::new("complete");
    let state = state(pool, &mock, dry_run).await;
    state.signing.ensure_user_key(user).await.unwrap();
    let addr = address(pool, user, "solana", DEST, "-1 hour").await;
    let id = withdrawal(pool, user, addr, DEST, USDC_SOL, micros).await;
    Setup { state, mock, user, id }
}

#[sqlx::test]
async fn a_withdrawal_goes_to_the_saved_address_and_completes(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    withdrawals::execute_pending(&s.state).await.unwrap();

    let sent = s.mock.last_transfer.lock().unwrap().clone();
    assert_eq!(sent["to"], DEST);
    assert_eq!((sent["qty"].as_str(), sent["qty_unit"].as_str(), sent["asset_id"].as_str()), (Some("10.00"), Some("base"), Some(USDC_SOL)));
    assert!(sent.get("network").is_none(), "DeFi transfers must not send a network");
    let (status, _) = status_of(&pool, "withdrawals", s.id).await;
    assert_eq!(status, "completed");
    let hash: Option<String> = sqlx::query_scalar("SELECT tx_hash FROM withdrawals").fetch_one(&pool).await.unwrap();
    assert_eq!(hash.as_deref(), Some("0xtransfer"));
    assert_eq!((s.mock.transfers(), s.mock.transfer_executes(), intents(&pool, "withdraw").await), (1, 1, 1));

    withdrawals::execute_pending(&s.state).await.unwrap();
    withdrawals::recover_stale(&s.state).await.unwrap();
    withdrawals::reconcile(&s.state).await.unwrap();
    assert_eq!((s.mock.transfers(), s.mock.transfer_executes()), (1, 1));
    let _ = s.user;
}

#[sqlx::test]
async fn a_transfer_that_does_not_match_the_request_is_never_signed(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    s.mock.transfer_tamper_to.store(true, Ordering::SeqCst);

    withdrawals::execute_pending(&s.state).await.unwrap();

    let (status, reason) = status_of(&pool, "withdrawals", s.id).await;
    assert_eq!(status, "failed");
    assert!(reason.unwrap().contains("did not match"));
    assert_eq!((s.mock.transfer_executes(), intents(&pool, "withdraw").await), (0, 0));
}

#[sqlx::test]
async fn an_address_still_in_cooldown_or_removed_is_refused_at_execution(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    sqlx::query("UPDATE withdrawal_addresses SET usable_at = now() + interval '1 hour'").execute(&pool).await.unwrap();
    withdrawals::execute_pending(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "failed");
    assert_eq!(s.mock.transfers(), 0);

    let addr = address(&pool, s.user, "solana", "11111111111111111111111111111112", "-1 hour").await;
    sqlx::query("UPDATE withdrawal_addresses SET removed_at = now() WHERE id = $1").bind(addr).execute(&pool).await.unwrap();
    let id = withdrawal(&pool, s.user, addr, "11111111111111111111111111111112", USDC_SOL, 5_000_000).await;
    withdrawals::execute_pending(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", id).await.0, "failed");
    assert_eq!(s.mock.transfers(), 0);
}

#[sqlx::test]
async fn the_wrong_network_for_the_address_is_refused(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    sqlx::query("UPDATE withdrawals SET asset_key = $1").bind(USDC_BASE).execute(&pool).await.unwrap(); // a Solana address, a Base asset
    withdrawals::execute_pending(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "failed");
    assert_eq!(s.mock.transfers(), 0);
}

#[sqlx::test]
async fn an_overdraft_never_reaches_the_gateway(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    *s.mock.usdc_available.lock().unwrap() = "9.99".into();
    withdrawals::execute_pending(&s.state).await.unwrap();
    let (status, reason) = status_of(&pool, "withdrawals", s.id).await;
    assert_eq!((status.as_str(), reason.as_deref()), ("failed", Some("insufficient balance")));
    assert_eq!(s.mock.transfers(), 0);
}

#[sqlx::test]
async fn a_withdrawal_over_the_signing_limit_is_not_signed(pool: PgPool) {
    let s = setup(&pool, 150_000_000, false).await; // the withdrawal limit is 100 USD
    *s.mock.usdc_available.lock().unwrap() = "1000".into();
    withdrawals::execute_pending(&s.state).await.unwrap();
    let (status, reason) = status_of(&pool, "withdrawals", s.id).await;
    assert_eq!(status, "failed");
    assert!(reason.unwrap().contains("signing policy"));
    assert_eq!(s.mock.transfer_executes(), 0);
}

#[sqlx::test]
async fn dry_run_withdrawals_never_touch_the_gateway(pool: PgPool) {
    let s = setup(&pool, 10_000_000, true).await;
    withdrawals::execute_pending(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "dry_run");
    assert_eq!((s.mock.transfers(), s.mock.transfer_executes()), (0, 0));
}

#[sqlx::test]
async fn an_ambiguous_execute_is_resolved_by_the_gateway_never_resent(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    s.mock.transfer_execute_fails.store(true, Ordering::SeqCst);
    s.mock.set_transfer_status("pending");
    withdrawals::execute_pending(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "submitted");

    withdrawals::recover_stale(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "submitted", "too soon to judge");

    sqlx::query("UPDATE withdrawals SET updated_at = now() - interval '10 minutes'").execute(&pool).await.unwrap();
    withdrawals::recover_stale(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "awaiting_settlement");

    s.mock.set_transfer_status("completed");
    withdrawals::reconcile(&s.state).await.unwrap();
    assert_eq!(status_of(&pool, "withdrawals", s.id).await.0, "completed");
    assert_eq!((s.mock.transfers(), s.mock.transfer_executes()), (1, 1));
}

#[sqlx::test]
async fn a_transfer_the_gateway_never_executed_is_failed_not_recreated(pool: PgPool) {
    let s = setup(&pool, 10_000_000, false).await;
    s.mock.transfer_execute_fails.store(true, Ordering::SeqCst);
    s.mock.set_transfer_status("awaiting_signature");
    withdrawals::execute_pending(&s.state).await.unwrap();
    sqlx::query("UPDATE withdrawals SET updated_at = now() - interval '10 minutes'").execute(&pool).await.unwrap();

    withdrawals::recover_stale(&s.state).await.unwrap();

    let (status, reason) = status_of(&pool, "withdrawals", s.id).await;
    assert_eq!(status, "failed");
    assert!(reason.unwrap().contains("nothing was sent"));
    withdrawals::execute_pending(&s.state).await.unwrap();
    assert_eq!(s.mock.transfers(), 1, "no second transfer is created behind the user's back");
}
