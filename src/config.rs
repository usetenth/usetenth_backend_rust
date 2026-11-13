#[derive(Clone)]
pub struct NgrokConfig {
    pub domain: String,
    pub auth_token: String,
}

#[derive(Clone)]
pub struct TurnkeyConfig {
    pub base_url: String,
    pub organization_id: String,
    pub secret_id: String,
    pub api_public_key: String,
    pub api_private_key: String,
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
    pub telegram_api_base: String,
    // Public https URL of the web app, used for Telegram web-app buttons.
    pub web_app_url: Option<String>,
    pub true_market_base_url: String,
    // Only used (and required) in development mode.
    pub true_market_key_file_path: Option<String>,
    // Only used (and required) in production mode.
    pub turnkey: Option<TurnkeyConfig>,
    pub true_market_env_mode: AppMode,
    pub true_market_organization_id: String,
    // HS256 secret for mobile access tokens (at least 32 bytes).
    pub jwt_secret: String,
    pub access_token_ttl_secs: i64,
    pub refresh_token_ttl_secs: i64,
    pub engine: EngineConfig,
    pub kyc: KycConfig,
    // Guards /admin. Unset disables the admin API entirely.
    pub admin_api_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub enabled: bool,
    // When true the engine plans orders but never calls the gateway to place or sign them.
    pub dry_run: bool,
    pub poll_secs: u64,
    // Symbols of the stablecoins whose incoming transfers count as deposits.
    pub settlement_symbols: Vec<String>,
    // Investments and order legs below this are not worth placing.
    pub global_floor_micros: i64,
    pub max_order_micros: i64,
    pub max_user_daily_micros: Option<i64>,
    // Gateway execute `auth_type`; no default, and live trading refuses to start without it.
    pub execute_auth_type: Option<String>,
    // How long a newly saved withdrawal address must wait before money can be sent to it.
    pub withdraw_address_cooldown_secs: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KycMode {
    // Approves everyone instantly. Development only.
    Auto,
    Vendor,
}

#[derive(Clone)]
pub struct KycConfig {
    pub mode: KycMode,
    pub vendors: Vec<crate::integrations::kyc_vendor::VendorConfig>,
    pub nin_hash_salt: String,
    pub nin_encryption_key: [u8; 32],
    pub max_lookups_per_user_per_day: i64,
    pub max_lookups_per_nin_per_day: i64,
    pub max_attempts: i32,
    pub retry_delay_secs: i64,
}

impl KycConfig {
    fn from_env(app_mode: &AppMode) -> anyhow::Result<Self> {
        use crate::integrations::kyc_vendor::VendorConfig;
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let number = |name: &str, default: i64| var(name).and_then(|v| v.parse().ok()).unwrap_or(default);
        let mode = match var("KYC_MODE").unwrap_or_else(|| "vendor".into()).to_lowercase().as_str() {
            "vendor" => KycMode::Vendor,
            "auto" => KycMode::Auto,
            other => anyhow::bail!("KYC_MODE must be 'vendor' or 'auto', got '{other}'"),
        };
        anyhow::ensure!(
            mode == KycMode::Vendor || *app_mode == AppMode::Development,
            "KYC_MODE=auto approves everyone and is only allowed with APP_MODE=development"
        );

        let mut config = Self {
            mode,
            vendors: vec![],
            nin_hash_salt: var("NIN_HASH_SALT").unwrap_or_default(),
            nin_encryption_key: [0u8; 32],
            max_lookups_per_user_per_day: number("KYC_MAX_LOOKUPS_PER_USER_PER_DAY", 3),
            max_lookups_per_nin_per_day: number("KYC_MAX_LOOKUPS_PER_NIN_PER_DAY", 3),
            max_attempts: number("KYC_VERIFY_ATTEMPTS", 5) as i32,
            retry_delay_secs: number("KYC_VERIFY_RETRY_DELAY_MS", 30_000) / 1000,
        };
        if mode == KycMode::Auto {
            return Ok(config);
        }

        for name in var("KYC_VENDORS").unwrap_or_else(|| "prembly".into()).split(',').map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()) {
            config.vendors.push(match name.as_str() {
                "prembly" => VendorConfig::Prembly {
                    base_url: var("PREMBLY_BASE_URL").unwrap_or_else(|| "https://api.prembly.com".into()),
                    api_key: var("PREMBLY_API_KEY").ok_or_else(|| anyhow::anyhow!("PREMBLY_API_KEY must be set for KYC_VENDORS=prembly"))?,
                    nin_path: var("PREMBLY_NIN_PATH"),
                },
                "dojah" => VendorConfig::Dojah {
                    base_url: var("DOJAH_BASE_URL").unwrap_or_else(|| "https://api.dojah.io".into()),
                    app_id: var("DOJAH_APP_ID").ok_or_else(|| anyhow::anyhow!("DOJAH_APP_ID must be set for KYC_VENDORS=dojah"))?,
                    secret_key: var("DOJAH_SECRET_KEY").ok_or_else(|| anyhow::anyhow!("DOJAH_SECRET_KEY must be set for KYC_VENDORS=dojah"))?,
                    nin_path: var("DOJAH_NIN_PATH"),
                },
                other => anyhow::bail!("Unsupported KYC vendor: {other}"),
            });
        }
        anyhow::ensure!(!config.vendors.is_empty(), "KYC_VENDORS must name at least one vendor");
        anyhow::ensure!(config.nin_hash_salt.len() >= 16, "NIN_HASH_SALT must be at least 16 characters");
        let key = hex::decode(var("NIN_ENCRYPTION_KEY").unwrap_or_default()).unwrap_or_default();
        config.nin_encryption_key = key.try_into().map_err(|_| anyhow::anyhow!("NIN_ENCRYPTION_KEY must be 32 bytes (64 hex characters)"))?;
        Ok(config)
    }
}

fn usd_env(name: &str, default: &str) -> anyhow::Result<i64> {
    let raw = std::env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.into());
    crate::signing::policy::parse_usd_micros(raw.trim())
        .map_err(|e| anyhow::anyhow!("{name} is not a valid USD amount: {e}"))
}

impl EngineConfig {
    fn from_env() -> anyhow::Result<Self> {
        let flag = |name: &str, default: bool| {
            std::env::var(name).ok().map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes")).unwrap_or(default)
        };
        let config = Self {
            enabled: flag("ENGINE_ENABLED", true),
            // Safe by default: nothing trades until DRY_RUN=false is set deliberately.
            dry_run: flag("DRY_RUN", true),
            poll_secs: std::env::var("ENGINE_POLL_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(30),
            settlement_symbols: std::env::var("TM_SETTLEMENT_ASSET")
                .unwrap_or_else(|_| "USDC".into())
                .split(',')
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty())
                .collect(),
            global_floor_micros: usd_env("GLOBAL_FLOOR_USD", "1")?,
            max_order_micros: usd_env("MAX_ORDER_USD", "50")?,
            max_user_daily_micros: match std::env::var("MAX_USER_SPEND_USD").ok().filter(|v| !v.trim().is_empty()) {
                Some(_) => Some(usd_env("MAX_USER_SPEND_USD", "0")?),
                None => None,
            },
            execute_auth_type: std::env::var("TM_EXECUTE_AUTH_TYPE").ok().filter(|v| !v.trim().is_empty()),
            withdraw_address_cooldown_secs: std::env::var("WITHDRAW_ADDRESS_COOLDOWN_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(86_400),
        };
        anyhow::ensure!(!config.settlement_symbols.is_empty(), "TM_SETTLEMENT_ASSET must name at least one symbol");
        anyhow::ensure!(
            config.dry_run || !config.enabled || config.execute_auth_type.is_some(),
            "DRY_RUN=false requires TM_EXECUTE_AUTH_TYPE (the gateway's execute auth type)"
        );
        Ok(config)
    }
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let app_mode: AppMode = std::env::var("APP_MODE")
            .map_err(|_| anyhow::anyhow!("APP_MODE must be set"))?
            .parse()?;
        Ok(Self {
            db_url: std::env::var("POSTGRESQL_DB_URL")
                .map_err(|_| anyhow::anyhow!("Database URL must be set"))?,
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN")
                .map_err(|_| anyhow::anyhow!("Telegram bot token must be provided"))?,
            telegram_webhook_secret: std::env::var("TELEGRAM_WEBHOOK_SECRET")
                .map_err(|_| anyhow::anyhow!("Telegram webhook secret must be provided"))?,
            telegram_api_base: std::env::var("TELEGRAM_API_BASE")
                .ok()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| "https://api.telegram.org".into())
                .trim_end_matches('/')
                .to_string(),
            web_app_url: std::env::var("WEB_APP_URL").ok().filter(|u| !u.is_empty()),
            true_market_base_url: std::env::var("TRUE_MARKET_BASE_URL")
                .map_err(|_| anyhow::anyhow!("True markets api base url must be provided"))?,
            true_market_key_file_path: std::env::var("TM_KEY_FILE").ok(),
            turnkey: match (
                std::env::var("TURNKEY_ORGANIZATION_ID"),
                std::env::var("TURNKEY_SECRET_ID"),
                std::env::var("TURNKEY_API_PUBLIC_KEY"),
                std::env::var("TURNKEY_API_PRIVATE_KEY"),
            ) {
                (Ok(organization_id), Ok(secret_id), Ok(api_public_key), Ok(api_private_key)) => {
                    Some(TurnkeyConfig {
                        base_url: std::env::var("TURNKEY_BASE_URL")
                            .unwrap_or_else(|_| "https://api.turnkey.com".into()),
                        organization_id,
                        secret_id,
                        api_public_key,
                        api_private_key,
                    })
                }
                _ => None,
            },
            true_market_env_mode: match std::env::var("TM_ENV") {
                Ok(mode) => mode.parse()?,
                Err(_) => std::env::var("APP_MODE")
                    .map_err(|_| anyhow::anyhow!("APP_MODE must be set"))?
                    .parse()?,
            },
            true_market_organization_id: std::env::var("TM_ORGANIZATION_ID")
                .map_err(|_| anyhow::anyhow!("TM_ORGANIZATION_ID must be provided"))?,
            engine: EngineConfig::from_env()?,
            kyc: KycConfig::from_env(&app_mode)?,
            admin_api_key: std::env::var("ADMIN_API_KEY").ok().filter(|k| k.len() >= 16),
            jwt_secret: {
                let secret = std::env::var("JWT_SECRET")
                    .map_err(|_| anyhow::anyhow!("JWT_SECRET must be provided"))?;
                anyhow::ensure!(secret.len() >= 32, "JWT_SECRET must be at least 32 bytes");
                secret
            },
            access_token_ttl_secs: std::env::var("ACCESS_TOKEN_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900),
            refresh_token_ttl_secs: std::env::var("REFRESH_TOKEN_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60 * 60 * 24 * 30),
            port: std::env::var("PORT").unwrap_or("3000".into()).parse()?,
            app_mode,
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

#[cfg(test)]
impl AppConfig {
    pub fn for_test(true_market_base_url: &str, engine: EngineConfig) -> Self {
        Self {
            db_url: String::new(),
            port: 0,
            ngrok: None,
            app_mode: AppMode::Development,
            telegram_bot_token: "test".into(),
            telegram_webhook_secret: "test".into(),
            telegram_api_base: "http://127.0.0.1:1".into(),
            web_app_url: None,
            true_market_base_url: true_market_base_url.into(),
            true_market_key_file_path: None,
            turnkey: None,
            true_market_env_mode: AppMode::Development,
            true_market_organization_id: "org".into(),
            jwt_secret: "test-secret-test-secret-test-secret-1234".into(),
            access_token_ttl_secs: 900,
            refresh_token_ttl_secs: 3600,
            engine,
            kyc: KycConfig::for_test("http://127.0.0.1:1"),
            admin_api_key: Some("test-admin-key-1234".into()),
        }
    }
}

#[cfg(test)]
impl EngineConfig {
    pub fn for_test(dry_run: bool) -> Self {
        Self {
            enabled: true,
            dry_run,
            poll_secs: 1,
            settlement_symbols: vec!["USDC".into()],
            global_floor_micros: 1_000_000,
            max_order_micros: 50_000_000,
            max_user_daily_micros: None,
            execute_auth_type: Some("test_auth".into()),
            withdraw_address_cooldown_secs: 86_400,
        }
    }
}

#[cfg(test)]
impl KycConfig {
    pub fn for_test(vendor_base_url: &str) -> Self {
        Self {
            mode: KycMode::Vendor,
            vendors: vec![crate::integrations::kyc_vendor::VendorConfig::Prembly {
                base_url: vendor_base_url.into(),
                api_key: "test-key".into(),
                nin_path: None,
            }],
            nin_hash_salt: "test-salt-test-salt".into(),
            nin_encryption_key: [7u8; 32],
            max_lookups_per_user_per_day: 3,
            max_lookups_per_nin_per_day: 3,
            max_attempts: 3,
            retry_delay_secs: 30,
        }
    }
}
