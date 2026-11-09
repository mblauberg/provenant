//! Joins use host-stamped attempt traces and existing graph labels.
use provenant_core::{AttemptId, RunId};
use provenant_records::{AttemptFact, telemetry_context::TelemetryContext};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    pub known: u64,
    pub unknown: u64,
}
#[derive(Debug, Serialize)]
pub struct AttemptContext {
    pub attempt: AttemptId,
    pub lane: crate::Totals,
    pub chair_session: Option<String>,
    pub graph: Option<String>,
    pub node: Option<String>,
    pub context: Option<TelemetryContext>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub attempts: Vec<AttemptContext>,
    pub coverage: BTreeMap<&'static str, Coverage>,
    pub comparability: &'static str,
}

/// Chair bindings must come from the existing host-owned ChairIndex, never a request proposal.
pub fn report(
    facts: &[&AttemptFact],
    chairs: &BTreeMap<RunId, String>,
    prices: &crate::PriceTable,
) -> Result<Report, crate::TelemetryError> {
    let mut coverage = BTreeMap::<_, Coverage>::new();
    let attempts = facts
        .iter()
        .map(|fact| {
            let context = TelemetryContext::from_trace(&fact.trace);
            let known = |field: fn(&TelemetryContext) -> bool| context.as_ref().is_some_and(field);
            let chair_session = fact.trace.run.and_then(|run| chairs.get(&run).cloned());
            let graph = fact
                .context
                .as_ref()
                .and_then(|context| context.graph_id.clone());
            for (field, present) in [
                (
                    "provenant_version",
                    known(|c| c.provenant_version.is_some()),
                ),
                ("provenant_digest", known(|c| c.provenant_digest.is_some())),
                (
                    "release_source_digest",
                    known(|c| c.release_source_digest.is_some()),
                ),
                ("instructions", known(|c| c.instructions.is_some())),
                ("harness_version", known(|c| c.harness_version.is_some())),
                (
                    "harness_experiment",
                    known(|c| c.harness_experiment.is_some()),
                ),
                (
                    "provenant_experiment",
                    known(|c| c.provenant_experiment.is_some()),
                ),
                ("system_prompt", known(|c| c.system_prompt.is_some())),
                ("tool_schema", known(|c| c.tool_schema.is_some())),
                ("chair_join", chair_session.is_some()),
                ("graph_join", graph.is_some()),
            ] {
                let count = coverage.entry(field).or_default();
                if present {
                    count.known += 1;
                } else {
                    count.unknown += 1;
                }
            }
            let lane = crate::stats([(**fact).clone()], prices, crate::Window::default())?.totals;
            Ok(AttemptContext {
                attempt: fact.attempt_id,
                lane,
                chair_session,
                graph,
                node: fact
                    .context
                    .as_ref()
                    .and_then(|context| context.node_id.clone()),
                context,
            })
        })
        .collect::<Result<Vec<_>, crate::TelemetryError>>()?;
    Ok(Report {
        attempts,
        coverage,
        comparability: "Observational comparisons cannot separate release, harness experiments, task difficulty and user interaction when their joint coverage is missing or they change together. Observed AGENTS.md bytes are not proof of the full effective instructions.",
    })
}
