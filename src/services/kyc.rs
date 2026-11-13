//! Identity checks, following the Swiftgigs flow: NIN lookup with vendor failover, name match, limits, review.

use crate::app_state::ApplicationState;
use crate::config::KycMode;
use crate::integrations::kyc_vendor::{self, ErrorKind};
use crate::services::auth::{AuthError, sha256_hex};
use crate::services::name_match::{self, MatchResult};
use crate::services::notify;
use crate::services::onboarding::{self, Stage};
use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand::RngCore;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct KycView {
    // none, pending, verified, review or failed.
    pub status: String,
    pub reason: Option<String>,
    pub nin_masked: Option<String>,
    pub can_retry: bool,
}

pub fn is_valid_nin(value: &str) -> bool {
    value.len() == 11 && value.bytes().all(|b| b.is_ascii_digit())
}

fn hash_nin(salt: &str, nin: &str) -> String {
    sha256_hex(&format!("{nin}{salt}"))
}

fn mask_nin(nin: &str) -> String {
    format!("{}{}", "•".repeat(nin.len().saturating_sub(4)), &nin[nin.len().saturating_sub(4)..])
}

pub fn encrypt_nin(key: &[u8; 32], nin: &str) -> Result<String> {
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let sealed = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
        .encrypt(Nonce::from_slice(&nonce), nin.as_bytes())
        .map_err(|_| anyhow!("could not encrypt the NIN"))?;
    Ok(format!("{}.{}", STANDARD.encode(nonce), STANDARD.encode(sealed)))
}

pub fn decrypt_nin(key: &[u8; 32], payload: &str) -> Result<String> {
    let (nonce, sealed) = payload.split_once('.').ok_or_else(|| anyhow!("malformed encrypted NIN"))?;
    let (nonce, sealed) = (STANDARD.decode(nonce)?, STANDARD.decode(sealed)?);
    anyhow::ensure!(nonce.len() == 12, "malformed encrypted NIN");
    let plain = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
        .decrypt(Nonce::from_slice(&nonce), sealed.as_ref())
        .map_err(|_| anyhow!("could not decrypt the NIN"))?;
    Ok(String::from_utf8(plain)?)
}

/// Marks the user verified and activates their rule, atomically.
async fn approve(state: &ApplicationState, user_id: Uuid) -> Result<()> {
    let mut tx = state.pool.begin().await?;
    approve_in(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(())
}

async fn approve_in(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, user_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE users SET verified = 'verified', updated_at = now() WHERE id = $1").bind(user_id).execute(&mut **tx).await?;
    sqlx::query("UPDATE rules SET enabled = true, enabled_at = COALESCE(enabled_at, now()), updated_at = now() WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Development only (KYC_MODE=auto): approves without any check.
pub async fn auto_approve(state: &ApplicationState, user_id: Uuid) -> Result<()> {
    anyhow::ensure!(state.config.kyc.mode == KycMode::Auto, "auto approval is switched off");
    approve(state, user_id).await
}

pub async fn current(state: &ApplicationState, user_id: Uuid) -> Result<KycView, AuthError> {
    let row: Option<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT status, reason, nin_masked FROM kyc_session_records WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let verified: bool = sqlx::query_scalar("SELECT verified = 'verified' FROM users WHERE id = $1").bind(user_id).fetch_one(&state.pool).await?;
    Ok(match row {
        _ if verified => KycView { status: "verified".into(), reason: None, nin_masked: None, can_retry: false },
        None => KycView { status: "none".into(), reason: None, nin_masked: None, can_retry: true },
        Some((status, reason, masked)) => {
            let can_retry = status == "failed";
            KycView { status, reason, nin_masked: Some(masked), can_retry }
        }
    })
}

#[derive(sqlx::FromRow)]
struct Record {
    id: Uuid,
    user_id: Uuid,
    full_name: String,
    nin_hash: String,
    nin_encrypted: Option<String>,
    attempts: i32,
}

/// Takes the NIN and name, stores the NIN encrypted, and starts the check (right away when the vendor answers).
pub async fn submit(state: &ApplicationState, user_id: Uuid, full_name: &str, nin: &str) -> Result<KycView, AuthError> {
    let cfg = &state.config.kyc;
    if cfg.mode == KycMode::Auto {
        approve(state, user_id).await.map_err(AuthError::Internal)?;
        return current(state, user_id).await;
    }
    match onboarding::stage_of(state, user_id).await.map_err(AuthError::Internal)? {
        Stage::Active => return current(state, user_id).await,
        Stage::NeedsKyc => {}
        _ => return Err(AuthError::Conflict("Finish the earlier setup steps first".into())),
    }

    let nin = nin.trim();
    if !is_valid_nin(nin) {
        return Err(AuthError::BadRequest("Your NIN is 11 digits".into()));
    }
    let full_name = full_name.trim();
    if name_match::tokens(full_name).len() < 2 || full_name.len() > 120 {
        return Err(AuthError::BadRequest("Enter your full name as it appears on your NIN".into()));
    }
    let hash = hash_nin(&cfg.nin_hash_salt, nin);

    let open: Option<String> = sqlx::query_scalar("SELECT status FROM kyc_session_records WHERE user_id = $1 AND status IN ('pending', 'review')")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await?;
    if open.is_some() {
        return current(state, user_id).await;
    }

    let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kyc_blocklist WHERE nin_hash = $1)").bind(&hash).fetch_one(&state.pool).await?;
    if blocked {
        record_failure(state, user_id, full_name, &hash, nin, "blocked").await?;
        return current(state, user_id).await;
    }
    let taken: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM kyc_session_records WHERE nin_hash = $1 AND status = 'verified' AND user_id <> $2)")
        .bind(&hash)
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;
    if taken {
        return Err(AuthError::Conflict("That ID is already linked to another account".into()));
    }

    let (by_user, by_nin): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE user_id = $1), COUNT(*) FILTER (WHERE nin_hash = $2) FROM kyc_lookups WHERE created_at > now() - interval '24 hours'",
    )
    .bind(user_id)
    .bind(&hash)
    .fetch_one(&state.pool)
    .await?;
    if by_user >= cfg.max_lookups_per_user_per_day || by_nin >= cfg.max_lookups_per_nin_per_day {
        return Err(AuthError::Conflict("You have reached today's verification limit. Try again tomorrow.".into()));
    }

    let encrypted = encrypt_nin(&cfg.nin_encryption_key, nin).map_err(AuthError::Internal)?;
    let inserted = sqlx::query(
        "INSERT INTO kyc_session_records (user_id, full_name, nin_hash, nin_masked, nin_encrypted, status) VALUES ($1, $2, $3, $4, $5, 'pending')",
    )
    .bind(user_id)
    .bind(full_name)
    .bind(&hash)
    .bind(mask_nin(nin))
    .bind(&encrypted)
    .execute(&state.pool)
    .await;
    match inserted {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => return current(state, user_id).await,
        Err(e) => return Err(e.into()),
    }

    if let Err(error) = process_due(state).await {
        eprintln!("KYC processing failed: {error:#}");
    }
    current(state, user_id).await
}

async fn record_failure(state: &ApplicationState, user_id: Uuid, full_name: &str, hash: &str, nin: &str, reason: &str) -> Result<(), AuthError> {
    sqlx::query("INSERT INTO kyc_session_records (user_id, full_name, nin_hash, nin_masked, status, reason) VALUES ($1, $2, $3, $4, 'failed', $5)")
        .bind(user_id)
        .bind(full_name)
        .bind(hash)
        .bind(mask_nin(nin))
        .bind(reason)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Works through checks that are due, claiming each by pushing its next attempt out so two workers never share one.
pub async fn process_due(state: &ApplicationState) -> Result<()> {
    loop {
        let claimed: Option<Record> = sqlx::query_as(
            "UPDATE kyc_session_records SET next_attempt_at = now() + interval '2 minutes', updated_at = now()
             WHERE id = (SELECT id FROM kyc_session_records WHERE status = 'pending' AND next_attempt_at <= now()
                         ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT 1)
             RETURNING id, user_id, full_name, nin_hash, nin_encrypted, attempts",
        )
        .fetch_optional(&state.pool)
        .await?;
        let Some(record) = claimed else { return Ok(()) };
        let id = record.id;
        if let Err(error) = process(state, record).await {
            eprintln!("KYC check {id} did not finish: {error:#}");
        }
    }
}

async fn process(state: &ApplicationState, rec: Record) -> Result<()> {
    let cfg = &state.config.kyc;
    let Some(encrypted) = rec.nin_encrypted.as_deref() else {
        return set_outcome(state, &rec, "failed", Some("nin_missing"), None, None).await;
    };
    let nin = decrypt_nin(&cfg.nin_encryption_key, encrypted)?;

    let entity = match kyc_vendor::lookup_with_failover(&state.http, &cfg.vendors, &nin).await {
        Ok(entity) => entity,
        Err(error) if error.kind == ErrorKind::Upstream => {
            let attempts = rec.attempts + 1;
            if attempts >= cfg.max_attempts {
                eprintln!("KYC check {} exhausted its retries ({}): sending to review", rec.id, error.message);
                return set_outcome(state, &rec, "review", Some("vendor_outage"), None, None).await;
            }
            let backoff = cfg.retry_delay_secs.saturating_mul(1 << (attempts - 1).min(10));
            sqlx::query("UPDATE kyc_session_records SET attempts = $2, next_attempt_at = now() + ($3::BIGINT * interval '1 second'), updated_at = now() WHERE id = $1")
                .bind(rec.id)
                .bind(attempts)
                .bind(backoff)
                .execute(&state.pool)
                .await?;
            return Ok(());
        }
        // Any definitive answer was billed, so it counts against the daily limits.
        Err(error) => {
            record_lookup(state, &rec).await?;
            let reason = if error.kind == ErrorKind::NotFound { "nin_not_found" } else { "nin_rejected" };
            return set_outcome(state, &rec, "failed", Some(reason), None, None).await;
        }
    };
    record_lookup(state, &rec).await?;

    let (result, hits) = name_match::match_nin_to_name(&entity.first_name, &entity.middle_name, &entity.last_name, &rec.full_name);
    match result {
        MatchResult::Pass => set_outcome(state, &rec, "verified", None, Some(&entity.vendor), Some(hits as i32)).await,
        MatchResult::Review => set_outcome(state, &rec, "review", Some("name_partial_match"), Some(&entity.vendor), Some(hits as i32)).await,
        MatchResult::Fail => set_outcome(state, &rec, "failed", Some("name_mismatch"), Some(&entity.vendor), Some(hits as i32)).await,
    }
}

async fn record_lookup(state: &ApplicationState, rec: &Record) -> Result<()> {
    sqlx::query("INSERT INTO kyc_lookups (user_id, nin_hash) VALUES ($1, $2)").bind(rec.user_id).bind(&rec.nin_hash).execute(&state.pool).await?;
    Ok(())
}

// Review keeps the encrypted NIN so a human decision never bills a second lookup; every other outcome wipes it.
async fn set_outcome(
    state: &ApplicationState,
    rec: &Record,
    status: &str,
    reason: Option<&str>,
    vendor: Option<&str>,
    hits: Option<i32>,
) -> Result<()> {
    let mut tx = state.pool.begin().await?;
    let mut status = status;
    let mut reason = reason;
    let keep_nin = status == "review";
    let updated = sqlx::query(
        "UPDATE kyc_session_records SET status = $2, reason = $3, vendor = COALESCE($4, vendor), name_match_hits = COALESCE($5, name_match_hits),
                nin_encrypted = CASE WHEN $6 THEN nin_encrypted ELSE NULL END, updated_at = now()
         WHERE id = $1",
    )
    .bind(rec.id)
    .bind(status)
    .bind(reason)
    .bind(vendor)
    .bind(hits)
    .bind(keep_nin)
    .execute(&mut *tx)
    .await;
    match updated {
        // Someone else already verified with this NIN since we started.
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            tx.rollback().await?;
            status = "failed";
            reason = Some("nin_in_use");
            sqlx::query("UPDATE kyc_session_records SET status = 'failed', reason = 'nin_in_use', nin_encrypted = NULL, updated_at = now() WHERE id = $1")
                .bind(rec.id)
                .execute(&state.pool)
                .await?;
            return notify_outcome(state, rec, status, reason);
        }
        other => {
            other?;
        }
    }
    match status {
        "verified" => approve_in(&mut tx, rec.user_id).await?,
        "review" => {
            sqlx::query("UPDATE users SET verified = 'review', updated_at = now() WHERE id = $1 AND verified <> 'verified'").bind(rec.user_id).execute(&mut *tx).await?;
        }
        _ => {
            sqlx::query("UPDATE users SET verified = 'unverified', updated_at = now() WHERE id = $1 AND verified <> 'verified'").bind(rec.user_id).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    notify_outcome(state, rec, status, reason)
}

fn notify_outcome(state: &ApplicationState, rec: &Record, status: &str, reason: Option<&str>) -> Result<()> {
    let text = match (status, reason) {
        ("verified", _) => "<b>You are verified</b>\n\nYour tenth is now active. Send /deposit to add funds.".to_string(),
        ("review", _) => "<b>We are taking a closer look</b>\n\nYour details need a quick manual check. We will message you as soon as it is done.".to_string(),
        (_, Some("name_mismatch")) => "<b>We could not match your name</b>\n\nThe name you gave does not match your NIN record. Check it and try again.".to_string(),
        (_, Some("nin_not_found" | "nin_rejected")) => "<b>We could not find that NIN</b>\n\nCheck the 11 digits and try again.".to_string(),
        _ => "<b>We could not verify you</b>\n\nPlease contact support.".to_string(),
    };
    notify::tell_user(state, rec.user_id, text);
    Ok(())
}

/// Resolves a check that went to review, as an operator would after looking at the case.
pub async fn resolve_review(state: &ApplicationState, record_id: Uuid, verified: bool) -> Result<bool, AuthError> {
    let rec: Option<Record> = sqlx::query_as(
        "SELECT id, user_id, full_name, nin_hash, nin_encrypted, attempts FROM kyc_session_records WHERE id = $1 AND status = 'review' FOR UPDATE",
    )
    .bind(record_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(rec) = rec else { return Ok(false) };
    let (status, reason) = if verified { ("verified", None) } else { ("failed", Some("rejected_in_review")) };
    set_outcome(state, &rec, status, reason, None, None).await.map_err(AuthError::Internal)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nins_are_eleven_digits() {
        assert!(is_valid_nin("12345678901"));
        for bad in ["", "1234567890", "123456789012", "1234567890a", "1234 567890"] {
            assert!(!is_valid_nin(bad), "{bad}");
        }
    }

    #[test]
    fn nins_round_trip_encrypted_and_never_in_the_clear() {
        let key = [9u8; 32];
        let sealed = encrypt_nin(&key, "12345678901").unwrap();
        assert!(!sealed.contains("12345678901"));
        assert_eq!(decrypt_nin(&key, &sealed).unwrap(), "12345678901");
        assert_ne!(sealed, encrypt_nin(&key, "12345678901").unwrap(), "a fresh nonce each time");
        assert!(decrypt_nin(&[1u8; 32], &sealed).is_err(), "the wrong key fails");
        assert!(decrypt_nin(&key, "garbage").is_err());
    }

    #[test]
    fn nins_are_masked_and_salted() {
        assert_eq!(mask_nin("12345678901"), "•••••••8901");
        assert_ne!(hash_nin("salt-one", "12345678901"), hash_nin("salt-two", "12345678901"));
        assert_eq!(hash_nin("s", "1").len(), 64);
    }
}
