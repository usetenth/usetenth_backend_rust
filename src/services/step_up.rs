//! Confirms an irreversible action the way the user's account is secured: password, or a Telegram code.

use crate::app_state::ApplicationState;
use crate::services::auth::{self, AuthError, normalize_code, random_code, sha256_hex};
use crate::services::telegram_service::{Button, TelegramService};
use chrono::{Duration, Utc};
use serde::Serialize;
use std::time::Duration as StdDuration;
use uuid::Uuid;

pub const CODE_TTL_SECS: i64 = 300;
const CODE_LEN: usize = 6;
const MAX_WRONG_TRIES: i32 = 5;
const INVALID: &str = "That code is incorrect or has expired";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Password,
    TelegramCode,
}

pub fn add_address_purpose(canonical_address: &str) -> String {
    format!("add_address:{canonical_address}")
}

pub fn withdraw_purpose(address_id: Uuid, asset_key: &str, amount_micros: i64, client_ref: &str) -> String {
    format!("withdraw:{address_id}:{asset_key}:{amount_micros}:{client_ref}")
}

/// Passwords win when set; a user without one confirms through the Telegram account they own.
pub async fn method_for(state: &ApplicationState, user_id: Uuid) -> Result<Method, AuthError> {
    let row: Option<(bool, bool)> = sqlx::query_as("SELECT password IS NOT NULL, telegram_id IS NOT NULL FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await?;
    match row {
        None => Err(AuthError::Unauthorized),
        Some((true, _)) => Ok(Method::Password),
        Some((false, true)) => Ok(Method::TelegramCode),
        Some((false, false)) => Err(AuthError::Conflict("Add an email and password, or link Telegram, before withdrawing".into())),
    }
}

/// Sends a request to Telegram when that is how this user confirms, with an Approve button and a code as a fallback.
/// A password user needs nothing sent.
pub async fn challenge(state: &ApplicationState, user_id: Uuid, purpose: &str, summary_html: &str) -> Result<(Method, Option<Uuid>), AuthError> {
    let method = method_for(state, user_id).await?;
    if method == Method::Password {
        return Ok((method, None));
    }
    if !state.limiter.allow(&format!("stepup-issue:{user_id}"), 5, StdDuration::from_secs(15 * 60)) {
        return Err(AuthError::TooManyRequests);
    }
    let chat_id: i64 = sqlx::query_scalar("SELECT telegram_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;

    let code = random_code(CODE_LEN);
    let mut tx = state.pool.begin().await?;
    // Only the newest challenge is ever live.
    sqlx::query("UPDATE action_confirmations SET used_at = now() WHERE user_id = $1 AND used_at IS NULL")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO action_confirmations (user_id, purpose_hash, code_hash, expires_at) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(user_id)
    .bind(sha256_hex(purpose))
    .bind(sha256_hex(&code))
    .bind(Utc::now() + Duration::seconds(CODE_TTL_SECS))
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    let text = format!(
        "{summary_html}\n\nTap <b>Approve</b> below, or enter this confirmation code in the app:\n<code>{code}</code>\n\nIt works once, for this action only, for {} minutes. If you did not ask for this, ignore it and never share the code.",
        CODE_TTL_SECS / 60
    );
    let approve = [Button::callback("Approve", &format!("ok:{id}"))];
    if let Err(error) = TelegramService::send_message(state, chat_id, &text, &approve).await {
        eprintln!("Could not send a confirmation code to user {user_id}: {error:#}");
        sqlx::query("UPDATE action_confirmations SET used_at = now() WHERE id = $1").bind(id).execute(&state.pool).await?;
        return Err(AuthError::Conflict("We could not reach you on Telegram. Please try again.".into()));
    }
    Ok((method, Some(id)))
}

/// Marks a live request as approved when the Telegram account that owns it taps Approve.
pub async fn approve(state: &ApplicationState, telegram_id: i64, id: Uuid) -> Result<bool, AuthError> {
    let result = sqlx::query(
        "UPDATE action_confirmations SET approved_at = now()
         WHERE id = $1 AND used_at IS NULL AND expires_at > now()
           AND user_id = (SELECT id FROM users WHERE telegram_id = $2)",
    )
    .bind(id)
    .bind(telegram_id)
    .execute(&state.pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// (approved, still live) for a request the user started.
pub async fn status(state: &ApplicationState, user_id: Uuid, id: Uuid) -> Result<(bool, bool), AuthError> {
    let row: Option<(bool, bool)> = sqlx::query_as(
        "SELECT approved_at IS NOT NULL, used_at IS NULL AND expires_at > now() FROM action_confirmations WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    row.ok_or_else(|| AuthError::NotFound("That request was not found".into()))
}

// Burns an approved request in one step, only if it was issued for exactly this purpose.
async fn use_approval(state: &ApplicationState, user_id: Uuid, purpose: &str, id: Uuid) -> Result<(), AuthError> {
    let result = sqlx::query(
        "UPDATE action_confirmations SET used_at = now()
         WHERE id = $1 AND user_id = $2 AND purpose_hash = $3 AND approved_at IS NOT NULL AND used_at IS NULL AND expires_at > now()",
    )
    .bind(id)
    .bind(user_id)
    .bind(sha256_hex(purpose))
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 1 { Ok(()) } else { Err(AuthError::BadRequest("That approval is missing or has expired".into())) }
}

/// Succeeds only with the user's password, with a Telegram approval for exactly this `purpose`, or with its code.
pub async fn confirm(
    state: &ApplicationState,
    user_id: Uuid,
    purpose: &str,
    password: Option<&str>,
    code: Option<&str>,
    approval: Option<Uuid>,
) -> Result<(), AuthError> {
    match method_for(state, user_id).await? {
        Method::Password => {
            let password = password.ok_or_else(|| AuthError::BadRequest("Enter your password".into()))?;
            auth::confirm_password(state, user_id, password).await
        }
        Method::TelegramCode => {
            if let Some(id) = approval {
                return use_approval(state, user_id, purpose, id).await;
            }
            let code = code
                .map(normalize_code)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| AuthError::BadRequest("Enter the confirmation code we sent you on Telegram".into()))?;
            check_code(state, user_id, purpose, &code).await
        }
    }
}

async fn check_code(state: &ApplicationState, user_id: Uuid, purpose: &str, code: &str) -> Result<(), AuthError> {
    let mut tx = state.pool.begin().await?;
    let live: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, code_hash FROM action_confirmations
         WHERE user_id = $1 AND purpose_hash = $2 AND used_at IS NULL AND expires_at > now() AND attempts < $3
         ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
    )
    .bind(user_id)
    .bind(sha256_hex(purpose))
    .bind(MAX_WRONG_TRIES)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, code_hash)) = live else {
        return Err(AuthError::BadRequest(INVALID.into()));
    };
    if code_hash == sha256_hex(code) {
        sqlx::query("UPDATE action_confirmations SET used_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(());
    }
    sqlx::query("UPDATE action_confirmations SET attempts = attempts + 1 WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Err(AuthError::BadRequest(INVALID.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purposes_bind_a_code_to_exactly_one_action() {
        let id = Uuid::nil();
        assert_eq!(add_address_purpose("0xabc"), "add_address:0xabc");
        let a = withdraw_purpose(id, "asset", 5_000_000, "ref-1");
        assert_eq!(a, format!("withdraw:{id}:asset:5000000:ref-1"));
        assert_ne!(a, withdraw_purpose(id, "asset", 5_000_001, "ref-1"), "amount is part of the purpose");
        assert_ne!(a, withdraw_purpose(id, "asset", 5_000_000, "ref-2"), "so is the client reference");
        assert_ne!(a, withdraw_purpose(id, "other", 5_000_000, "ref-1"), "and the asset");
    }

    #[test]
    fn methods_serialize_for_the_mobile_app() {
        assert_eq!(serde_json::to_string(&Method::Password).unwrap(), "\"password\"");
        assert_eq!(serde_json::to_string(&Method::TelegramCode).unwrap(), "\"telegram_code\"");
    }
}
