use std::sync::Arc;

use anyhow::Context;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

use crate::{
    config::AppConfig,
    integrations::tm_client::TrueMarketClient,
    signing::{LocalKeyWrapper, SigningLimits, SigningService},
};

#[derive(Clone)]
pub struct ApplicationState {
    pub pool: Pool<Postgres>,
    pub config: AppConfig,
    pub http: reqwest::Client,
    pub tm_client: Arc<TrueMarketClient>,
    pub signing: Arc<SigningService<LocalKeyWrapper>>,
    pub limiter: Arc<crate::services::rate_limit::RateLimiter>,
}

impl ApplicationState {
    pub async fn new(config: AppConfig) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(config.db_url.as_str())
            .await?;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        let tm_client = Arc::new(
            TrueMarketClient::new(config.clone(), 30)
                .await
                .context("Failed to instantiate True Market client")?,
        );

        let signing = Arc::new(SigningService::new(
            pool.clone(),
            LocalKeyWrapper::from_env().context("Failed to load signing master keys")?,
            SigningLimits::from_env().context("Invalid signing limits")?,
        ));

        Ok(ApplicationState {
            pool,
            config,
            http,
            tm_client,
            signing,
            limiter: Arc::new(Default::default()),
        })
    }
}

#[cfg(test)]
impl ApplicationState {
    pub fn for_test(
        pool: Pool<Postgres>,
        config: AppConfig,
        tm_client: TrueMarketClient,
        signing: SigningService<LocalKeyWrapper>,
    ) -> Self {
        Self {
            pool,
            config,
            http: reqwest::Client::new(),
            tm_client: Arc::new(tm_client),
            signing: Arc::new(signing),
            limiter: Arc::new(Default::default()),
        }
    }
}
