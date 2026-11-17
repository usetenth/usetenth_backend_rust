use super::worker::*;
use crate::engine::worker_tests::{Mock, received, seed_assets, seed_deposit, seed_user, state, with_alchemy};
use crate::handlers::chain_hooks;
use crate::services::pending::{self, ChainTransfer};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

const USDC_CONTRACT: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const WALLET: &str = "0xBE3F4b43DB5EB49D1F48F53443B9ABCE45DA3B79";

async fn stablecoin(pool: &PgPool) {
    sqlx::query("INSERT INTO assets (asset_key, symbol, name, chain, address, tradable, stable) VALUES ('usdc-base', 'USDC', 'USD Coin', 'base', $1, false, true)")
        .bind(USDC_CONTRACT)
        .execute(pool)
        .await
        .unwrap();
}

async fn wallet(pool: &PgPool, user: Uuid) {
    sqlx::query("INSERT INTO wallets (user_id, address, network) VALUES ($1, $2, 'evm')").bind(user).bind(WALLET).execute(pool).await.unwrap();
}

fn transfer(to: &str, contract: &str, hash: &str) -> ChainTransfer {
    ChainTransfer { to: to.to_lowercase(), contract: contract.to_lowercase(), tx_hash: hash.into(), micros: Some(5_000_000) }
}

async fn pending_rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pending_deposits").fetch_one(pool).await.unwrap()
}

fn signed(body: &str) -> HeaderMap {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"sign-key").unwrap();
    mac.update(body.as_bytes());
    let mut headers = HeaderMap::new();
    headers.insert("x-alchemy-signature", HeaderValue::from_str(&hex::encode(mac.finalize().into_bytes())).unwrap());
    headers
}

#[test]
fn token_transfers_are_read_from_an_evm_alert_and_removed_or_native_ones_are_not() {
    let payload = json!({ "event": { "network": "BASE_MAINNET", "activity": [
        { "category": "token", "hash": "0xABC", "toAddress": "0xBE3F4b43DB5EB49D1F48F53443B9ABCE45DA3B79", "value": 5.25, "asset": "USDC",
          "rawContract": { "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913", "decimals": 6 }, "log": { "removed": false } },
        { "category": "external", "hash": "0xDEF", "toAddress": "0x1111111111111111111111111111111111111111", "value": 1.0, "asset": "ETH" },
        { "category": "token", "hash": "0xGHI", "toAddress": "0x2222222222222222222222222222222222222222", "value": 2.0,
          "rawContract": { "address": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913" }, "log": { "removed": true } }
    ]}});
    let found = pending::evm_transfers(&payload);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].to, "0xbe3f4b43db5eb49d1f48f53443b9abce45da3b79");
    assert_eq!(found[0].contract, "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913");
    assert_eq!(found[0].tx_hash, "0xabc");
    assert_eq!(found[0].micros, Some(5_250_000));
    assert!(pending::evm_transfers(&json!({})).is_empty());
}

#[sqlx::test]
async fn only_a_known_stablecoin_sent_to_a_users_own_address_is_noted_once(pool: PgPool) {
    stablecoin(&pool).await;
    let user = seed_user(&pool).await;
    wallet(&pool, user).await;

    let noted = pending::record_pending(
        &pool,
        &[
            transfer(WALLET, USDC_CONTRACT, "0xaaa"),
            transfer(WALLET, "0x9999999999999999999999999999999999999999", "0xbbb"),
            transfer("0x1111111111111111111111111111111111111111", USDC_CONTRACT, "0xccc"),
        ],
    )
    .await
    .unwrap();
    assert_eq!(noted, 1, "a look-alike token and a stranger's address are ignored");

    let again = pending::record_pending(&pool, &[transfer(WALLET, USDC_CONTRACT, "0xaaa")]).await.unwrap();
    assert_eq!(again, 0, "the same transfer is not noted twice");

    let credited = seed_deposit(&pool, user, 5_000_000, "1 minute").await;
    sqlx::query("UPDATE deposits SET tx_hash = '0xDDD' WHERE id = $1").bind(credited).execute(&pool).await.unwrap();
    let late = pending::record_pending(&pool, &[transfer(WALLET, USDC_CONTRACT, "0xddd")]).await.unwrap();
    assert_eq!(late, 0, "a transfer already credited is not shown as arriving");
}

#[sqlx::test]
async fn a_signed_alert_for_a_token_transfer_shows_the_payment_as_arriving(pool: PgPool) {
    stablecoin(&pool).await;
    let mock = Mock::new("complete");
    let mut st = state(&pool, &mock, true).await;
    with_alchemy(&mut st);
    st.config.engine.deposit_backstop_secs = 300;
    let user = seed_user(&pool).await;
    wallet(&pool, user).await;

    let body = json!({ "event": { "activity": [{
        "category": "token", "hash": "0xfeed", "toAddress": WALLET.to_lowercase(), "value": 7.5, "asset": "USDC",
        "rawContract": { "address": USDC_CONTRACT, "decimals": 6 }
    }]}})
    .to_string();
    let status = chain_hooks::alchemy(State(st.clone()), signed(&body), Bytes::from(body)).await;
    assert_eq!(status, StatusCode::OK);

    let view = pending::pending_for(&pool, user).await.unwrap();
    assert_eq!(view["deposits"][0]["stage"], "confirming");
    assert_eq!(view["deposits"][0]["asset"], "USDC");
    assert_eq!(view["deposits"][0]["amount_usd"], "7.50");
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[sqlx::test]
async fn a_pending_transfer_listed_by_the_gateway_is_shown_until_it_is_credited(pool: PgPool) {
    let mock = Mock::new("complete");
    let st = state(&pool, &mock, true).await;
    let user = seed_user(&pool).await;
    let mut waiting = received("USDC", "10");
    waiting["status"] = "pending".into();
    *mock.transactions.lock().unwrap() = vec![waiting.clone()];

    check_user(&st, user).await.unwrap();
    let view = pending::pending_for(&pool, user).await.unwrap();
    assert_eq!(view["deposits"].as_array().unwrap().len(), 1);
    assert_eq!(view["deposits"][0]["amount_usd"], "10.00");

    waiting["status"] = "completed".into();
    *mock.transactions.lock().unwrap() = vec![waiting];
    check_user(&st, user).await.unwrap();
    assert_eq!(pending_rows(&pool).await, 0, "credited, so no longer pending");
    let view = pending::pending_for(&pool, user).await.unwrap();
    assert!(view["deposits"].as_array().unwrap().iter().all(|d| d["stage"] != "confirming"));
}

#[sqlx::test]
async fn payments_being_invested_and_sales_settling_are_listed_and_finished_ones_are_not(pool: PgPool) {
    seed_assets(&pool).await;
    let user = seed_user(&pool).await;
    let deposit = seed_deposit(&pool, user, 20_000_000, "1 minute").await;
    sqlx::query("INSERT INTO investments (user_id, deposit_id, deposit_micros, invest_micros, percentage, status) VALUES ($1, $2, 20000000, 4000000, 20, 'pending')")
        .bind(user)
        .bind(deposit)
        .execute(&pool)
        .await
        .unwrap();
    for (status, micros) in [("awaiting_settlement", 9_000_000), ("completed", 3_000_000), ("failed", 2_000_000)] {
        sqlx::query("INSERT INTO orders (user_id, kind, side, asset_key, amount_micros, qty_base, status) VALUES ($1, 'manual', 'sell', 'spy-id', $2, '0.5', $3)")
            .bind(user)
            .bind(micros)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO orders (user_id, kind, side, asset_key, amount_micros, status) VALUES ($1, 'manual', 'buy', 'qqq-id', 1000000, 'awaiting_settlement')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();

    let view = pending::pending_for(&pool, user).await.unwrap();
    let deposits = view["deposits"].as_array().unwrap();
    assert_eq!(deposits.len(), 1);
    assert_eq!((deposits[0]["stage"].as_str(), deposits[0]["invest_usd"].as_str()), (Some("investing"), Some("4.00")));
    let sales = view["sales"].as_array().unwrap();
    assert_eq!(sales.len(), 1, "only the sale still settling");
    assert_eq!(sales[0]["asset_key"], "spy-id");
    assert_eq!(sales[0]["qty"], "0.5");
    assert_eq!(sales[0]["amount_usd"], "9.00");
}

#[sqlx::test]
async fn pending_work_is_private_to_its_user(pool: PgPool) {
    let first = seed_user(&pool).await;
    let second = seed_user(&pool).await;
    sqlx::query("INSERT INTO pending_deposits (user_id, tx_hash, asset, amount_micros) VALUES ($1, '0xaaa', 'USDC', 1000000)").bind(first).execute(&pool).await.unwrap();

    assert_eq!(pending::pending_for(&pool, first).await.unwrap()["deposits"].as_array().unwrap().len(), 1);
    assert!(pending::pending_for(&pool, second).await.unwrap()["deposits"].as_array().unwrap().is_empty());
}
