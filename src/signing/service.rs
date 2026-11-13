use super::{
    policy::{self, IntentKind, PayloadRef, PolicyError, SigningLimits},
    stamp::{public_key_hex, stamp_payload},
    wrap::{KeyWrapper, WrappedKey},
};
use anyhow::{Result, ensure};
use p256::ecdsa::SigningKey;
use rand::rngs::OsRng;
use sqlx::PgPool;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Debug, Clone)]
pub struct SigningPayload {
    pub digest: String,
    pub payload: String,
}

#[derive(sqlx::FromRow)]
struct IntentRow {
    user_id: Uuid,
    kind: String,
    amount_micros: i64,
    status: String,
    live: bool,
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    public_key: String,
    wrapped_key: Vec<u8>,
    nonce: Vec<u8>,
    key_version: i32,
}

pub struct SigningService<W> {
    pool: PgPool,
    wrapper: W,
    limits: SigningLimits,
}

impl<W: KeyWrapper> SigningService<W> {
    pub fn new(pool: PgPool, wrapper: W, limits: SigningLimits) -> Self {
        Self { pool, wrapper, limits }
    }

    pub async fn ensure_user_key(&self, user_id: Uuid) -> Result<String> {
        if let Some(existing) = self.active_public_key(user_id).await? {
            return Ok(existing);
        }

        let key = SigningKey::random(&mut OsRng);
        let secret = Zeroizing::new(key.to_bytes().to_vec());
        let wrapped = self.wrapper.wrap(user_id, &secret).await?;
        sqlx::query(
            "INSERT INTO signer_keys (user_id, public_key, wrapped_key, nonce, key_version)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(public_key_hex(&key))
        .bind(&wrapped.ciphertext)
        .bind(&wrapped.nonce)
        .bind(wrapped.key_version)
        .execute(&self.pool)
        .await?;

        self.active_public_key(user_id)
            .await?
            .ok_or_else(|| PolicyError::NoActiveKey.into())
    }

    pub async fn revoke_user_key(&self, user_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE signer_keys SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub fn limits(&self) -> &SigningLimits {
        &self.limits
    }

    pub async fn create_buy_intent(&self, user_id: Uuid, order_id: &str, amount_usd: &str) -> Result<Uuid> {
        self.create_intent(user_id, order_id, IntentKind::Buy, amount_usd).await
    }

    /// Records an intent to sign `amount_usd` of `kind`; repeats return it, changing amount or kind is refused.
    pub async fn create_intent(
        &self,
        user_id: Uuid,
        order_id: &str,
        kind: IntentKind,
        amount_usd: &str,
    ) -> Result<Uuid> {
        let amount = policy::parse_usd_micros(amount_usd)?;
        let (max_order, _) = kind.limits(&self.limits);
        if amount > max_order {
            return Err(PolicyError::OrderTooLarge.into());
        }

        sqlx::query(
            "INSERT INTO signing_intents (user_id, order_id, kind, amount_micros, expires_at)
             VALUES ($1, $2, $3, $4, now() + ($5::BIGINT * interval '1 second'))
             ON CONFLICT (user_id, order_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(order_id)
        .bind(kind.as_str())
        .bind(amount)
        .bind(self.limits.intent_ttl_secs)
        .execute(&self.pool)
        .await?;

        let (id, stored_amount, stored_kind): (Uuid, i64, String) = sqlx::query_as(
            "SELECT id, amount_micros, kind FROM signing_intents WHERE user_id = $1 AND order_id = $2",
        )
        .bind(user_id)
        .bind(order_id)
        .fetch_one(&self.pool)
        .await?;
        ensure!(
            stored_amount == amount && stored_kind == kind.as_str(),
            "order already has an intent with a different amount or kind"
        );
        Ok(id)
    }

    pub async fn sign_intent(
        &self,
        intent_id: Uuid,
        payloads: &[SigningPayload],
    ) -> Result<Vec<String>> {
        match self.try_sign(intent_id, payloads).await {
            Ok(stamps) => Ok(stamps),
            Err(error) => {
                self.audit_rejection(intent_id, &error.to_string()).await;
                Err(error)
            }
        }
    }

    async fn try_sign(&self, intent_id: Uuid, payloads: &[SigningPayload]) -> Result<Vec<String>> {
        let mut tx = self.pool.begin().await?;

        let intent: IntentRow = sqlx::query_as(
            "SELECT user_id, kind, amount_micros, status, (expires_at > now()) AS live
             FROM signing_intents WHERE id = $1 FOR UPDATE",
        )
        .bind(intent_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(PolicyError::IntentNotFound)?;

        if intent.status != "pending" {
            return Err(PolicyError::IntentNotPending.into());
        }
        if !intent.live {
            return Err(PolicyError::IntentExpired.into());
        }

        let key_row: KeyRow = sqlx::query_as(
            "SELECT public_key, wrapped_key, nonce, key_version
             FROM signer_keys WHERE user_id = $1 AND revoked_at IS NULL FOR UPDATE",
        )
        .bind(intent.user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(PolicyError::NoActiveKey)?;

        let signed_last_day: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(amount_micros), 0)::BIGINT FROM signing_intents
             WHERE user_id = $1 AND kind = $2 AND status = 'signed' AND signed_at > now() - interval '24 hours'",
        )
        .bind(intent.user_id)
        .bind(&intent.kind)
        .fetch_one(&mut *tx)
        .await?;

        let kind = match intent.kind.as_str() {
            "buy" => IntentKind::Buy,
            "sell" => IntentKind::Sell,
            "withdraw" => IntentKind::Withdraw,
            other => anyhow::bail!("unknown intent kind {other}"),
        };
        policy::check_amount_for(kind, &self.limits, intent.amount_micros, signed_last_day)?;
        let refs: Vec<PayloadRef<'_>> = payloads
            .iter()
            .map(|p| PayloadRef { digest: &p.digest, payload: &p.payload })
            .collect();
        policy::check_payloads(&self.limits, &refs)?;

        let secret = self
            .wrapper
            .unwrap(
                intent.user_id,
                &WrappedKey {
                    key_version: key_row.key_version,
                    nonce: key_row.nonce,
                    ciphertext: key_row.wrapped_key,
                },
            )
            .await?;
        let key = SigningKey::from_slice(&secret)
            .map_err(|_| anyhow::anyhow!("stored signer key is invalid"))?;
        ensure!(
            public_key_hex(&key) == key_row.public_key,
            "stored signer key does not match its public key"
        );

        let stamps = payloads
            .iter()
            .map(|p| stamp_payload(&key, &p.payload))
            .collect::<Result<Vec<_>>>()?;

        sqlx::query("UPDATE signing_intents SET status = 'signed', signed_at = now() WHERE id = $1")
            .bind(intent_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO signing_audit (user_id, intent_id, outcome) VALUES ($1, $2, 'signed')",
        )
        .bind(intent.user_id)
        .bind(intent_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(stamps)
    }

    async fn active_public_key(&self, user_id: Uuid) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT public_key FROM signer_keys WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    async fn audit_rejection(&self, intent_id: Uuid, reason: &str) {
        let result = sqlx::query(
            "INSERT INTO signing_audit (user_id, intent_id, outcome, reason)
             VALUES ((SELECT user_id FROM signing_intents WHERE id = $1), $1, 'rejected', $2)",
        )
        .bind(intent_id)
        .bind(reason)
        .execute(&self.pool)
        .await;
        if let Err(error) = result {
            eprintln!("signing audit write failed for intent {intent_id}: {error}");
        }
    }
}
