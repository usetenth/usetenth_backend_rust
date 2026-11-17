use super::worker::*;
use crate::engine::worker_tests::{Mock, received, seed_user, state, with_alchemy};
use crate::handlers::chain_hooks;
use crate::services::chain_watch::sync_addresses;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::PgPool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use uuid::Uuid;

const BASE_ADDRESS: &str = "0xBE3F4b43DB5EB49D1F48F53443B9ABCE45DA3B79";

async fn deposits(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM deposits").fetch_one(pool).await.unwrap()
}

async fn add_wallet(pool: &PgPool, user: Uuid, address: &str, network: &str) {
    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, $2, $3)").bind(user).bind(address).bind(network).execute(pool).await.unwrap();
}

fn signed(body: &str, key: &str) -> HeaderMap {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let mut headers = HeaderMap::new();
    headers.insert("x-alchemy-signature", HeaderValue::from_str(&hex::encode(mac.finalize().into_bytes())).unwrap());
    headers
}

async fn wait_for_deposits(pool: &PgPool, wanted: i64) -> bool {
    for _ in 0..60 {
        if deposits(pool).await >= wanted {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn activity_to(address: &str) -> String {
    format!(r#"{{"type":"ADDRESS_ACTIVITY","event":{{"activity":[{{"toAddress":"{address}","asset":"USDC"}}]}}}}"#)
}

// ---- Polling backstop ----

#[sqlx::test]
async fn a_user_checked_recently_is_left_alone_until_the_backstop_passes(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    st.config.engine.deposit_backstop_secs = 300;
    let user = seed_user(&pool).await;
    *mock.transactions.lock().unwrap() = vec![received("USDC", "10")];

    detect_deposits(&st).await.unwrap();
    assert_eq!(deposits(&pool).await, 1);

    mock.transactions.lock().unwrap().push(received("USDC", "5"));
    detect_deposits(&st).await.unwrap();
    assert_eq!(deposits(&pool).await, 1, "checked a moment ago, so not asked again");
    assert_eq!(mock.transaction_calls.load(Ordering::SeqCst), 1);

    sqlx::query("UPDATE deposit_checks SET last_checked_at = now() - interval '10 minutes' WHERE user_id = $1").bind(user).execute(&pool).await.unwrap();
    detect_deposits(&st).await.unwrap();
    assert_eq!(deposits(&pool).await, 2, "after the backstop the new payment is found");
}

#[sqlx::test]
async fn users_never_checked_go_before_users_checked_long_ago(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let old = seed_user(&pool).await;
    sqlx::query("INSERT INTO deposit_checks (user_id, last_checked_at) VALUES ($1, now() - interval '1 hour')").bind(old).execute(&pool).await.unwrap();
    let fresh = seed_user(&pool).await;

    let first: Uuid = sqlx::query_scalar(
        "SELECT u.id FROM users u LEFT JOIN deposit_checks c ON c.user_id = u.id ORDER BY c.last_checked_at NULLS FIRST LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(first, fresh);

    detect_deposits(&st).await.unwrap();
    let checked: i64 = sqlx::query_scalar("SELECT count(*) FROM deposit_checks").fetch_one(&pool).await.unwrap();
    assert_eq!(checked, 2, "both are now recorded as checked");
}

// ---- Hints ----

#[sqlx::test]
async fn a_hint_looks_the_user_up_and_keeps_asking_until_the_deposit_shows(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    st.config.engine.deposit_backstop_secs = 300;
    let user = seed_user(&pool).await;

    hint_checks(&st, user, &[0, 0, 0]).await;
    assert_eq!(mock.transaction_calls.load(Ordering::SeqCst), 3, "nothing credited yet, so it asks every time");
    assert_eq!(deposits(&pool).await, 0);

    *mock.transactions.lock().unwrap() = vec![received("USDC", "12")];
    hint_checks(&st, user, &[0, 0, 0]).await;
    assert_eq!(mock.transaction_calls.load(Ordering::SeqCst), 4, "found at the first look, so it stops");
    assert_eq!(deposits(&pool).await, 1);
}

#[sqlx::test]
async fn a_hint_for_a_user_who_is_not_verified_asks_nobody(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (telegram_id, gateway_user_id) VALUES ($1, 'gw-x') RETURNING id")
        .bind(rand::random::<i64>())
        .fetch_one(&pool)
        .await
        .unwrap();
    hint_checks(&st, stranger, &[0]).await;
    assert_eq!(mock.transaction_calls.load(Ordering::SeqCst), 0);
}

// ---- The webhook ----

#[sqlx::test]
async fn a_signed_webhook_for_a_users_address_makes_the_server_look_that_user_up(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    with_alchemy(&mut st);
    st.config.engine.deposit_backstop_secs = 300;
    let user = seed_user(&pool).await;
    add_wallet(&pool, user, BASE_ADDRESS, "evm").await;
    *mock.transactions.lock().unwrap() = vec![received("USDC", "12")];

    let body = activity_to(&BASE_ADDRESS.to_lowercase());
    let status = chain_hooks::alchemy(State(st.clone()), signed(&body, "sign-key"), Bytes::from(body)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(wait_for_deposits(&pool, 1).await, "the deposit is recorded without waiting for the poll");
}

#[sqlx::test]
async fn a_webhook_with_a_wrong_signature_or_no_setup_is_refused_and_looks_nobody_up(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    add_wallet(&pool, user, BASE_ADDRESS, "evm").await;
    let body = activity_to(&BASE_ADDRESS.to_lowercase());

    let status = chain_hooks::alchemy(State(st.clone()), signed(&body, "sign-key"), Bytes::from(body.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "off until it is configured");

    with_alchemy(&mut st);
    let status = chain_hooks::alchemy(State(st.clone()), signed(&body, "wrong-key"), Bytes::from(body.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let status = chain_hooks::alchemy(State(st.clone()), HeaderMap::new(), Bytes::from(body.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no signature at all");

    let tampered = body.replace("USDC", "SHIB");
    let status = chain_hooks::alchemy(State(st), signed(&body, "sign-key"), Bytes::from(tampered)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a signature only covers the exact body");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mock.transaction_calls.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn a_signed_webhook_for_an_address_that_is_not_ours_is_accepted_and_ignored(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    with_alchemy(&mut st);
    let user = seed_user(&pool).await;
    add_wallet(&pool, user, BASE_ADDRESS, "evm").await;

    let body = activity_to("0x1111111111111111111111111111111111111111");
    let status = chain_hooks::alchemy(State(st.clone()), signed(&body, "sign-key"), Bytes::from(body)).await;
    assert_eq!(status, StatusCode::OK);

    let junk = "not json";
    let status = chain_hooks::alchemy(State(st), signed(junk, "sign-key"), Bytes::from(junk)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(mock.transaction_calls.load(Ordering::SeqCst), 0);
}

// ---- Registering addresses ----

#[sqlx::test]
async fn new_wallet_addresses_go_to_each_matching_webhook_once(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    with_alchemy(&mut st);
    let one = seed_user(&pool).await;
    let two = seed_user(&pool).await;
    add_wallet(&pool, one, BASE_ADDRESS, "evm").await;
    add_wallet(&pool, one, "7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV", "solana").await;
    add_wallet(&pool, two, "0x2222222222222222222222222222222222222222", "evm").await;

    sync_addresses(&st).await.unwrap();
    let calls = mock.alchemy.lock().unwrap().clone();
    assert!(calls.iter().all(|(token, _)| token == "tok"), "every call carries the auth token");
    let mut sent: Vec<(String, usize)> = calls
        .iter()
        .map(|(_, body)| (body["webhook_id"].as_str().unwrap().to_string(), body["addresses_to_add"].as_array().unwrap().len()))
        .collect();
    sent.sort();
    assert_eq!(sent, vec![("wh_base".to_string(), 2), ("wh_robinhood".to_string(), 2), ("wh_sol".to_string(), 1)]);

    sync_addresses(&st).await.unwrap();
    assert_eq!(mock.alchemy.lock().unwrap().len(), 3, "addresses already registered are not sent again");
}

#[sqlx::test]
async fn a_failed_registration_is_retried_on_the_next_pass(pool: PgPool) {
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    with_alchemy(&mut st);
    let user = seed_user(&pool).await;
    add_wallet(&pool, user, BASE_ADDRESS, "evm").await;
    let unwatched = || sqlx::query_scalar::<_, i64>("SELECT count(*) FROM wallets WHERE watched_at IS NULL").fetch_one(&pool);

    mock.alchemy_down.store(true, Ordering::SeqCst);
    sync_addresses(&st).await.unwrap();
    assert_eq!(unwatched().await.unwrap(), 1, "still waiting to be registered");

    mock.alchemy_down.store(false, Ordering::SeqCst);
    sync_addresses(&st).await.unwrap();
    assert_eq!(unwatched().await.unwrap(), 0);
}

#[sqlx::test]
async fn without_the_provider_configured_nothing_is_sent(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    add_wallet(&pool, user, BASE_ADDRESS, "evm").await;

    sync_addresses(&st).await.unwrap();
    assert!(mock.alchemy.lock().unwrap().is_empty());
}
