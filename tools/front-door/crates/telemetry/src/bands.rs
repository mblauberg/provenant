//! Spend by context band: each provider reply priced and grouped by the
//! context it sent (fresh input, cached reads and cache writes). Cost grows
//! with context length times turns, so this shows how much spend happens at
//! large contexts; compare windows with `--since` and `--until`.
use super::{Accumulator, Totals, priced};
use crate::{PriceTable, TelemetryError, ingest::Row, ingest::Source};
use provenant_records::Usage;
use serde::Serialize;
use std::collections::BTreeMap;

/// Lower bounds of the bands, in tokens; each band ends where the next starts.
pub const CONTEXT_BAND_FLOORS: [u64; 5] = [0, 50_000, 100_000, 200_000, 400_000];

/// One band's replies for one source.
#[derive(Clone, Debug, Serialize)]
pub struct ContextBand {
    pub source: Source,
    /// Smallest context in the band, in tokens.
    pub min_tokens: u64,
    /// First context past the band; absent for the last band.
    pub below_tokens: Option<u64>,
    /// `attempts` counts replies here; the cost is an API-equivalent estimate.
    pub totals: Totals,
}

/// The context one reply sent, in tokens.
pub fn reply_context(row: &Row) -> u64 {
    row.uncached_input
        .saturating_add(row.cache_read)
        .saturating_add(row.cache_write)
        .saturating_add(row.cache_write_1h)
}

fn floor(context: u64) -> u64 {
    CONTEXT_BAND_FLOORS
        .iter()
        .copied()
        .rfind(|floor| context >= *floor)
        .unwrap_or(0)
}

/// Replies grouped by source and context band, in source then band order.
/// Only bands with replies are listed.
pub fn context_bands(
    rows: &[Row],
    prices: &PriceTable,
) -> Result<Vec<ContextBand>, TelemetryError> {
    let mut groups = BTreeMap::<(Source, u64), Accumulator>::new();
    for row in rows {
        let reasoning = row.reasoning_tokens;
        let usage = Usage {
            uncached_input: Some(row.uncached_input),
            cache_read: Some(row.cache_read),
            cache_write: Some(row.cache_write.saturating_add(row.cache_write_1h)),
            cache_write_1h: Some(row.cache_write_1h),
            output_excluding_reasoning: Some(row.output.saturating_sub(reasoning.unwrap_or(0))),
            reasoning,
        };
        let model = if row.model.contains('/') {
            row.model.clone()
        } else {
            format!("{}/{}", row.provider, row.model)
        };
        let cost = priced(&usage, Some(&row.provider), Some(&model), prices)?;
        groups
            .entry((row.source, floor(reply_context(row))))
            .or_default()
            .record_usage(
                &usage,
                None,
                cost.numerator,
                cost.missing_cost,
                cost.subscription,
            )?;
    }
    groups
        .into_iter()
        .map(|((source, min_tokens), accumulator)| {
            Ok(ContextBand {
                source,
                min_tokens,
                below_tokens: CONTEXT_BAND_FLOORS
                    .iter()
                    .copied()
                    .find(|floor| *floor > min_tokens),
                totals: accumulator.finish()?,
            })
        })
        .collect()
}
