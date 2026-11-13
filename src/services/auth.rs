
use crate::app_state::ApplicationState;
use crate::services::onboarding;
use anyhow::{Context, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const MIN_PASSWORD_LEN: usize = 10;
const LOGIN_CODE_TTL_SECS: i64 = 300;
// No 0/O/1/I/L, so a code read off a phone screen is hard to mistype.
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const CODE_LEN: usize = 8;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("{0}")]
    BadRequest(String),
    #[error("Invalid email or password")]
    InvalidCredentials,
    #[error("Unauthorized")]
    Unauthorized,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("Too many attempts. Try again later.")]
    TooManyRequests,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for AuthError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

#[derive(Debug, Serialize)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: Uuid,
    iat: i64,
    exp: i64,
}

pub(crate) fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub fn normalize_email(email: &str) -> Result<String, AuthError> {
    let email = email.trim().to_lowercase();
    let valid = email.len() <= 254
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && domain.contains('.')
                && !email.contains(char::is_whitespace)
        });
    if valid { Ok(email) } else { Err(AuthError::BadRequest("Enter a valid email address".into())) }
}

fn check_password(password: &str) -> Result<(), AuthError> {
    // Argon2 has no practical limit, but bound the work an unauthenticated caller can request.
    if password.chars().count() < MIN_PASSWORD_LEN || password.len() > 256 {
        return Err(AuthError::BadRequest(format!(
            "Password must be {MIN_PASSWORD_LEN} to 256 characters"
        )));
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AuthError::Internal(anyhow!("password hashing failed: {e}")))
}

fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

// Password hashing is deliberately slow, so it must not run on the async executor.
async fn hash_blocking(password: String) -> Result<String, AuthError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AuthError::Internal(e.into()))?
}

async fn verify_blocking(password: String, stored: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &stored))
        .await
        .unwrap_or(false)
}


fn issue_access_token(state: &ApplicationState, user_id: Uuid) -> Result<String, AuthError> {
    let now = Utc::now().timestamp();
    let claims = Claims { sub: user_id, iat: now, exp: now + state.config.access_token_ttl_secs };
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(state.config.jwt_secret.as_bytes()),
    )
    .context("could not sign access token")
    .map_err(AuthError::Internal)
}

pub fn verify_access_token(state: &ApplicationState, token: &str) -> Result<Uuid, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 5;
    jsonwebtoken::decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.config.jwt_secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims.sub)
    .map_err(|_| AuthError::Unauthorized)
}

async fn issue_refresh_token(
    state: &ApplicationState,
    user_id: Uuid,
    family_id: Uuid,
) -> Result<String, AuthError> {
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token = URL_SAFE_NO_PAD.encode(raw);
    sqlx::query(
        "INSERT INTO auth_refresh_tokens (user_id, family_id, token_hash, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(family_id)
    .bind(sha256_hex(&token))
    .bind(Utc::now() + Duration::seconds(state.config.refresh_token_ttl_secs))
    .execute(&state.pool)
    .await?;
    Ok(token)
}

async fn start_session(state: &ApplicationState, user_id: Uuid) -> Result<TokenPair, AuthError> {
    token_pair(state, user_id, Uuid::new_v4()).await
}

async fn token_pair(state: &ApplicationState, user_id: Uuid, family_id: Uuid) -> Result<TokenPair, AuthError> {
    Ok(TokenPair {
        access_token: issue_access_token(state, user_id)?,
        refresh_token: issue_refresh_token(state, user_id, family_id).await?,
        token_type: "Bearer",
        expires_in: state.config.access_token_ttl_secs,
    })
}

/// Rotates a refresh token; replaying a used one revokes its whole family.
pub async fn refresh(state: &ApplicationState, presented: &str) -> Result<TokenPair, AuthError> {
    let hash = sha256_hex(presented);
    let claimed: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE auth_refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()
         RETURNING user_id, family_id",
    )
    .bind(&hash)
    .fetch_optional(&state.pool)
    .await?;

    let Some((user_id, family_id)) = claimed else {
        sqlx::query(
            "UPDATE auth_refresh_tokens SET revoked_at = now()
             WHERE revoked_at IS NULL
               AND family_id = (SELECT family_id FROM auth_refresh_tokens WHERE token_hash = $1)",
        )
        .bind(&hash)
        .execute(&state.pool)
        .await?;
        return Err(AuthError::Unauthorized);
    };
    token_pair(state, user_id, family_id).await
}

pub async fn logout(state: &ApplicationState, presented: &str) -> Result<(), AuthError> {
    sqlx::query(
        "UPDATE auth_refresh_tokens SET revoked_at = now()
         WHERE revoked_at IS NULL
           AND family_id = (SELECT family_id FROM auth_refresh_tokens WHERE token_hash = $1)",
    )
    .bind(sha256_hex(presented))
    .execute(&state.pool)
    .await?;
    Ok(())
}


pub async fn register(state: &ApplicationState, email: &str, password: &str) -> Result<TokenPair, AuthError> {
    let email = normalize_email(email)?;
    check_password(password)?;
    let hash = hash_blocking(password.to_string()).await?;

    let inserted: Result<Uuid, sqlx::Error> =
        sqlx::query_scalar("INSERT INTO users (email, password) VALUES ($1, $2) RETURNING id")
            .bind(&email)
            .bind(&hash)
            .fetch_one(&state.pool)
            .await;
    match inserted {
        Ok(user_id) => start_session(state, user_id).await,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(AuthError::Conflict("An account with this email already exists".into()))
        }
        Err(e) => Err(e.into()),
    }
}

// Verified against a dummy hash for unknown emails so response time does not reveal them.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$kWw0vKq3iQqEw0zQ2cH7bX0gQdXo7Lr2uQmH3S1VYbA";

pub async fn login(state: &ApplicationState, email: &str, password: &str) -> Result<TokenPair, AuthError> {
    let email = normalize_email(email).map_err(|_| AuthError::InvalidCredentials)?;
    let row: Option<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, password FROM users WHERE lower(email) = $1")
            .bind(&email)
            .fetch_optional(&state.pool)
            .await?;

    let (user_id, stored) = match row {
        Some((id, Some(hash))) => (Some(id), hash),
        _ => (None, DUMMY_HASH.to_string()),
    };
    let ok = verify_blocking(password.to_string(), stored).await;
    match (user_id, ok) {
        (Some(id), true) => start_session(state, id).await,
        _ => Err(AuthError::InvalidCredentials),
    }
}

pub async fn set_credentials(
    state: &ApplicationState,
    user_id: Uuid,
    email: &str,
    password: &str,
) -> Result<(), AuthError> {
    let email = normalize_email(email)?;
    check_password(password)?;
    let hash = hash_blocking(password.to_string()).await?;
    let result = sqlx::query(
        "UPDATE users SET email = $2, password = $3, updated_at = now()
         WHERE id = $1 AND email IS NULL",
    )
    .bind(user_id)
    .bind(&email)
    .bind(&hash)
    .execute(&state.pool)
    .await;
    match result {
        Ok(r) if r.rows_affected() == 1 => Ok(()),
        Ok(_) => Err(AuthError::Conflict("This account already has an email".into())),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(AuthError::Conflict("An account with this email already exists".into()))
        }
        Err(e) => Err(e.into()),
    }
}

/// Re-checks the password before something irreversible, throttling wrong guesses per user.
pub async fn confirm_password(state: &ApplicationState, user_id: Uuid, password: &str) -> Result<(), AuthError> {
    let window = std::time::Duration::from_secs(15 * 60);
    let key = format!("reauth:{user_id}");
    if state.limiter.is_blocked(&key, 5, window) {
        return Err(AuthError::TooManyRequests);
    }
    let stored: Option<Option<String>> = sqlx::query_scalar("SELECT password FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await?;
    let Some(stored) = stored else { return Err(AuthError::Unauthorized) };
    let Some(stored) = stored else {
        return Err(AuthError::Conflict("Set an email and password before withdrawing".into()));
    };
    if verify_blocking(password.to_string(), stored).await {
        Ok(())
    } else {
        state.limiter.record(&key, window);
        Err(AuthError::BadRequest("Incorrect password".into()))
    }
}


pub(crate) fn random_code(len: usize) -> String {
    let mut rng = rand::rngs::OsRng;
    (0..len).map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char).collect()
}

pub async fn issue_telegram_code(
    state: &ApplicationState,
    telegram_id: i64,
    username: Option<&str>,
) -> anyhow::Result<String> {
    let code = random_code(CODE_LEN);
    // A user only ever has one live code.
    sqlx::query("UPDATE telegram_login_codes SET used_at = now() WHERE telegram_id = $1 AND used_at IS NULL")
        .bind(telegram_id)
        .execute(&state.pool)
        .await?;
    sqlx::query("INSERT INTO telegram_login_codes (telegram_id, username, code_hash, expires_at) VALUES ($1, $2, $3, $4)")
        .bind(telegram_id)
        .bind(username)
        .bind(sha256_hex(&code))
        .bind(Utc::now() + Duration::seconds(LOGIN_CODE_TTL_SECS))
        .execute(&state.pool)
        .await?;
    Ok(code)
}

pub(crate) fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase()
}

async fn consume_code(state: &ApplicationState, code: &str) -> Result<(i64, Option<String>), AuthError> {
    sqlx::query_as(
        "UPDATE telegram_login_codes SET used_at = now()
         WHERE code_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING telegram_id, username",
    )
    .bind(sha256_hex(&normalize_code(code)))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AuthError::BadRequest("That code is invalid or has expired".into()))
}

async fn telegram_session(state: &ApplicationState, telegram_id: i64, username: Option<&str>) -> Result<TokenPair, AuthError> {
    let user = onboarding::ensure_user(state, telegram_id, username).await.map_err(AuthError::Internal)?;
    start_session(state, user.user_id).await
}

pub async fn telegram_login(state: &ApplicationState, code: &str) -> Result<TokenPair, AuthError> {
    let (telegram_id, username) = consume_code(state, code).await?;
    telegram_session(state, telegram_id, username.as_deref()).await
}

/// Attaches the Telegram account behind the code to this account; a Telegram account that only ever said /start is absorbed.
pub async fn telegram_link(state: &ApplicationState, user_id: Uuid, code: &str) -> Result<(), AuthError> {
    let (telegram_id, username) = consume_code(state, code).await?;
    let mut tx = state.pool.begin().await?;

    let current: Option<Option<i64>> = sqlx::query_scalar("SELECT telegram_id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    match current {
        None => return Err(AuthError::Unauthorized),
        Some(Some(_)) => return Err(AuthError::Conflict("This account is already linked to Telegram".into())),
        Some(None) => {}
    }

    // Unused means no gateway account, email, wallet or signer key, so nothing of value can be lost.
    let other: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT id, (gateway_user_id IS NULL AND email IS NULL
                     AND NOT EXISTS (SELECT 1 FROM wallets w WHERE w.user_id = users.id)
                     AND NOT EXISTS (SELECT 1 FROM signer_keys k WHERE k.user_id = users.id))
         FROM users WHERE telegram_id = $1 FOR UPDATE",
    )
    .bind(telegram_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((other_id, unused)) = other {
        if !unused {
            return Err(AuthError::Conflict(
                "That Telegram account already has its own account. Sign in with Telegram instead.".into(),
            ));
        }
        // Their tenth choice moves over unless this account already picked one.
        sqlx::query("UPDATE rules SET user_id = $1 WHERE user_id = $2 AND NOT EXISTS (SELECT 1 FROM rules WHERE user_id = $1)")
            .bind(user_id)
            .bind(other_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(other_id).execute(&mut *tx).await?;
    }

    let result = sqlx::query("UPDATE users SET telegram_id = $2, username = COALESCE(username, $3), updated_at = now() WHERE id = $1")
        .bind(user_id)
        .bind(telegram_id)
        .bind(&username)
        .execute(&mut *tx)
        .await;
    match result {
        Ok(_) => Ok(tx.commit().await?),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(AuthError::Conflict("That Telegram account already has its own account".into()))
        }
        Err(e) => Err(e.into()),
    }
}

// ---- Mini App ----

#[derive(Debug, PartialEq, Eq)]
pub struct WebAppUser {
    pub id: i64,
    pub username: Option<String>,
}

const WEBAPP_MAX_AGE_SECS: i64 = 3600;

/// Checks the signed launch data Telegram hands a Mini App (HMAC keyed from the bot token) and returns who opened it.
pub fn verify_init_data(init_data: &str, bot_token: &str, now: i64) -> Result<WebAppUser, AuthError> {
    use hmac::{Hmac, Mac};
    let mut fields: Vec<(String, String)> = url::form_urlencoded::parse(init_data.as_bytes()).into_owned().collect();
    let hash = fields.iter().find(|(k, _)| k == "hash").map(|(_, v)| v.clone()).ok_or(AuthError::Unauthorized)?;
    fields.retain(|(k, _)| k != "hash");
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    let check_string = fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");

    let mut key = Hmac::<Sha256>::new_from_slice(b"WebAppData").map_err(|_| AuthError::Unauthorized)?;
    key.update(bot_token.as_bytes());
    let mut mac = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes()).map_err(|_| AuthError::Unauthorized)?;
    mac.update(check_string.as_bytes());
    mac.verify_slice(&hex::decode(hash).map_err(|_| AuthError::Unauthorized)?).map_err(|_| AuthError::Unauthorized)?;

    let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let auth_date: i64 = field("auth_date").and_then(|v| v.parse().ok()).ok_or(AuthError::Unauthorized)?;
    if auth_date > now + 60 || now - auth_date > WEBAPP_MAX_AGE_SECS {
        return Err(AuthError::Unauthorized);
    }
    let user: serde_json::Value = serde_json::from_str(field("user").ok_or(AuthError::Unauthorized)?).map_err(|_| AuthError::Unauthorized)?;
    Ok(WebAppUser {
        id: user["id"].as_i64().ok_or(AuthError::Unauthorized)?,
        username: user["username"].as_str().map(str::to_string),
    })
}

/// Signs in the Telegram user who opened the Mini App, creating their account on first launch.
pub async fn telegram_webapp_login(state: &ApplicationState, init_data: &str) -> Result<TokenPair, AuthError> {
    let user = verify_init_data(init_data, &state.config.telegram_bot_token, Utc::now().timestamp())?;
    telegram_session(state, user.id, user.username.as_deref()).await
}

/// Builds launch data signed the way Telegram does, for tests.
#[cfg(test)]
pub fn sign_init_data(bot_token: &str, auth_date: i64, user: &serde_json::Value) -> String {
    use hmac::{Hmac, Mac};
    let mut fields = vec![
        ("auth_date".to_string(), auth_date.to_string()),
        ("query_id".to_string(), "AAH-test".to_string()),
        ("user".to_string(), user.to_string()),
    ];
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    let check = fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n");
    let mut key = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
    key.update(bot_token.as_bytes());
    let mut mac = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes()).unwrap();
    mac.update(check.as_bytes());
    fields.push(("hash".into(), hex::encode(mac.finalize().into_bytes())));
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish()
}

#[cfg(test)]
pub async fn access_token_for_tests(state: &ApplicationState, user_id: Uuid) -> String {
    issue_access_token(state, user_id).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_are_normalized_and_validated() {
        assert_eq!(normalize_email("  Ada@Example.COM ").unwrap(), "ada@example.com");
        for bad in ["", "ada", "ada@", "@example.com", "ada@example", "a b@example.com", "ada@.com"] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn short_passwords_are_rejected() {
        assert!(check_password("short").is_err());
        assert!(check_password("long enough pw").is_ok());
        assert!(check_password(&"x".repeat(257)).is_err());
    }

    #[test]
    fn password_round_trip() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("anything", "not a hash"));
    }

    #[test]
    fn launch_data_must_be_signed_by_this_bot_and_fresh() {
        let user = serde_json::json!({ "id": 42, "username": "ada" });
        let now = 1_800_000_000;
        let signed = sign_init_data("bot-token", now - 10, &user);
        assert_eq!(verify_init_data(&signed, "bot-token", now).unwrap(), WebAppUser { id: 42, username: Some("ada".into()) });
        assert!(verify_init_data(&signed, "other-bot", now).is_err());
        assert!(verify_init_data(&signed.replace("42", "43"), "bot-token", now).is_err());
        assert!(verify_init_data(&signed, "bot-token", now + 7200).is_err(), "expired");
        assert!(verify_init_data(&sign_init_data("bot-token", now + 3600, &user), "bot-token", now).is_err(), "from the future");
        assert!(verify_init_data("auth_date=1&user=%7B%7D", "bot-token", now).is_err(), "no hash");
        assert!(verify_init_data("", "bot-token", now).is_err());
    }

    #[test]
    fn dummy_hash_parses() {
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
    }

    #[test]
    fn codes_ignore_case_and_separators() {
        assert_eq!(normalize_code("ab3d-ef4g"), "AB3DEF4G");
    }
}
