use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde::Serialize;

const SCHEME: &str = "SIGNATURE_SCHEME_TK_API_P256";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Stamp<'a> {
    public_key: &'a str,
    signature: String,
    scheme: &'static str,
}

pub fn public_key_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_encoded_point(true).as_bytes())
}

pub fn stamp_payload(key: &SigningKey, payload: &str) -> Result<String> {
    let signature: Signature = key.sign(payload.as_bytes());
    let public_key = public_key_hex(key);
    let stamp = Stamp {
        public_key: &public_key,
        signature: hex::encode(signature.to_der().as_bytes()),
        scheme: SCHEME,
    };
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&stamp)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{VerifyingKey, signature::Verifier};
    use rand::rngs::OsRng;
    use serde_json::Value;

    #[test]
    fn public_key_is_compressed_66_hex() {
        let key = SigningKey::random(&mut OsRng);
        let hex_key = public_key_hex(&key);
        assert_eq!(hex_key.len(), 66);
        assert!(hex_key.starts_with("02") || hex_key.starts_with("03"));
    }

    #[test]
    fn stamp_verifies_against_exact_payload_bytes() {
        let key = SigningKey::random(&mut OsRng);
        let payload = "{\"type\":\"ACTIVITY_TYPE_X\"}\n";
        let stamp = stamp_payload(&key, payload).unwrap();

        let json: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(stamp).unwrap()).unwrap();
        assert_eq!(json["scheme"], SCHEME);
        assert_eq!(json["publicKey"], public_key_hex(&key));

        let verifier =
            VerifyingKey::from_sec1_bytes(&hex::decode(json["publicKey"].as_str().unwrap()).unwrap())
                .unwrap();
        let der = hex::decode(json["signature"].as_str().unwrap()).unwrap();
        let signature = Signature::from_der(&der).unwrap();
        assert!(verifier.verify(payload.as_bytes(), &signature).is_ok());
        assert!(verifier.verify(b"other", &signature).is_err());
    }
}
