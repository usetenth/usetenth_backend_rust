use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

use crate::config::AppConfig;

#[derive(Clone)]
pub struct ApplicationState {
    pub pool: Pool<Postgres>,
    pub config: AppConfig,
}

impl ApplicationState {
    pub async fn new(config: AppConfig) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(config.db_url.as_str())
            .await
            .unwrap();

        Ok(ApplicationState { pool, config })
    }
}
