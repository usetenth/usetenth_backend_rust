use super::{
    IntentKind, PolicyError,
    test_support::{ACTIVITY, insert_user, limits, payload, payload_of_type, service},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::task::JoinSet;

fn policy_error(error: &anyhow::Error) -> &PolicyError {
    error.downcast_ref::<PolicyError>().expect("expected a policy error")
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn intent_status(pool: &PgPool, id: uuid::Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM signing_intents WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn key_creation_is_idempotent_and_stores_only_wrapped_key(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;

    let first = svc.ensure_user_key(user).await.unwrap();
    let second = svc.ensure_user_key(user).await.unwrap();

    assert_eq!(first, second);
    assert_eq!(first.len(), 66);
    assert_eq!(count(&pool, "SELECT count(*) FROM signer_keys").await, 1);
    let (wrapped_len, version): (i32, i32) =
        sqlx::query_as("SELECT length(wrapped_key), key_version FROM signer_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(wrapped_len, 32 + 16);
    assert_eq!(version, 1);
}

#[sqlx::test]
async fn concurrent_key_creation_yields_one_key(pool: PgPool) {
    let svc = Arc::new(service(&pool, limits(50, 100)).await);
    let user = insert_user(&pool).await;

    let mut tasks = JoinSet::new();
    for _ in 0..8 {
        let svc = svc.clone();
        tasks.spawn(async move { svc.ensure_user_key(user).await.unwrap() });
    }
    let mut keys = Vec::new();
    while let Some(result) = tasks.join_next().await {
        keys.push(result.unwrap());
    }

    assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(count(&pool, "SELECT count(*) FROM signer_keys").await, 1);
}

#[sqlx::test]
async fn keys_require_an_existing_user(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    assert!(svc.ensure_user_key(uuid::Uuid::new_v4()).await.is_err());
}

#[sqlx::test]
async fn intents_are_idempotent_per_order_and_size_checked(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;

    let first = svc.create_intent(user, "order-1", IntentKind::Buy, "10.50").await.unwrap();
    let again = svc.create_intent(user, "order-1", IntentKind::Buy, "10.50").await.unwrap();
    assert_eq!(first, again);

    assert!(svc.create_intent(user, "order-1", IntentKind::Buy, "11").await.is_err());
    let too_big = svc.create_intent(user, "order-2", IntentKind::Buy, "50.01").await.unwrap_err();
    assert_eq!(policy_error(&too_big), &PolicyError::OrderTooLarge);
    assert!(svc.create_intent(user, "order-3", IntentKind::Buy, "0").await.is_err());
    assert_eq!(count(&pool, "SELECT count(*) FROM signing_intents").await, 1);
}

#[sqlx::test]
async fn a_valid_intent_signs_exactly_once(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    let public_key = svc.ensure_user_key(user).await.unwrap();
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();

    let stamps = svc.sign_intent(intent, &[payload("a"), payload("b")]).await.unwrap();
    assert_eq!(stamps.len(), 2);
    for stamp in &stamps {
        let json: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(stamp).unwrap()).unwrap();
        assert_eq!(json["publicKey"], public_key);
        assert_eq!(json["scheme"], "SIGNATURE_SCHEME_TK_API_P256");
    }
    assert_eq!(intent_status(&pool, intent).await, "signed");

    let replay = svc.sign_intent(intent, &[payload("a")]).await.unwrap_err();
    assert_eq!(policy_error(&replay), &PolicyError::IntentNotPending);
    assert_eq!(count(&pool, "SELECT count(*) FROM signing_audit WHERE outcome = 'signed'").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM signing_audit WHERE outcome = 'rejected'").await, 1);
}

#[sqlx::test]
async fn concurrent_signing_of_one_intent_succeeds_once(pool: PgPool) {
    let svc = Arc::new(service(&pool, limits(50, 100)).await);
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();

    let mut tasks = JoinSet::new();
    for _ in 0..6 {
        let svc = svc.clone();
        tasks.spawn(async move { svc.sign_intent(intent, &[payload("a")]).await.is_ok() });
    }
    let mut successes = 0;
    while let Some(result) = tasks.join_next().await {
        successes += usize::from(result.unwrap());
    }
    assert_eq!(successes, 1);
}

#[sqlx::test]
async fn expired_intents_are_rejected(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();
    sqlx::query("UPDATE signing_intents SET expires_at = now() - interval '1 minute'")
        .execute(&pool)
        .await
        .unwrap();

    let error = svc.sign_intent(intent, &[payload("a")]).await.unwrap_err();
    assert_eq!(policy_error(&error), &PolicyError::IntentExpired);
    assert_eq!(intent_status(&pool, intent).await, "pending");
}

#[sqlx::test]
async fn daily_limit_counts_signed_orders(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();

    for order in ["o1", "o2"] {
        let intent = svc.create_intent(user, order, IntentKind::Buy, "40").await.unwrap();
        svc.sign_intent(intent, &[payload(order)]).await.unwrap();
    }
    let third = svc.create_intent(user, "o3", IntentKind::Buy, "40").await.unwrap();
    let error = svc.sign_intent(third, &[payload("o3")]).await.unwrap_err();
    assert_eq!(policy_error(&error), &PolicyError::DailyLimitExceeded);
    assert_eq!(intent_status(&pool, third).await, "pending");

    let small = svc.create_intent(user, "o4", IntentKind::Buy, "20").await.unwrap();
    assert!(svc.sign_intent(small, &[payload("o4")]).await.is_ok());
}

#[sqlx::test]
async fn daily_limit_holds_under_concurrency(pool: PgPool) {
    let svc = Arc::new(service(&pool, limits(60, 100)).await);
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();

    let mut tasks = JoinSet::new();
    for order in ["a", "b", "c", "d"] {
        let svc = svc.clone();
        tasks.spawn(async move {
            let intent = svc.create_intent(user, order, IntentKind::Buy, "60").await.unwrap();
            svc.sign_intent(intent, &[payload(order)]).await.is_ok()
        });
    }
    let mut successes = 0;
    while let Some(result) = tasks.join_next().await {
        successes += usize::from(result.unwrap());
    }
    assert_eq!(successes, 1);
}

#[sqlx::test]
async fn limits_are_per_user(pool: PgPool) {
    let svc = service(&pool, limits(50, 50)).await;
    let alice = insert_user(&pool).await;
    let bob = insert_user(&pool).await;
    for (user, order) in [(alice, "a"), (bob, "b")] {
        svc.ensure_user_key(user).await.unwrap();
        let intent = svc.create_intent(user, order, IntentKind::Buy, "50").await.unwrap();
        assert!(svc.sign_intent(intent, &[payload(order)]).await.is_ok());
    }
}

#[sqlx::test]
async fn revoked_keys_cannot_sign(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();
    svc.revoke_user_key(user).await.unwrap();

    let error = svc.sign_intent(intent, &[payload("a")]).await.unwrap_err();
    assert_eq!(policy_error(&error), &PolicyError::NoActiveKey);
}

#[sqlx::test]
async fn users_without_a_key_cannot_sign(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();

    let error = svc.sign_intent(intent, &[payload("a")]).await.unwrap_err();
    assert_eq!(policy_error(&error), &PolicyError::NoActiveKey);
}

#[sqlx::test]
async fn bad_payloads_are_rejected_and_leave_the_intent_usable(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();

    let mut tampered = payload("a");
    tampered.payload.push(' ');
    let digest_error = svc.sign_intent(intent, &[tampered]).await.unwrap_err();
    assert_eq!(policy_error(&digest_error), &PolicyError::DigestMismatch(0));

    let wrong_type = payload_of_type("ACTIVITY_TYPE_SIGN_RAW_PAYLOAD", "a");
    let type_error = svc.sign_intent(intent, &[wrong_type]).await.unwrap_err();
    assert!(matches!(policy_error(&type_error), PolicyError::ActivityTypeNotAllowed(_)));

    let forbidden = payload_of_type("ACTIVITY_TYPE_CREATE_API_KEYS", "a");
    let forbidden_error = svc.sign_intent(intent, &[forbidden]).await.unwrap_err();
    assert!(matches!(policy_error(&forbidden_error), PolicyError::ForbiddenActivityType(_)));

    assert_eq!(intent_status(&pool, intent).await, "pending");
    assert_eq!(count(&pool, "SELECT count(*) FROM signing_audit WHERE outcome = 'rejected'").await, 3);
    assert!(svc.sign_intent(intent, &[payload("a")]).await.is_ok());
}

#[sqlx::test]
async fn a_key_moved_to_another_user_row_is_unusable(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let alice = insert_user(&pool).await;
    let bob = insert_user(&pool).await;
    svc.ensure_user_key(alice).await.unwrap();
    svc.ensure_user_key(bob).await.unwrap();
    sqlx::query(
        "UPDATE signer_keys SET wrapped_key = b.wrapped_key, nonce = b.nonce
         FROM signer_keys b WHERE signer_keys.user_id = $1 AND b.user_id = $2",
    )
    .bind(alice)
    .bind(bob)
    .execute(&pool)
    .await
    .unwrap();
    let intent = svc.create_intent(alice, "order-1", IntentKind::Buy, "10").await.unwrap();

    assert!(svc.sign_intent(intent, &[payload("a")]).await.is_err());
    assert_eq!(intent_status(&pool, intent).await, "pending");
}

#[sqlx::test]
async fn unknown_intents_are_rejected_and_audited(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let error = svc.sign_intent(uuid::Uuid::new_v4(), &[payload("a")]).await.unwrap_err();
    assert_eq!(policy_error(&error), &PolicyError::IntentNotFound);
    assert_eq!(count(&pool, "SELECT count(*) FROM signing_audit WHERE outcome = 'rejected'").await, 1);
}

#[sqlx::test]
async fn only_the_allowed_activity_is_signed(pool: PgPool) {
    let svc = service(&pool, limits(50, 100)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    let intent = svc.create_intent(user, "order-1", IntentKind::Buy, "10").await.unwrap();
    assert!(svc.sign_intent(intent, &[payload_of_type(ACTIVITY, "a")]).await.is_ok());
}

#[sqlx::test]
async fn sell_and_withdraw_intents_have_their_own_ceilings(pool: PgPool) {
    use super::IntentKind::{Buy, Sell, Withdraw};
    let svc = service(&pool, limits(50, 200)).await; // sells 500 / 2000, withdrawals 100 / 200
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();

    assert!(svc.create_intent(user, "s1", Sell, "500").await.is_ok());
    assert_eq!(policy_error(&svc.create_intent(user, "s2", Sell, "500.01").await.unwrap_err()), &PolicyError::OrderTooLarge);
    assert!(svc.create_intent(user, "w1", Withdraw, "100").await.is_ok());
    assert_eq!(policy_error(&svc.create_intent(user, "w2", Withdraw, "100.01").await.unwrap_err()), &PolicyError::OrderTooLarge);
    assert_eq!(policy_error(&svc.create_intent(user, "b1", Buy, "50.01").await.unwrap_err()), &PolicyError::OrderTooLarge);
}

#[sqlx::test]
async fn daily_limits_are_counted_per_kind(pool: PgPool) {
    use super::IntentKind::{Buy, Withdraw};
    let svc = service(&pool, limits(50, 200)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();

    for order in ["w1", "w2"] {
        let intent = svc.create_intent(user, order, Withdraw, "100").await.unwrap();
        svc.sign_intent(intent, &[payload(order)]).await.unwrap();
    }
    let third = svc.create_intent(user, "w3", Withdraw, "1").await.unwrap();
    let error = svc.sign_intent(third, &[payload("w3")]).await.unwrap_err();
    assert_eq!(policy_error(&error), &PolicyError::DailyLimitExceeded);

    let buy = svc.create_intent(user, "b1", Buy, "40").await.unwrap();
    assert!(svc.sign_intent(buy, &[payload("b1")]).await.is_ok());
}

#[sqlx::test]
async fn an_order_cannot_switch_kind_or_amount(pool: PgPool) {
    use super::IntentKind::{Sell, Withdraw};
    let svc = service(&pool, limits(50, 200)).await;
    let user = insert_user(&pool).await;
    let first = svc.create_intent(user, "o1", Sell, "20").await.unwrap();
    assert_eq!(svc.create_intent(user, "o1", Sell, "20").await.unwrap(), first);
    assert!(svc.create_intent(user, "o1", Withdraw, "20").await.is_err(), "a sale cannot be re-used to authorize a withdrawal");
    assert!(svc.create_intent(user, "o1", Sell, "21").await.is_err());
}

async fn stamp_public_key(stamp: &str) -> String {
    let json: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(stamp).unwrap()).unwrap();
    json["publicKey"].as_str().unwrap().to_string()
}

#[sqlx::test]
async fn every_payload_is_stamped_with_the_users_own_key(pool: PgPool) {
    let svc = service(&pool, limits(50, 200)).await;
    let (alice, bob) = (insert_user(&pool).await, insert_user(&pool).await);
    let alice_key = svc.ensure_user_key(alice).await.unwrap();
    let bob_key = svc.ensure_user_key(bob).await.unwrap();
    assert_ne!(alice_key, bob_key, "every user has their own key");

    let intent = svc.create_intent(alice, "order-1", IntentKind::Buy, "10").await.unwrap();
    let stamps = svc.sign_intent(intent, &[payload("a"), payload("b")]).await.unwrap();
    assert_eq!(stamps.len(), 2);
    assert_ne!(stamps[0], stamps[1], "different payloads, different signatures");
    for stamp in &stamps {
        assert_eq!(stamp_public_key(stamp).await, alice_key);
    }
}

#[sqlx::test]
async fn keys_wrapped_before_a_rotation_keep_signing_and_new_keys_use_the_new_version(pool: PgPool) {
    use super::{LocalKeyWrapper, SigningService};
    let spec = |entries: &[(i32, u8)]| {
        entries.iter().map(|(v, b)| format!("{v}:{}", hex::encode([*b; 32]))).collect::<Vec<_>>().join(",")
    };
    let old = SigningService::new(pool.clone(), LocalKeyWrapper::from_spec(&spec(&[(1, 7)]), 1).unwrap(), limits(50, 200));
    let rotated = SigningService::new(pool.clone(), LocalKeyWrapper::from_spec(&spec(&[(1, 7), (2, 9)]), 2).unwrap(), limits(50, 200));

    let early = insert_user(&pool).await;
    let early_key = old.ensure_user_key(early).await.unwrap();
    let late = insert_user(&pool).await;
    rotated.ensure_user_key(late).await.unwrap();

    let versions: Vec<(i32,)> = sqlx::query_as("SELECT key_version FROM signer_keys ORDER BY created_at").fetch_all(&pool).await.unwrap();
    assert_eq!(versions, vec![(1,), (2,)]);

    let intent = rotated.create_intent(early, "order-1", IntentKind::Buy, "10").await.unwrap();
    let stamps = rotated.sign_intent(intent, &[payload("a")]).await.unwrap();
    assert_eq!(stamp_public_key(&stamps[0]).await, early_key);
    assert!(old.sign_intent(rotated.create_intent(late, "order-2", IntentKind::Buy, "10").await.unwrap(), &[payload("b")]).await.is_err(), "a service without the new master key cannot read it");
}

#[sqlx::test]
async fn the_audit_trail_records_who_and_why(pool: PgPool) {
    let svc = service(&pool, limits(50, 200)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    let ok = svc.create_intent(user, "o1", IntentKind::Buy, "10").await.unwrap();
    svc.sign_intent(ok, &[payload("a")]).await.unwrap();
    let bad = svc.create_intent(user, "o2", IntentKind::Buy, "10").await.unwrap();
    svc.sign_intent(bad, &[payload_of_type("ACTIVITY_TYPE_EXPORT_SECRETS", "x")]).await.unwrap_err();

    let rows: Vec<(Option<uuid::Uuid>, uuid::Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT user_id, intent_id, outcome, reason FROM signing_audit ORDER BY id").fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].0, rows[0].1, rows[0].2.as_str(), rows[0].3.as_deref()), (Some(user), ok, "signed", None));
    assert_eq!((rows[1].0, rows[1].1, rows[1].2.as_str()), (Some(user), bad, "rejected"));
    assert!(rows[1].3.as_deref().unwrap().contains("forbidden"));
}

#[sqlx::test]
async fn a_revoked_user_never_silently_gets_a_new_key(pool: PgPool) {
    let svc = service(&pool, limits(50, 200)).await;
    let user = insert_user(&pool).await;
    svc.ensure_user_key(user).await.unwrap();
    svc.revoke_user_key(user).await.unwrap();
    svc.revoke_user_key(user).await.unwrap();
    let revoked: i64 = count(&pool, "SELECT count(*) FROM signer_keys WHERE revoked_at IS NOT NULL").await;
    assert_eq!(revoked, 1, "revoking twice is harmless");
    assert!(svc.ensure_user_key(user).await.is_err(), "a revoked user never silently gets a new key");
    assert_eq!(count(&pool, "SELECT count(*) FROM signer_keys").await, 1);
}
