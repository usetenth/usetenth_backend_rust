use crate::app_state::ApplicationState;
use crate::services::bot::{TelegramUpdate, handle_update};
use crate::services::response::ApiResponse;
use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};

// Telegram expects a fast 200, so the update is processed in the background.
pub async fn webhook_handler(
    State(state): State<ApplicationState>,
    Json(update): Json<TelegramUpdate>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    tokio::spawn(async move {
        if let Err(error) = handle_update(&state, update).await {
            eprintln!("Telegram update failed: {error:#}");
        }
    });
    (
        StatusCode::OK,
        Json(ApiResponse { success: true, message: "ok".into(), data: None }),
    )
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub async fn telegram_auth_middleware(
    State(state): State<ApplicationState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let secret = req
        .headers()
        .get("x-telegram-bot-api-secret-token")
        .and_then(|s| s.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if constant_time_eq(secret.as_bytes(), state.config.telegram_webhook_secret.as_bytes()) {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn secrets_match_only_when_every_byte_does() {
        assert!(constant_time_eq(b"s3cret", b"s3cret"));
        assert!(!constant_time_eq(b"s3cret", b"s3creT"));
        assert!(!constant_time_eq(b"s3cret", b"s3cret!"), "different lengths never match");
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
