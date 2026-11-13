use axum::{middleware, routing::{Router, delete, get, post, put}, Json};
use reqwest::StatusCode;
use crate::{
    app_state::ApplicationState,
    handlers::{admin, auth, mobile, money_out},
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

    let auth_routes = Router::new()
        .route("/register", post(auth::register))
        .route("/login", post(auth::login))
        .route("/refresh", post(auth::refresh))
        .route("/logout", post(auth::logout))
        .route("/me", get(auth::me))
        .route("/credentials", post(auth::set_credentials))
        .route("/telegram/login", post(auth::telegram_login))
        .route("/telegram/webapp", post(auth::telegram_webapp))
        .route("/telegram/link", post(auth::telegram_link));

    let v1 = Router::new()
        .route("/onboarding", get(mobile::onboarding_status))
        .route("/onboarding/advance", post(mobile::onboarding_advance))
        .route("/kyc", get(mobile::kyc_status).post(mobile::kyc_submit))
        .route("/rule", get(mobile::get_rule).put(mobile::set_rule))
        .route("/rule/custom", get(mobile::get_custom_picks).put(mobile::set_custom_picks))
        .route("/plans", get(mobile::list_plans))
        .route("/assets", get(mobile::list_assets))
        .route("/portfolio", get(mobile::portfolio))
        .route("/deposit", get(mobile::deposit))
        .route("/investments", get(mobile::list_investments))
        .route("/trades/quote", post(mobile::quote))
        .route("/trades", post(mobile::buy))
        .route("/orders", get(mobile::list_orders))
        .route("/orders/{id}", get(mobile::get_order))
        .route("/holdings", get(money_out::holdings))
        .route("/sells/quote", post(money_out::sell_quote))
        .route("/sells", post(money_out::sell))
        .route("/confirm", post(money_out::challenge))
        .route("/withdraw/assets", get(money_out::withdraw_assets))
        .route("/withdraw/addresses", get(money_out::list_addresses).post(money_out::add_address))
        .route("/withdraw/addresses/{id}", delete(money_out::remove_address))
        .route("/withdrawals", get(money_out::list_withdrawals).post(money_out::withdraw))
        .route("/withdrawals/{id}", get(money_out::get_withdrawal));

    let admin_routes = Router::new()
        .route("/assets/sync", post(admin::sync_assets))
        .route("/plans/{plan}/allocations", put(admin::set_allocations))
        .route("/orders/review", get(admin::orders_to_review))
        .route("/kyc/review", get(admin::kyc_to_review))
        .route("/kyc/{id}/resolve", post(admin::kyc_resolve))
        .route("/orders/{id}/resolve", post(admin::resolve_order));

    Router::new()
        .route("/", get(root))
        .nest("/auth", auth_routes)
        .nest("/v1", v1)
        .nest("/admin", admin_routes)
        .route("/health", get(health))
        .merge(webhook)
        .with_state(state)
}

async fn root() -> &'static str {
    "Welcome to usetenth rust API"
}
async fn health() -> (StatusCode, Json<ApiResponse<&'static str>>) {ApiResponse::ok("OK", "Server is healthy")}
