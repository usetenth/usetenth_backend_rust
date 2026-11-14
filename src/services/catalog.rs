use crate::app_state::ApplicationState;
use crate::integrations::types::{AssetClass, AssetStatus};
use crate::models::schemas::InvestmentPlan;
use anyhow::Result;

// Stablecoins are never "tradable" at the gateway, so withdrawals are judged by stable plus a network the wallets use.
pub const WITHDRAWABLE: &str = "stable AND chain = ANY(ARRAY['solana', 'base'])";

// (ticker, weight in basis points); tickers True Markets does not list are skipped and the rest are scaled to 100%.
const STEADY: &[(&str, i32)] = &[("AAPL", 2000), ("MSFT", 2000), ("GOOGL", 1500), ("AMZN", 1500), ("JNJ", 1000), ("KO", 1000), ("V", 1000)];
const GROWTH: &[(&str, i32)] = &[("NVDA", 2500), ("AMZN", 2000), ("TSLA", 1500), ("META", 1500), ("NFLX", 1500), ("AMD", 1000)];
const TECH: &[(&str, i32)] = &[("NVDA", 2000), ("AAPL", 1500), ("MSFT", 1500), ("GOOGL", 1500), ("ORCL", 1000), ("AVGO", 1000), ("CRM", 1000), ("ADBE", 500)];

pub fn presets() -> [(InvestmentPlan, &'static [(&'static str, i32)]); 3] {
    [(InvestmentPlan::Steady, STEADY), (InvestmentPlan::Growth, GROWTH), (InvestmentPlan::Tech, TECH)]
}

/// Copies the gateway's asset list into `assets`; returns (stored, tradable).
pub async fn sync_assets(state: &ApplicationState) -> Result<(usize, usize)> {
    let list = state.tm_client.get_asset_list().await?;
    if list.pagination.as_ref().and_then(|p| p.next_cursor.as_ref()).is_some() {
        eprintln!("The gateway has more assets than one page; only the first page was synced");
    }
    let (mut synced, mut tradable) = (0, 0);
    for item in list.data {
        let (Some(id), Some(symbol)) = (item.id, item.symbol.clone()) else { continue };
        let is_tradable =
            item.tradeable == Some(true) && item.status == Some(AssetStatus::Active) && item.is_active != Some(false);
        let class = match item.asset_class {
            Some(AssetClass::Stock) => "stock",
            Some(AssetClass::Crypto) => "crypto",
            None => "",
        };
        sqlx::query(
            "INSERT INTO assets (asset_key, symbol, name, chain, address, asset_class, tradable, stable, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
             ON CONFLICT (asset_key) DO UPDATE SET symbol = EXCLUDED.symbol, name = EXCLUDED.name, chain = EXCLUDED.chain,
                 address = EXCLUDED.address, asset_class = EXCLUDED.asset_class, tradable = EXCLUDED.tradable,
                 stable = EXCLUDED.stable, updated_at = now()",
        )
        .bind(id.to_string())
        .bind(&symbol)
        .bind(item.name.unwrap_or_else(|| symbol.clone()))
        .bind(item.chain)
        .bind(item.address)
        .bind(class)
        .bind(is_tradable)
        .bind(item.stable.unwrap_or(false))
        .execute(&state.pool)
        .await?;
        synced += 1;
        tradable += is_tradable as usize;
    }
    Ok((synced, tradable))
}

// Orders listings of one company: Coinbase tokens on Base, then Solana, then the exchange, then Robinhood Chain.
const LISTING_RANK: &str = "CASE a.chain WHEN 'base' THEN 0 WHEN 'solana' THEN 1 ELSE CASE WHEN a.chain IS NULL THEN 2 ELSE 3 END END";

/// One row per company, using its best listing; (asset_key, symbol, name, asset_class).
pub async fn listable(pool: &sqlx::PgPool) -> Result<Vec<(String, String, String, Option<String>)>> {
    Ok(sqlx::query_as(&format!(
        "SELECT asset_key, symbol, name, asset_class FROM (
             SELECT DISTINCT ON (lower(a.name), a.asset_class) a.asset_key, a.symbol, a.name, a.asset_class
             FROM assets a WHERE a.tradable AND NOT a.stable
             ORDER BY lower(a.name), a.asset_class, {LISTING_RANK}, a.asset_key
         ) best ORDER BY lower(name)"
    ))
    .fetch_all(pool)
    .await?)
}

/// The settlement stablecoin on the same network as `base_key`, falling back to the exchange one.
pub async fn quote_asset_for(pool: &sqlx::PgPool, base_key: &str, symbols: &[String]) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT q.asset_key FROM assets b JOIN assets q ON q.stable AND upper(q.symbol) = ANY($2)
         WHERE b.asset_key = $1
         ORDER BY (q.chain IS NOT DISTINCT FROM b.chain) DESC, (q.chain IS NULL) DESC, q.asset_key LIMIT 1",
    )
    .bind(base_key)
    .bind(symbols)
    .fetch_optional(pool)
    .await?)
}

/// Scales weights so they add to exactly 10,000, with the rounding leftover going to the heaviest.
pub fn normalize_bps(weights: &[(String, i32)]) -> Vec<(String, i32)> {
    let total: i64 = weights.iter().map(|(_, w)| *w as i64).sum();
    if total <= 0 {
        return vec![];
    }
    let mut scaled: Vec<(String, i32)> = weights.iter().map(|(k, w)| (k.clone(), (*w as i64 * 10_000 / total) as i32)).collect();
    let leftover = 10_000 - scaled.iter().map(|(_, w)| *w).sum::<i32>();
    if let Some(heaviest) = (0..scaled.len()).max_by_key(|i| (scaled[*i].1, std::cmp::Reverse(*i))) {
        scaled[heaviest].1 += leftover;
    }
    scaled
}

pub struct SeedReport {
    pub plan: &'static str,
    pub used: Vec<String>,
    pub missing: Vec<String>,
}

/// Fills the steady, growth and tech mixes from the preset tickers that are tradable stocks.
pub async fn seed_plans(state: &ApplicationState) -> Result<Vec<SeedReport>> {
    let mut reports = vec![];
    for (plan, tickers) in presets() {
        let names: Vec<String> = tickers.iter().map(|(t, _)| t.to_string()).collect();
        // A ticker may be listed as T, TC (Coinbase) or TX (xStocks); the best listing wins.
        let found: Vec<(String, String)> = sqlx::query_as(&format!(
            "SELECT DISTINCT ON (t.ticker) t.ticker, a.asset_key FROM unnest($1::text[]) AS t(ticker)
             JOIN assets a ON a.asset_class = 'stock' AND a.tradable AND NOT a.stable
                           AND upper(a.symbol) IN (t.ticker, t.ticker || 'C', t.ticker || 'X')
             ORDER BY t.ticker, {LISTING_RANK}, a.asset_key"
        ))
        .bind(&names)
        .fetch_all(&state.pool)
        .await?;
        let weights: Vec<(String, i32)> = tickers
            .iter()
            .filter_map(|(t, w)| found.iter().find(|(s, _)| s == t).map(|(_, key)| (key.clone(), *w)))
            .collect();
        let used: Vec<String> = tickers.iter().filter(|(t, _)| found.iter().any(|(s, _)| s == t)).map(|(t, _)| t.to_string()).collect();
        let missing: Vec<String> = tickers.iter().filter(|(t, _)| !found.iter().any(|(s, _)| s == t)).map(|(t, _)| t.to_string()).collect();

        let mut tx = state.pool.begin().await?;
        sqlx::query("DELETE FROM plan_allocations WHERE plan = $1").bind(&plan).execute(&mut *tx).await?;
        for (key, bps) in normalize_bps(&weights) {
            sqlx::query("INSERT INTO plan_allocations (plan, asset_key, weight_bps) VALUES ($1, $2, $3)")
                .bind(&plan)
                .bind(key)
                .bind(bps)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        let name = match plan {
            InvestmentPlan::Steady => "steady",
            InvestmentPlan::Growth => "growth",
            _ => "tech",
        };
        reports.push(SeedReport { plan: name, used, missing });
    }
    Ok(reports)
}

/// `cargo run -- seed`: loads the asset list, then fills the three mixes, and says what it could not find.
pub async fn run_seed(state: &ApplicationState) -> Result<()> {
    let (synced, tradable) = sync_assets(state).await?;
    println!("Synced {synced} assets ({tradable} tradable)");
    for r in seed_plans(state).await? {
        println!("{}: {} stocks ({})", r.plan, r.used.len(), r.used.join(", "));
        if !r.missing.is_empty() {
            println!("  not found at True Markets: {}", r.missing.join(", "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pairs: &[(&str, i32)]) -> Vec<(String, i32)> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn the_preset_mixes_each_add_to_exactly_one_hundred_percent() {
        for (plan, tickers) in presets() {
            assert_eq!(tickers.iter().map(|(_, w)| *w).sum::<i32>(), 10_000, "{plan:?}");
        }
    }

    #[test]
    fn weights_are_scaled_to_a_full_tenthousand() {
        let out = normalize_bps(&w(&[("a", 2000), ("b", 1000)]));
        assert_eq!(out, w(&[("a", 6667), ("b", 3333)]));
        assert_eq!(out.iter().map(|(_, v)| v).sum::<i32>(), 10_000);
        let odd = normalize_bps(&w(&[("a", 1), ("b", 1), ("c", 1)]));
        assert_eq!(odd.iter().map(|(_, v)| v).sum::<i32>(), 10_000);
        assert_eq!(normalize_bps(&w(&[("only", 123)])), w(&[("only", 10_000)]));
        assert!(normalize_bps(&[]).is_empty());
    }
}
