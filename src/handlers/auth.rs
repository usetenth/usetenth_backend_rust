use crate::app_state::ApplicationState;
use crate::services::auth::{self, AuthError, TokenPair};
use crate::services::response::ApiResponse;
use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AuthError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AuthError::InvalidCredentials => (StatusCode::UNAUTHORIZED, self.to_string()),
            AuthError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized".into()),
            AuthError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AuthError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            AuthError::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            AuthError::Internal(error) => {
                eprintln!("Auth request failed: {error:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error".into())
            }
        };
        (status, Json(ApiResponse::<()> { success: false, message, data: None })).into_response()
    }
}

pub type ApiResult<T> = Result<(StatusCode, Json<ApiResponse<T>>), AuthError>;

pub struct AuthUser(pub Uuid);

impl FromRequestParts<ApplicationState> for AuthUser {
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &ApplicationState) -> Result<Self, AuthError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AuthError::Unauthorized)?;
        auth::verify_access_token(state, token).map(AuthUser)
    }
}

#[derive(Deserialize)]
pub struct Credentials {
    email: String,
    password: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    refresh_token: String,
}

#[derive(Deserialize)]
pub struct CodeRequest {
    code: String,
}

const MINUTE: Duration = Duration::from_secs(60);
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);

pub async fn register(State(state): State<ApplicationState>, Json(body): Json<Credentials>) -> ApiResult<TokenPair> {
    if !state.limiter.allow("register", 60, MINUTE) {
        return Err(AuthError::TooManyRequests);
    }
    let tokens = auth::register(&state, &body.email, &body.password).await?;
    Ok(created("Account created", tokens))
}

pub async fn login(State(state): State<ApplicationState>, Json(body): Json<Credentials>) -> ApiResult<TokenPair> {
    // Only failures count, so a stranger cannot lock someone out by logging in as them successfully.
    let key = format!("login:{}", body.email.trim().to_lowercase());
    if state.limiter.is_blocked(&key, 10, LOGIN_WINDOW) {
        return Err(AuthError::TooManyRequests);
    }
    match auth::login(&state, &body.email, &body.password).await {
        Ok(tokens) => Ok(ApiResponse::ok("Signed in", tokens)),
        Err(error @ AuthError::InvalidCredentials) => {
            state.limiter.record(&key, LOGIN_WINDOW);
            Err(error)
        }
        Err(error) => Err(error),
    }
}

// Capping failed code guesses across everyone makes brute force impractical, at the cost of pausable sign-in.
async fn redeem<T>(state: &ApplicationState, attempt: impl std::future::Future<Output = Result<T, AuthError>>) -> Result<T, AuthError> {
    if state.limiter.is_blocked("code-failures", 30, MINUTE) {
        return Err(AuthError::TooManyRequests);
    }
    let result = attempt.await;
    if matches!(result, Err(AuthError::BadRequest(_))) {
        state.limiter.record("code-failures", MINUTE);
    }
    result
}

pub async fn refresh(State(state): State<ApplicationState>, Json(body): Json<RefreshRequest>) -> ApiResult<TokenPair> {
    Ok(ApiResponse::ok("Token refreshed", auth::refresh(&state, &body.refresh_token).await?))
}

pub async fn logout(State(state): State<ApplicationState>, Json(body): Json<RefreshRequest>) -> ApiResult<()> {
    auth::logout(&state, &body.refresh_token).await?;
    Ok(ApiResponse::ok("Signed out", ()))
}

pub async fn telegram_login(State(state): State<ApplicationState>, Json(body): Json<CodeRequest>) -> ApiResult<TokenPair> {
    Ok(ApiResponse::ok("Signed in", redeem(&state, auth::telegram_login(&state, &body.code)).await?))
}

#[derive(Deserialize)]
pub struct WebAppRequest {
    init_data: String,
}

pub async fn telegram_webapp(State(state): State<ApplicationState>, Json(body): Json<WebAppRequest>) -> ApiResult<TokenPair> {
    if state.limiter.is_blocked("webapp-failures", 60, MINUTE) {
        return Err(AuthError::TooManyRequests);
    }
    let result = auth::telegram_webapp_login(&state, &body.init_data).await;
    if matches!(result, Err(AuthError::Unauthorized)) {
        state.limiter.record("webapp-failures", MINUTE);
    }
    Ok(ApiResponse::ok("Signed in", result?))
}

pub async fn telegram_link(
    State(state): State<ApplicationState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<CodeRequest>,
) -> ApiResult<()> {
    redeem(&state, auth::telegram_link(&state, user_id, &body.code)).await?;
    Ok(ApiResponse::ok("Telegram linked", ()))
}

pub async fn set_credentials(
    State(state): State<ApplicationState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<Credentials>,
) -> ApiResult<()> {
    auth::set_credentials(&state, user_id, &body.email, &body.password).await?;
    Ok(ApiResponse::ok("Email and password saved", ()))
}

#[derive(Serialize)]
pub struct Me {
    id: Uuid,
    email: Option<String>,
    telegram_linked: bool,
}

pub async fn me(State(state): State<ApplicationState>, AuthUser(user_id): AuthUser) -> ApiResult<Me> {
    let row: Option<(Option<String>, bool)> =
        sqlx::query_as("SELECT email, telegram_id IS NOT NULL FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?;
    let (email, telegram_linked) = row.ok_or(AuthError::Unauthorized)?;
    Ok(ApiResponse::ok("Profile", Me { id: user_id, email, telegram_linked }))
}

pub fn created<T: Serialize>(message: &str, data: T) -> (StatusCode, Json<ApiResponse<T>>) {
    (
        StatusCode::CREATED,
        Json(ApiResponse { success: true, message: message.into(), data: Some(data) }),
    )
}
