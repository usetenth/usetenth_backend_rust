use super::auth::{JWTToken, MintTokenRequest, TrueMarketApiKey};
use crate::config::AppConfig;
use crate::integrations::types::{
    AssetListResponse, AssetResponse, BalanceResponse, CreateGatewayUserRequest,
    ExecuteRequest, ExecuteResponse, OrderListResponse, OrderResponse,
    OrderStatusResponse, PositionsResponse, PortfolioResponse, QuoteRequest,
    QuoteResponse, RampRequest, RampResponse, TransactionsResponse, TransferListResponse,
    TransferRequest, TransferResponse, GatewayUserResponse,
};
use anyhow::{Context, bail};
use chrono::Utc;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    refreshed_at: chrono::DateTime<Utc>,
}

pub struct TrueMarketClient {
    http: reqwest::Client,
    base_url: String,
    account_base_url: String,
    api_key: TrueMarketApiKey,
    token: RwLock<Option<CachedToken>>,
    cache_duration_secs: i64,
    retry_duration: u16,
}

impl TrueMarketClient {
    pub fn new(config: AppConfig, retry_duration: u16) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build True Markets HTTP client")?;

        let key_path = std::path::PathBuf::from(&config.true_market_key_file_path);
        let key_contents = std::fs::read_to_string(&key_path)
            .with_context(|| format!("Failed to read True Markets key file at {}", key_path.display()))?;
        let api_key: TrueMarketApiKey = serde_json::from_str(&key_contents)
            .context("True Markets API key file is not valid expected JSON/JWK")?;

        let base_url = config.true_market_base_url.trim_end_matches('/').to_string();
        // Configure this separately for UAT vs production if Accounts API has a different host.
        let origin = base_url.strip_suffix("/v1/gateway").unwrap_or(&base_url);
        let account_base_url = format!("{origin}/v1/account");

        Ok(Self {
            http,
            base_url,
            account_base_url,
            api_key,
            token: RwLock::new(None),
            cache_duration_secs: 3600,
            retry_duration,
        })
    }

    async fn mint_token(&self) -> anyhow::Result<JWTToken> {
        let timestamp = Utc::now().timestamp();
        let request = MintTokenRequest {
            key_id: self.api_key.key_id.clone(),
            timestamp,
            signature: self.api_key.sign_auth_challenge(timestamp)?,
        };

        let origin = self.base_url.strip_suffix("/v1/gateway").unwrap_or(&self.base_url);
        let url = format!("{origin}/v1/auth/api-key/token");
        let response = self.http.post(url)
            .json(&request)
            .send().await
            .context("Failed to call True Markets token endpoint")?
            .error_for_status()
            .context("True Markets rejected organization-token request")?;
        response.json::<JWTToken>().await.context("Invalid JWT response from True Markets")
    }

    async fn get_token(&self) -> anyhow::Result<String> {
        {
            let guard = self.token.read().await;
            if let Some(cached) = guard.as_ref() {
                let age = Utc::now().signed_duration_since(cached.refreshed_at).num_seconds();
                if age >= 0 && age < self.cache_duration_secs - 60 {
                    return Ok(cached.value.clone());
                }
            }
        }

        let mut guard = self.token.write().await;
        if let Some(cached) = guard.as_ref() {
            let age = Utc::now().signed_duration_since(cached.refreshed_at).num_seconds();
            if age >= 0 && age < self.cache_duration_secs - 60 {
                return Ok(cached.value.clone());
            }
        }

        let minted = self.mint_token().await?;
        let value = minted.access_token.clone();
        *guard = Some(CachedToken { value: value.clone(), refreshed_at: Utc::now() });
        Ok(value)
    }

    async fn invalidate_token(&self) {
        *self.token.write().await = None;
    }

    async fn request<T, B>(
        &self,
        method: Method,
        base: &str,
        path: &str,
        user_id: Option<&str>,
        body: Option<&B>,
    ) -> anyhow::Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));

        // Only retry once for a 401. The request body is replayed as-is; callers should not
        // use this as an idempotency substitute for non-idempotent operations.
        for attempt in 0..=1 {
            let token = self.get_token().await?;
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?);
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(id) = user_id {
                headers.insert("TM-On-Behalf-Of", HeaderValue::from_str(id)
                    .context("Gateway user ID is not a valid header value")?);
            }

            let mut req = self.http.request(method.clone(), &url).headers(headers);
            if let Some(value) = body { req = req.json(value); }
            let response = req.send().await.context("True Markets HTTP request failed")?;
            let status = response.status();

            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.invalidate_token().await;
                continue;
            }

            if status == StatusCode::NO_CONTENT {
                if status.is_success() {
                    // A generic JSON-returning method cannot represent 204. Use request_no_content.
                    bail!("True Markets returned 204 No Content; call a no-content method for this route");
                }
            }

            if !status.is_success() {
                let body_text = response.text().await.unwrap_or_default();
                bail!("True Markets request failed ({status}) for {path}: {body_text}");
            }
            return response.json::<T>().await
                .with_context(|| format!("Could not decode True Markets response for {path}"));
        }
        bail!("True Markets authorization failed after refreshing the token")
    }

    async fn request_no_content<B: Serialize + ?Sized>(
        &self, method: Method, base: &str, path: &str, user_id: Option<&str>, body: Option<&B>,
    ) -> anyhow::Result<()> {
        let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
        let token = self.get_token().await?;
        let mut req = self.http.request(method, url).bearer_auth(token);
        if let Some(id) = user_id {
            req = req.header("TM-On-Behalf-Of", id);
        }
        if let Some(value) = body { req = req.json(value); }
        let response = req.send().await.context("True Markets HTTP request failed")?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.invalidate_token().await;
            bail!("True Markets returned 401; token invalidated. Retry this operation explicitly.");
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("True Markets request failed ({status}) for {path}: {body}");
        }
        Ok(())
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str, user_id: Option<&str>) -> anyhow::Result<T> {
        self.request::<T, Value>(Method::GET, &self.base_url, path, user_id, None).await
    }

    pub async fn post<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self, path: &str, user_id: Option<&str>, body: &B,
    ) -> anyhow::Result<T> {
        self.request(Method::POST, &self.base_url, path, user_id, Some(body)).await
    }

    pub async fn patch<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self, path: &str, user_id: Option<&str>, body: &B,
    ) -> anyhow::Result<T> {
        self.request(Method::PATCH, &self.base_url, path, user_id, Some(body)).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str, user_id: Option<&str>) -> anyhow::Result<T> {
        self.request::<T, Value>(Method::DELETE, &self.base_url, path, user_id, None).await
    }

    pub async fn delete_no_content(&self, path: &str, user_id: Option<&str>) -> anyhow::Result<()> {
        self.request_no_content::<Value>(Method::DELETE, &self.base_url, path, user_id, None).await
    }

    pub async fn get_asset_list(&self) -> anyhow::Result<AssetListResponse> {
        self.get("assets", None).await
    }

    pub async fn get_asset(&self, chain: &str, address: &str) -> anyhow::Result<AssetResponse> {
        self.get(&format!("assets/{chain}/{address}"), None).await
    }

    pub async fn get_market_quote(&self, user_id: &str, request: &QuoteRequest) -> anyhow::Result<QuoteResponse> {
        self.post("quotes", Some(user_id), request).await
    }

    pub async fn get_user_balance(&self, user_id: &str) -> anyhow::Result<BalanceResponse> {
        self.get("balances", Some(user_id)).await
    }

    pub async fn get_unified_balances(&self, user_id: &str) -> anyhow::Result<BalanceResponse> {
        self.get("balances/unified", Some(user_id)).await
    }

    pub async fn get_portfolio(&self, user_id: &str) -> anyhow::Result<PortfolioResponse> {
        self.get("portfolio", Some(user_id)).await
    }

    pub async fn get_positions(&self, user_id: &str) -> anyhow::Result<PositionsResponse> {
        self.get("positions", Some(user_id)).await
    }

    pub async fn create_order(&self, user_id: &str, request: &crate::integrations::types::CreateOrderRequest) -> anyhow::Result<OrderResponse> {
        self.post("orders", Some(user_id), request).await
    }

    pub async fn list_orders(&self, user_id: &str) -> anyhow::Result<OrderListResponse> {
        self.get("orders", Some(user_id)).await
    }

    pub async fn get_order(&self, user_id: &str, order_id: &str) -> anyhow::Result<OrderResponse> {
        self.get(&format!("orders/{order_id}"), Some(user_id)).await
    }

    pub async fn get_order_status(&self, user_id: &str, order_id: &str) -> anyhow::Result<OrderStatusResponse> {
        self.get(&format!("orders/{order_id}/status"), Some(user_id)).await
    }

    pub async fn execute_order(&self, user_id: &str, order_id: &str, request: &ExecuteRequest) -> anyhow::Result<ExecuteResponse> {
        self.post(&format!("orders/{order_id}/execute"), Some(user_id), request).await
    }

    pub async fn cancel_order(&self, user_id: &str, order_id: &str) -> anyhow::Result<OrderResponse> {
        self.post(&format!("orders/{order_id}/cancel"), Some(user_id), &json!({})).await
    }

    pub async fn modify_order(&self, user_id: &str, order_id: &str, changes: &Value) -> anyhow::Result<OrderResponse> {
        self.patch(&format!("orders/{order_id}"), Some(user_id), changes).await
    }

    pub async fn create_transfer(&self, user_id: &str, request: &TransferRequest) -> anyhow::Result<TransferResponse> {
        self.post("transfers", Some(user_id), request).await
    }

    pub async fn list_transfers(&self, user_id: &str) -> anyhow::Result<TransferListResponse> {
        self.get("transfers", Some(user_id)).await
    }

    pub async fn get_transfer(&self, user_id: &str, transfer_id: &str) -> anyhow::Result<TransferResponse> {
        self.get(&format!("transfers/{transfer_id}"), Some(user_id)).await
    }

    pub async fn execute_transfer(&self, user_id: &str, transfer_id: &str, request: &ExecuteRequest) -> anyhow::Result<ExecuteResponse> {
        self.post(&format!("transfers/{transfer_id}/execute"), Some(user_id), request).await
    }

    pub async fn list_transactions(&self, user_id: &str) -> anyhow::Result<TransactionsResponse> {
        self.get("transactions", Some(user_id)).await
    }

    pub async fn create_ramp(&self, user_id: &str, request: &RampRequest) -> anyhow::Result<RampResponse> {
        self.post("ramps", Some(user_id), request).await
    }

    pub async fn get_ramp(&self, user_id: &str, ramp_id: &str) -> anyhow::Result<RampResponse> {
        self.get(&format!("ramps/{ramp_id}"), Some(user_id)).await
    }

    pub async fn create_user(&self, organization_id: &str, user: &CreateGatewayUserRequest) -> anyhow::Result<GatewayUserResponse> {
        self.request(Method::POST, &self.account_base_url,
                     &format!("organizations/{organization_id}/users"), None, Some(user)).await
    }

    // Available for observability/config validation; this is not a transaction retry policy.
    pub fn configured_retry_duration(&self) -> u16 { self.retry_duration }
}
