use super::withdrawals;
use super::worker::*;
use crate::engine::worker_tests::{Mock, USDC_SOL, seed_deposit, seed_user, state};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::atomic::Ordering;
use uuid::Uuid;

fn texts(mock: &Mock) -> Vec<String> {
    mock.telegram.lock().unwrap().iter().filter_map(|m| m["text"].as_str().map(String::from)).collect()
}

fn messages(mock: &Mock) -> Vec<Value> {
    mock.telegram.lock().unwrap().clone()
}

async fn investment(pool: &PgPool, user: Uuid, status: &str, invest_micros: i64) -> Uuid {
    let deposit = seed_deposit(pool, user, 100_000_000, "1 hour").await;
    sqlx::query_scalar(
        "INSERT INTO investments (user_id, deposit_id, deposit_micros, invest_micros, percentage, status) VALUES ($1, $2, 100000000, $3, 10, $4) RETURNING id",
    )
    .bind(user)
    .bind(deposit)
    .bind(invest_micros)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn notified(pool: &PgPool, table: &str, id: Uuid) -> bool {
    sqlx::query_scalar(&format!("SELECT notified_at IS NOT NULL FROM {table} WHERE id = $1")).bind(id).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn each_finished_investment_is_announced_once_in_plain_words(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let done = investment(&pool, user, "completed", 10_000_000).await;
    let part = investment(&pool, user, "partial", 4_500_000).await;
    let failed = investment(&pool, user, "failed", 2_000_000).await;
    let running = investment(&pool, user, "pending", 1_000_000).await;

    notify_users(&st).await.unwrap();
    let all = texts(&mock);
    assert_eq!(all.len(), 3);
    assert!(all.iter().any(|t| t.contains("Your tenth is invested") && t.contains("$10.00")));
    assert!(all.iter().any(|t| t.contains("Part of your tenth is invested") && t.contains("$4.50")));
    assert!(all.iter().any(|t| t.contains("could not invest your latest tenth")));
    for id in [done, part, failed] {
        assert!(notified(&pool, "investments", id).await);
    }
    assert!(!notified(&pool, "investments", running).await, "unfinished work is not announced");

    notify_users(&st).await.unwrap();
    assert_eq!(texts(&mock).len(), 3, "a second pass says nothing new");
}

#[sqlx::test]
async fn simulated_runs_and_users_without_telegram_are_closed_out_silently(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let simulated = investment(&pool, user, "simulated", 10_000_000).await;

    let email_user: Uuid = sqlx::query_scalar("INSERT INTO users (email, gateway_user_id, verified) VALUES ('m@example.com', 'gw-m', 'verified') RETURNING id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let silent = investment(&pool, email_user, "completed", 10_000_000).await;

    notify_users(&st).await.unwrap();
    assert!(texts(&mock).is_empty());
    assert!(notified(&pool, "investments", simulated).await);
    assert!(notified(&pool, "investments", silent).await);
    assert!(notices(&pool, user).await.is_empty(), "a simulated run is not listed");
    assert_eq!(notices(&pool, email_user).await.len(), 1, "a user without Telegram still sees it in the app");
}

#[sqlx::test]
async fn a_telegram_outage_leaves_the_notice_for_the_next_pass(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let id = investment(&pool, user, "completed", 10_000_000).await;

    mock.telegram_down.store(true, Ordering::SeqCst);
    notify_users(&st).await.unwrap();
    assert!(!notified(&pool, "investments", id).await, "not marked sent when it was not sent");

    mock.telegram_down.store(false, Ordering::SeqCst);
    notify_users(&st).await.unwrap();
    assert_eq!(texts(&mock).len(), 1);
    assert!(notified(&pool, "investments", id).await);
}

#[sqlx::test]
async fn notices_go_to_the_right_chat(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let chat: i64 = sqlx::query_scalar("SELECT telegram_id FROM users WHERE id = $1").bind(user).fetch_one(&pool).await.unwrap();
    investment(&pool, user, "completed", 10_000_000).await;
    notify_users(&st).await.unwrap();
    assert_eq!(messages(&mock)[0]["chat_id"], chat);
}

async fn withdrawal(pool: &PgPool, user: Uuid, status: &str, micros: i64) -> Uuid {
    sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, tradable, stable) VALUES ($1, 'USDC', 'USDC', 'solana', true, true) ON CONFLICT DO NOTHING")
        .bind(USDC_SOL)
        .execute(pool)
        .await
        .unwrap();
    let address: Uuid = sqlx::query_scalar(
        "INSERT INTO withdrawal_addresses (user_id, chain_family, address, usable_at) VALUES ($1, 'solana', $2, now()) RETURNING id",
    )
    .bind(user)
    .bind(Uuid::new_v4().to_string())
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO withdrawals (user_id, address_id, to_address, asset_key, amount_micros, status) VALUES ($1, $2, 'dest', $3, $4, $5) RETURNING id",
    )
    .bind(user)
    .bind(address)
    .bind(USDC_SOL)
    .bind(micros)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn withdrawals_are_announced_when_they_end_and_never_twice(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let sent = withdrawal(&pool, user, "completed", 25_000_000).await;
    let failed = withdrawal(&pool, user, "failed", 7_500_000).await;
    let dry = withdrawal(&pool, user, "dry_run", 1_000_000).await;
    let waiting = withdrawal(&pool, user, "awaiting_settlement", 3_000_000).await;

    withdrawals::notify(&st).await.unwrap();
    let all = texts(&mock);
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|t| t.contains("Withdrawal sent") && t.contains("$25.00")));
    assert!(all.iter().any(|t| t.contains("Withdrawal not sent") && t.contains("$7.50") && t.contains("Nothing left your account")));
    for id in [sent, failed, dry] {
        assert!(notified(&pool, "withdrawals", id).await);
    }
    assert!(!notified(&pool, "withdrawals", waiting).await);

    withdrawals::notify(&st).await.unwrap();
    assert_eq!(texts(&mock).len(), 2);
}

#[sqlx::test]
async fn a_failed_withdrawal_notice_is_retried_after_an_outage(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let id = withdrawal(&pool, user, "completed", 5_000_000).await;

    mock.telegram_down.store(true, Ordering::SeqCst);
    withdrawals::notify(&st).await.unwrap();
    assert!(!notified(&pool, "withdrawals", id).await);
    mock.telegram_down.store(false, Ordering::SeqCst);
    withdrawals::notify(&st).await.unwrap();
    assert!(notified(&pool, "withdrawals", id).await);
    assert_eq!(texts(&mock).len(), 1);
}

// ---- Deposit notices ----

async fn notices(pool: &PgPool, user: Uuid) -> Vec<(String, String)> {
    sqlx::query_as("SELECT title, body FROM notifications WHERE user_id = $1 ORDER BY created_at").bind(user).fetch_all(pool).await.unwrap()
}

#[test]
fn a_deposit_notice_says_what_arrived_and_what_happens_next() {
    assert_eq!(
        deposit_summary(19_840_000, "USDC", Some(3_968_000), Some("pending"), None),
        "$19.84 USDC just arrived in your wallet. We are investing your tenth, $3.968."
    );
    assert!(deposit_summary(5_000_000, "USDC", Some(0), Some("skipped"), Some("below_minimum")).contains("too small to invest"));
    assert!(deposit_summary(5_000_000, "USDC", Some(0), Some("skipped"), Some("daily_limit")).contains("investing limit"));
    assert!(deposit_summary(5_000_000, "USDC", Some(0), Some("skipped"), Some("rule_disabled")).contains("Nothing was invested"));
    assert!(deposit_summary(5_000_000, "USDC", Some(500_000), Some("simulated"), None).contains("Test mode is on"));
    assert_eq!(deposit_summary(5_000_000, "USDC", None, None, None), "$5.00 USDC just arrived in your wallet.");
}

#[sqlx::test]
async fn every_deposit_is_announced_in_the_app_and_on_telegram_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    investment(&pool, user, "pending", 3_968_000).await;
    sqlx::query("UPDATE deposits SET amount_micros = 19840000").execute(&pool).await.unwrap();

    notify_deposits(&st).await.unwrap();
    let telegram = texts(&mock);
    assert_eq!(telegram.len(), 1);
    assert!(telegram[0].starts_with("<b>Payment received</b>") && telegram[0].contains("$19.84 USDC") && telegram[0].contains("$3.968"));
    let rows = notices(&pool, user).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "Payment received");

    notify_deposits(&st).await.unwrap();
    assert_eq!(texts(&mock).len(), 1, "a deposit is announced once");
    assert_eq!(notices(&pool, user).await.len(), 1);
}

#[sqlx::test]
async fn a_deposit_with_nothing_invested_is_still_announced(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    sqlx::query("UPDATE rules SET enabled = false WHERE user_id = $1").bind(user).execute(&pool).await.unwrap();
    let deposit = seed_deposit(&pool, user, 20_000_000, "1 hour").await;
    plan_investments(&pool, &st.config.engine).await.unwrap();

    notify_deposits(&st).await.unwrap();
    let all = texts(&mock);
    assert_eq!(all.len(), 1);
    assert!(all[0].contains("$20.00 USDC") && all[0].contains("Nothing was invested"), "{}", all[0]);
    assert!(notified(&pool, "deposits", deposit).await);
}

#[sqlx::test]
async fn users_without_telegram_get_the_in_app_notice_only(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let email_user: Uuid = sqlx::query_scalar("INSERT INTO users (email, gateway_user_id, verified) VALUES ('m@example.com', 'gw-m', 'verified') RETURNING id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let deposit = seed_deposit(&pool, email_user, 12_000_000, "1 hour").await;
    plan_investments(&pool, &st.config.engine).await.unwrap();

    notify_deposits(&st).await.unwrap();
    assert!(texts(&mock).is_empty());
    assert_eq!(notices(&pool, email_user).await.len(), 1);
    assert!(notified(&pool, "deposits", deposit).await);
}

#[sqlx::test]
async fn a_telegram_outage_retries_the_message_without_a_second_app_notice(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let deposit = seed_deposit(&pool, user, 20_000_000, "1 hour").await;
    plan_investments(&pool, &st.config.engine).await.unwrap();

    mock.telegram_down.store(true, Ordering::SeqCst);
    notify_deposits(&st).await.unwrap();
    assert!(!notified(&pool, "deposits", deposit).await);
    assert_eq!(notices(&pool, user).await.len(), 1, "the app already shows it");

    mock.telegram_down.store(false, Ordering::SeqCst);
    notify_deposits(&st).await.unwrap();
    assert_eq!(texts(&mock).len(), 1);
    assert!(notified(&pool, "deposits", deposit).await);
    assert_eq!(notices(&pool, user).await.len(), 1);
}

#[sqlx::test]
async fn a_new_deposit_waits_for_its_investment_to_be_planned_so_the_notice_is_accurate(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let deposit = seed_deposit(&pool, user, 20_000_000, "1 hour").await;

    notify_deposits(&st).await.unwrap();
    assert!(texts(&mock).is_empty(), "not yet planned");

    sqlx::query("UPDATE deposits SET created_at = now() - interval '3 minutes' WHERE id = $1").bind(deposit).execute(&pool).await.unwrap();
    notify_deposits(&st).await.unwrap();
    assert_eq!(texts(&mock).len(), 1, "if planning is stuck, the payment is still announced");
}

#[sqlx::test]
async fn withdrawals_and_investments_are_listed_in_the_app_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    withdrawal(&pool, user, "completed", 8_000_000).await;
    withdrawal(&pool, user, "failed", 3_000_000).await;
    withdrawal(&pool, user, "dry_run", 1_000_000).await;
    investment(&pool, user, "completed", 4_000_000).await;

    withdrawals::notify(&st).await.unwrap();
    notify_users(&st).await.unwrap();
    withdrawals::notify(&st).await.unwrap();
    notify_users(&st).await.unwrap();

    let rows = notices(&pool, user).await;
    assert_eq!(rows.len(), 3, "a dry run is not listed, and nothing is listed twice");
    assert!(rows.contains(&("Withdrawal sent".to_string(), "$8.00 is on its way to your saved address.".to_string())));
    assert!(rows.iter().any(|(t, b)| t == "Withdrawal not sent" && b.contains("Nothing left your account")));
    assert!(rows.iter().any(|(t, b)| t == "Your tenth is invested" && b.contains("$4.00")));
    assert_eq!(texts(&mock).len(), 3, "and Telegram still hears about each one");
}
