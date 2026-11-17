use crate::engine::money::format_micros;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

// A payment this long in the arriving list without being credited is dropped from it.
const ARRIVING_MINUTES: i64 = 30;
// A sale or buy that finished this recently still counts until the balances catch up.
const SETTLED_MINUTES: i64 = 5;
// The same credited payment is recognised by amount for this long after it is seen.
const MATCH_WINDOW_MINUTES: i64 = 10;

pub struct ChainTransfer {
    pub to: String,
    pub contract: String,
    pub tx_hash: String,
    pub micros: Option<i64>,
}

/// Transaction hashes compare equal however they are written: lowercase, without a leading 0x.
pub fn norm_hash(hash: &str) -> String {
    let lower = hash.trim().to_lowercase();
    lower.strip_prefix("0x").map(str::to_string).unwrap_or(lower)
}

/// Token transfers in an EVM address-activity payload, with addresses lowercased and hashes normalised.
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
                tx_hash: norm_hash(a["hash"].as_str()?),
                micros: a["value"].as_f64().filter(|v| *v > 0.0).map(|v| (v * 1_000_000.0).round() as i64),
            })
        })
        .collect()
}

// True when this payment is already a credited deposit, by hash or by the same amount a moment ago.
async fn credited(pool: &PgPool, user_id: Uuid, hash: &str, asset: &str, micros: Option<i64>) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM deposits d WHERE d.user_id = $1 AND (
             regexp_replace(lower(d.tx_hash), '^0x', '') = $2
          OR ($4::BIGINT IS NOT NULL AND upper(d.asset) = upper($3) AND d.amount_micros BETWEEN $4 - 1 AND $4 + 1
              AND d.created_at > now() - ($5::BIGINT * interval '1 minute'))))",
    )
    .bind(user_id)
    .bind(hash)
    .bind(asset)
    .bind(micros)
    .bind(MATCH_WINDOW_MINUTES)
    .fetch_one(pool)
    .await
}

/// Notes an incoming stablecoin transfer so the app can show it before the gateway credits it.
pub async fn note(pool: &PgPool, user_id: Uuid, hash: &str, symbol: &str, micros: Option<i64>) -> sqlx::Result<bool> {
    let hash = norm_hash(hash);
    if credited(pool, user_id, &hash, symbol, micros).await? {
        return Ok(false);
    }
    let result = sqlx::query(
        "INSERT INTO pending_deposits (user_id, tx_hash, asset, amount_micros) VALUES ($1, $2, upper($3), $4)
         ON CONFLICT (user_id, tx_hash) DO NOTHING",
    )
    .bind(user_id)
    .bind(&hash)
    .bind(symbol)
    .bind(micros)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Notes incoming EVM stablecoin transfers to users' own addresses.
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
        if note(pool, user_id, &transfer.tx_hash, &symbol, transfer.micros).await? {
            noted += 1;
        }
    }
    Ok(noted)
}

/// A transfer the gateway lists but has not completed yet; seen again, it stays on the list.
pub async fn note_gateway_pending(pool: &PgPool, user_id: Uuid, tx_hash: &str, symbol: &str, micros: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO pending_deposits (user_id, tx_hash, asset, amount_micros) VALUES ($1, $2, upper($3), $4)
         ON CONFLICT (user_id, tx_hash) DO UPDATE SET amount_micros = EXCLUDED.amount_micros, seen_at = now()",
    )
    .bind(user_id)
    .bind(norm_hash(tx_hash))
    .bind(symbol)
    .bind(micros)
    .execute(pool)
    .await?;
    Ok(())
}

/// The transfer has been credited, so it is no longer pending.
pub async fn clear(pool: &PgPool, user_id: Uuid, tx_hash: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM pending_deposits WHERE user_id = $1 AND tx_hash = $2").bind(user_id).bind(norm_hash(tx_hash)).execute(pool).await?;
    Ok(())
}

/// A deposit was just credited: whatever was shown as arriving for that amount is done, however its hash was written.
pub async fn settle(pool: &PgPool, user_id: Uuid, asset: &str, micros: i64) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM pending_deposits WHERE user_id = $1 AND asset = upper($2)
           AND amount_micros BETWEEN $3 - 1 AND $3 + 1 AND seen_at > now() - interval '1 day'",
    )
    .bind(user_id)
    .bind(asset)
    .bind(micros)
    .execute(pool)
    .await?;
    Ok(())
}

type TradeRow = (Uuid, String, Option<String>, Option<String>, i64, DateTime<Utc>, bool);

fn trade_json(row: TradeRow) -> Value {
    let (id, asset_key, symbol, qty, micros, since, done) = row;
    json!({ "id": id, "asset_key": asset_key, "asset": symbol, "qty": qty, "amount_usd": format_micros(micros), "since": since, "done": done })
}

async fn trades(pool: &PgPool, user_id: Uuid, side: &str, manual_only: bool) -> sqlx::Result<Vec<Value>> {
    let rows: Vec<TradeRow> = sqlx::query_as(
        "SELECT o.id, o.asset_key, a.symbol, o.qty_base, o.amount_micros, o.created_at, o.status = 'completed' FROM orders o
         LEFT JOIN assets a ON a.asset_key = o.asset_key
         WHERE o.user_id = $1 AND o.side = $2 AND ($3 = false OR o.kind = 'manual')
           AND (o.status IN ('pending', 'processing', 'submitted', 'awaiting_settlement')
                OR (o.status = 'completed' AND o.updated_at > now() - ($4::BIGINT * interval '1 minute')))
         ORDER BY o.created_at DESC LIMIT 10",
    )
    .bind(user_id)
    .bind(side)
    .bind(manual_only)
    .bind(SETTLED_MINUTES)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(trade_json).collect())
}

/// What is still in motion for a user: payments arriving or being invested, and buys and sales settling.
/// A buy or sale that has just finished stays listed (done) until the balances catch up.
pub async fn pending_for(pool: &PgPool, user_id: Uuid) -> sqlx::Result<Value> {
    let arriving: Vec<(String, String, Option<i64>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT p.tx_hash, p.asset, p.amount_micros, p.seen_at FROM pending_deposits p
         WHERE p.user_id = $1 AND p.seen_at > now() - ($2::BIGINT * interval '1 minute')
           AND NOT EXISTS (SELECT 1 FROM deposits d WHERE d.user_id = p.user_id AND (
                 regexp_replace(lower(d.tx_hash), '^0x', '') = p.tx_hash
              OR (p.amount_micros IS NOT NULL AND upper(d.asset) = p.asset AND d.amount_micros BETWEEN p.amount_micros - 1 AND p.amount_micros + 1
                  AND d.created_at >= p.seen_at - ($3::BIGINT * interval '1 minute'))))
         ORDER BY p.seen_at DESC LIMIT 10",
    )
    .bind(user_id)
    .bind(ARRIVING_MINUTES)
    .bind(MATCH_WINDOW_MINUTES)
    .fetch_all(pool)
    .await?;
    let investing: Vec<(Uuid, String, i64, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT i.id, d.asset, i.deposit_micros, i.invest_micros, i.created_at FROM investments i JOIN deposits d ON d.id = i.deposit_id
         WHERE i.user_id = $1 AND i.status = 'pending' ORDER BY i.created_at DESC LIMIT 10",
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
        "sales": trades(pool, user_id, "sell", false).await?,
        "buys": trades(pool, user_id, "buy", true).await?,
    }))
}
