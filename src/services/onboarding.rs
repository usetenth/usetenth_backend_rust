use crate::app_state::ApplicationState;
use crate::integrations::types::{ChainFamily, CreateGatewayUserRequest};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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

// Creates the user on first contact, refreshes their username and returns their stage.
pub async fn ensure_user(
    state: &ApplicationState,
    telegram_id: i64,
    username: Option<&str>,
) -> Result<Onboarding> {
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

    let stage = stage_from(&row);
    Ok(Onboarding {
        user_id: row.id,
        stage,
    })
}

fn stage_from(row: &Row) -> Stage {
    if !row.has_rule {
        Stage::NeedsRule
    } else if row.gateway_user_id.is_none() {
        Stage::NeedsGateway
    } else if row.verified != VerificationStatus::Verified {
        Stage::NeedsKyc
    } else {
        Stage::Active
    }
}

// Where an existing user is in onboarding. Used by the mobile app, which knows the user id.
pub async fn stage_of(state: &ApplicationState, user_id: Uuid) -> Result<Stage> {
    let row: Row = sqlx::query_as(
        "SELECT u.id, u.verified, u.gateway_user_id,
                EXISTS (SELECT 1 FROM rules r WHERE r.user_id = u.id) AS has_rule
         FROM users u WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(stage_from(&row))
}

// Runs every automatic step the user is ready for; a failed gateway call leaves them put, so calling again retries.
pub async fn advance(state: &ApplicationState, user_id: Uuid) -> Result<Stage> {
    for _ in 0..4 {
        match stage_of(state, user_id).await? {
            Stage::NeedsGateway => {
                let gateway_user_id = provision_gateway_user(state, user_id).await?;
                set_gateway_user_id(state, user_id, &gateway_user_id).await?;
            }
            Stage::NeedsKyc if state.config.kyc.mode == crate::config::KycMode::Auto => {
                crate::services::kyc::auto_approve(state, user_id).await?
            }
            stage => return Ok(stage),
        }
    }
    stage_of(state, user_id).await
}

// Saves the tenth; a new rule starts disabled and changing one keeps its enabled flag.
pub async fn save_rule(
    state: &ApplicationState,
    user_id: Uuid,
    percent: i16,
    plan: &InvestmentPlan,
) -> Result<()> {
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

// Safe to retry: the signer key is idempotent, and a gateway rejection surfaces so the bot offers a retry.
pub async fn provision_gateway_user(state: &ApplicationState, user_id: Uuid) -> Result<String> {
    let signer_public_key = state.signing.ensure_user_key(user_id).await?;
    let request = CreateGatewayUserRequest {
        external_ref_id: user_id.to_string(),
        signer_public_key,
    };
    let response = state
        .tm_client
        .create_user(&state.config.true_market_organization_id, &request)
        .await?;

    for wallet in &response.wallets {
        let network = match wallet.chain_family {
            ChainFamily::Evm => "evm",
            ChainFamily::Solana => "solana",
        };
        sqlx::query(
            "INSERT INTO wallets (user_id, address, network) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, network) DO NOTHING",
        )
        .bind(user_id)
        .bind(&wallet.address)
        .bind(network)
        .execute(&state.pool)
        .await?;
    }
    Ok(response.user_id)
}

pub async fn set_gateway_user_id(
    state: &ApplicationState,
    user_id: Uuid,
    gateway_user_id: &str,
) -> Result<()> {
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

    fn row(has_rule: bool, gateway: Option<&str>, verified: VerificationStatus) -> Row {
        Row { id: Uuid::nil(), verified, gateway_user_id: gateway.map(String::from), has_rule }
    }

    #[test]
    fn the_stage_follows_the_first_missing_step() {
        use VerificationStatus::*;
        assert_eq!(stage_from(&row(false, None, Unverified)), Stage::NeedsRule);
        assert_eq!(stage_from(&row(false, Some("gw"), Verified)), Stage::NeedsRule, "the rule always comes first");
        assert_eq!(stage_from(&row(true, None, Unverified)), Stage::NeedsGateway);
        assert_eq!(stage_from(&row(true, Some("gw"), Unverified)), Stage::NeedsKyc);
        assert_eq!(stage_from(&row(true, Some("gw"), Review)), Stage::NeedsKyc, "review is not verified");
        assert_eq!(stage_from(&row(true, Some("gw"), Verified)), Stage::Active);
    }

    #[test]
    fn stages_serialize_in_snake_case() {
        assert_eq!(serde_json::to_string(&Stage::NeedsKyc).unwrap(), "\"needs_kyc\"");
        assert_eq!(serde_json::to_string(&Stage::NeedsGateway).unwrap(), "\"needs_gateway\"");
    }

    #[test]
    fn every_offered_percentage_parses_and_nothing_else() {
        for p in PERCENTAGES {
            assert_eq!(parse_percentage(&p.to_string()), Some(p));
        }
        assert_eq!(parse_percentage(""), None);
        assert_eq!(parse_percentage("10.5"), None);
    }

    #[test]
    fn plan_names_are_case_sensitive_and_never_include_custom() {
        assert_eq!(parse_plan("growth"), Some(InvestmentPlan::Growth));
        assert_eq!(parse_plan("tech"), Some(InvestmentPlan::Tech));
        assert_eq!(parse_plan("Steady"), None);
    }
}
