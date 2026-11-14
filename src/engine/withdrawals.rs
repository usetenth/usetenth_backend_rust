//! Withdrawals: destination and gateway record are re-verified before signing, and nothing is re-created or re-signed once signed.

use super::address;
use super::money::{cmp_decimal, format_micros};
use crate::app_state::ApplicationState;
use crate::integrations::types::{QuantityUnit, TransferRequest, TransferStatus};
use crate::services::telegram_service::TelegramService;
use crate::signing::{IntentKind, PolicyError, SigningPayload};
use crate::utils::usd;
use sqlx::PgPool;
use std::cmp::Ordering;
use uuid::Uuid;

const MAX_ATTEMPTS: i32 = 5;
const STALE_SECS: i64 = 300;

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    address_id: Uuid,
    to_address: String,
    asset_key: String,
    amount_micros: i64,
    attempt_count: i32,
}

async fn set_status(pool: &PgPool, id: Uuid, status: &str, reason: Option<&str>) -> anyhow::Result<()> {
    sqlx::query("UPDATE withdrawals SET status = $2, failure_reason = $3, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(status)
        .bind(reason)
        .execute(pool)
        .await?;
    Ok(())
}

async fn finish(pool: &PgPool, id: Uuid, status: &str, tx_hash: Option<&str>, reason: Option<&str>) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE withdrawals SET status = $2, tx_hash = COALESCE(NULLIF($3, ''), tx_hash), failure_reason = $4, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(tx_hash)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

async fn gateway_user(pool: &PgPool, user_id: Uuid) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT gateway_user_id FROM users WHERE id = $1").bind(user_id).fetch_one(pool).await?)
}

pub async fn execute_pending(state: &ApplicationState) -> anyhow::Result<()> {
    for _ in 0..10 {
        let row: Option<Row> = sqlx::query_as(
            "UPDATE withdrawals SET status = 'processing', attempt_count = attempt_count + 1, updated_at = now()
             WHERE id = (SELECT id FROM withdrawals WHERE status = 'pending' ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1)
             RETURNING id, user_id, address_id, to_address, asset_key, amount_micros, attempt_count",
        )
        .fetch_optional(&state.pool)
        .await?;
        let Some(row) = row else { break };
        if row.attempt_count > MAX_ATTEMPTS {
            set_status(&state.pool, row.id, "failed", Some("too many attempts")).await?;
            continue;
        }
        let id = row.id;
        if let Err(error) = process(state, row).await {
            // Left 'processing' or 'submitted'; recover_stale asks the gateway what happened.
            eprintln!("Withdrawal {id} did not finish: {error:#}");
        }
    }
    Ok(())
}

// Nothing has been created at the gateway yet, so the next tick can simply try again.
async fn try_again(pool: &PgPool, id: Uuid, reason: &str) -> anyhow::Result<()> {
    set_status(pool, id, "pending", Some(reason)).await
}

async fn process(state: &ApplicationState, w: Row) -> anyhow::Result<()> {
    let pool = &state.pool;
    if state.config.engine.dry_run {
        return set_status(pool, w.id, "dry_run", None).await;
    }
    let auth_type = state
        .config
        .engine
        .execute_auth_type
        .clone()
        .ok_or_else(|| anyhow::anyhow!("TM_EXECUTE_AUTH_TYPE is not configured"))?;
    let fail = |reason: String| async move { set_status(pool, w.id, "failed", Some(&reason)).await };

    let dest: Option<(String, bool, bool)> = sqlx::query_as(
        "SELECT chain_family, removed_at IS NOT NULL, usable_at <= now() FROM withdrawal_addresses WHERE id = $1 AND user_id = $2",
    )
    .bind(w.address_id)
    .bind(w.user_id)
    .fetch_optional(pool)
    .await?;
    let Some((family, removed, usable)) = dest else { return fail("destination not found".into()).await };
    if removed || !usable {
        return fail("destination is no longer available".into()).await;
    }
    let chain: Option<String> = sqlx::query_scalar(&format!("SELECT chain FROM assets WHERE asset_key = $1 AND {}", crate::services::catalog::WITHDRAWABLE))
        .bind(&w.asset_key)
        .fetch_optional(pool)
        .await?
        .flatten();
    let Some(chain) = chain else { return fail("asset cannot be withdrawn".into()).await };
    let family_enum = address::family_of_chain(&chain);
    if family_enum.as_str() != family || address::validate(family_enum, &w.to_address).is_err() {
        return fail("destination does not match the asset's network".into()).await;
    }

    let Some(gateway_user) = gateway_user(pool, w.user_id).await? else {
        return fail("user has no gateway account".into()).await;
    };
    let qty = format_micros(w.amount_micros);

    let balances = match state.tm_client.get_user_balance(&gateway_user).await {
        Ok(b) => b,
        Err(e) => return try_again(pool, w.id, &format!("could not read balance: {e:#}")).await,
    };
    let available = balances
        .data
        .iter()
        .find(|b| b.asset_id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(&w.asset_key)))
        .and_then(|b| b.available.clone())
        .unwrap_or_else(|| "0".into());
    if cmp_decimal(&available, &qty) == Some(Ordering::Less) {
        return fail("insufficient balance".into()).await;
    }

    // 3. Create the transfer. Nothing moves until it is executed, so a failure here just retries.
    let request = TransferRequest {
        asset_id: w.asset_key.clone(),
        qty: qty.clone(),
        qty_unit: QuantityUnit::Base,
        to: w.to_address.clone(),
    };
    let transfer = match state.tm_client.create_transfer(&gateway_user, &request).await {
        Ok(t) => t,
        Err(e) => return try_again(pool, w.id, &format!("create transfer failed: {e:#}")).await,
    };
    let transfer_id = transfer.id.to_string();

    // 4. The gateway's record must say exactly what we asked for. If not, do not sign.
    let matches_request = transfer.status == TransferStatus::AwaitingSignature
        && !transfer.payloads.is_empty()
        && transfer.asset_id.to_string().eq_ignore_ascii_case(&w.asset_key)
        && address::same_address(family_enum, &transfer.to, &w.to_address)
        && transfer.qty_unit == QuantityUnit::Base
        && cmp_decimal(&transfer.qty, &qty) == Some(Ordering::Equal);
    if !matches_request {
        eprintln!("Withdrawal {}: gateway transfer {transfer_id} does not match the request; not signing", w.id);
        return fail("the gateway's transfer did not match the request, so it was not signed".into()).await;
    }
    let updated = sqlx::query(
        "UPDATE withdrawals SET gateway_transfer_id = $2, status = 'submitted', updated_at = now() WHERE id = $1",
    )
    .bind(w.id)
    .bind(&transfer_id)
    .execute(pool)
    .await?;
    anyhow::ensure!(updated.rows_affected() == 1, "withdrawal vanished");

    let payloads: Vec<SigningPayload> =
        transfer.payloads.into_iter().map(|p| SigningPayload { digest: p.digest, payload: p.payload }).collect();
    let signed = async {
        let intent = state.signing.create_intent(w.user_id, &transfer_id, IntentKind::Withdraw, &qty).await?;
        state.signing.sign_intent(intent, &payloads).await
    }
    .await;
    let stamps = match signed {
        Ok(stamps) => stamps,
        // Nothing was executed; the unexecuted transfer is harmless.
        Err(e) if e.downcast_ref::<PolicyError>().is_some() => {
            return fail(format!("rejected by signing policy: {e}")).await;
        }
        Err(e) => return Err(e),
    };

    // 6. Execute. Any error from here is ambiguous: leave it 'submitted' for recover_stale.
    let done = state
        .tm_client
        .execute_transfer(
            &gateway_user,
            &transfer_id,
            &crate::integrations::types::ExecuteRequest { signatures: stamps, auth_type },
        )
        .await?;
    match done.status {
        TransferStatus::Completed => finish(pool, w.id, "completed", Some(&done.tx_hash), None).await,
        TransferStatus::Failed => finish(pool, w.id, "failed", Some(&done.tx_hash), Some("the transfer failed on-chain")).await,
        TransferStatus::Pending => finish(pool, w.id, "awaiting_settlement", Some(&done.tx_hash), None).await,
        // Executed but still showing as unsigned: let recovery look again later.
        TransferStatus::AwaitingSignature => Ok(()),
    }
}

pub async fn recover_stale(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    let stuck: Vec<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, user_id, gateway_transfer_id FROM withdrawals
         WHERE status IN ('processing', 'submitted') AND updated_at < now() - ($1::BIGINT * interval '1 second')
         ORDER BY updated_at LIMIT 20",
    )
    .bind(STALE_SECS)
    .fetch_all(pool)
    .await?;
    for (id, user_id, transfer_id) in stuck {
        let result: anyhow::Result<()> = async {
            let Some(transfer_id) = transfer_id else {
                return set_status(pool, id, "pending", None).await;
            };
            let Some(gateway_user) = gateway_user(pool, user_id).await? else { return Ok(()) };
            let transfer = state.tm_client.get_transfer(&gateway_user, &transfer_id).await?;
            match transfer.status {
                TransferStatus::Completed => finish(pool, id, "completed", Some(&transfer.tx_hash), None).await?,
                TransferStatus::Pending => finish(pool, id, "awaiting_settlement", Some(&transfer.tx_hash), None).await?,
                TransferStatus::Failed => finish(pool, id, "failed", Some(&transfer.tx_hash), Some("the transfer failed")).await?,
                // It was never executed and its signature is spent: the money did not move.
                TransferStatus::AwaitingSignature => {
                    set_status(pool, id, "failed", Some("the transfer was never executed; nothing was sent")).await?
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("Could not resolve stuck withdrawal {id}: {error:#}");
        }
    }
    Ok(())
}

pub async fn reconcile(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    let open: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id, gateway_transfer_id FROM withdrawals
         WHERE status = 'awaiting_settlement' AND gateway_transfer_id IS NOT NULL ORDER BY updated_at LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    for (id, user_id, transfer_id) in open {
        let result: anyhow::Result<()> = async {
            let Some(gateway_user) = gateway_user(pool, user_id).await? else { return Ok(()) };
            let transfer = state.tm_client.get_transfer(&gateway_user, &transfer_id).await?;
            match transfer.status {
                TransferStatus::Completed => finish(pool, id, "completed", Some(&transfer.tx_hash), None).await?,
                TransferStatus::Failed => finish(pool, id, "failed", Some(&transfer.tx_hash), Some("the transfer failed on-chain")).await?,
                _ => {}
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            eprintln!("Could not reconcile withdrawal {id}: {error:#}");
        }
    }
    sqlx::query(
        "UPDATE withdrawals SET status = 'needs_review', failure_reason = 'not settled after 24 hours', updated_at = now()
         WHERE status = 'awaiting_settlement' AND updated_at < now() - interval '24 hours'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn notify(state: &ApplicationState) -> anyhow::Result<()> {
    let pool = &state.pool;
    sqlx::query("UPDATE withdrawals SET notified_at = now() WHERE notified_at IS NULL AND status = 'dry_run'").execute(pool).await?;
    let due: Vec<(Uuid, Uuid, Option<i64>, String, i64)> = sqlx::query_as(
        "SELECT w.id, w.user_id, u.telegram_id, w.status, w.amount_micros FROM withdrawals w JOIN users u ON u.id = w.user_id
         WHERE w.notified_at IS NULL AND w.status IN ('completed', 'failed') LIMIT 50",
    )
    .fetch_all(pool)
    .await?;
    for (id, user_id, chat, status, micros) in due {
        let amount = usd(&format_micros(micros));
        let (title, body) = if status == "completed" {
            ("Withdrawal sent", format!("{amount} is on its way to your saved address."))
        } else {
            ("Withdrawal not sent", format!("Your {amount} withdrawal did not go through. Nothing left your account."))
        };
        crate::services::notify::record(pool, user_id, "withdrawal", id, title, &body).await?;
        if let Some(chat_id) = chat {
            if let Err(error) = TelegramService::send_message(state, chat_id, &format!("<b>{title}</b>\n\n{body}"), &[]).await {
                eprintln!("Could not notify about withdrawal {id}: {error:#}");
                continue;
            }
        }
        sqlx::query("UPDATE withdrawals SET notified_at = now() WHERE id = $1").bind(id).execute(pool).await?;
    }
    Ok(())
}
