//! KYC.
//!
//! TODO(prembly): replace the auto-approval with NIN + face verification.
//! That should record the attempt in `kyc_session_records`, move the user to
//! `review` while pending, and call `approve` only on a pass.

use crate::app_state::ApplicationState;
use anyhow::Result;
use uuid::Uuid;

/// Runs KYC for a user. For now every user passes immediately.
pub async fn run(state: &ApplicationState, user_id: Uuid) -> Result<()> {
    approve(state, user_id).await
}

/// Marks the user verified and activates their rule, atomically.
async fn approve(state: &ApplicationState, user_id: Uuid) -> Result<()> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE users SET verified = 'verified', updated_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE rules SET enabled = true, updated_at = now() WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
