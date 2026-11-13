use crate::{
    app_state::ApplicationState,
    config::{AppConfig, AppMode},
};
use axum::serve;
use ngrok::{config::ForwarderBuilder, tunnel::EndpointInfo};
mod app_state;
mod config;
mod engine;
mod handlers;
mod models;
mod routes;
mod services;
#[allow(dead_code)]
mod signing;
mod utils;
#[cfg(test)]
mod api_tests;
mod integrations;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let config = AppConfig::from_env()?;
    let application_state = ApplicationState::new(config.clone()).await?;
    if config.engine.enabled {
        tokio::spawn(engine::worker::run(application_state.clone()));
    }
    let router = routes::app_router(application_state);

    let addrs = format!("0.0.0.0:{}", config.port);
    println!("Server listening at: http://localhost:{}", config.port);
    let listener = tokio::net::TcpListener::bind(addrs).await?;

    let _tunnel = match (config.ngrok, config.app_mode) {
        (Some(ngrok_config), AppMode::Development) => {
            let session = ngrok::Session::builder()
                .authtoken(ngrok_config.auth_token)
                .connect()
                .await?;
            let tunnel = session
                .http_endpoint()
                .domain(ngrok_config.domain)
                .listen_and_forward(url::Url::parse(&format!(
                    "http://localhost:{}",
                    config.port
                ))?)
                .await?;

            println!("🚀 Public tunnel url at: {}/webhook", tunnel.url());
            Some(tunnel)
        }
        (None, AppMode::Development) => {
            println!("No ngrok configuration environment variable provided");
            None
        }
        (_, AppMode::Production) => {
            println!("Server running in production environment");
            None
        }
    };

    serve(listener, router).with_graceful_shutdown(shutdown_signal()).await?;

    Ok(())
}

// An order interrupted by shutdown is resumed by stale-order recovery on the next start.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    println!("Shutting down");
}
