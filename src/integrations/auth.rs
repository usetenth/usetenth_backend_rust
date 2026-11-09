
use anyhow::{Context, Result};
use base64::{
    engine::general_purpose::URL_SAFE_NO_PAD,
    Engine as _,
};
use p256::ecdsa::{
    signature::Signer,
    Signature,
    SigningKey,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct PrivateKey {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
    pub d: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrueMarketApiKey {
    pub key_id: String,
    pub label: String,
    pub fingerprint: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub private_key: PrivateKey,
}

#[derive(Debug, Serialize)]
pub struct MintTokenRequest {
    pub key_id: String,
    pub timestamp: i64,
    pub signature: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JWTToken {
    pub access_token: String,
}

impl TrueMarketApiKey {
    pub fn signing_key(&self) -> Result<SigningKey> {
        anyhow::ensure!(
            self.private_key.kty == "EC"
                && self.private_key.crv == "P-256",
            "Expected an EC P-256 API key"
        );

        let decoded = URL_SAFE_NO_PAD
            .decode(&self.private_key.d)
            .context("Failed to decode API private key")?;

        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!(
                "P-256 private key must be 32 bytes"
            ))?;

        SigningKey::from_bytes((&bytes).into())
            .map_err(|_| anyhow::anyhow!(
                "Invalid P-256 private key"
            ))
    }

    pub fn sign_auth_challenge(
        &self,
        timestamp: i64,
    ) -> Result<String> {
        let key = self.signing_key()?;

        let message = format!("{}.{}", self.key_id, timestamp);

        // ECDSA signatures are encoded as fixed-width r || s here.
        let signature: Signature = key.sign(message.as_bytes());

        Ok(URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }
}
