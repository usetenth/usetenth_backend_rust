//! Operator endpoints, off unless ADMIN_API_KEY (16+ chars) is set and sent as `x-admin-key`.

use super::auth::ApiResult;
use super::mobile::validate_weights;
use super::webhook::constant_time_eq;
use crate::app_state::ApplicationState;
use crate::integrations::types::{AssetClass, AssetStatus};
use crate::models::schemas::InvestmentPlan;
use crate::services::auth::AuthError;
use crate::services::response::ApiResponse;
use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::request::Parts,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

pub struct Admin;

impl FromRequestParts<ApplicationState> for Admin {
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &ApplicationState) -> Result<Self, AuthError> {
        let window = Duration::from_secs(60);
        if state.limiter.is_blocked("admin-failures", 10, window) {
            return Err(AuthError::TooManyRequests);
        }
        let expected = state.config.admin_api_key.as_deref().ok_or(AuthError::Unauthorized)?;
        let given = parts.headers.get("x-admin-key").and_then(|v| v.to_str().ok()).unwrap_or("");
        if constant_time_eq(given.as_bytes(), expected.as_bytes()) {
            Ok(Admin)
        } else {
            state.limiter.record("admin-failures", window);
            Err(AuthError::Unauthorized)
        }
    }
}

/// Copies the gateway's first page of assets into `assets`; tradable means active and tradeable.
pub async fn sync_assets(State(state): State<ApplicationState>, _: Admin) -> ApiResult<Value> {
    let list = state.tm_client.get_asset_list().await.map_err(AuthError::Internal)?;
    let (mut synced, mut tradable) = (0, 0);
    for item in list.data {
        let (Some(id), Some(symbol)) = (item.id, item.symbol.clone()) else { continue };
        let is_tradable =
            item.tradeable == Some(true) && item.status == Some(AssetStatus::Active) && item.is_active != Some(false);
        let class = match item.asset_class {
            Some(AssetClass::Stock) => "stock",
            Some(AssetClass::Crypto) => "crypto",
            None => "",
        };
        sqlx::query(
            "INSERT INTO assets (asset_key, symbol, name, chain, address, asset_class, tradable, stable, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
             ON CONFLICT (asset_key) DO UPDATE SET symbol = EXCLUDED.symbol, name = EXCLUDED.name, chain = EXCLUDED.chain,
                 address = EXCLUDED.address, asset_class = EXCLUDED.asset_class, tradable = EXCLUDED.tradable,
                 stable = EXCLUDED.stable, updated_at = now()",
        )
        .bind(id.to_string())
        .bind(&symbol)
        .bind(item.name.unwrap_or_else(|| symbol.clone()))
        .bind(item.chain)
        .bind(item.address)
        .bind(class)
        .bind(is_tradable)
        .bind(item.stable.unwrap_or(false))
        .execute(&state.pool)
        .await?;
        synced += 1;
        tradable += is_tradable as usize;
    }
    Ok(ApiResponse::ok("Assets synced", json!({ "synced": synced, "tradable": tradable })))
}

#[derive(Deserialize)]
pub struct Allocation {
    asset_key: String,
    weight_bps: i32,
}

pub async fn set_allocations(
    State(state): State<ApplicationState>,
    _: Admin,
    Path(plan): Path<String>,
    Json(body): Json<Vec<Allocation>>,
) -> ApiResult<usize> {
    let plan = crate::services::onboarding::parse_plan(&plan)
        .filter(|p| *p != InvestmentPlan::Custom)
        .ok_or_else(|| AuthError::BadRequest("Plan must be steady, growth or tech".into()))?;
    let weights: Vec<(String, i32)> = body.into_iter().map(|a| (a.asset_key, a.weight_bps)).collect();
    validate_weights(&weights)?;
    let keys: Vec<String> = weights.iter().map(|(k, _)| k.clone()).collect();
    let ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE asset_key = ANY($1) AND tradable AND NOT stable")
        .bind(&keys)
        .fetch_one(&state.pool)
        .await?;
    if ok as usize != keys.len() {
        return Err(AuthError::BadRequest("Every asset must exist and be tradable (run /admin/assets/sync)".into()));
    }
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM plan_allocations WHERE plan = $1").bind(&plan).execute(&mut *tx).await?;
    for (key, bps) in &weights {
        sqlx::query("INSERT INTO plan_allocations (plan, asset_key, weight_bps) VALUES ($1, $2, $3)")
            .bind(&plan)
            .bind(key)
            .bind(bps)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(ApiResponse::ok("Allocations saved", weights.len()))
}

pub async fn orders_to_review(State(state): State<ApplicationState>, _: Admin) -> ApiResult<Vec<Value>> {
    let rows: Vec<(Uuid, Uuid, Option<String>, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, user_id, gateway_order_id, amount_micros, failure_reason, tx_hash
         FROM orders WHERE status = 'needs_review' ORDER BY updated_at",
    )
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(|(id, user_id, gateway_order_id, micros, reason, tx_hash)| {
            json!({ "id": id, "user_id": user_id, "gateway_order_id": gateway_order_id,
                    "amount_usd": crate::engine::money::format_micros(micros), "reason": reason, "tx_hash": tx_hash })
        })
        .collect();
    Ok(ApiResponse::ok("Orders to review", items))
}

#[derive(Deserialize)]
pub struct Resolution {
    status: String,
    note: String,
}

/// Records a manual review outcome; no retry, since re-placing is only safe after a human confirms.
pub async fn resolve_order(
    State(state): State<ApplicationState>,
    _: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<Resolution>,
) -> ApiResult<()> {
    if !matches!(body.status.as_str(), "completed" | "failed") || body.note.trim().is_empty() {
        return Err(AuthError::BadRequest("status must be completed or failed, with a note".into()));
    }
    let result = sqlx::query(
        "UPDATE orders SET status = $2, failure_reason = $3, updated_at = now() WHERE id = $1 AND status = 'needs_review'",
    )
    .bind(id)
    .bind(&body.status)
    .bind(format!("reviewed: {}", body.note.trim()))
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AuthError::NotFound("No order waiting for review with that id".into()));
    }
    Ok(ApiResponse::ok("Order resolved", ()))
}


/// Identity checks waiting for a human decision. The NIN itself is never shown, only its masked tail.
pub async fn kyc_to_review(State(state): State<ApplicationState>, _: Admin) -> ApiResult<Vec<Value>> {
    let rows: Vec<(Uuid, Uuid, String, String, Option<String>, Option<i32>, Option<String>)> = sqlx::query_as(
        "SELECT id, user_id, full_name, nin_masked, reason, name_match_hits, vendor FROM kyc_session_records WHERE status = 'review' ORDER BY updated_at",
    )
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .into_iter()
        .map(|(id, user_id, name, masked, reason, hits, vendor)| {
            json!({ "id": id, "user_id": user_id, "full_name": name, "nin_masked": masked, "reason": reason, "name_match_hits": hits, "vendor": vendor })
        })
        .collect();
    Ok(ApiResponse::ok("Identity checks to review", items))
}

#[derive(Deserialize)]
pub struct KycDecision {
    verified: bool,
}

pub async fn kyc_resolve(State(state): State<ApplicationState>, _: Admin, Path(id): Path<Uuid>, Json(body): Json<KycDecision>) -> ApiResult<()> {
    if !crate::services::kyc::resolve_review(&state, id, body.verified).await? {
        return Err(AuthError::NotFound("No identity check waiting for review with that id".into()));
    }
    Ok(ApiResponse::ok("Identity check resolved", ()))
}
