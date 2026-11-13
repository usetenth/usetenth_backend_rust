use crate::app_state::ApplicationState;
use crate::services::telegram_service::TelegramService;
use uuid::Uuid;

// Best effort: a failed notice must never block or fail the request that triggered it.
pub fn tell_user(state: &ApplicationState, user_id: Uuid, text: String) {
    let state = state.clone();
    tokio::spawn(async move {
        let chat: Option<Option<i64>> = sqlx::query_scalar("SELECT telegram_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
        if let Some(Some(chat_id)) = chat {
            let _ = TelegramService::send_message(&state, chat_id, &text, &[]).await;
        }
    });
}
