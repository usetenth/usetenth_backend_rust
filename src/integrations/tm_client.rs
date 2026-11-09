use super::auth::{JWTToken, MintTokenRequest, TrueMarketApiKey};
use crate::config::AppConfig;
use anyhow::Context;
use axum::http::HeaderValue;
use chrono::{DateTime, Utc};
use reqwest;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

pub struct TrueMarketClient {
    http: reqwest::Client,
    base_url: String,
    key_path: PathBuf,
    api_key: TrueMarketApiKey,
    token: tokio::sync::RwLock<Option<String>>,
    retry_duration: u16,
    cache_last_refresh: tokio::sync::RwLock<Option<DateTime<Utc>>>,
    cache_duration: u16,
    account_base_url: String,
}

impl TrueMarketClient {
    pub fn new(config: AppConfig, retry_duration: u16) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        let key_path = PathBuf::from(config.true_market_key_file_path);
        let api_key_as_str =
            std::fs::read_to_string(&key_path).context("Invalid True market api key path")?;
        let api_key: TrueMarketApiKey =
            serde_json::from_str(&api_key_as_str).context("Invalid True market api key JSON")?;
        let token = tokio::sync::RwLock::new(None);
        let base_url = config
            .true_market_base_url
            .trim_end_matches("/")
            .to_string();
        let cache_last_refresh = tokio::sync::RwLock::new(None);
        let cache_duration = 3600;

        Ok(Self {
            http,
            base_url,
            token,
            api_key,
            key_path,
            retry_duration,
            cache_last_refresh,
            cache_duration,
            account_base_url: "https://api.truemarkets.co/v1/account".to_string(),
        })
    }
    async fn mint_token(&self) -> anyhow::Result<JWTToken> {
        let timestamp = Utc::now().timestamp();

        let request = MintTokenRequest {
            key_id: self.api_key.key_id.clone(),
            timestamp,
            signature: self.api_key.sign_auth_challenge(timestamp)?,
        };

        let origin = self.base_url.trim_end_matches("/v1/gateway");

        let token: JWTToken = self
            .http
            .post(format!("{origin}/v1/auth/api-key/token"))
            .json(&request)
            .send()
            .await?
            .error_for_status()
            .context("Failed to mint organization JWT")?
            .json()
            .await?;

        Ok(token)
    }

    async fn get_token(&self) -> anyhow::Result<String> {
        // Fast path: use the cached token while it is fresh.
        {
            let token_guard = self.token.read().await;
            let refreshed_guard = self.cache_last_refresh.read().await;

            if let (Some(token), Some(refreshed_at)) =
                (token_guard.as_ref(), refreshed_guard.as_ref())
            {
                let age = Utc::now().signed_duration_since(*refreshed_at);

                if age.num_seconds() < self.cache_duration as i64 - 60 {
                    return Ok(token.clone());
                }
            }
        } // Both read guards are dropped before acquiring write locks.

        // Recheck after obtaining the write lock to prevent
        // multiple concurrent refreshes.
        let mut token_guard = self.token.write().await;
        let mut refreshed_guard = self.cache_last_refresh.write().await;

        if let (Some(token), Some(refreshed_at)) = (token_guard.as_ref(), refreshed_guard.as_ref())
        {
            let age = Utc::now().signed_duration_since(*refreshed_at);

            if age.num_seconds() < self.cache_duration as i64 - 60 {
                return Ok(token.clone());
            }
        }

        let minted = self.mint_token().await?;

        *token_guard = Some(minted.access_token.clone());
        *refreshed_guard = Some(Utc::now());

        Ok(minted.access_token)
    }

    async fn request<T, B>(
        &self,
        method: Method,
        path: &str,
        user_id: Option<&str>,
        body: Option<&B>,
    ) -> anyhow::Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let token = self.get_token().await?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(id) = user_id {
            headers.insert("TM-On-Behalf-Of", HeaderValue::from_str(id)?);
        }

        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );

        let mut request = self.http.request(method, url).headers(headers);

        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        let status = response.status();

        if status == StatusCode::UNAUTHORIZED {
            // A later request will mint a fresh token.
            *self.token.write().await = None;
            *self.cache_last_refresh.write().await = None;
        }

        let response = response
            .error_for_status()
            .with_context(|| format!("True Markets request failed: {status}"))?;

        if response.status() == StatusCode::NO_CONTENT {
            anyhow::bail!("Received 204 No Content; use a no-content response type");
        }

        Ok(response.json::<T>().await?)
    }

    pub async fn get<T>(&self, path: &str, user_id: Option<&str>) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.request::<T, serde_json::Value>(reqwest::Method::GET, path, user_id, None)
            .await
    }

    pub async fn post<T, B>(&self, path: &str, user_id: Option<&str>, body: &B) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        self.request(reqwest::Method::POST, path, user_id, Some(body))
            .await
    }

    pub async fn patch<T, B>(
        &self,
        path: &str,
        user_id: Option<&str>,
        body: &B,
    ) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        self.request(reqwest::Method::PATCH, path, user_id, Some(body))
            .await
    }

    pub async fn delete<T>(&self, path: &str, user_id: Option<&str>) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.request::<T, serde_json::Value>(reqwest::Method::DELETE, path, user_id, None)
            .await
    }

    pub async fn get_asset_list(&self) -> anyhow::Result<Value> {
        self.get("assets", None).await
    }

    pub async fn get_asset(&self, chain: &str, address: &str) -> anyhow::Result<Value> {
        self.get(&format!("assets/{chain}/{address}"), None).await
    }

    // QUOTES

    pub async fn get_market_quote(
        &self,
        user_id: &str,
        base_asset: &str,
        quote_asset: &str,
        qty: &str,
        qty_unit: &str,
        side: &str,
    ) -> anyhow::Result<Value> {
        let body = json!({
            "base_asset": base_asset,
            "quote_asset": quote_asset,
            "qty": qty,
            "qty_unit": qty_unit,
            "side": side
        });

        self.post("quotes", Some(user_id), &body).await
    }

    // BALANCES AND PORTFOLIO

    pub async fn get_user_balance(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("balances", Some(user_id)).await
    }

    pub async fn get_unified_balances(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("balances/unified", Some(user_id)).await
    }

    pub async fn get_portfolio(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("portfolio", Some(user_id)).await
    }

    pub async fn get_positions(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("positions", Some(user_id)).await
    }

    // ORDERS

    pub async fn create_order(
        &self,
        user_id: &str,
        asset_id: &str,
        qty: &str,
        qty_unit: &str,
        side: &str,
        order_type: &str,
    ) -> anyhow::Result<Value> {
        let body = json!({
            "asset_id": asset_id,
            "qty": qty,
            "qty_unit": qty_unit,
            "side": side,
            "type": order_type
        });

        self.post("orders", Some(user_id), &body).await
    }

    pub async fn list_orders(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("orders", Some(user_id)).await
    }

    pub async fn get_order(&self, user_id: &str, order_id: &str) -> anyhow::Result<Value> {
        self.get(&format!("orders/{order_id}"), Some(user_id)).await
    }

    pub async fn get_order_status(&self, user_id: &str, order_id: &str) -> anyhow::Result<Value> {
        self.get(&format!("orders/{order_id}/status"), Some(user_id))
            .await
    }

    pub async fn execute_order(
        &self,
        user_id: &str,
        order_id: &str,
        signatures: &[String],
        auth_type: &str,
    ) -> anyhow::Result<Value> {
        let body = json!({
            "signatures": signatures,
            "auth_type": auth_type
        });

        self.post(&format!("orders/{order_id}/execute"), Some(user_id), &body)
            .await
    }

    pub async fn cancel_order(&self, user_id: &str, order_id: &str) -> anyhow::Result<Value> {
        self.post(
            &format!("orders/{order_id}/cancel"),
            Some(user_id),
            &json!({}),
        )
        .await
    }

    pub async fn modify_order(
        &self,
        user_id: &str,
        order_id: &str,
        changes: &Value,
    ) -> anyhow::Result<Value> {
        self.patch(&format!("orders/{order_id}"), Some(user_id), changes)
            .await
    }

    // TRANSFERS

    pub async fn create_transfer(&self, user_id: &str, transfer: &Value) -> anyhow::Result<Value> {
        self.post("transfers", Some(user_id), transfer).await
    }

    pub async fn list_transfers(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("transfers", Some(user_id)).await
    }

    pub async fn get_transfer(&self, user_id: &str, transfer_id: &str) -> anyhow::Result<Value> {
        self.get(&format!("transfers/{transfer_id}"), Some(user_id))
            .await
    }

    pub async fn execute_transfer(
        &self,
        user_id: &str,
        transfer_id: &str,
        signatures: &[String],
        auth_type: &str,
    ) -> anyhow::Result<Value> {
        let body = json!({
            "signatures": signatures,
            "auth_type": auth_type
        });

        self.post(
            &format!("transfers/{transfer_id}/execute"),
            Some(user_id),
            &body,
        )
        .await
    }

    // TRANSACTION HISTORY

    pub async fn list_transactions(&self, user_id: &str) -> anyhow::Result<Value> {
        self.get("transactions", Some(user_id)).await
    }

    // RAMPS

    pub async fn create_ramp(&self, user_id: &str, ramp: &Value) -> anyhow::Result<Value> {
        self.post("ramps", Some(user_id), ramp).await
    }

    pub async fn get_ramp(&self, user_id: &str, ramp_id: &str) -> anyhow::Result<Value> {
        self.get(&format!("ramps/{ramp_id}"), Some(user_id)).await
    }

    pub async fn create_user(
        &self,
        organization_id: &str,
        user: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let token = self.get_token().await?;

        let url = format!(
            "{}/organizations/{}/users",
            self.account_base_url.trim_end_matches('/'),
            organization_id,
        );

        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(user)
            .send()
            .await?
            .error_for_status()
            .context("Failed to create Gateway user")?;

        Ok(response.json().await?)
    }
}
