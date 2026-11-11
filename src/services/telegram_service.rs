use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

/// An inline keyboard button.
#[derive(Clone, Debug)]
pub enum Button {
    /// Opens the web app.
    WebApp { text: String, url: String },
    /// Sends `data` back to the bot as a callback query (max 64 bytes).
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
    /// Sends an HTML message. One button per row so long labels stay readable.
    /// Text is HTML: callers must escape anything that is not their own markup.
    pub async fn send_message(
        http: &reqwest::Client,
        bot_token: &str,
        chat_id: i64,
        text: &str,
        buttons: &[Button],
    ) -> Result<()> {
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
        Self::call(http, bot_token, "sendMessage", &body).await
    }

    /// Stops the loading spinner on a pressed inline button.
    pub async fn answer_callback_query(
        http: &reqwest::Client,
        bot_token: &str,
        callback_query_id: &str,
    ) -> Result<()> {
        let body = json!({ "callback_query_id": callback_query_id });
        Self::call(http, bot_token, "answerCallbackQuery", &body).await
    }

    async fn call(http: &reqwest::Client, bot_token: &str, method: &str, body: &Value) -> Result<()> {
        // The bot token is part of the URL, so strip the URL from any reqwest error.
        let response = http
            .post(format!("https://api.telegram.org/bot{bot_token}/{method}"))
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
