use crate::{app_state::ApplicationState, engine::worker};
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

// The provider signs the raw body with each webhook's own key; any configured key may match.
pub fn signature_valid(keys: &[String], body: &[u8], signature: &str) -> bool {
    let Ok(expected) = hex::decode(signature.trim()) else { return false };
    keys.iter().any(|key| {
        let Ok(mut mac) = HmacSha256::new_from_slice(key.as_bytes()) else { return false };
        mac.update(body);
        mac.verify_slice(&expected).is_ok()
    })
}

/// Every address-looking string in the payload: EVM ones lowercased, the rest (Solana) as written.
pub fn candidate_addresses(payload: &Value) -> (Vec<String>, Vec<String>) {
    fn walk(value: &Value, evm: &mut Vec<String>, other: &mut Vec<String>) {
        match value {
            Value::String(s) => {
                if s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit()) {
                    evm.push(s.to_lowercase());
                } else if (32..=44).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric()) {
                    other.push(s.clone());
                }
            }
            Value::Array(items) => items.iter().for_each(|v| walk(v, evm, other)),
            Value::Object(map) => map.values().for_each(|v| walk(v, evm, other)),
            _ => {}
        }
    }
    let (mut evm, mut other) = (vec![], vec![]);
    walk(payload, &mut evm, &mut other);
    evm.sort();
    evm.dedup();
    other.sort();
    other.dedup();
    (evm, other)
}

async fn users_for(pool: &PgPool, evm: &[String], other: &[String]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT DISTINCT user_id FROM wallets WHERE lower(address) = ANY($1) OR address = ANY($2)")
        .bind(evm)
        .bind(other)
        .fetch_all(pool)
        .await
}

// Only a hint: the gateway is asked whether the money really arrived before anything is invested.
pub async fn alchemy(State(state): State<ApplicationState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Some(cfg) = state.config.alchemy.as_ref().filter(|c| !c.signing_keys.is_empty()) else {
        return StatusCode::NOT_FOUND;
    };
    let signature = headers.get("x-alchemy-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !signature_valid(&cfg.signing_keys, &body, signature) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else { return StatusCode::BAD_REQUEST };
    let (evm, other) = candidate_addresses(&payload);
    match users_for(&state.pool, &evm, &other).await {
        Ok(users) => {
            if !users.is_empty() {
                println!("Chain alert matched {} user(s)", users.len());
            }
            for user in users {
                worker::hint_user(state.clone(), user);
            }
            StatusCode::OK
        }
        Err(error) => {
            eprintln!("Chain hook lookup failed: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sign(key: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn a_signature_matches_only_the_exact_body_and_a_configured_key() {
        let keys = vec!["first-key".to_string(), "second-key".to_string()];
        let body = br#"{"type":"ADDRESS_ACTIVITY"}"#;
        assert!(signature_valid(&keys, body, &sign("first-key", body)));
        assert!(signature_valid(&keys, body, &sign("second-key", body)), "each webhook has its own key");
        assert!(!signature_valid(&keys, body, &sign("other-key", body)));
        assert!(!signature_valid(&keys, br#"{"type":"CHANGED"}"#, &sign("first-key", body)));
        assert!(!signature_valid(&keys, body, "not-hex"));
        assert!(!signature_valid(&keys, body, ""));
        assert!(!signature_valid(&[], body, &sign("first-key", body)));
    }

    #[test]
    fn evm_payloads_yield_lowercase_addresses_and_ignore_hashes_and_topics() {
        let payload = json!({
            "event": { "network": "BASE_MAINNET", "activity": [{
                "hash": "0x7a4a39da1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6",
                "fromAddress": "0x503828976D22510aad0201ac7ec88293211D23Da",
                "toAddress": "0xbe3f4b43db5eb49d1f48f53443b9abce45da3b79",
                "asset": "USDC",
                "log": { "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"] }
            }]}
        });
        let (evm, other) = candidate_addresses(&payload);
        assert_eq!(evm, vec!["0x503828976d22510aad0201ac7ec88293211d23da".to_string(), "0xbe3f4b43db5eb49d1f48f53443b9abce45da3b79".to_string()]);
        assert!(other.is_empty());
    }

    #[test]
    fn solana_style_addresses_are_kept_as_written_and_signatures_are_ignored() {
        let payload = json!({ "event": { "transaction": [{
            "signature": "5j7s6NiJS3JAkvgkoc18WVAsiSaci2pxB2A6ueCJP4tprA2TFg9wSyTLeYouxPBJEMzJinENTkpA52YStRW5Dia7",
            "owner": "7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV",
            "amount": 5
        }]}});
        let (evm, other) = candidate_addresses(&payload);
        assert!(evm.is_empty());
        assert_eq!(other, vec!["7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV".to_string()]);
    }
}
