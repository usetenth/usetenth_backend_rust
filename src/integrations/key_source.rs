use super::auth::TrueMarketApiKey;
use crate::config::{AppConfig, AppMode, TurnkeyConfig};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hpke::{
    Deserializable, Kem as KemTrait, OpModeR, Serializable,
    aead::AesGcm256, kdf::HkdfSha256, kem::DhP256HkdfSha256,
};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde::Deserialize;
use serde_json::json;

type Kem = DhP256HkdfSha256;

pub async fn load_api_key(config: &AppConfig) -> Result<TrueMarketApiKey> {
    match config.app_mode {
        AppMode::Production => {
            let tk = config.turnkey.as_ref().context(
                "APP_MODE=production requires TURNKEY_* settings; refusing to fall back to a key file",
            )?;
            load_from_turnkey(tk).await
        }
        AppMode::Development => {
            let path = config
                .true_market_key_file_path
                .as_deref()
                .context("TM_KEY_FILE must be set when APP_MODE=development")?;
            load_from_file(path)
        }
    }
}

fn load_from_file(path: &str) -> Result<TrueMarketApiKey> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read True Markets key file at {path}"))?;
    serde_json::from_str(&contents).context("True Markets key file is not the expected JSON")
}

#[derive(Deserialize)]
struct ExportResponse {
    activity: Activity,
}
#[derive(Deserialize)]
struct Activity {
    status: String,
    result: Option<ActivityResult>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityResult {
    export_secrets_result: Option<ExportSecretsResult>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportSecretsResult {
    secret_payloads: Vec<String>,
}
// Enclave -> client message (`ServerSendMsg`). Field names follow Turnkey's
// SDK convention; verify against a live response.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerSendMsg {
    encapped_public: String,
    ciphertext: String,
}

async fn load_from_turnkey(tk: &TurnkeyConfig) -> Result<TrueMarketApiKey> {
    // Single-use target key pair for the HPKE transfer.
    let mut rng = rand::thread_rng();
    let (target_sk, target_pk) = Kem::gen_keypair(&mut rng);
    let target_pk_bytes = target_pk.to_bytes(); // uncompressed SEC1 point
    let target_pk_hex = hex::encode(&target_pk_bytes);

    let body = json!({
        "type": "ACTIVITY_TYPE_EXPORT_SECRETS",
        "timestampMs": chrono::Utc::now().timestamp_millis().to_string(),
        "organizationId": tk.organization_id,
        "parameters": { "secrets": [{
            "secretId": tk.secret_id,
            "targetPublicKey": target_pk_hex,
            "encryptionSuite": "TRANSPORT_ENCRYPTION_SUITE_ENCLAVE_ENCRYPT_V1",
        }]}
    })
    .to_string();

    let stamp = stamp(&body, &tk.api_public_key, &tk.api_private_key)?;
    let url = format!(
        "{}/public/v1/submit/export_secrets",
        tk.base_url.trim_end_matches('/')
    );
    let resp = reqwest::Client::new()
        .post(url)
        .header("X-Stamp", stamp)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .context("Failed to call Turnkey export_secrets")?
        .error_for_status()
        .context("Turnkey rejected export_secrets (check policy / credentials)")?
        .json::<ExportResponse>()
        .await
        .context("Unexpected Turnkey export_secrets response")?;

    if resp.activity.status != "ACTIVITY_STATUS_COMPLETED" {
        bail!(
            "Turnkey export did not complete (status {}); a policy may require approval",
            resp.activity.status
        );
    }
    let payload = resp
        .activity
        .result
        .and_then(|r| r.export_secrets_result)
        .and_then(|r| r.secret_payloads.into_iter().next())
        .context("Turnkey response contained no secret payload")?;

    let msg: ServerSendMsg =
        serde_json::from_str(&payload).context("Unexpected Turnkey secret payload format")?;
    let encapped = hex::decode(&msg.encapped_public)?;
    let ciphertext = hex::decode(&msg.ciphertext)?;

    // AAD = encapped public key || receiver (target) public key.
    let mut aad = encapped.clone();
    aad.extend_from_slice(&target_pk_bytes);

    let encapped_key = <Kem as KemTrait>::EncappedKey::from_bytes(&encapped)
        .map_err(|e| anyhow::anyhow!("Invalid encapsulated key: {e}"))?;
    let plaintext = hpke::single_shot_open::<AesGcm256, HkdfSha256, Kem>(
        &OpModeR::Base,
        &target_sk,
        &encapped_key,
        b"turnkey_hpke",
        &ciphertext,
        &aad,
    )
    .map_err(|e| anyhow::anyhow!("Failed to decrypt Turnkey secret: {e}"))?;

    serde_json::from_slice(&plaintext)
        .context("Turnkey secret is not the expected True Markets key JSON")
}

// `X-Stamp` header: base64url(JSON{publicKey, scheme, signature}) where the
// signature is a hex DER ECDSA P-256/SHA-256 signature over the body.
fn stamp(body: &str, api_public_key_hex: &str, api_private_key_hex: &str) -> Result<String> {
    let sk_bytes = hex::decode(api_private_key_hex).context("TURNKEY_API_PRIVATE_KEY must be hex")?;
    let sk = SigningKey::from_slice(&sk_bytes).context("Invalid Turnkey API private key")?;
    let sig: Signature = sk.sign(body.as_bytes());
    let stamp = json!({
        "publicKey": api_public_key_hex,
        "scheme": "SIGNATURE_SCHEME_TK_API_P256",
        "signature": hex::encode(sig.to_der().as_bytes()),
    });
    Ok(URL_SAFE_NO_PAD.encode(stamp.to_string()))
}
