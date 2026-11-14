
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

#[allow(dead_code)] // Fields kept so the whole key file deserializes.
#[derive(Debug, Clone, Deserialize)]
pub struct PrivateKey {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
    pub d: String,
}

#[allow(dead_code)] // Fields kept so the whole key file deserializes.
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

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{VerifyingKey, signature::Verifier};
    use rand::rngs::OsRng;

    fn api_key(kty: &str, crv: &str, d: String) -> (TrueMarketApiKey, SigningKey) {
        let secret = SigningKey::random(&mut OsRng);
        let key = TrueMarketApiKey {
            key_id: "key-1".into(),
            label: "test".into(),
            fingerprint: "fp".into(),
            created_at: chrono::Utc::now(),
            private_key: PrivateKey { kty: kty.into(), crv: crv.into(), x: String::new(), y: String::new(), d },
        };
        (key, secret)
    }

    fn encoded(secret: &SigningKey) -> String {
        URL_SAFE_NO_PAD.encode(secret.to_bytes())
    }

    #[test]
    fn the_challenge_signature_verifies_against_the_key_id_and_timestamp() {
        let secret = SigningKey::random(&mut OsRng);
        let (key, _) = api_key("EC", "P-256", encoded(&secret));
        let signature = key.sign_auth_challenge(1_800_000_000).unwrap();

        let bytes = URL_SAFE_NO_PAD.decode(signature).unwrap();
        assert_eq!(bytes.len(), 64, "fixed-width r || s");
        let parsed = Signature::from_slice(&bytes).unwrap();
        let verifier = VerifyingKey::from(&secret);
        assert!(verifier.verify(b"key-1.1800000000", &parsed).is_ok());
        assert!(verifier.verify(b"key-1.1800000001", &parsed).is_err(), "bound to the timestamp");
    }

    #[test]
    fn only_ec_p256_keys_of_the_right_size_are_accepted() {
        let secret = SigningKey::random(&mut OsRng);
        assert!(api_key("RSA", "P-256", encoded(&secret)).0.signing_key().is_err());
        assert!(api_key("EC", "P-384", encoded(&secret)).0.signing_key().is_err());
        assert!(api_key("EC", "P-256", "not base64!!".into()).0.signing_key().is_err());
        assert!(api_key("EC", "P-256", URL_SAFE_NO_PAD.encode([1u8; 16])).0.signing_key().is_err(), "too short");
        assert!(api_key("EC", "P-256", URL_SAFE_NO_PAD.encode([0u8; 32])).0.signing_key().is_err(), "zero is not a valid scalar");
        assert!(api_key("EC", "P-256", encoded(&secret)).0.signing_key().is_ok());
    }

    #[test]
    fn a_key_file_deserializes_with_extra_fields_ignored() {
        let json = r#"{"key_id":"k","label":"l","fingerprint":"f","created_at":"2026-01-01T00:00:00Z",
            "private_key":{"kty":"EC","crv":"P-256","x":"x","y":"y","d":"d","extra":1},"unknown":true}"#;
        let key: TrueMarketApiKey = serde_json::from_str(json).unwrap();
        assert_eq!(key.key_id, "k");
        assert_eq!(key.private_key.crv, "P-256");
    }
}
