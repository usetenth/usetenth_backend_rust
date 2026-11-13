//! Deposits to investments: signed and limit-checked, doubt resolved at the gateway, DRY_RUN (default) never trades.

pub mod address;
pub mod money;
pub mod planner;
pub mod withdrawals;
pub mod worker;

#[cfg(test)]
pub(crate) mod worker_tests;

#[cfg(test)]
mod money_out_tests;
