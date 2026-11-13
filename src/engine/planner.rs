
use super::money::{percent_of, split_bps};

pub struct RuleInput {
    pub percentage: i16,
    pub minimum_micros: i64,
    pub max_per_transaction: Option<i64>,
    pub max_daily: Option<i64>,
}

pub struct Context<'a> {
    pub global_floor_micros: i64,
    pub max_user_daily_micros: Option<i64>,
    // Invested for this user in the last 24 hours, not counting skipped or failed.
    pub spent_last_day: i64,
    // (asset_key, weight in basis points) for tradable assets only.
    pub allocations: &'a [(String, i32)],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Skip(&'static str),
    Invest { invest_micros: i64, legs: Vec<(String, i64)> },
}

pub fn decide(deposit_micros: i64, rule: &RuleInput, ctx: &Context) -> Decision {
    if rule.percentage <= 0 {
        return Decision::Skip("zero_percentage");
    }
    let total_bps: i64 = ctx.allocations.iter().map(|(_, w)| *w as i64).sum();
    if ctx.allocations.is_empty() || total_bps != 10_000 {
        return Decision::Skip("allocation_unavailable");
    }

    let mut invest = percent_of(deposit_micros, rule.percentage.min(100));
    if let Some(cap) = rule.max_per_transaction {
        invest = invest.min(cap);
    }
    let daily_cap = match (rule.max_daily, ctx.max_user_daily_micros) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if let Some(cap) = daily_cap {
        let remaining = cap - ctx.spent_last_day;
        if remaining <= 0 {
            return Decision::Skip("daily_limit");
        }
        invest = invest.min(remaining);
    }

    let threshold = ctx.global_floor_micros.max(rule.minimum_micros);
    if invest <= 0 || invest < threshold {
        return Decision::Skip("below_minimum");
    }

    // A leg under the floor is not worth an order; what is left is still invested.
    let legs: Vec<(String, i64)> = split_bps(invest, ctx.allocations)
        .into_iter()
        .filter(|(_, micros)| *micros >= ctx.global_floor_micros && *micros > 0)
        .collect();
    if legs.is_empty() {
        return Decision::Skip("below_minimum");
    }
    Decision::Invest { invest_micros: legs.iter().map(|(_, v)| v).sum(), legs }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc(pairs: &[(&str, i32)]) -> Vec<(String, i32)> {
        pairs.iter().map(|(k, w)| (k.to_string(), *w)).collect()
    }
    fn rule(percentage: i16) -> RuleInput {
        RuleInput { percentage, minimum_micros: 0, max_per_transaction: None, max_daily: None }
    }
    fn ctx<'a>(allocations: &'a [(String, i32)]) -> Context<'a> {
        Context { global_floor_micros: 1_000_000, max_user_daily_micros: None, spent_last_day: 0, allocations }
    }

    #[test]
    fn invests_the_tenth_split_across_the_mix() {
        let a = alloc(&[("spy", 6000), ("qqq", 4000)]);
        assert_eq!(
            decide(100_000_000, &rule(10), &ctx(&a)),
            Decision::Invest {
                invest_micros: 10_000_000,
                legs: vec![("spy".into(), 6_000_000), ("qqq".into(), 4_000_000)]
            }
        );
    }

    #[test]
    fn skips_without_a_usable_mix() {
        assert_eq!(decide(100_000_000, &rule(10), &ctx(&[])), Decision::Skip("allocation_unavailable"));
        let short = alloc(&[("spy", 9000)]);
        assert_eq!(decide(100_000_000, &rule(10), &ctx(&short)), Decision::Skip("allocation_unavailable"));
    }

    #[test]
    fn skips_small_deposits() {
        let a = alloc(&[("spy", 10_000)]);
        assert_eq!(decide(5_000_000, &rule(10), &ctx(&a)), Decision::Skip("below_minimum"));
        assert_eq!(decide(100_000_000, &rule(0), &ctx(&a)), Decision::Skip("zero_percentage"));
    }

    #[test]
    fn respects_rule_minimum() {
        let a = alloc(&[("spy", 10_000)]);
        let r = RuleInput { minimum_micros: 20_000_000, ..rule(10) };
        assert_eq!(decide(100_000_000, &r, &ctx(&a)), Decision::Skip("below_minimum"));
    }

    #[test]
    fn caps_per_transaction_and_per_day() {
        let a = alloc(&[("spy", 10_000)]);
        let r = RuleInput { max_per_transaction: Some(5_000_000), ..rule(20) };
        assert_eq!(
            decide(100_000_000, &r, &ctx(&a)),
            Decision::Invest { invest_micros: 5_000_000, legs: vec![("spy".into(), 5_000_000)] }
        );

        let r = RuleInput { max_daily: Some(12_000_000), ..rule(20) };
        let mut c = ctx(&a);
        c.spent_last_day = 10_000_000;
        assert_eq!(
            decide(100_000_000, &r, &c),
            Decision::Invest { invest_micros: 2_000_000, legs: vec![("spy".into(), 2_000_000)] }
        );
        c.spent_last_day = 12_000_000;
        assert_eq!(decide(100_000_000, &r, &c), Decision::Skip("daily_limit"));
    }

    #[test]
    fn global_daily_limit_applies_when_rule_has_none() {
        let a = alloc(&[("spy", 10_000)]);
        let mut c = ctx(&a);
        c.max_user_daily_micros = Some(3_000_000);
        assert_eq!(
            decide(100_000_000, &rule(10), &c),
            Decision::Invest { invest_micros: 3_000_000, legs: vec![("spy".into(), 3_000_000)] }
        );
    }

    #[test]
    fn drops_legs_below_the_floor_and_reports_what_is_invested() {
        let a = alloc(&[("spy", 9500), ("tiny", 500)]);
        // 10 USD split 9.50 / 0.50: the 0.50 leg is under the 1 USD floor.
        assert_eq!(
            decide(100_000_000, &rule(10), &ctx(&a)),
            Decision::Invest { invest_micros: 9_500_000, legs: vec![("spy".into(), 9_500_000)] }
        );
    }

    #[test]
    fn percentage_above_100_is_clamped() {
        let a = alloc(&[("spy", 10_000)]);
        assert_eq!(
            decide(10_000_000, &rule(500), &ctx(&a)),
            Decision::Invest { invest_micros: 10_000_000, legs: vec![("spy".into(), 10_000_000)] }
        );
    }
}
