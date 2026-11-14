use crate::app_state::ApplicationState;
use crate::services::telegram_service::{Button, TelegramService};
use uuid::Uuid;

// Best effort: a failed notice must never block or fail the request that triggered it.
pub fn tell_user(state: &ApplicationState, user_id: Uuid, text: String) {
    tell_user_all(state, user_id, vec![(text, vec![])]);
}

/// Sends several messages in order, each with its own buttons.
pub fn tell_user_all(state: &ApplicationState, user_id: Uuid, messages: Vec<(String, Vec<Button>)>) {
    let state = state.clone();
    tokio::spawn(async move {
        let chat: Option<Option<i64>> = sqlx::query_scalar("SELECT telegram_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
        if let Some(Some(chat_id)) = chat {
            for (text, buttons) in messages {
                if TelegramService::send_message(&state, chat_id, &text, &buttons).await.is_err() {
                    break;
                }
            }
        }
    });
}

/// Adds one entry to the user's notification list; the same thing is never listed twice.
pub async fn record(pool: &sqlx::PgPool, user_id: Uuid, kind: &str, source_id: Uuid, title: &str, body: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO notifications (user_id, kind, title, body, source_id) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (kind, source_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(kind)
    .bind(title)
    .bind(body)
    .bind(source_id)
    .execute(pool)
    .await?;
    Ok(())
}
