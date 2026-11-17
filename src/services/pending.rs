use crate::engine::money::format_micros;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

// How long an incoming payment is shown as arriving before it is assumed lost track of.
const ARRIVING_HOURS: i64 = 2;

pub struct ChainTransfer {
    pub to: String,
    pub contract: String,
    pub tx_hash: String,
    pub micros: Option<i64>,
}

/// Token transfers in an EVM address-activity payload, with addresses and hashes lowercased.
pub fn evm_transfers(payload: &Value) -> Vec<ChainTransfer> {
    let Some(activity) = payload.pointer("/event/activity").and_then(Value::as_array) else { return vec![] };
    activity
        .iter()
        .filter_map(|a| {
            if a.pointer("/log/removed").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            if !matches!(a["category"].as_str()?, "token" | "erc20") {
                return None;
            }
            Some(ChainTransfer {
                to: a["toAddress"].as_str()?.to_lowercase(),
                contract: a.pointer("/rawContract/address").and_then(Value::as_str)?.to_lowercase(),
                tx_hash: a["hash"].as_str()?.to_lowercase(),
                micros: a["value"].as_f64().filter(|v| *v > 0.0).map(|v| (v * 1_000_000.0).round() as i64),
            })
        })
        .collect()
}

/// Notes incoming stablecoin transfers to users' own addresses, so the app can show them before the gateway credits them.
/// Only a token contract we know as a stablecoin counts, so a look-alike token never shows up.
pub async fn record_pending(pool: &PgPool, transfers: &[ChainTransfer]) -> sqlx::Result<usize> {
    let mut noted = 0;
    for transfer in transfers {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT w.user_id, a.symbol FROM wallets w JOIN assets a ON a.stable AND lower(a.address) = $2
             WHERE w.network = 'evm' AND lower(w.address) = $1 LIMIT 1",
        )
        .bind(&transfer.to)
        .bind(&transfer.contract)
        .fetch_optional(pool)
        .await?;
        let Some((user_id, symbol)) = row else { continue };
        let credited: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deposits WHERE user_id = $1 AND lower(tx_hash) = $2)")
            .bind(user_id)
            .bind(&transfer.tx_hash)
            .fetch_one(pool)
            .await?;
        if credited {
            continue;
        }
        let result = sqlx::query(
            "INSERT INTO pending_deposits (user_id, tx_hash, asset, amount_micros) VALUES ($1, $2, upper($3), $4)
             ON CONFLICT (user_id, tx_hash) DO NOTHING",
        )
        .bind(user_id)
        .bind(&transfer.tx_hash)
        .bind(&symbol)
        .bind(transfer.micros)
        .execute(pool)
        .await?;
        noted += result.rows_affected() as usize;
    }
    Ok(noted)
}

/// A transfer the gateway lists but has not completed yet.
pub async fn note_gateway_pending(pool: &PgPool, user_id: Uuid, tx_hash: &str, symbol: &str, micros: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO pending_deposits (user_id, tx_hash, asset, amount_micros) VALUES ($1, lower($2), upper($3), $4)
         ON CONFLICT (user_id, tx_hash) DO UPDATE SET amount_micros = EXCLUDED.amount_micros",
    )
    .bind(user_id)
    .bind(tx_hash)
    .bind(symbol)
    .bind(micros)
    .execute(pool)
    .await?;
    Ok(())
}

/// The transfer has been credited, so it is no longer pending.
pub async fn clear(pool: &PgPool, user_id: Uuid, tx_hash: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM pending_deposits WHERE user_id = $1 AND tx_hash = lower($2)").bind(user_id).bind(tx_hash).execute(pool).await?;
    Ok(())
}

/// What is still in motion for a user: payments arriving or being invested, and sales settling.
pub async fn pending_for(pool: &PgPool, user_id: Uuid) -> sqlx::Result<Value> {
    let arriving: Vec<(String, String, Option<i64>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT p.tx_hash, p.asset, p.amount_micros, p.seen_at FROM pending_deposits p
         WHERE p.user_id = $1 AND p.seen_at > now() - ($2::BIGINT * interval '1 hour')
           AND NOT EXISTS (SELECT 1 FROM deposits d WHERE d.user_id = p.user_id AND lower(d.tx_hash) = p.tx_hash)
         ORDER BY p.seen_at DESC LIMIT 10",
    )
    .bind(user_id)
    .bind(ARRIVING_HOURS)
    .fetch_all(pool)
    .await?;
    let investing: Vec<(Uuid, String, i64, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT i.id, d.asset, i.deposit_micros, i.invest_micros, i.created_at FROM investments i JOIN deposits d ON d.id = i.deposit_id
         WHERE i.user_id = $1 AND i.status = 'pending' ORDER BY i.created_at DESC LIMIT 10",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let sales: Vec<(Uuid, String, Option<String>, Option<String>, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT o.id, o.asset_key, a.symbol, o.qty_base, o.amount_micros, o.created_at FROM orders o
         LEFT JOIN assets a ON a.asset_key = o.asset_key
         WHERE o.user_id = $1 AND o.side = 'sell' AND o.status IN ('pending', 'processing', 'submitted', 'awaiting_settlement')
         ORDER BY o.created_at DESC LIMIT 10",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let mut deposits: Vec<Value> = arriving
        .into_iter()
        .map(|(hash, asset, micros, since)| json!({ "id": hash, "stage": "confirming", "asset": asset, "amount_usd": micros.map(format_micros), "since": since }))
        .collect();
    deposits.extend(investing.into_iter().map(|(id, asset, deposit, invest, since)| {
        json!({ "id": id, "stage": "investing", "asset": asset, "amount_usd": format_micros(deposit), "invest_usd": format_micros(invest), "since": since })
    }));
    Ok(json!({
        "deposits": deposits,
        "sales": sales
            .into_iter()
            .map(|(id, asset_key, symbol, qty, micros, since)| json!({ "id": id, "asset_key": asset_key, "asset": symbol, "qty": qty, "amount_usd": format_micros(micros), "since": since }))
            .collect::<Vec<_>>(),
    }))
}
