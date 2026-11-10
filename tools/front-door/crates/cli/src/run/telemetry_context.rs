//! Joins measurement to retained task and delivery evidence; no collection.
use super::*;
use provenant_records::workflow::DecisionStatus;

pub(super) const ACCEPTED_DEFINITION: &str = "distinct delivery IDs with a recorded accepted workflow receipt, among selected finished project attempts; provider success alone is not acceptance";

fn coverage(known: usize, total: usize) -> Value {
    json!({"known": known, "unknown": total.saturating_sub(known)})
}

/// `stats` reports the accepted count only. Its cost is a cost producer's
/// output, so it is read with `telemetry value`.
fn accepted_outcomes(denominator: usize) -> Value {
    json!({
        "definition": ACCEPTED_DEFINITION,
        "denominator": denominator,
        "numerator_command": "telemetry value",
    })
}

/// Latest receipt state per delivery, read once for each delivery in `facts`.
fn delivery_states(
    facts: &[&provenant_records::AttemptFact],
    edge: &Edge<'_>,
) -> BTreeMap<provenant_core::DeliveryId, Option<(DecisionStatus, String)>> {
    let mut states = BTreeMap::new();
    for fact in facts {
        if let Some(delivery) = fact.delivery_id {
            states.entry(delivery).or_insert_with(|| {
                provenant_workflow::store::ReceiptStore::new(
                    edge.roots.state.clone(),
                    fact.project_id,
                    delivery,
                )
                .read()
                .ok()
                .flatten()
                .map(|receipt| {
                    (
                        receipt.decision().status(),
                        receipt.updated_at().to_string(),
                    )
                })
            });
        }
    }
    states
}

fn accepted_deliveries(
    states: &BTreeMap<provenant_core::DeliveryId, Option<(DecisionStatus, String)>>,
) -> std::collections::BTreeSet<provenant_core::DeliveryId> {
    states
        .iter()
        .filter(|(_, state)| {
            state
                .as_ref()
                .is_some_and(|(status, _)| *status == DecisionStatus::Accepted)
        })
        .map(|(id, _)| *id)
        .collect()
}

/// The accepted-delivery count among `facts`, and the attempts that carry one.
pub(super) fn accepted_attempts_among(
    facts: &[&provenant_records::AttemptFact],
    edge: &Edge<'_>,
) -> (usize, std::collections::BTreeSet<String>) {
    let accepted = accepted_deliveries(&delivery_states(facts, edge));
    let attempts = facts
        .iter()
        .filter(|fact| fact.delivery_id.is_some_and(|id| accepted.contains(&id)))
        .map(|fact| fact.attempt_id.to_string())
        .collect();
    (accepted.len(), attempts)
}

/// Latest receipt state and graph snapshot are labelled separately from the
/// attempt's effective time. Missing receipts never imply rejection.
pub(super) fn report(
    facts: &[&provenant_records::AttemptFact],
    graphs: &[Value],
    edge: &Edge<'_>,
) -> Result<Value, Fail> {
    let states = delivery_states(facts, edge);
    let accepted = accepted_deliveries(&states);
    let mut risks = BTreeMap::<String, usize>::new();
    let mut known_risks = 0;
    let mut graph_nodes = std::collections::BTreeSet::new();
    let mut known_files = 0;
    let mut known_lines = 0;
    let mut diffs = BTreeMap::new();
    let rows: Vec<_> = facts.iter().map(|fact| {
        let context = fact.context.as_ref();
        let risk = context.and_then(|context| context.risk_tier.as_deref());
        if let Some(risk) = risk {
            known_risks += 1;
            *risks.entry(risk.to_owned()).or_default() += 1;
        }
        let graph = context.and_then(|context| context.graph_id.as_deref());
        let node = context.and_then(|context| context.node_id.as_deref());
        if let (Some(graph), Some(node)) = (graph, node) {
            graph_nodes.insert((graph, node));
        }
        let captured = provenant_records::telemetry_context::TelemetryContext::from_trace(&fact.trace);
        let diff = captured.as_ref().and_then(|context| {
            let base = context.git_base.as_ref()?.value.to_string();
            let head = context.git_head.as_ref()?.value.to_string();
            let key = (base.clone(), head.clone());
            if !diffs.contains_key(&key) && diffs.len() < 100 {
                diffs.insert(key.clone(), provenant_telemetry::outcome::diff_size(edge.cwd, &base, &head).ok());
            }
            diffs.get(&key).cloned().flatten()
        });
        known_files += usize::from(diff.is_some());
        known_lines += usize::from(diff.as_ref().is_some_and(|diff| diff.lines.is_some()));
        json!({
            "attempt_id": fact.attempt_id,
            "source": "terminal_attempt_fact.work_context",
            "risk_tier_effective_at": fact.queued_at,
            "risk_tier": risk,
            "graph_id": graph,
            "node_id": node,
            "acceptance_items": null,
            "diff": diff,
            "diff_scope": "captured canonical task base/head committed comparison at dispatch; unavailable objects and comparisons beyond 100 distinct pairs remain unknown",
            "diff_lines": diff.as_ref().and_then(|diff| diff.lines),
            "files": diff.as_ref().map(|diff| diff.files),
            "first_pass_ci": null,
            "reverts": null,
            "compactions": null,
            "acceptance": fact.delivery_id.and_then(|id| states.get(&id)).and_then(|state| state.as_ref()).map(|(state, at)| json!({"status": match state { DecisionStatus::Accepted => "accepted", DecisionStatus::Pending => "pending", DecisionStatus::Abandoned => "abandoned" }, "source": "workflow_delivery_receipt", "effective_at": at})),
        })
    }).collect();
    let total = facts.len();
    let linked = facts
        .iter()
        .filter(|fact| {
            fact.delivery_id
                .and_then(|id| states.get(&id))
                .is_some_and(Option::is_some)
        })
        .count();
    let graph_known = graphs
        .iter()
        .filter(|graph| graph["checkpoint"] == "read")
        .count();
    Ok(json!({
        "scope": "selected finished project attempts; delivery decisions and graph checkpoints are latest retained snapshots",
        "rows": rows,
        "coverage": {
            "risk_tier": coverage(known_risks, total),
            "acceptance_items": coverage(0, total),
            "diff_lines": coverage(known_lines, total),
            "files": coverage(known_files, total),
            "first_pass_ci": coverage(0, total),
            "reverts": coverage(0, total),
            "compactions": coverage(0, total),
            "delivery_decision": coverage(linked, total),
            "review_findings_and_repair_rounds": coverage(graph_known, graphs.len()),
        },
        "risk_tiers": risks,
        "slices": {"known": graph_nodes.len(), "definition": "distinct observed graph_id/node_id pairs; not an inferred count of planned work slices", "attempts_without_graph_node": facts.iter().filter(|fact| fact.context.as_ref().is_none_or(|context| context.graph_id.is_none() || context.node_id.is_none())).count()},
        "graph_outcomes": graphs,
        "accepted_outcomes": accepted_outcomes(accepted.len()),
        "comparability": "observational evidence cannot separate release effects from harness experiments, task difficulty or user activity when their coverage or independent variation is missing",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_accepted_outcomes_report_only_the_denominator() {
        let accepted = accepted_outcomes(2);
        assert_eq!(accepted["denominator"], 2);
        assert_eq!(accepted["definition"], ACCEPTED_DEFINITION);
        assert_eq!(accepted["numerator_command"], "telemetry value");
        assert!(accepted.get("cost").is_none());
        assert!(accepted.get("microdollars_per_accepted_outcome").is_none());
    }
}
