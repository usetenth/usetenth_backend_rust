use axum::{
    middleware,
    routing::{Router, get},
};

use crate::{
    app_state::ApplicationState,
    handlers::webhook::{get_webhook_updates, telegram_auth_middleware, webhook_handler},
};

pub fn app_router(state: ApplicationState) -> Router {
    let application_router = Router::new()
        .route("/", get(root))
        .route("/webhook", get(get_webhook_updates).post(webhook_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            telegram_auth_middleware,
        ))
        .with_state(state);

    application_router
}

async fn root() -> &'static str {
    "Welcome to usetenth rust API"
}
