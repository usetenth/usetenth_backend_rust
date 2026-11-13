//! Confirms an irreversible action the way the user's account is secured: password, or a Telegram code.

use crate::app_state::ApplicationState;
use crate::services::auth::{self, AuthError, normalize_code, random_code, sha256_hex};
use crate::services::telegram_service::TelegramService;
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

/// Sends a code to Telegram when that is how this user confirms; a password user needs nothing sent.
pub async fn challenge(state: &ApplicationState, user_id: Uuid, purpose: &str, summary_html: &str) -> Result<Method, AuthError> {
    let method = method_for(state, user_id).await?;
    if method == Method::Password {
        return Ok(method);
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
        "{summary_html}\n\nYour confirmation code:\n<code>{code}</code>\n\nIt works once, for this action only, for {} minutes. If you did not ask for this, ignore it and never share the code.",
        CODE_TTL_SECS / 60
    );
    if let Err(error) = TelegramService::send_message(state, chat_id, &text, &[]).await {
        eprintln!("Could not send a confirmation code to user {user_id}: {error:#}");
        sqlx::query("UPDATE action_confirmations SET used_at = now() WHERE id = $1").bind(id).execute(&state.pool).await?;
        return Err(AuthError::Conflict("We could not reach you on Telegram. Please try again.".into()));
    }
    Ok(method)
}

/// Succeeds only with the user's password, or with the code that was issued for exactly this `purpose`.
pub async fn confirm(
    state: &ApplicationState,
    user_id: Uuid,
    purpose: &str,
    password: Option<&str>,
    code: Option<&str>,
) -> Result<(), AuthError> {
    match method_for(state, user_id).await? {
        Method::Password => {
            let password = password.ok_or_else(|| AuthError::BadRequest("Enter your password".into()))?;
            auth::confirm_password(state, user_id, password).await
        }
        Method::TelegramCode => {
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
