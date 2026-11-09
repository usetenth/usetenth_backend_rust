#[derive(Clone)]
pub struct NgrokConfig {
    pub domain: String,
    pub auth_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppMode {
    Production,
    Development,
}

impl std::str::FromStr for AppMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "production" | "prod" => Ok(Self::Production),
            "development" | "dev" => Ok(Self::Development),
            other => Err(anyhow::anyhow!(
                "Invalid application mode provider: {other} expected 'production' or 'development'"
            )),
        }
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub db_url: String,
    pub port: usize,
    pub ngrok: Option<NgrokConfig>,
    pub app_mode: AppMode,
    pub telegram_bot_token: String,
    pub telegram_webhook_secret: String,
    pub true_market_base_url: String,
    pub true_market_key_file_path: String,
    pub true_market_env_mode: AppMode,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            db_url: std::env::var("POSTGRESQL_DB_URL")
                .map_err(|_| anyhow::anyhow!("Database URL must be set"))?,
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN")
                .map_err(|_| anyhow::anyhow!("Telegram bot token must be provided"))?,
            telegram_webhook_secret: std::env::var("TELEGRAM_WEBHOOK_SECRET")
                .map_err(|_| anyhow::anyhow!("Telegram webhook secret must be provided"))?,
            true_market_base_url: std::env::var("TRUE_MARKET_BASE_URL")
                .map_err(|_| anyhow::anyhow!("True markets api base url must be provided"))?,
            true_market_key_file_path: std::env::var("TM_KEY_FILE")
                .map_err(|_| anyhow::anyhow!("True markets api base url must be provided"))?,
            true_market_env_mode: std::env::var("TM_ENV").unwrap().parse()?,
            port: std::env::var("PORT").unwrap_or("3000".into()).parse()?,
            app_mode: std::env::var("APP_MODE").unwrap().parse()?,
            ngrok: match (
                std::env::var("NGROK_DOMAIN"),
                std::env::var("NGROK_AUTH_TOKEN"),
            ) {
                (Ok(domain), Ok(auth_token)) => Some(NgrokConfig { domain, auth_token }),
                _ => None,
            },
        })
    }
}
