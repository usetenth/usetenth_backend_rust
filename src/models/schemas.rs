use serde::{Deserialize, Serialize};
use sqlx::Type;

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "verification_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum VerificationStatus { Unverified, Review, Verified }

#[derive(Serialize, Deserialize, Clone, Debug, Type, PartialEq, Eq)]
#[sqlx(type_name = "investment_plan", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum InvestmentPlan { Steady, Growth, Tech, Custom }
