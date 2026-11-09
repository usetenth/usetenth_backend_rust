use serde::{Deserialize, Serialize};

pub struct TelegramService;
#[derive(Clone, Debug, Serialize)]
pub struct WebAppButtonResponse {
    pub text: String,
    pub url: String,
}

impl TelegramService {
    pub fn cal() {}
    pub fn send_message() {}
    pub fn send_photo() {}
}
