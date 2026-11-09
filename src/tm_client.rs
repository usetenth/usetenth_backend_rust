use crate::config::AppConfig;
use anyhow::{Context, Result};
use base64::Engine;
use chrono::{DateTime, Utc};
use p256::ecdsa::SigningKey;
use serde::Deserialize;

struct TrueMarketClient {
    base_url: String,
    retry_duration: u16,
    key_path: String,
}

#[derive(Clone, Deserialize)]
struct PrivateKey {
    kty: String,
    crv: String,
    x: String,
    y: String,
    d: String,
}

#[derive(Clone, Deserialize)]
struct JWTToken {
    token: String,
}

#[derive(Clone, Deserialize)]
struct TrueMarketApiKey {
    key_id: String,
    label: String,
    fingerprint: String,
    created_at: DateTime<Utc>,
    private_key: PrivateKey,
}

impl TrueMarketClient {
    pub fn new(config: AppConfig, retry_duration: u16) -> Self {
        Self {
            base_url: config.true_market_base_url,
            retry_duration,
            key_path: config.true_market_key_file_path,
        }
    }
    pub fn fetch_api_key(&self) -> Result<TrueMarketApiKey, String> {
        // Use key from file system. Practically it will be stored in a KMS and fetched
        let api_key_as_str =
            std::fs::read_to_string(&self.key_path).expect("No api key found in api_key");
        Ok(serde_json::from_str(&api_key_as_str).unwrap())
    }

    pub fn generate_signing_key(api_key: TrueMarketApiKey) -> Result<SigningKey> {
        let d = api_key.private_key.d;

        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(d)
            .context("Failed to decode private key")?;

        let bytes: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("Private key must be exactly 32 bytes"))?;

        SigningKey::from_bytes((&bytes).into())
            .map_err(|_| anyhow::anyhow!("Invalid ECDSA signing key"))
    }

    pub async fn get_market(&self, symbol: &str) {}
    pub async fn post<T>(data: T) {}
    pub async fn stamp<T>() {}
    pub async fn execute<T>() {}
    pub async fn create_order<T>() {}
    pub async fn get_asset_list() {}
    pub async fn mint_jwt_token() -> Result<JWTToken, String> {}
    pub async fn refresh_jwt_token() {}
    pub async fn get_user_balance(user_id: &str) {}
    pub async fn deposit() {}
    pub async fn withdraw() {}
    pub async fn create_user() {}
}
