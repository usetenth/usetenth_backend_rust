use crate::app_state::ApplicationState;
use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub enum Button {
    // Opens the web app.
    WebApp { text: String, url: String },
    // Sends `data` back to the bot as a callback query (max 64 bytes).
    Callback { text: String, data: String },
}

impl Button {
    pub fn callback(text: &str, data: &str) -> Self {
        Self::Callback { text: text.into(), data: data.into() }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::WebApp { text, url } => json!({ "text": text, "web_app": { "url": url } }),
            Self::Callback { text, data } => json!({ "text": text, "callback_data": data }),
        }
    }
}

#[derive(Deserialize)]
struct TelegramReply {
    ok: bool,
    description: Option<String>,
}

pub struct TelegramService;

impl TelegramService {
    // Sends an HTML message, one button per row; callers must escape text that isn't their own markup.
    pub async fn send_message(state: &ApplicationState, chat_id: i64, text: &str, buttons: &[Button]) -> Result<()> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
            "link_preview_options": { "is_disabled": true },
        });
        if !buttons.is_empty() {
            let rows: Vec<Value> = buttons.iter().map(|b| json!([b.to_json()])).collect();
            body["reply_markup"] = json!({ "inline_keyboard": rows });
        }
        Self::call(state, "sendMessage", &body).await
    }

    // Stops the loading spinner on a pressed inline button.
    pub async fn answer_callback_query(state: &ApplicationState, callback_query_id: &str) -> Result<()> {
        let body = json!({ "callback_query_id": callback_query_id });
        Self::call(state, "answerCallbackQuery", &body).await
    }

    async fn call(state: &ApplicationState, method: &str, body: &Value) -> Result<()> {
        // The bot token is part of the URL, so strip the URL from any reqwest error.
        let url = format!("{}/bot{}/{method}", state.config.telegram_api_base, state.config.telegram_bot_token);
        let response = state
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| e.without_url())?;
        let status = response.status();
        let reply = response.json::<TelegramReply>().await.ok();
        match reply {
            Some(r) if r.ok => Ok(()),
            Some(r) => bail!("telegram {method} failed: {}", r.description.unwrap_or_default()),
            None => bail!("telegram {method} failed: HTTP {status}"),
        }
    }
}
