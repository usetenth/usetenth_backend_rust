use super::auth::{JWTToken, MintTokenRequest, TrueMarketApiKey};
use crate::config::AppConfig;
use crate::integrations::types::{
    AssetItem, CancelOrderResponse, CreateGatewayUserRequest, CreateOrderResponse,
    CreateRampResponse, ExecuteOrderResponse, ExecuteRequest, GatewayUserResponse,
    GetOrderStatusResponse, GetOrdersResponse, ListAssetsResponse, ListBalancesResponse,
    ListPositionsResponse, ListTransactionsResponse, ListTransfersResponse, ModifyOrderResponse,
    OrderDetail, PortfolioResponse, QuoteRequest, QuoteResponse, RampDetail, RampRequest,
    TransferDetail, TransferRequest,
};
use anyhow::{Context, bail};
use chrono::Utc;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::RwLock;

/// Accepts the host, the host plus /v1, or the full gateway address, and returns (host, gateway address).
fn split_base_url(raw: &str) -> (String, String) {
    let trimmed = raw.trim().trim_end_matches('/');
    let origin = trimmed.strip_suffix("/v1/gateway").or_else(|| trimmed.strip_suffix("/v1")).unwrap_or(trimmed);
    (origin.to_string(), format!("{origin}/v1/gateway"))
}

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    refreshed_at: chrono::DateTime<Utc>,
}

#[allow(dead_code)] // Mirrors the gateway API; not every call is used yet.
pub struct TrueMarketClient {
    http: reqwest::Client,
    base_url: String,
    account_base_url: String,
    api_key: TrueMarketApiKey,
    token: RwLock<Option<CachedToken>>,
    cache_duration_secs: i64,
    retry_duration: u16,
}

#[allow(dead_code)] // Mirrors the gateway API; not every call is used yet.
impl TrueMarketClient {
    pub async fn new(config: AppConfig, retry_duration: u16) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("Failed to build True Markets HTTP client")?;

        let api_key = super::key_source::load_api_key(&config).await?;

        let (origin, base_url) = split_base_url(&config.true_market_base_url);
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

        let origin = self
            .base_url
            .strip_suffix("/v1/gateway")
            .unwrap_or(&self.base_url);
        let url = format!("{origin}/v1/auth/api-key/token");
        let response = self
            .http
            .post(url)
            .json(&request)
            .send()
            .await
            .context("Failed to call True Markets token endpoint")?
            .error_for_status()
            .context("True Markets rejected organization-token request")?;
        response
            .json::<JWTToken>()
            .await
            .context("Invalid JWT response from True Markets")
    }

    async fn get_token(&self) -> anyhow::Result<String> {
        {
            let guard = self.token.read().await;
            if let Some(cached) = guard.as_ref() {
                let age = Utc::now()
                    .signed_duration_since(cached.refreshed_at)
                    .num_seconds();
                if age >= 0 && age < self.cache_duration_secs - 60 {
                    return Ok(cached.value.clone());
                }
            }
        }

        let mut guard = self.token.write().await;
        if let Some(cached) = guard.as_ref() {
            let age = Utc::now()
                .signed_duration_since(cached.refreshed_at)
                .num_seconds();
            if age >= 0 && age < self.cache_duration_secs - 60 {
                return Ok(cached.value.clone());
            }
        }

        let minted = self.mint_token().await?;
        let value = minted.access_token.clone();
        *guard = Some(CachedToken {
            value: value.clone(),
            refreshed_at: Utc::now(),
        });
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
        let url = format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        );

        // Retries a 401 once with the body replayed as-is, so it is no idempotency guarantee.
        for attempt in 0..=1 {
            let token = self.get_token().await?;
            let mut headers = HeaderMap::new();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(id) = user_id {
                headers.insert(
                    "TM-On-Behalf-Of",
                    HeaderValue::from_str(id)
                        .context("Gateway user ID is not a valid header value")?,
                );
            }

            let mut req = self.http.request(method.clone(), &url).headers(headers);
            if let Some(value) = body {
                req = req.json(value);
            }
            let response = req
                .send()
                .await
                .context("True Markets HTTP request failed")?;
            let status = response.status();

            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.invalidate_token().await;
                continue;
            }

            if status == StatusCode::NO_CONTENT {
                if status.is_success() {
                    // A generic JSON-returning method cannot represent 204. Use request_no_content.
                    bail!(
                        "True Markets returned 204 No Content; call a no-content method for this route"
                    );
                }
            }

            if !status.is_success() {
                let body_text = response.text().await.unwrap_or_default();
                bail!("True Markets request failed ({status}) for {path}: {body_text}");
            }
            let bytes = response
                .bytes()
                .await
                .context("Could not read True Markets response body")?;
            let bytes = if bytes.is_empty() { "{}".as_bytes().into() } else { bytes };
            return serde_json::from_slice::<T>(&bytes)
                .with_context(|| format!("Could not decode True Markets response for {path}"));
        }
        bail!("True Markets authorization failed after refreshing the token")
    }

    async fn request_no_content<B: Serialize + ?Sized>(
        &self,
        method: Method,
        base: &str,
        path: &str,
        user_id: Option<&str>,
        body: Option<&B>,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let token = self.get_token().await?;
        let mut req = self.http.request(method, url).bearer_auth(token);
        if let Some(id) = user_id {
            req = req.header("TM-On-Behalf-Of", id);
        }
        if let Some(value) = body {
            req = req.json(value);
        }
        let response = req
            .send()
            .await
            .context("True Markets HTTP request failed")?;
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

    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        user_id: Option<&str>,
    ) -> anyhow::Result<T> {
        self.request::<T, Value>(Method::GET, &self.base_url, path, user_id, None)
            .await
    }

    pub async fn post<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        user_id: Option<&str>,
        body: &B,
    ) -> anyhow::Result<T> {
        self.request(Method::POST, &self.base_url, path, user_id, Some(body))
            .await
    }

    pub async fn patch<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        user_id: Option<&str>,
        body: &B,
    ) -> anyhow::Result<T> {
        self.request(Method::PATCH, &self.base_url, path, user_id, Some(body))
            .await
    }

    pub async fn delete<T: DeserializeOwned>(
        &self,
        path: &str,
        user_id: Option<&str>,
    ) -> anyhow::Result<T> {
        self.request::<T, Value>(Method::DELETE, &self.base_url, path, user_id, None)
            .await
    }

    pub async fn delete_no_content(&self, path: &str, user_id: Option<&str>) -> anyhow::Result<()> {
        self.request_no_content::<Value>(Method::DELETE, &self.base_url, path, user_id, None)
            .await
    }

    pub async fn get_asset_list(&self) -> anyhow::Result<ListAssetsResponse> {
        // The gateway lists crypto only unless stocks are asked for.
        self.get("assets?asset_class=crypto,stock", None).await
    }

    pub async fn get_asset(&self, chain: &str, address: &str) -> anyhow::Result<AssetItem> {
        self.get(&format!("assets/{chain}/{address}"), None).await
    }

    pub async fn get_market_quote(
        &self,
        user_id: &str,
        request: &QuoteRequest,
    ) -> anyhow::Result<QuoteResponse> {
        self.post("quotes", Some(user_id), request).await
    }

    pub async fn get_user_balance(&self, user_id: &str) -> anyhow::Result<ListBalancesResponse> {
        self.get("balances", Some(user_id)).await
    }

    pub async fn get_unified_balances(&self, user_id: &str) -> anyhow::Result<ListBalancesResponse> {
        self.get("balances/unified", Some(user_id)).await
    }

    pub async fn get_portfolio(&self, user_id: &str) -> anyhow::Result<PortfolioResponse> {
        self.get("portfolio", Some(user_id)).await
    }

    pub async fn get_positions(&self, user_id: &str) -> anyhow::Result<ListPositionsResponse> {
        self.get("positions", Some(user_id)).await
    }

    pub async fn create_order(
        &self,
        user_id: &str,
        request: &crate::integrations::types::CreateOrderRequest,
    ) -> anyhow::Result<CreateOrderResponse> {
        self.post("orders", Some(user_id), request).await
    }

    pub async fn list_orders(&self, user_id: &str) -> anyhow::Result<GetOrdersResponse> {
        self.get("orders", Some(user_id)).await
    }

    pub async fn get_order(&self, user_id: &str, order_id: &str) -> anyhow::Result<OrderDetail> {
        self.get(&format!("orders/{order_id}"), Some(user_id)).await
    }

    pub async fn get_order_status(
        &self,
        user_id: &str,
        order_id: &str,
    ) -> anyhow::Result<GetOrderStatusResponse> {
        self.get(&format!("orders/{order_id}/status"), Some(user_id))
            .await
    }

    pub async fn execute_order(
        &self,
        user_id: &str,
        order_id: &str,
        request: &ExecuteRequest,
    ) -> anyhow::Result<ExecuteOrderResponse> {
        self.post(
            &format!("orders/{order_id}/execute"),
            Some(user_id),
            request,
        )
        .await
    }

    pub async fn cancel_order(
        &self,
        user_id: &str,
        order_id: &str,
    ) -> anyhow::Result<CancelOrderResponse> {
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
    ) -> anyhow::Result<ModifyOrderResponse> {
        self.patch(&format!("orders/{order_id}"), Some(user_id), changes)
            .await
    }

    pub async fn create_transfer(
        &self,
        user_id: &str,
        request: &TransferRequest,
    ) -> anyhow::Result<TransferDetail> {
        self.post("transfers", Some(user_id), request).await
    }

    pub async fn list_transfers(&self, user_id: &str) -> anyhow::Result<ListTransfersResponse> {
        self.get("transfers", Some(user_id)).await
    }

    pub async fn get_transfer(
        &self,
        user_id: &str,
        transfer_id: &str,
    ) -> anyhow::Result<TransferDetail> {
        self.get(&format!("transfers/{transfer_id}"), Some(user_id))
            .await
    }

    pub async fn execute_transfer(
        &self,
        user_id: &str,
        transfer_id: &str,
        request: &ExecuteRequest,
    ) -> anyhow::Result<TransferDetail> {
        self.post(
            &format!("transfers/{transfer_id}/execute"),
            Some(user_id),
            request,
        )
        .await
    }

    pub async fn list_transactions(&self, user_id: &str) -> anyhow::Result<ListTransactionsResponse> {
        self.get("transactions", Some(user_id)).await
    }

    pub async fn create_ramp(
        &self,
        user_id: &str,
        request: &RampRequest,
    ) -> anyhow::Result<CreateRampResponse> {
        self.post("ramps", Some(user_id), request).await
    }

    pub async fn get_ramp(&self, user_id: &str, ramp_id: &str) -> anyhow::Result<RampDetail> {
        self.get(&format!("ramps/{ramp_id}"), Some(user_id)).await
    }

    pub async fn create_user(
        &self,
        organization_id: &str,
        user: &CreateGatewayUserRequest,
    ) -> anyhow::Result<GatewayUserResponse> {
        self.request(
            Method::POST,
            &self.account_base_url,
            &format!("organizations/{organization_id}/users"),
            None,
            Some(user),
        )
        .await
    }

    #[cfg(test)]
    pub fn for_test(base_url: &str) -> Self {
        use super::auth::PrivateKey;
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let key = p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng);
        let base_url = base_url.trim_end_matches('/').to_string();
        let origin = base_url.strip_suffix("/v1/gateway").unwrap_or(&base_url).to_string();
        Self {
            http: reqwest::Client::new(),
            account_base_url: format!("{origin}/v1/account"),
            base_url,
            api_key: TrueMarketApiKey {
                key_id: "test-key".into(),
                label: "test".into(),
                fingerprint: "test".into(),
                created_at: Utc::now(),
                private_key: PrivateKey {
                    kty: "EC".into(),
                    crv: "P-256".into(),
                    x: String::new(),
                    y: String::new(),
                    d: URL_SAFE_NO_PAD.encode(key.to_bytes()),
                },
            },
            token: RwLock::new(None),
            cache_duration_secs: 3600,
            retry_duration: 0,
        }
    }

    // Available for observability/config validation; this is not a transaction retry policy.
    pub fn configured_retry_duration(&self) -> u16 {
        self.retry_duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::{get, post}};
    use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

    #[test]
    fn every_accepted_base_url_form_gives_the_same_addresses() {
        for raw in [
            "https://api.truemarkets.co",
            "https://api.truemarkets.co/",
            "https://api.truemarkets.co/v1",
            "https://api.truemarkets.co/v1/",
            " https://api.truemarkets.co/v1/gateway/ ",
        ] {
            assert_eq!(
                split_base_url(raw),
                ("https://api.truemarkets.co".to_string(), "https://api.truemarkets.co/v1/gateway".to_string()),
                "{raw}"
            );
        }
    }

    #[derive(Clone, Default)]
    struct Gateway {
        mints: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        reject_first_call: Arc<std::sync::atomic::AtomicBool>,
        seen: Arc<Mutex<Vec<(String, Option<String>)>>>,
    }

    async fn serve(gateway: Gateway) -> String {
        async fn token(State(g): State<Gateway>) -> Json<Value> {
            let n = g.mints.fetch_add(1, Ordering::SeqCst) + 1;
            Json(json!({ "access_token": format!("token-{n}") }))
        }
        async fn portfolio(State(g): State<Gateway>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
            let n = g.calls.fetch_add(1, Ordering::SeqCst);
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
            let on_behalf = headers.get("tm-on-behalf-of").and_then(|v| v.to_str().ok()).map(String::from);
            g.seen.lock().unwrap().push((auth, on_behalf));
            if n == 0 && g.reject_first_call.load(Ordering::SeqCst) {
                return (StatusCode::UNAUTHORIZED, Json(json!({})));
            }
            (StatusCode::OK, Json(json!({ "balances": [], "positions": [] })))
        }
        async fn broken() -> (StatusCode, &'static str) {
            (StatusCode::INTERNAL_SERVER_ERROR, "boom")
        }
        async fn empty() -> StatusCode {
            StatusCode::NO_CONTENT
        }
        async fn garbage() -> &'static str {
            "{\"unexpected\": true"
        }
        let app = Router::new()
            .route("/v1/auth/api-key/token", post(token))
            .route("/v1/gateway/portfolio", get(portfolio))
            .route("/v1/gateway/broken", get(broken))
            .route("/v1/gateway/empty", get(empty))
            .route("/v1/gateway/garbage", get(garbage))
            .with_state(gateway);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/v1/gateway")
    }

    #[tokio::test]
    async fn the_token_is_minted_once_and_reused_with_the_user_header() {
        let gateway = Gateway::default();
        let client = TrueMarketClient::for_test(&serve(gateway.clone()).await);
        client.get_portfolio("gw-user").await.unwrap();
        client.get_portfolio("gw-user").await.unwrap();

        assert_eq!(gateway.mints.load(Ordering::SeqCst), 1);
        let seen = gateway.seen.lock().unwrap();
        assert_eq!(seen[0], ("Bearer token-1".to_string(), Some("gw-user".to_string())));
        assert_eq!(seen[1].0, "Bearer token-1");
    }

    #[tokio::test]
    async fn a_rejected_token_is_replaced_and_the_call_retried_once() {
        let gateway = Gateway::default();
        gateway.reject_first_call.store(true, Ordering::SeqCst);
        let client = TrueMarketClient::for_test(&serve(gateway.clone()).await);
        client.get_portfolio("gw-user").await.unwrap();

        assert_eq!(gateway.mints.load(Ordering::SeqCst), 2, "a fresh token was minted after the 401");
        let seen = gateway.seen.lock().unwrap();
        assert_eq!(seen[0].0, "Bearer token-1");
        assert_eq!(seen[1].0, "Bearer token-2");
    }

    #[tokio::test]
    async fn failures_carry_the_status_and_body() {
        let client = TrueMarketClient::for_test(&serve(Gateway::default()).await);
        let error = client.get::<Value>("broken", None).await.unwrap_err().to_string();
        assert!(error.contains("500") && error.contains("boom"), "{error}");
        let error = client.get::<Value>("garbage", None).await.unwrap_err().to_string();
        assert!(error.contains("decode"), "{error}");
        let error = client.get::<Value>("empty", None).await.unwrap_err().to_string();
        assert!(error.contains("204"), "{error}");
    }

    #[tokio::test]
    async fn an_unreachable_gateway_is_an_error_not_a_panic() {
        let client = TrueMarketClient::for_test("http://127.0.0.1:1/v1/gateway");
        assert!(client.get_portfolio("gw-user").await.is_err());
    }
}
