use crate::{app_state::ApplicationState, config::AlchemyConfig};
use serde_json::json;

const BATCH: i64 = 200;

/// Registers wallet addresses that are not watched yet with the provider's webhooks, so it tells us when money arrives.
pub async fn sync_addresses(state: &ApplicationState) -> anyhow::Result<()> {
    let Some(cfg) = state.config.alchemy.as_ref() else { return Ok(()) };
    let Some(token) = cfg.auth_token.as_deref() else { return Ok(()) };
    for network in ["evm", "solana"] {
        let hooks: Vec<&str> = cfg.webhooks.iter().filter(|(n, _)| n == network).map(|(_, id)| id.as_str()).collect();
        if hooks.is_empty() {
            continue;
        }
        let addresses: Vec<String> =
            sqlx::query_scalar("SELECT address FROM wallets WHERE network = $1 AND watched_at IS NULL ORDER BY created_at LIMIT $2")
                .bind(network)
                .bind(BATCH)
                .fetch_all(&state.pool)
                .await?;
        if addresses.is_empty() {
            continue;
        }
        let mut all_added = true;
        for hook in hooks {
            if let Err(error) = add_to_webhook(state, cfg, token, hook, &addresses).await {
                eprintln!("Could not register {network} addresses with webhook {hook}: {error:#}");
                all_added = false;
            }
        }
        if all_added {
            sqlx::query("UPDATE wallets SET watched_at = now() WHERE network = $1 AND address = ANY($2)")
                .bind(network)
                .bind(&addresses)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(())
}

async fn add_to_webhook(state: &ApplicationState, cfg: &AlchemyConfig, token: &str, hook: &str, addresses: &[String]) -> anyhow::Result<()> {
    let response = state
        .http
        .patch(format!("{}/update-webhook-addresses", cfg.api_base))
        .header("X-Alchemy-Token", token)
        .json(&json!({ "webhook_id": hook, "addresses_to_add": addresses, "addresses_to_remove": [] }))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("{status}: {}", body.chars().take(200).collect::<String>());
    }
    Ok(())
}
