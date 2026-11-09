//! Attempt outcomes joined to retained access and route-selection evidence.
use std::collections::BTreeMap;

use provenant_records::{AttemptFact, AttemptOutcome};
use serde::Serialize;

/// Values read from the immutable request and attempt receipt. Missing receipts
/// remain unknown; a requested tier alone does not prove random assignment.
pub struct Evidence {
    pub fact: AttemptFact,
    pub access: Option<String>,
    pub pool: Option<String>,
    pub partial: bool,
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub route: String,
    pub access: String,
    pub effort: String,
    pub pool: Option<String>,
    pub comparison: &'static str,
    pub cost_basis: &'static str,
    pub samples: u64,
    pub successes: u64,
    pub success_rate: Option<f64>,
    pub success_interval: Option<crate::suggest::Interval>,
    pub cost_per_success_microdollars: Option<f64>,
    pub time_per_success_ms: Option<f64>,
    pub missing_cost: u64,
    pub missing_time: u64,
}

/// A pool receipt can also describe a preference override or availability
/// fallback. Only the original weighted hash draw supports this comparison.
pub fn hash_assigned(selection: &provenant_records::RouteSelectionReceipt) -> bool {
    selection.total_weight > 0
        && selection.redraws.is_empty()
        && selection
            .extra
            .get("selection_rule")
            .and_then(serde_json::Value::as_str)
            == Some("weighted_draw")
}

/// Cost and time include failed attempts: dividing only successful attempts'
/// spend would hide the cost of failures. Success means a successful terminal
/// settlement with no retained partial-completion signal.
pub fn report(
    evidence: impl IntoIterator<Item = Evidence>,
    prices: &crate::PriceTable,
) -> Result<Vec<Row>, crate::TelemetryError> {
    let mut groups = BTreeMap::<_, Vec<Evidence>>::new();
    for (index, item) in evidence.into_iter().enumerate() {
        if index as u64 >= crate::MAX_ATTEMPTS {
            return Err(crate::TelemetryError::TooLarge("100,000 selected attempts"));
        }
        let route = item.fact.resolved_route.as_ref();
        let provider = route
            .and_then(|r| r.provider.as_deref())
            .unwrap_or("unknown");
        let model = route.and_then(|r| r.model.as_deref()).unwrap_or("unknown");
        let route_name = if model.starts_with(&format!("{provider}/")) {
            model.to_owned()
        } else {
            format!("{provider}/{model}")
        };
        let effort = route
            .and_then(|r| r.effort.clone())
            .unwrap_or_else(|| "unknown".into());
        let key = (
            route_name,
            item.access.clone().unwrap_or_else(|| "unknown".into()),
            effort,
            item.pool.clone(),
        );
        groups.entry(key).or_default().push(item);
        if groups.len() as u64 > crate::MAX_GROUPS {
            return Err(crate::TelemetryError::TooLarge("10,000 groups"));
        }
    }
    groups
        .into_iter()
        .map(|((route, access, effort, pool), items)| {
            let samples = items.len() as u64;
            let successes = items
                .iter()
                .filter(|item| {
                    item.fact.terminal_state == AttemptOutcome::Succeeded && !item.partial
                })
                .count() as u64;
            let mut time = 0u64;
            let mut missing_time = 0;
            for item in &items {
                if let Some(ms) = item.fact.provider_ms {
                    time = time
                        .checked_add(ms)
                        .ok_or(crate::TelemetryError::Overflow)?;
                } else {
                    missing_time += 1;
                }
            }
            let totals = crate::stats(
                items.into_iter().map(|item| item.fact),
                prices,
                crate::Window::default(),
            )?
            .totals;
            Ok(Row {
                route,
                access,
                effort,
                comparison: if pool.is_some() {
                    "hash_assigned_pool"
                } else {
                    "observational_not_hash_assigned"
                },
                pool,
                cost_basis: totals.cost.cost_basis,
                samples,
                successes,
                success_rate: (samples > 0).then(|| successes as f64 / samples as f64),
                success_interval: crate::suggest::wilson(successes, samples),
                cost_per_success_microdollars: totals
                    .cost
                    .complete
                    .filter(|_| successes > 0)
                    .map(|cost| cost.microdollars as f64 / successes as f64),
                time_per_success_ms: (successes > 0 && missing_time == 0)
                    .then(|| time as f64 / successes as f64),
                missing_cost: totals.cost.missing_attempts,
                missing_time,
            })
        })
        .collect()
}
