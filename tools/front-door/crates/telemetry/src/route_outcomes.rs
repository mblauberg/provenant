//! Attempt outcomes joined to retained access and route-selection evidence.
use std::collections::BTreeMap;

use provenant_records::{AttemptFact, AttemptOutcome};
use provenant_routing::{ModelId, PriorKey, RoutePrior};
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
    pub samples: u64,
    pub successes: u64,
    pub success_rate: Option<f64>,
    pub success_interval: Option<crate::suggest::Interval>,
    pub time_per_success_ms: Option<f64>,
    pub missing_time: u64,
}

fn exact_route(route: &provenant_records::RouteFact) -> Option<String> {
    let (provider, model) = (route.provider.as_deref()?, route.model.as_deref()?);
    if provider == "unknown" || model == "unknown" {
        return None;
    }
    Some(if model.starts_with(&format!("{provider}/")) {
        model.to_owned()
    } else {
        format!("{provider}/{model}")
    })
}

/// Account identity evidence is independent of whether provider work finished.
/// Q1 keeps unobserved accounts unknown; it must not invent model failures.
pub fn provider_work_incomplete(signals: &[provenant_records::run::PartialSignal]) -> bool {
    signals
        .iter()
        .any(|signal| *signal != provenant_records::run::PartialSignal::IdentityUnobserved)
}

// Host stops still describe who stopped the process. A settlement declaration
// that certifies completed work supplies quality evidence without turning a
// host failure into a model failure or relabelling the stop as provider-caused.
fn has_quality_outcome(fact: &AttemptFact) -> bool {
    use provenant_records::run::{CauseSource, SettlementTerminalOutcome};
    fact.cause_source == CauseSource::Provider
        || (fact.cause_source == CauseSource::Host
            && fact.terminal_state == AttemptOutcome::Succeeded
            && fact.settlement_reason.is_some_and(|reason| {
                reason.rule().terminal_outcome == SettlementTerminalOutcome::Succeeded
            }))
}

pub fn priors(
    evidence: impl IntoIterator<Item = Evidence>,
) -> Result<Vec<RoutePrior>, crate::TelemetryError> {
    let mut groups = BTreeMap::<(ModelId, String, PriorKey), (u64, u64)>::new();
    for (index, item) in evidence.into_iter().enumerate() {
        if index as u64 >= crate::MAX_ATTEMPTS {
            return Err(crate::TelemetryError::TooLarge("100,000 selected attempts"));
        }
        if !has_quality_outcome(&item.fact) {
            continue;
        }
        let Some(pool) = item.pool else {
            continue;
        };
        if item.access.is_none()
            || item.fact.started_at.is_none()
            || item.fact.tier.as_deref() != Some(pool.as_str())
        {
            continue;
        }
        let Some(context) = item.fact.routing_context.as_ref() else {
            continue;
        };
        let role = PriorKey::for_attempt(
            context.workload,
            context.writes,
            item.fact.context.as_ref().and_then(|c| c.role.as_deref()),
        );
        let Some(route) = item.fact.resolved_route.as_ref() else {
            continue;
        };
        let Some(model_name) = exact_route(route) else {
            continue;
        };
        if [&item.fact.applied_route, &item.fact.observed_route]
            .into_iter()
            .flatten()
            .any(|actual| exact_route(actual).as_ref() != Some(&model_name))
        {
            continue;
        }
        let Ok(model) = ModelId::parse(&model_name) else {
            continue;
        };
        let success = match item.fact.terminal_state {
            AttemptOutcome::Succeeded => !item.partial,
            AttemptOutcome::Failed | AttemptOutcome::Rejected | AttemptOutcome::ReviewRequired => {
                false
            }
            _ => continue,
        };
        let counts = groups.entry((model, pool, role)).or_default();
        if success {
            counts.0 += 1;
        } else {
            counts.1 += 1;
        }
        if groups.len() as u64 > crate::MAX_GROUPS {
            return Err(crate::TelemetryError::TooLarge("10,000 groups"));
        }
    }
    Ok(groups
        .into_iter()
        .map(|((model, pool, role), (successes, failures))| RoutePrior {
            extra: Default::default(),
            model,
            pool,
            role: role.as_str().to_owned(),
            successes,
            failures,
            source: "route_outcomes:attributed_terminal_pool_draws".into(),
        })
        .collect())
}

/// A pool receipt can also describe a preference override or availability
/// fallback. Only the original weighted hash draw supports this comparison.
/// A draw shaped by a per-dispatch exclusion (a graph restart excluding its
/// predecessors' models, or a caller's flag) depends on what happened before,
/// so it is observational. Only standing configuration exclusions, which shape
/// every draw alike, keep it hash-assigned. Receipts record `selection_rule`
/// and typed exclusion sources together; malformed evidence fails decoding.
pub fn hash_assigned(selection: &provenant_records::RouteSelectionReceipt) -> bool {
    use provenant_records::route_selection::{ExclusionSource, SelectionRule};
    selection
        .considered_exclusions
        .iter()
        .all(|rule| rule.source == ExclusionSource::Config)
        && selection.total_weight > 0
        && selection.redraws.is_empty()
        && matches!(
            selection.selection_rule,
            SelectionRule::WeightedDraw | SelectionRule::ExplorationDraw
        )
}

/// Time includes failed attempts: dividing only successful attempts
/// time would hide the delay of failures. Success means a successful terminal
/// settlement with no retained partial-completion signal.
pub fn report(
    evidence: impl IntoIterator<Item = Evidence>,
) -> Result<Vec<Row>, crate::TelemetryError> {
    let mut groups = BTreeMap::<_, Vec<Evidence>>::new();
    for (index, item) in evidence.into_iter().enumerate() {
        if index as u64 >= crate::MAX_ATTEMPTS {
            return Err(crate::TelemetryError::TooLarge("100,000 selected attempts"));
        }
        if !has_quality_outcome(&item.fact) {
            continue;
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
                samples,
                successes,
                success_rate: (samples > 0).then(|| successes as f64 / samples as f64),
                success_interval: crate::suggest::wilson(successes, samples),
                time_per_success_ms: (successes > 0 && missing_time == 0)
                    .then(|| time as f64 / successes as f64),
                missing_time,
            })
        })
        .collect()
}
