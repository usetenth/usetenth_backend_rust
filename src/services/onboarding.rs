use crate::app_state::ApplicationState;
use crate::models::schemas::{InvestmentPlan, VerificationStatus};
use anyhow::{Result, bail};
use uuid::Uuid;

// The only tenth percentages the bot offers.
pub const PERCENTAGES: [i16; 3] = [5, 10, 20];

pub fn parse_percentage(s: &str) -> Option<i16> {
    s.parse().ok().filter(|p| PERCENTAGES.contains(p))
}

pub fn parse_plan(s: &str) -> Option<InvestmentPlan> {
    match s {
        "steady" => Some(InvestmentPlan::Steady),
        "growth" => Some(InvestmentPlan::Growth),
        "tech" => Some(InvestmentPlan::Tech),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    NeedsRule,
    NeedsGateway,
    NeedsKyc,
    Active,
}

#[derive(Debug, Clone, Copy)]
pub struct Onboarding {
    pub user_id: Uuid,
    pub stage: Stage,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    verified: VerificationStatus,
    gateway_user_id: Option<String>,
    has_rule: bool,
}

/// Creates the user on first contact, refreshes their username, and returns
/// where they are in onboarding.
pub async fn ensure_user(state: &ApplicationState, telegram_id: i64, username: Option<&str>) -> Result<Onboarding> {
    let row: Row = sqlx::query_as(
        "WITH u AS (
             INSERT INTO users (telegram_id, username) VALUES ($1, $2)
             ON CONFLICT (telegram_id) DO UPDATE SET username = EXCLUDED.username
             RETURNING id, verified, gateway_user_id
         )
         SELECT u.id, u.verified, u.gateway_user_id,
                EXISTS (SELECT 1 FROM rules r WHERE r.user_id = u.id) AS has_rule
         FROM u",
    )
    .bind(telegram_id)
    .bind(username)
    .fetch_one(&state.pool)
    .await?;

    let stage = if !row.has_rule {
        Stage::NeedsRule
    } else if row.gateway_user_id.is_none() {
        Stage::NeedsGateway
    } else if row.verified != VerificationStatus::Verified {
        Stage::NeedsKyc
    } else {
        Stage::Active
    };
    Ok(Onboarding { user_id: row.id, stage })
}

// Saves the user's tenth. A new rule starts disabled; changing an existing
// rule leaves its enabled flag alone.
pub async fn save_rule(state: &ApplicationState, user_id: Uuid, percent: i16, plan: &InvestmentPlan) -> Result<()> {
    if !PERCENTAGES.contains(&percent) {
        bail!("unsupported tenth percentage: {percent}");
    }
    sqlx::query(
        "INSERT INTO rules (user_id, plan, percentage) VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE
         SET plan = EXCLUDED.plan, percentage = EXCLUDED.percentage, updated_at = now()",
    )
    .bind(user_id)
    .bind(plan)
    .bind(percent)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Creates the user's gateway (True Markets) account and returns its id.
///
/// TODO(gateway): call `TrueMarketClient::create_user`. Pass `user_id` as an
/// idempotency / external reference so a retry can never create a second
/// account. Not wired yet, so the bot tells the user setup is pending and
/// offers a retry.
pub async fn provision_gateway_user(_state: &ApplicationState, _user_id: Uuid) -> Result<String> {
    bail!("gateway user provisioning is not wired yet")
}

pub async fn set_gateway_user_id(state: &ApplicationState, user_id: Uuid, gateway_user_id: &str) -> Result<()> {
    // Only ever set once.
    sqlx::query("UPDATE users SET gateway_user_id = $2, updated_at = now() WHERE id = $1 AND gateway_user_id IS NULL")
        .bind(user_id)
        .bind(gateway_user_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_offered_percentages_parse() {
        assert_eq!(parse_percentage("10"), Some(10));
        assert_eq!(parse_percentage("15"), None);
        assert_eq!(parse_percentage("-5"), None);
        assert_eq!(parse_percentage("abc"), None);
    }

    #[test]
    fn plans_parse() {
        assert_eq!(parse_plan("steady"), Some(InvestmentPlan::Steady));
        assert_eq!(parse_plan("custom"), None);
    }
}
