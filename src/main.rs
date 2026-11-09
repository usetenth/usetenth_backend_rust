use crate::{
    app_state::ApplicationState,
    config::{AppConfig, AppMode},
};
use axum::serve;
use ngrok::{config::ForwarderBuilder, tunnel::EndpointInfo};
mod app_state;
mod config;
mod handlers;
mod models;
mod routes;
mod services;
mod utils;
mod integrations;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    let config = AppConfig::from_env()?;
    let application_state = ApplicationState::new(config.clone()).await?;
    let router = routes::app_router(application_state);

    //TCP Listener to listen for connections
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

            //Expose tunnel url
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

    //Serves the backend application
    serve(listener, router).await?;

    Ok(())
}
