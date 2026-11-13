use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use anyhow::{Context, Result, anyhow, ensure};
use rand::{RngCore, rngs::OsRng};
use std::{collections::HashMap, future::Future};
use uuid::Uuid;
use zeroize::Zeroizing;

pub struct WrappedKey {
    pub key_version: i32,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub trait KeyWrapper: Send + Sync {
    fn wrap(
        &self,
        user_id: Uuid,
        plaintext: &[u8],
    ) -> impl Future<Output = Result<WrappedKey>> + Send;

    fn unwrap(
        &self,
        user_id: Uuid,
        wrapped: &WrappedKey,
    ) -> impl Future<Output = Result<Zeroizing<Vec<u8>>>> + Send;
}

pub struct LocalKeyWrapper {
    keys: HashMap<i32, Zeroizing<Vec<u8>>>,
    active: i32,
}

impl LocalKeyWrapper {
    pub fn from_spec(spec: &str, active: i32) -> Result<Self> {
        let mut keys = HashMap::new();
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (version, hex_key) = entry
                .split_once(':')
                .context("master key entry must be version:hex")?;
            let version: i32 = version.parse().context("master key version must be an integer")?;
            let bytes = Zeroizing::new(hex::decode(hex_key).context("master key must be hex")?);
            ensure!(bytes.len() == 32, "master key must be 32 bytes");
            ensure!(keys.insert(version, bytes).is_none(), "duplicate master key version {version}");
        }
        ensure!(keys.contains_key(&active), "active master key version {active} is not configured");
        Ok(Self { keys, active })
    }

    pub fn from_env() -> Result<Self> {
        let spec = std::env::var("SIGNING_MASTER_KEYS").context("SIGNING_MASTER_KEYS is not set")?;
        let active = std::env::var("SIGNING_ACTIVE_KEY_VERSION")
            .context("SIGNING_ACTIVE_KEY_VERSION is not set")?
            .parse()
            .context("SIGNING_ACTIVE_KEY_VERSION must be an integer")?;
        Self::from_spec(&spec, active)
    }

    fn cipher(&self, version: i32) -> Result<Aes256Gcm> {
        let key = self
            .keys
            .get(&version)
            .ok_or_else(|| anyhow!("master key version {version} is not configured"))?;
        Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key)))
    }
}

fn aad(user_id: Uuid, version: i32) -> Vec<u8> {
    let mut aad = user_id.as_bytes().to_vec();
    aad.extend_from_slice(&version.to_be_bytes());
    aad
}

impl KeyWrapper for LocalKeyWrapper {
    async fn wrap(&self, user_id: Uuid, plaintext: &[u8]) -> Result<WrappedKey> {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher(self.active)?
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload { msg: plaintext, aad: &aad(user_id, self.active) },
            )
            .map_err(|_| anyhow!("key wrapping failed"))?;
        Ok(WrappedKey { key_version: self.active, nonce: nonce.to_vec(), ciphertext })
    }

    async fn unwrap(&self, user_id: Uuid, wrapped: &WrappedKey) -> Result<Zeroizing<Vec<u8>>> {
        ensure!(wrapped.nonce.len() == 12, "stored nonce has the wrong length");
        let plaintext = self
            .cipher(wrapped.key_version)?
            .decrypt(
                Nonce::from_slice(&wrapped.nonce),
                Payload { msg: &wrapped.ciphertext, aad: &aad(user_id, wrapped.key_version) },
            )
            .map_err(|_| anyhow!("key unwrapping failed"))?;
        Ok(Zeroizing::new(plaintext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(entries: &[(i32, u8)]) -> String {
        entries
            .iter()
            .map(|(v, b)| format!("{v}:{}", hex::encode([*b; 32])))
            .collect::<Vec<_>>()
            .join(",")
    }

    #[tokio::test]
    async fn round_trips() {
        let wrapper = LocalKeyWrapper::from_spec(&spec(&[(1, 7)]), 1).unwrap();
        let user = Uuid::new_v4();
        let wrapped = wrapper.wrap(user, b"secret").await.unwrap();
        assert_ne!(wrapped.ciphertext, b"secret");
        assert_eq!(wrapper.unwrap(user, &wrapped).await.unwrap().as_slice(), b"secret");
    }

    #[tokio::test]
    async fn bound_to_user() {
        let wrapper = LocalKeyWrapper::from_spec(&spec(&[(1, 7)]), 1).unwrap();
        let wrapped = wrapper.wrap(Uuid::new_v4(), b"secret").await.unwrap();
        assert!(wrapper.unwrap(Uuid::new_v4(), &wrapped).await.is_err());
    }

    #[tokio::test]
    async fn old_versions_stay_readable_after_rotation() {
        let user = Uuid::new_v4();
        let old = LocalKeyWrapper::from_spec(&spec(&[(1, 7)]), 1).unwrap();
        let wrapped = old.wrap(user, b"secret").await.unwrap();

        let rotated = LocalKeyWrapper::from_spec(&spec(&[(1, 7), (2, 9)]), 2).unwrap();
        assert_eq!(rotated.unwrap(user, &wrapped).await.unwrap().as_slice(), b"secret");
        assert_eq!(rotated.wrap(user, b"x").await.unwrap().key_version, 2);
    }

    #[tokio::test]
    async fn tampering_is_detected() {
        let wrapper = LocalKeyWrapper::from_spec(&spec(&[(1, 7)]), 1).unwrap();
        let user = Uuid::new_v4();
        let mut wrapped = wrapper.wrap(user, b"secret").await.unwrap();
        wrapped.ciphertext[0] ^= 1;
        assert!(wrapper.unwrap(user, &wrapped).await.is_err());
    }

    #[test]
    fn rejects_bad_specs() {
        assert!(LocalKeyWrapper::from_spec(&spec(&[(1, 7)]), 2).is_err());
        assert!(LocalKeyWrapper::from_spec("1:abcd", 1).is_err());
        assert!(LocalKeyWrapper::from_spec("", 1).is_err());
    }
}
