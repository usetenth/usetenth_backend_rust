use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use axum::http::HeaderMap;
use chrono::Utc;
use serde::Deserialize;
use sqlx::query_as;
use crate::app_state::ApplicationState;
use crate::models::schemas::TelegramSession;
use crate::services::response::{ApiResponse};

#[derive(Deserialize, Clone)]
pub struct User {
    pub id: String,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub is_bot: bool,
}

#[derive(Deserialize, Clone)]
pub struct Chat {
    pub id: String,
}

#[derive(Deserialize, Clone)]
pub struct Message {
    pub message_id: String,
    pub from: User,
    pub date: String,
    pub chat: Chat,
    pub text: String,
}

#[derive(Deserialize, Clone)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub data: String
}
#[derive(Deserialize, Clone)]
pub struct WebhookUpdate {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

pub async fn webhook_handler(State(state): State<ApplicationState>) {}
pub async fn get_webhook_updates(
    State(state): State<ApplicationState>,
    headers: HeaderMap,
    Json(payload): Json<WebhookUpdate>,
) -> (StatusCode, Json<Response>) {
    let existing_session = query_as::<_, TelegramSession>("SELECT update_id FROM telegram_sessions WHERE update_id = $1")
        .bind(payload.update_id)
        .fetch_one(&state.pool)
        .await?;
    ApiResponse::ok("Success", serde_json::json!({}));

    let expiry = Utc::now() + chrono::Duration::hours(1);
    match (payload.message, payload.callback_query ){
        Some((message, callback_query)) => {
            
        },
        None => {}
    }
}

pub async fn telegram_auth_middleware(
    State(state): State<ApplicationState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let bot_token = req
        .headers()
        .get("x-telegram-bot-api-secret-token")
        .and_then(|s| s.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if bot_token == state.config.telegram_webhook_secret.as_str() {
        let res = next.run(req).await;
        Ok(res)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
