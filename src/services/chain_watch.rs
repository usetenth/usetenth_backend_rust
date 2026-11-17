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

pub struct SolanaTransfer {
    pub owner: String,
    pub mint: String,
    pub micros: i64,
}

/// Strings in an alert that look like Solana transaction signatures.
pub fn solana_signatures(payload: &serde_json::Value) -> Vec<String> {
    fn walk(value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) if (80..=90).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric()) => found.push(s.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, found)),
            serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, found)),
            _ => {}
        }
    }
    let mut found = vec![];
    walk(payload, &mut found);
    found.sort();
    found.dedup();
    found
}

fn token_amount(entry: &serde_json::Value) -> Option<i128> {
    let raw: i128 = entry.pointer("/uiTokenAmount/amount")?.as_str()?.parse().ok()?;
    let decimals = entry.pointer("/uiTokenAmount/decimals")?.as_u64()? as u32;
    // Scaled to millionths of a unit, the same as every other amount here.
    Some(if decimals >= 6 { raw / 10i128.pow(decimals - 6) } else { raw * 10i128.pow(6 - decimals) })
}

/// Token balance increases per owner and mint in a getTransaction result.
pub fn solana_transfers(transaction: &serde_json::Value) -> Vec<SolanaTransfer> {
    let balances = |key: &str| transaction.pointer(&format!("/result/meta/{key}")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let before = balances("preTokenBalances");
    let mut gained = vec![];
    for after in balances("postTokenBalances") {
        let (Some(owner), Some(mint), Some(now)) = (after["owner"].as_str(), after["mint"].as_str(), token_amount(&after)) else { continue };
        let was = before
            .iter()
            .find(|b| b["owner"].as_str() == Some(owner) && b["mint"].as_str() == Some(mint))
            .and_then(token_amount)
            .unwrap_or(0);
        if now > was {
            gained.push(SolanaTransfer { owner: owner.into(), mint: mint.into(), micros: (now - was).min(i64::MAX as i128) as i64 });
        }
    }
    gained
}

async fn fetch_solana(state: &ApplicationState, rpc: &str, signature: &str) -> anyhow::Result<serde_json::Value> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "getTransaction",
        "params": [signature, { "encoding": "jsonParsed", "maxSupportedTransactionVersion": 0, "commitment": "confirmed" }] });
    Ok(state.http.post(rpc).json(&body).send().await?.error_for_status()?.json().await?)
}

/// Reads a Solana transaction named in an alert and notes any stablecoin it credits to a user's own address as arriving.
pub async fn note_solana(state: &ApplicationState, signature: &str) -> anyhow::Result<usize> {
    let Some(rpc) = state.config.alchemy.as_ref().and_then(|c| c.solana_rpc.as_deref()) else { return Ok(0) };
    // The transaction can take a moment to be readable after the alert.
    for wait in [0u64, 2, 4] {
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        let transaction = fetch_solana(state, rpc, signature).await?;
        if transaction["result"].is_null() {
            continue;
        }
        let mut noted = 0;
        for transfer in solana_transfers(&transaction) {
            let row: Option<(uuid::Uuid, String)> = sqlx::query_as(
                "SELECT w.user_id, a.symbol FROM wallets w JOIN assets a ON a.stable AND a.address = $2
                 WHERE w.network = 'solana' AND w.address = $1 LIMIT 1",
            )
            .bind(&transfer.owner)
            .bind(&transfer.mint)
            .fetch_optional(&state.pool)
            .await?;
            if let Some((user_id, symbol)) = row {
                if crate::services::pending::note(&state.pool, user_id, signature, &symbol, Some(transfer.micros)).await? {
                    noted += 1;
                }
            }
        }
        return Ok(noted);
    }
    Ok(0)
}
