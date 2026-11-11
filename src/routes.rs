use axum::{middleware, routing::{Router, get}, Json};
use reqwest::StatusCode;
use crate::{
    app_state::ApplicationState,
    handlers::webhook::{get_webhook_updates, telegram_auth_middleware, webhook_handler},
};
use crate::services::response::ApiResponse;

pub fn app_router(state: ApplicationState) -> Router {
    // Only the webhook requires Telegram's secret token; / and /health stay public.
    let webhook = Router::new()
        .route("/webhook", get(get_webhook_updates).post(webhook_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            telegram_auth_middleware,
        ));

    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .merge(webhook)
        .with_state(state)
}

async fn root() -> &'static str {
    "Welcome to usetenth rust API"
}
async fn health() -> (StatusCode, Json<ApiResponse<&'static str>>) {ApiResponse::ok("OK", "Server is healthy")}
