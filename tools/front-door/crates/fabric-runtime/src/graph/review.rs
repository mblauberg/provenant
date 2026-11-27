//! Deterministic contract for review template results and coverage.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// The one result order every review prompt states: one verdict line after the
/// findings and prose, as the very last thing. A reader owes no declaration, so
/// nothing follows the verdict.
pub(super) const VERDICT_LAST: &str = "After all findings and prose, end with exactly one verdict line: VERDICT: PASS, VERDICT: FAIL or VERDICT: INCONCLUSIVE, with nothing after it.";

/// A delivery review is acceptable only with complete structured evidence.
/// Keep the original verdict in the record; this policy affects the gate only.
pub(super) fn delivery_matches(
    threshold: AcceptBelow,
    outputs: &NodeOutputs,
    expected: &str,
) -> bool {
    use provenant_records::workflow::ReviewVerdict;
    if ReviewVerdict::from_token(expected) != Some(ReviewVerdict::Approve) {
        return outputs.verdict.as_deref().is_some_and(|actual| {
            match (
                ReviewVerdict::from_token(actual),
                ReviewVerdict::from_token(expected),
            ) {
                (Some(actual), Some(expected)) => actual == expected,
                _ => actual == expected,
            }
        });
    }
    if outputs.partial
        || outputs.findings_available != Some(true)
        || outputs.verdict_error.is_some()
    {
        return false;
    }
    let agrees = match outputs
        .verdict
        .as_deref()
        .and_then(ReviewVerdict::from_token)
    {
        Some(ReviewVerdict::Approve) => true,
        Some(ReviewVerdict::Inconclusive) => threshold != AcceptBelow::Off,
        _ => false,
    };
    agrees
        && matches!(
            acceptance_decision(&outputs.findings, threshold, true),
            Acceptance::Accept
        )
}

pub(super) fn malformed_delivery_evidence_error(code: Option<&str>) -> bool {
    matches!(
        code,
        Some(
            "review_verdict_invalid"
                | "review_findings_invalid"
                | "review_triage_missing"
                | "review_verdict_conflict"
                | "review_propagation_invalid"
                | "findings_required"
                | "disposition_overflow_incomplete"
                | "disposition_overflow_inconsistent"
                | "verification_invalid"
                | "verification_incomplete"
        )
    )
}

/// A composite delivery-review boundary has no attempt settlement to own its
/// evidence failure. Treat failed coverage and partial or unparseable aggregate
/// evidence as malformed so the graph can park the boundary through the inbox.
/// A complete, parseable FAIL remains a review decision and can route to repair.
pub(super) fn malformed_composite_delivery_evidence(
    node: &PlannedNode,
    progress: &NodeProgress,
) -> bool {
    if node.composite.is_none() || node.delivery_review.is_none() {
        return false;
    }
    match progress.state {
        NodeState::Pending | NodeState::Running | NodeState::Skipped => false,
        NodeState::Failed | NodeState::Blocked => true,
        NodeState::Succeeded => progress.outputs.as_ref().is_none_or(|outputs| {
            outputs.partial
                || outputs.findings_available != Some(true)
                || outputs.verdict_error.is_some()
                || outputs
                    .verdict
                    .as_deref()
                    .and_then(provenant_records::workflow::ReviewVerdict::from_token)
                    .is_none()
        }),
    }
}

pub(super) fn partial(outputs: &NodeOutputs) -> bool {
    outputs.partial
}

/// Coverage is separate from whether the consumer wrote a parseable block.
/// A complete overflow proof can cover truncated exports, but cannot replace
/// an unavailable lens or verifier.
pub(super) fn partial_inputs(
    node: &PlannedNode,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    node.inputs.iter().any(|input| {
        input.outputs.iter().any(|kind| {
            matches!(
                kind,
                OutputKind::Findings | OutputKind::Result | OutputKind::Verdict
            )
        }) && positions.get(&input.node).is_some_and(|&index| {
            let source = &checkpoint.nodes[index];
            source.state != NodeState::Succeeded
                || source.outputs.as_ref().is_none_or(|outputs| {
                    partial(outputs)
                        || outputs
                            .verdict
                            .as_deref()
                            .and_then(provenant_records::workflow::ReviewVerdict::from_token)
                            == Some(provenant_records::workflow::ReviewVerdict::Inconclusive)
                        || (input.outputs.contains(&OutputKind::Findings)
                            && outputs.findings_available != Some(true)
                            && source.truncation_warnings().is_empty())
                })
        })
    })
}

/// Keep evidence from an invalid review reply without counting it as coverage.
/// This does not export failed writers or provider failures as review evidence.
pub(super) fn retained_findings(node: &PlannedNode, progress: &NodeProgress) -> bool {
    progress.state == NodeState::Failed
        && ((node.composite.is_some() && progress.outputs.as_ref().is_some_and(partial))
            || (template::review_behaviour(node).is_some()
                && progress.attempts.last().is_some_and(|attempt| {
                    attempt.outcome == Some(GraphAttemptOutcome::TaskFailed)
                })))
        && progress
            .outputs
            .as_ref()
            .is_some_and(|outputs| !outputs.findings.is_empty())
}

pub(super) fn mark_partial(outputs: &mut NodeOutputs) {
    outputs.partial = true;
    outputs.findings_available = Some(false);
    // Blocking evidence still warrants FAIL; approval cannot restore coverage.
    if outputs
        .verdict
        .as_deref()
        .and_then(provenant_records::workflow::ReviewVerdict::from_token)
        .is_none_or(|verdict| verdict != provenant_records::workflow::ReviewVerdict::RequestChanges)
    {
        outputs.verdict = Some("VERDICT: INCONCLUSIVE".into());
    }
}

pub(super) fn contract_error(
    node: &PlannedNode,
    upstream: &[Finding],
    outputs: &NodeOutputs,
    threshold: AcceptBelow,
) -> Option<(&'static str, String)> {
    let repair_triage = node.repair_triage;
    if template::review_behaviour(node).is_none()
        && node.delivery_review.is_none()
        && !repair_triage
    {
        return None;
    }
    // ResultCapture has already counted `VERDICT:` lines outside fenced
    // blocks. `verdict_error` also carries source truncation, which is
    // diagnosed by findings availability below; a captured verdict is not
    // reported as malformed for that reason.
    if !outputs
        .verdict
        .as_deref()
        .is_some_and(provenant_records::result_capture::is_review_verdict)
    {
        let cause = match outputs.verdict.as_deref() {
            Some(other) => format!("`{other}` is not VERDICT: PASS, FAIL or INCONCLUSIVE"),
            None => outputs
                .verdict_error
                .clone()
                .unwrap_or_else(|| "no VERDICT: line outside fenced blocks".to_owned()),
        };
        return Some((
            "review_verdict_invalid",
            format!(
                "review needs exactly one VERDICT: PASS, FAIL or INCONCLUSIVE line outside fenced blocks: {cause}"
            ),
        ));
    }
    if outputs.findings_available != Some(true) {
        return Some(("review_findings_invalid", "review findings are missing, malformed or incomplete; retained findings remain available as partial evidence".into()));
    }
    let mut ids = std::collections::BTreeSet::new();
    for finding in &outputs.findings {
        if upstream.iter().any(|source| source.id == finding.id) {
            // Identity-only decisions inherit the source evidence. Copies from
            // multiple verifiers are checked by their disposition contract.
            continue;
        }
        if !ids.insert(&finding.id) {
            return Some((
                "review_findings_invalid",
                format!("finding {} repeats an independent id", finding.id),
            ));
        }
        for (field, value) in [
            ("id", Some(finding.id.as_str())),
            ("severity", Some(finding.severity.as_str())),
            ("file", Some(finding.file.as_str())),
            ("summary", Some(finding.summary.as_str())),
            ("status", Some(finding.status.as_str())),
            ("evidence", finding.evidence.as_deref()),
            ("proposed_fix", finding.proposed_fix.as_deref()),
        ] {
            if value.is_none_or(|value| value.trim().is_empty()) {
                return Some((
                    "review_findings_invalid",
                    format!("finding {} needs non-empty string {field}", finding.id),
                ));
            }
        }
    }
    if repair_triage
        || matches!(
            template::review_behaviour(node),
            Some(template::ReviewBehaviour::Verify | template::ReviewBehaviour::Synthesis)
        )
    {
        for finding in &outputs.findings {
            let valid = finding.triage().is_some_and(|triage| {
                matches!(triage.decision.as_str(), "confirmed" | "rejected")
                    && !triage.reason.trim().is_empty()
                    && !triage.root_cause.trim().is_empty()
                    && matches!(
                        triage.blocking_class.as_str(),
                        "acceptance" | "default_path_defect" | "boundary" | "data_loss" | "none"
                    )
            });
            if !valid {
                return Some((
                    "review_triage_missing",
                    format!(
                        "finding {} needs checked triage: decision, reason, worth_fixing, root_cause, blocking_class",
                        finding.id
                    ),
                ));
            }
        }
    }
    if outputs.verdict.as_deref() == Some("VERDICT: PASS")
        && outputs.findings.iter().any(|finding| {
            (repair::blocks_acceptance_for(finding, threshold)
                && !(upstream.iter().any(|source| source.id == finding.id)
                    && finding.verification.as_deref() == Some("falsified")
                    && finding.falsified_with_evidence()))
                || finding.verification.as_deref() == Some("unverifiable")
        })
    {
        return Some((
            "review_verdict_conflict",
            "PASS contradicts unresolved blocking, accepted or unverifiable findings".into(),
        ));
    }
    None
}

/// Every grouped item must return a typed outcome, including disputes.
pub(super) fn repair_result_error(
    node: &PlannedNode,
    upstream: &[Finding],
    outputs: &NodeOutputs,
    threshold: AcceptBelow,
) -> Option<(&'static str, String)> {
    if !repair_writer(node) {
        return None;
    }
    let required: std::collections::BTreeSet<_> = upstream
        .iter()
        .filter(|finding| repair::repair_eligible_for(finding, threshold))
        .filter_map(Finding::triage)
        .map(|triage| triage.root_cause)
        .collect();
    let Some(items) = outputs.repair_items.as_ref() else {
        return Some((
            "repair_items_missing",
            "repair writer must report every grouped item with a typed outcome".into(),
        ));
    };
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        if !required.contains(&item.id) || !seen.insert(item.id.clone()) {
            return Some((
                "repair_items_invalid",
                "repair outcomes must name each grouped item exactly once".into(),
            ));
        }
    }
    if seen != required {
        return Some((
            "repair_items_incomplete",
            "every grouped fix item needs a repair outcome".into(),
        ));
    }
    None
}

/// Round two and later review the repair delta against the named invariants.
/// An unnamed finding is flagged first as a warning; a blocking finding with
/// no invariant is refused at settlement.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
fn delta_round(node: &PlannedNode) -> bool {
    node.round.as_ref().is_some_and(|round| round.round >= 2)
        && (template::review_behaviour(node).is_some() || repair_writer(node))
}

pub(super) fn invariant_warnings(node: &PlannedNode, outputs: &NodeOutputs) -> Vec<String> {
    if !delta_round(node) {
        return Vec::new();
    }
    outputs
        .findings
        .iter()
        .filter(|finding| finding.named_invariant().is_none())
        .map(|finding| {
            format!(
                "finding_invariant_missing: {} names no invariant; warn first, refuse when it blocks",
                finding.id
            )
        })
        .collect()
}

pub(super) fn invariant_error(
    node: &PlannedNode,
    outputs: &NodeOutputs,
) -> Option<(&'static str, String)> {
    if !delta_round(node) {
        return None;
    }
    let missing = outputs.findings.iter().find(|finding| {
        finding.named_invariant().is_none() && repair::blocking_severity(finding)
    })?;
    Some((
        "review_invariant_missing",
        format!(
            "blocking finding {} names no invariant; round-2 findings must name the invariant they break",
            missing.id
        ),
    ))
}

/// A fix round returns, per invariant, the sweep it ran, every site it changed
/// and the test that pins the invariant. A missing item is flagged at settlement.
/// Spec: docs/specs/fabric.md#fix-round-invariants-and-split-verdict-adjudication
pub(super) fn repair_return_warnings(node: &PlannedNode, outputs: &NodeOutputs) -> Vec<String> {
    if !repair_writer(node) {
        return Vec::new();
    }
    let Some(items) = outputs.repair_items.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let mut missing = Vec::new();
            if item
                .sweep
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                missing.push("sweep");
            }
            if item.sites.as_ref().is_none_or(|sites| {
                sites.is_empty() || sites.iter().any(|site| site.trim().is_empty())
            }) {
                missing.push("sites");
            }
            if item
                .pin
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            {
                missing.push("pin");
            }
            (!missing.is_empty()).then(|| {
                format!(
                    "repair_invariant_return_missing: {} has no {}",
                    item.id,
                    missing.join(", ")
                )
            })
        })
        .collect()
}

/// Review evidence can cross several aggregation boundaries.
pub(super) fn composite_review(
    spec: &CompositeSpec,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    spec.exports.iter().any(|export| {
        let node = &plan.nodes[positions[&export.node]];
        template::review_behaviour(node).is_some()
            || node
                .composite
                .as_ref()
                .is_some_and(|spec| composite_review(spec, plan, positions))
    })
}

// ── Review convergence (batch 1 slice 27, §3.7) ───────────────────────────
//
// Every round reads and writes one finding ledger keyed by the exact head it
// reviews. The ledger is the only evidence a later round or an acceptance
// decision may carry forward: one reviewer's output alone is never enough.

/// Warning emitted when a reviewer used a severity word outside the alias
/// table. The word falls back to `High`, never to a lower severity, so an
/// unknown word cannot be silently accepted below a `high` threshold.
pub(super) const FINDING_SEVERITY_UNKNOWN: provenant_records::warning::WarningCode =
    provenant_records::warning::WarningCode("FINDING_SEVERITY_UNKNOWN");

/// Stable owner for review retry and finding-ledger state. Loop members and an
/// initial panel explicitly bound into a repair loop share that loop ID; an
/// otherwise standalone panel uses its reduced-verdict boundary. Explicit
/// initial bindings are resolved through their findings/verdict input closure,
/// so a sibling synthesis node still belongs to the panel it summarises.
pub(super) fn chain_owner_id(node_index: usize, plan: &PlannedGraph) -> String {
    let Some(node) = plan.nodes.get(node_index) else {
        return String::new();
    };
    if let Some(round) = &node.round {
        return round.loop_id.clone();
    }
    let panel = plan
        .nodes
        .iter()
        .filter(|candidate| {
            candidate
                .composite
                .as_ref()
                .is_some_and(|spec| spec.reduce_verdict)
                && (node.id == candidate.id || node.id.starts_with(&format!("{}--", candidate.id)))
        })
        .max_by_key(|candidate| candidate.id.len())
        .map_or_else(|| node.id.clone(), |candidate| candidate.id.clone());
    let Some(&panel_index) = plan
        .nodes
        .iter()
        .position(|candidate| candidate.id == panel)
        .as_ref()
    else {
        return panel;
    };
    let positions: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.id.clone(), index))
        .collect();
    let consumers: BTreeSet<_> = plan
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.loop_body.as_ref().is_some_and(|loop_body| {
                loop_body.previous_bindings.values().any(|binding| {
                    evidence_dependency_closure(binding, plan, &positions).contains(&panel_index)
                })
            })
        })
        .map(|candidate| candidate.id.as_str())
        .collect();
    if consumers.len() == 1 {
        consumers
            .first()
            .map_or_else(|| panel.clone(), |id| (*id).to_owned())
    } else {
        // With multiple explicit consumers, keep one immutable seed at the
        // standalone panel. Each loop copies that seed into its own chain when
        // it begins; choosing the first loop would starve the others.
        panel
    }
}

/// Follow only declared evidence-bearing inputs (plus their composite and
/// source relationships) from an initial binding. Ordinary ordering edges do
/// not transfer review-chain ownership.
pub(super) fn evidence_dependency_closure(
    start: &str,
    plan: &PlannedGraph,
    positions: &BTreeMap<String, usize>,
) -> BTreeSet<usize> {
    let mut closure = BTreeSet::new();
    let mut pending = vec![start.to_owned()];
    while let Some(id) = pending.pop() {
        let index = positions.get(&id).copied().or_else(|| {
            let canonical = id.replace('.', "--");
            positions.get(&canonical).copied()
        });
        let Some(index) = index else {
            continue;
        };
        if !closure.insert(index) {
            continue;
        }
        let node = &plan.nodes[index];
        if let Some(parent) = node.parent_node.as_deref() {
            pending.push(parent.to_owned());
        }
        pending.extend(node.composite_members.iter().cloned());
        pending.extend(
            node.inputs
                .iter()
                .filter(|input| {
                    input.outputs.contains(&OutputKind::Findings)
                        || input.outputs.contains(&OutputKind::Verdict)
                })
                .map(|input| input.node.clone()),
        );
        if let Some(SourceBinding::Node { node } | SourceBinding::Head { node }) = &node.source {
            pending.push(node.clone());
        }
    }
    closure
}

/// A repair loop may seed its owner ledger from one explicitly bound initial
/// panel when multiple loops share that immutable panel seed.
pub(super) fn initial_chain_ledger(
    loop_id: &str,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Option<FindingLedger> {
    let positions: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.id.clone(), index))
        .collect();
    let loop_node = plan.nodes.iter().find(|node| node.id == loop_id)?;
    let loop_body = loop_node.loop_body.as_ref()?;
    let mut panels = BTreeSet::new();
    for binding in loop_body.previous_bindings.values() {
        let closure = evidence_dependency_closure(binding, plan, &positions);
        panels.extend(plan.nodes.iter().enumerate().filter_map(|(index, node)| {
            (closure.contains(&index)
                && node
                    .composite
                    .as_ref()
                    .is_some_and(|spec| spec.reduce_verdict))
            .then_some(index)
        }));
    }
    let outermost: Vec<_> = panels
        .iter()
        .copied()
        .filter(|candidate| {
            let id = plan.nodes[*candidate].id.as_str();
            panels.iter().all(|other| {
                let other = plan.nodes[*other].id.as_str();
                other == id || other.starts_with(&format!("{id}--"))
            })
        })
        .collect();
    if outermost.len() != 1 {
        return None;
    }
    let panel = outermost[0];
    let panel_owner = chain_owner_id(panel, plan);
    checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == panel_owner)
        .and_then(|progress| progress.finding_ledger.clone())
}

/// Decode a finding's severity and flag an unknown source word.
pub(super) fn finding_severity(finding: &Finding) -> (Severity, bool) {
    model::decode_severity(&finding.severity)
}

/// A finding stays open until it is resolved, dispositioned or falsified with
/// evidence. Only open findings take part in acceptance and carry-forward.
pub(super) fn is_open(finding: &Finding) -> bool {
    !matches!(finding.status.as_str(), "closed" | "resolved" | "fixed")
        && !finding.falsified_with_evidence()
        && finding
            .disposition
            .as_deref()
            .is_none_or(|value| !matches!(value, "accepted" | "rejected" | "wont_fix"))
}

/// Boundary and data-loss risks stay in the persisted ledger until evidence
/// falsifies them, even when a nominal disposition says they are closed.
pub(super) fn ledger_unresolved(finding: &Finding) -> bool {
    is_open(finding) || (finding.boundary_or_data_loss() && !finding.falsified_with_evidence())
}

pub(super) fn validate_ledger_blob(ledger: &FindingLedger) -> Result<(), String> {
    let size = serde_json::to_vec(ledger)
        .map_err(|_| "review finding ledger could not be serialised".to_owned())?
        .len();
    if size > MAX_CHECKPOINT_BYTES as usize {
        return Err(format!(
            "review finding ledger exceeds the {} byte prompt-store cap; complete review evidence is unavailable",
            MAX_CHECKPOINT_BYTES
        ));
    }
    Ok(())
}

/// The exclusive ceiling a `domains.review.accept_below` value allows. `Off`
/// accepts no open finding; `Blocking` accepts everything strictly below it.
fn threshold_severity(threshold: AcceptBelow) -> Option<Severity> {
    match threshold {
        AcceptBelow::Off => None,
        AcceptBelow::Nit => Some(Severity::Nit),
        AcceptBelow::Low => Some(Severity::Low),
        AcceptBelow::Medium => Some(Severity::Medium),
        AcceptBelow::High => Some(Severity::High),
        AcceptBelow::Blocking => Some(Severity::Blocking),
    }
}

/// Whether an unresolved finding blocks acceptance at the configured ceiling.
/// Boundary and data-loss findings remain blocking until evidence falsifies
/// them, regardless of their severity or disposition.
pub(super) fn blocks_threshold(finding: &Finding, threshold: AcceptBelow) -> bool {
    if finding.boundary_or_data_loss() {
        return !finding.falsified_with_evidence();
    }
    if !is_open(finding) {
        return false;
    }
    if finding.has_blocking_class() {
        return true;
    }
    let Some(ceiling) = threshold_severity(threshold) else {
        return true;
    };
    finding_severity(finding).0 >= ceiling
}

/// The acceptance decision for one exact reviewed head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Acceptance {
    Accept,
    Reject(String),
}

/// Accept only when CI is complete and green on the exact head, no open
/// finding carries a blocking class, and every open finding is strictly below
/// the configured severity. Lower findings are posted, never sent to repair.
pub(super) fn acceptance_decision(
    findings: &[Finding],
    threshold: AcceptBelow,
    ci_complete_green: bool,
) -> Acceptance {
    if !ci_complete_green {
        return Acceptance::Reject("ci is not complete and green on the exact head".into());
    }
    for finding in findings {
        if !blocks_threshold(finding, threshold) {
            continue;
        }
        if finding.boundary_or_data_loss() && !finding.falsified_with_evidence() {
            return Acceptance::Reject(format!(
                "finding {} is an unresolved boundary or data-loss risk",
                finding.id
            ));
        }
        if !is_open(finding) {
            continue;
        }
        if threshold == AcceptBelow::Off {
            return Acceptance::Reject(format!(
                "finding {} is open but acceptance is off",
                finding.id
            ));
        }
        if finding.has_blocking_class() {
            return Acceptance::Reject(format!(
                "finding {} has an open blocking class",
                finding.id
            ));
        }
        if let Some(ceiling) = threshold_severity(threshold) {
            let (severity, _) = finding_severity(finding);
            if severity >= ceiling {
                return Acceptance::Reject(format!(
                    "finding {} severity {severity:?} is not below {ceiling:?}",
                    finding.id
                ));
            }
        }
    }
    Acceptance::Accept
}

/// How a lens that did not complete ends a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LensOutcome {
    Completed,
    Retry,
    Unavailable,
}

/// Missing or malformed evidence re-reviews the same lens on the same route;
/// an unavailable lens retries on the next eligible route and then becomes
/// `Unavailable`. Neither outcome sends the work to repair.
pub(super) fn lens_outcome(
    end_code: Option<&str>,
    retries_used: u32,
    max_retries: u32,
    eligible_route: bool,
) -> LensOutcome {
    let can_retry = retries_used < max_retries;
    match end_code {
        Some("missing_evidence") => {
            if can_retry {
                LensOutcome::Retry
            } else {
                LensOutcome::Unavailable
            }
        }
        Some("blocked_environment" | "model_unavailable") => {
            if can_retry && eligible_route {
                LensOutcome::Retry
            } else {
                LensOutcome::Unavailable
            }
        }
        _ => LensOutcome::Completed,
    }
}

/// The head range one review round covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RoundScope {
    Delta { base: String, head: String },
    Full { head: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RoundMode {
    Delta,
    Full,
}

/// A later round reviews `last_reviewed_head..head` plus the ledger's open
/// findings and invariants. A moved or missing base, an unattributable delta,
/// or an invariant whose only lens was unavailable widens to the full head.
pub(super) fn round_scope(
    ledger: &FindingLedger,
    head: &str,
    base_head: &str,
    previous_base: Option<&str>,
    mode: RoundMode,
) -> RoundScope {
    let last = ledger.last_reviewed_head.as_deref();
    let base_moved = previous_base != Some(base_head);
    if mode == RoundMode::Delta && !base_moved && last.is_some_and(|last_head| last_head != head) {
        let previous = last.and_then(|last_head| ledger.heads.get(last_head));
        let attributable = previous.is_some_and(|entry| {
            entry.unresolved.iter().all(|finding| {
                finding
                    .lens
                    .as_deref()
                    .is_some_and(|lens| !lens.trim().is_empty())
                    && finding.named_invariant().is_none_or(|invariant| {
                        entry
                            .invariant_coverage
                            .get(invariant)
                            .is_some_and(|lenses| {
                                !lenses.is_empty()
                                    && lenses
                                        .iter()
                                        .any(|lens| !entry.unavailable_lenses.contains(lens))
                            })
                    })
            })
        });
        if attributable {
            RoundScope::Delta {
                base: last.unwrap_or("").to_owned(),
                head: head.to_owned(),
            }
        } else {
            RoundScope::Full {
                head: head.to_owned(),
            }
        }
    } else {
        RoundScope::Full {
            head: head.to_owned(),
        }
    }
}

/// Record the evidence for one exact head in the ledger.
pub(super) fn record_head(
    ledger: &mut FindingLedger,
    head: &str,
    findings: &[Finding],
    dispositions: BTreeMap<String, String>,
    covered_lenses: BTreeSet<String>,
    invariant_coverage: BTreeMap<String, BTreeSet<String>>,
) {
    let entry = ledger.heads.entry(head.to_owned()).or_default();
    entry.unresolved = findings
        .iter()
        .filter(|finding| ledger_unresolved(finding))
        .cloned()
        .collect();
    entry.dispositions = dispositions;
    entry.covered_lenses = covered_lenses;
    entry.invariant_coverage = invariant_coverage;
    ledger.last_reviewed_head = Some(head.to_owned());
}

/// Open findings carried from the last reviewed head into this round.
pub(super) fn carried_findings(ledger: &FindingLedger, head: &str) -> Vec<Finding> {
    ledger
        .heads
        .get(head)
        .map_or_else(Vec::new, |entry| entry.unresolved.clone())
}

/// Coverage gaps for a panel: lenses that never completed and invariants that
/// no completed lens covered. An uncovered invariant makes the round
/// INCONCLUSIVE rather than an agreed PASS.
pub(super) fn coverage_gaps(
    covered_lenses: &BTreeSet<String>,
    required_lenses: &[String],
    invariant_coverage: &BTreeMap<String, BTreeSet<String>>,
    invariants: &[String],
) -> Vec<String> {
    let mut gaps = Vec::new();
    let missing: Vec<_> = required_lenses
        .iter()
        .filter(|lens| !covered_lenses.contains(*lens))
        .cloned()
        .collect();
    if !missing.is_empty() {
        gaps.push(format!("lenses: {}", missing.join(", ")));
    }
    for invariant in invariants {
        if invariant_coverage
            .get(invariant)
            .is_none_or(|lenses| lenses.is_empty())
        {
            gaps.push(format!("invariant {invariant}: uncovered"));
        }
    }
    gaps
}

/// A split on one head goes to exactly one adjudication lane, not a new round.
pub(super) fn split_needs_adjudicator(
    verdicts: &[provenant_records::workflow::ReviewVerdict],
) -> bool {
    use provenant_records::workflow::ReviewVerdict;
    verdicts.iter().any(|verdict| {
        matches!(
            verdict,
            ReviewVerdict::Approve | ReviewVerdict::ApproveWithNits
        )
    }) && verdicts.contains(&ReviewVerdict::RequestChanges)
}

/// Conflicting judgements about the same blocking finding require adjudication.
pub(super) fn contradictory_blocking_findings(findings: &[Finding]) -> bool {
    findings.iter().enumerate().any(|(index, left)| {
        findings[index + 1..].iter().any(|right| {
            let same = left.file == right.file
                && left.line == right.line
                && left.summary == right.summary
                && left.named_invariant().is_some()
                && left.named_invariant() == right.named_invariant();
            let upheld = |finding: &Finding| {
                (finding.has_blocking_class() || repair::blocking_severity(finding))
                    && finding
                        .triage
                        .as_ref()
                        .is_some_and(|triage| triage.decision == "confirmed")
            };
            let rejected = |finding: &Finding| {
                finding.falsified_with_evidence()
                    || finding
                        .triage
                        .as_ref()
                        .is_some_and(|triage| triage.decision == "rejected")
            };
            same && ((upheld(left) && rejected(right)) || (upheld(right) && rejected(left)))
        })
    })
}

/// Rebuild the ledger from reviewer rows already in the checkpoint. Each round
/// reads the same facts under the journal lock, so a replay carries the same
/// open findings and coverage.
pub(super) fn ledger_from_checkpoint(
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    boundary: usize,
    current_head: &str,
) -> FindingLedger {
    let Some(owner) = plan.nodes.get(boundary) else {
        return FindingLedger::default();
    };
    let chain_owner = chain_owner_id(boundary, plan);
    let mut ledger = checkpoint
        .nodes
        .iter()
        .find(|progress| progress.id == chain_owner)
        .and_then(|progress| progress.finding_ledger.clone())
        .or_else(|| initial_chain_ledger(&chain_owner, plan, checkpoint))
        .unwrap_or_default();
    let previous_last_reviewed_head = ledger.last_reviewed_head.clone();
    let stored_current = ledger.heads.remove(current_head);
    let positions: BTreeMap<_, _> = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    let mut members = BTreeSet::new();
    add_review_closure(
        checkpoint.nodes.get(boundary).map(|node| node.id.as_str()),
        plan,
        checkpoint,
        &positions,
        &mut members,
    );

    // Round one consumes an initial panel outside the loop. Its explicit
    // previous_bindings are the only safe way to attach that panel to this
    // repair chain; matching by head would merge unrelated queue items.
    let chain = owner.round.as_ref();
    if let Some(round) = chain
        && let Some(loop_index) = positions.get(round.loop_id.as_str())
        && let Some(loop_spec) = plan.nodes[*loop_index].loop_body.as_ref()
    {
        for initial in loop_spec.previous_bindings.values() {
            add_review_closure(Some(initial), plan, checkpoint, &positions, &mut members);
        }
    }
    if let Some(round) = chain {
        for node in &plan.nodes {
            if node.round.as_ref().is_some_and(|member| {
                member.loop_id == round.loop_id && member.round <= round.round
            }) {
                add_review_closure(Some(&node.id), plan, checkpoint, &positions, &mut members);
            }
        }
    }
    // Dynamic adjudicators are recorded in patch expansions. Include them by
    // their declared panel parent even if the progress row's child list or
    // round metadata does not yet contain the patch relationship.
    let adjudicators: Vec<_> = checkpoint
        .expansions
        .iter()
        .flat_map(|expansion| &expansion.nodes)
        .filter(|node| {
            node.kind == Some(NodeKind::Adjudicate)
                && node
                    .parent_node
                    .as_deref()
                    .and_then(|parent| positions.get(parent))
                    .is_some_and(|parent| members.contains(parent))
        })
        .filter_map(|node| positions.contains_key(&node.id).then(|| node.id.clone()))
        .collect();
    for id in adjudicators {
        add_review_closure(Some(&id), plan, checkpoint, &positions, &mut members);
    }

    let mut candidates: Vec<_> = plan
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, node)| {
            members.contains(index)
                || chain.is_some_and(|round| {
                    node.round.as_ref().is_some_and(|member| {
                        member.loop_id == round.loop_id && member.round <= round.round
                    })
                })
        })
        .collect();
    candidates
        .sort_by_key(|(index, node)| (node.round.as_ref().map_or(0, |round| round.round), *index));
    let panel_ids: Vec<_> = plan
        .nodes
        .iter()
        .enumerate()
        .filter(|(index, node)| {
            members.contains(index)
                && node
                    .composite
                    .as_ref()
                    .is_some_and(|spec| spec.reduce_verdict)
        })
        .map(|(_, node)| node.id.clone())
        .collect();
    let mut heads = BTreeMap::<String, ReviewHeadBuilder>::new();
    let mut last_previous_head = None::<(usize, usize, String)>;

    // A head can be reviewed by several members in a round. Merge those rows,
    // but when the same head is reviewed again replace its prior-round snapshot
    // instead of appending stale findings or coverage.
    for (index, node) in candidates {
        if checkpoint.nodes.get(index).is_none_or(|progress| {
            progress.state != NodeState::Succeeded
                || progress
                    .outputs
                    .as_ref()
                    .is_none_or(|outputs| outputs.partial || outputs.verdict_error.is_some())
        }) {
            continue;
        }

        let progress = &checkpoint.nodes[index];
        let adjudicator = node.kind == Some(NodeKind::Adjudicate);
        let (head, base, round_number, panel_id, verdict_key, verdict) = if adjudicator {
            let Some(parent_id) = node.parent_node.as_deref() else {
                continue;
            };
            let pinned_head = match node.source.as_ref() {
                Some(SourceBinding::Commit { commit }) => Some(commit.as_str()),
                _ => None,
            };
            let Some((head, base, round_number)) =
                panel_review_settlement(parent_id, pinned_head, plan, checkpoint, &positions)
            else {
                continue;
            };
            let Some(verdict) = progress
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.verdict.as_deref())
                .and_then(provenant_records::workflow::ReviewVerdict::from_token)
            else {
                continue;
            };
            (
                head,
                base,
                round_number,
                parent_id.to_owned(),
                format!("adjudicator_verdict:{parent_id}"),
                verdict.as_token().to_owned(),
            )
        } else {
            let Some(settlement) = progress.review_settlement.as_ref() else {
                continue;
            };
            let panel_id = panel_ids
                .iter()
                .filter(|panel| {
                    node.id.as_str() == panel.as_str() || node.id.starts_with(&format!("{panel}--"))
                })
                .max_by_key(|panel| panel.len())
                .cloned()
                .unwrap_or_else(|| node.id.clone());
            (
                settlement.head.to_string(),
                settlement.base.map(|base| base.to_string()),
                node.round.as_ref().map_or(0, |round| round.round),
                panel_id,
                format!("verdict:{}", node.id),
                settlement.verdict.as_token().to_owned(),
            )
        };

        let record = heads.entry(head.clone()).or_default();
        if round_number < record.round {
            continue;
        }
        if round_number > record.round {
            *record = ReviewHeadBuilder {
                round: round_number,
                ..Default::default()
            };
        }
        record.head.round = round_number;
        if let Some(base) = base {
            record.head.dispositions.insert("base".to_owned(), base);
        }
        record.head.dispositions.insert(verdict_key, verdict);

        let findings = outputs::materialised_findings(index, plan, checkpoint, &positions);
        for finding in &findings {
            let disposition = finding
                .disposition
                .clone()
                .unwrap_or_else(|| finding.status.clone());
            if adjudicator {
                record
                    .adjudicated_dispositions
                    .entry(panel_id.clone())
                    .or_default()
                    .insert(finding.id.clone(), disposition);
                record
                    .adjudicated_records
                    .entry(panel_id.clone())
                    .or_default()
                    .insert(finding.id.clone(), finding.clone());
            } else {
                record
                    .member_dispositions
                    .entry(panel_id.clone())
                    .or_default()
                    .insert(finding.id.clone(), disposition);
                record
                    .member_records
                    .entry(panel_id.clone())
                    .or_default()
                    .insert(finding.id.clone(), finding.clone());
            }
            if ledger_unresolved(finding) {
                let by_panel = if adjudicator {
                    &mut record.adjudicated_findings
                } else {
                    &mut record.member_findings
                };
                let panel_findings = by_panel.entry(panel_id.clone()).or_default();
                if !panel_findings.contains(finding) {
                    panel_findings.push(finding.clone());
                }
            }
        }
        if !adjudicator && let Some(lens) = node.review_lens.as_deref() {
            record.head.covered_lenses.insert(lens.to_owned());
        }
        let (invariants, lenses) = outputs::finding_coverage(&findings);
        for (invariant, covered) in invariants {
            record
                .head
                .invariant_coverage
                .entry(invariant)
                .or_default()
                .extend(covered);
        }
        if !adjudicator {
            record.head.covered_lenses.extend(lenses);
        }
        if head != current_head {
            let position = last_previous_head
                .as_ref()
                .map_or(true, |(last_round, last_index, _)| {
                    (round_number, index) >= (*last_round, *last_index)
                });
            if position {
                last_previous_head = Some((round_number, index, head));
            }
        }
    }

    for (head, mut record) in heads {
        // An adjudicator resolves its panel's split. Its disposition replaces
        // that panel's reviewer findings; other panels in the same round stay.
        let mut findings = record.member_findings;
        for (panel, adjudicated) in record.adjudicated_findings {
            findings.insert(panel, adjudicated);
        }
        for panel_findings in findings.into_values() {
            for finding in panel_findings {
                if !record.head.unresolved.contains(&finding) {
                    record.head.unresolved.push(finding);
                }
            }
        }
        let mut dispositions = record.member_dispositions;
        for (panel, adjudicated) in record.adjudicated_dispositions {
            dispositions.insert(panel, adjudicated);
        }
        for (panel, findings) in dispositions {
            for (id, disposition) in findings {
                record
                    .head
                    .dispositions
                    .insert(format!("finding_disposition:{panel}:{id}"), disposition);
            }
        }
        let mut records = record.member_records;
        for (panel, adjudicated) in record.adjudicated_records {
            records.insert(panel, adjudicated);
        }
        record.head.findings = records
            .into_values()
            .flat_map(|values| values.into_values())
            .collect();
        ledger.heads.insert(head, record.head);
    }
    if !ledger.heads.contains_key(current_head)
        && let Some(previous) = stored_current
    {
        ledger.heads.insert(current_head.to_owned(), previous);
    }
    if let Some((_, _, head)) = last_previous_head {
        let new_round = ledger.heads.get(&head).map_or(0, |entry| entry.round);
        let old_round = previous_last_reviewed_head
            .as_deref()
            .and_then(|previous| ledger.heads.get(previous))
            .map_or(0, |entry| entry.round);
        if new_round >= old_round {
            ledger.last_reviewed_head = Some(head);
        }
    } else if previous_last_reviewed_head
        .as_deref()
        .is_some_and(|previous| ledger.heads.contains_key(previous))
    {
        ledger.last_reviewed_head = previous_last_reviewed_head;
    }
    ledger
}

#[derive(Default)]
struct ReviewHeadBuilder {
    round: usize,
    head: FindingHead,
    member_records: BTreeMap<String, BTreeMap<String, Finding>>,
    adjudicated_records: BTreeMap<String, BTreeMap<String, Finding>>,
    member_findings: BTreeMap<String, Vec<Finding>>,
    adjudicated_findings: BTreeMap<String, Vec<Finding>>,
    member_dispositions: BTreeMap<String, BTreeMap<String, String>>,
    adjudicated_dispositions: BTreeMap<String, BTreeMap<String, String>>,
}

fn add_review_closure(
    root: Option<&str>,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
    members: &mut BTreeSet<usize>,
) {
    let Some(root) = root else {
        return;
    };
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let Some(index) = positions.get(id).copied() else {
            continue;
        };
        if !members.insert(index) {
            continue;
        }
        if let Some(progress) = checkpoint.nodes.get(index) {
            pending.extend(progress.children.iter().map(String::as_str));
        }
        pending.extend(
            plan.nodes[index]
                .inputs
                .iter()
                .filter(|input| {
                    input.outputs.contains(&OutputKind::Findings)
                        || input.outputs.contains(&OutputKind::Verdict)
                })
                .map(|input| input.node.as_str()),
        );
    }
}

fn panel_review_settlement(
    panel_id: &str,
    pinned_head: Option<&str>,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &BTreeMap<String, usize>,
) -> Option<(String, Option<String>, usize)> {
    let mut panel_members = BTreeSet::new();
    add_review_closure(
        Some(panel_id),
        plan,
        checkpoint,
        positions,
        &mut panel_members,
    );
    panel_members
        .into_iter()
        .filter_map(|index| {
            let settlement = checkpoint.nodes.get(index)?.review_settlement.as_ref()?;
            let head = settlement.head.to_string();
            if pinned_head.is_some_and(|pinned| pinned != head.as_str()) {
                return None;
            }
            Some((
                head,
                settlement.base.map(|base| base.to_string()),
                plan.nodes[index]
                    .round
                    .as_ref()
                    .map_or(0, |round| round.round),
                index,
            ))
        })
        .max_by_key(|(_, _, round, index)| (*round, *index))
        .map(|(head, base, round, _)| (head, base, round))
}

#[cfg(test)]
mod convergence_tests {
    use super::*;

    fn finding(id: &str, severity: &str, status: &str) -> Finding {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "severity": severity,
            "file": "a.rs",
            "line": 1,
            "summary": "defect",
            "status": status
        }))
        .unwrap()
    }

    #[test]
    fn initial_synthesis_binding_resolves_panel_owner_and_shared_seed_stays_unassigned() {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "graph_id": "synthesis-panel-owner",
            "nodes": [
                {
                    "id": "initial-panel",
                    "kind": "composite",
                    "access": "read_only",
                    "composite": {
                        "reduce_verdict": true,
                        "nodes": [{"id":"lens", "access":"read_only", "prompt":"review"}],
                        "exports": [{"node":"lens", "outputs":["findings", "verdict"]}]
                    }
                },
                {
                    "id": "initial-synthesis",
                    "access": "read_only",
                    "prompt": "synthesise the panel",
                    "inputs": [{"node":"initial-panel", "outputs":["findings", "verdict"]}]
                },
                {"id":"cycle-a", "access":"read_only", "prompt":"repair"}
            ]
        }))
        .unwrap();
        let limits = crate::config::GraphLimits::default();
        let expanded = plans::expand(&document, &[], &[], &limits).unwrap();
        let (mut plan, _) = validate_flat_plan(&expanded, &limits).unwrap();
        let cycle = plan
            .nodes
            .iter_mut()
            .find(|node| node.id == "cycle-a")
            .unwrap();
        cycle.loop_body = Some(
            serde_json::from_value(serde_json::json!({
                "body": [], "until": "accepted", "max_rounds": 2,
                "previous_bindings": {"review": "initial-synthesis"}
            }))
            .unwrap(),
        );
        let panel = plan
            .nodes
            .iter()
            .position(|node| node.id == "initial-panel")
            .unwrap();
        assert_eq!(chain_owner_id(panel, &plan), "cycle-a");

        let mut second_cycle = plan.nodes[plan
            .nodes
            .iter()
            .position(|node| node.id == "cycle-a")
            .unwrap()]
        .clone();
        second_cycle.id = "cycle-b".into();
        plan.nodes.push(second_cycle);
        assert_eq!(
            chain_owner_id(panel, &plan),
            "initial-panel",
            "a shared initial panel remains a standalone seed for both consumers"
        );
        let mut checkpoint = transitions::initial_checkpoint(
            &document,
            &plan,
            "synthesis-panel-owner".into(),
            None,
            None,
            None,
        );
        let mut seed = FindingLedger::default();
        record_head(
            &mut seed,
            "1".repeat(40).as_str(),
            &[finding("F1", "high", "open")],
            BTreeMap::new(),
            BTreeSet::from(["correctness".into()]),
            BTreeMap::new(),
        );
        let seed_row = checkpoint
            .nodes
            .iter_mut()
            .find(|progress| progress.id == "initial-panel")
            .unwrap();
        seed_row.finding_ledger = Some(seed.clone());
        assert_eq!(
            initial_chain_ledger("cycle-a", &plan, &checkpoint),
            Some(seed.clone())
        );
        assert_eq!(
            initial_chain_ledger("cycle-b", &plan, &checkpoint),
            Some(seed)
        );
    }

    #[test]
    fn severity_aliases_decode() {
        assert_eq!(
            model::decode_severity("critical"),
            (Severity::Blocking, false)
        );
        assert_eq!(
            model::decode_severity("blocker"),
            (Severity::Blocking, false)
        );
        assert_eq!(
            model::decode_severity("blocking"),
            (Severity::Blocking, false)
        );
        assert_eq!(model::decode_severity("major"), (Severity::High, false));
        assert_eq!(model::decode_severity("high"), (Severity::High, false));
        assert_eq!(model::decode_severity("medium"), (Severity::Medium, false));
        assert_eq!(
            model::decode_severity("moderate"),
            (Severity::Medium, false)
        );
        assert_eq!(model::decode_severity("minor"), (Severity::Low, false));
        assert_eq!(model::decode_severity("low"), (Severity::Low, false));
        assert_eq!(model::decode_severity("nit"), (Severity::Nit, false));
        assert_eq!(model::decode_severity("info"), (Severity::Nit, false));
        assert_eq!(model::decode_severity("style"), (Severity::Nit, false));
        assert_eq!(model::decode_severity("  HIGH  "), (Severity::High, false));
        assert_eq!(
            model::decode_severity("catastrophic"),
            (Severity::High, true)
        );
        assert_eq!(
            FINDING_SEVERITY_UNKNOWN.as_str(),
            "FINDING_SEVERITY_UNKNOWN"
        );
    }

    #[test]
    fn accept_below_respects_blocking_class() {
        let high = finding("F1", "high", "open");
        assert!(!blocks_threshold(&high, AcceptBelow::Blocking));
        assert!(blocks_threshold(&high, AcceptBelow::High));
        assert_eq!(
            acceptance_decision(std::slice::from_ref(&high), AcceptBelow::Blocking, true),
            Acceptance::Accept
        );
        assert_eq!(
            acceptance_decision(std::slice::from_ref(&high), AcceptBelow::High, true),
            Acceptance::Reject("finding F1 severity High is not below High".into())
        );
        let boundary: Finding = serde_json::from_value(serde_json::json!({
            "id":"F2", "severity":"nit", "file":"a.rs", "summary":"defect", "status":"open",
            "triage": {
                "decision":"confirmed", "reason":"r", "worth_fixing":true,
                "root_cause":"c", "blocking_class":"boundary"
            }
        }))
        .unwrap();
        assert!(blocks_threshold(&boundary, AcceptBelow::Blocking));
        assert!(matches!(
            acceptance_decision(std::slice::from_ref(&boundary), AcceptBelow::Blocking, true),
            Acceptance::Reject(_)
        ));
        let mut closed_boundary = boundary.clone();
        closed_boundary.status = "closed".into();
        closed_boundary.disposition = Some("accepted".into());
        assert!(blocks_threshold(&closed_boundary, AcceptBelow::Blocking));
        assert!(matches!(
            acceptance_decision(&[closed_boundary], AcceptBelow::Blocking, true),
            Acceptance::Reject(_)
        ));
        let mut falsified_boundary = boundary.clone();
        falsified_boundary.verification = Some("falsified".into());
        falsified_boundary.evidence = Some("counterexample disproved by test".into());
        assert!(!blocks_threshold(
            &falsified_boundary,
            AcceptBelow::Blocking
        ));
        assert_eq!(
            acceptance_decision(&[falsified_boundary], AcceptBelow::Blocking, true),
            Acceptance::Accept
        );
        assert!(blocks_threshold(
            &finding("F-off", "nit", "open"),
            AcceptBelow::Off
        ));
        let mut no_trigger = finding("F3", "nit", "open");
        no_trigger.triage = Some(FindingTriage {
            decision: "confirmed".into(),
            reason: "r".into(),
            worth_fixing: true,
            root_cause: "c".into(),
            blocking_class: "acceptance".into(),
            shared_flaw: false,
        });
        let mut closed = no_trigger.clone();
        closed.status = "closed".into();
        assert!(!blocks_threshold(&closed, AcceptBelow::Off));
        assert!(!repair::blocks_acceptance_for(&closed, AcceptBelow::Off));
        assert_eq!(
            acceptance_decision(&[closed], AcceptBelow::Off, true),
            Acceptance::Accept
        );
        assert!(matches!(
            acceptance_decision(&[no_trigger], AcceptBelow::Blocking, true),
            Acceptance::Reject(_)
        ));
        assert!(matches!(
            acceptance_decision(&[], AcceptBelow::Off, true),
            Acceptance::Accept
        ));
        assert!(matches!(
            acceptance_decision(&[], AcceptBelow::High, false),
            Acceptance::Reject(_)
        ));
    }

    #[test]
    fn delta_round_carries_open_findings() {
        let mut ledger = FindingLedger::default();
        let mut open = finding("F1", "high", "open");
        open.lens = Some("correctness".into());
        let fixed = finding("F2", "low", "fixed");
        record_head(
            &mut ledger,
            "aaa",
            &[open.clone(), fixed],
            BTreeMap::new(),
            BTreeSet::from(["correctness".to_owned()]),
            BTreeMap::new(),
        );
        assert_eq!(ledger.last_reviewed_head.as_deref(), Some("aaa"));
        assert_eq!(carried_findings(&ledger, "aaa"), vec![open]);
        let scope = round_scope(&ledger, "bbb", "base1", Some("base1"), RoundMode::Delta);
        assert_eq!(
            scope,
            RoundScope::Delta {
                base: "aaa".into(),
                head: "bbb".into()
            }
        );
    }

    #[test]
    fn delta_widens_for_unattributable_findings_or_uncovered_invariants() {
        let open = finding("F1", "high", "open");
        let mut ledger = FindingLedger::default();
        record_head(
            &mut ledger,
            "aaa",
            &[open.clone()],
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeMap::new(),
        );
        assert_eq!(
            round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
            RoundScope::Full { head: "bbb".into() }
        );

        let mut attributable = open;
        attributable.lens = Some("correctness".into());
        attributable.class = Some("invariant-a".into());
        record_head(
            &mut ledger,
            "aaa",
            &[attributable],
            BTreeMap::new(),
            BTreeSet::from(["correctness".into()]),
            BTreeMap::new(),
        );
        assert_eq!(
            round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
            RoundScope::Full { head: "bbb".into() }
        );

        // An invariant still has a usable prior lens, so one unrelated
        // unavailable lens does not widen every later round.
        ledger
            .heads
            .get_mut("aaa")
            .unwrap()
            .invariant_coverage
            .get_mut("invariant-a")
            .unwrap()
            .insert("tests".into());
        assert_eq!(
            round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
            RoundScope::Delta {
                base: "aaa".into(),
                head: "bbb".into()
            }
        );
        // Without a pinned prior base the old delta cannot be attributed.
        assert_eq!(
            round_scope(&ledger, "bbb", "base", None, RoundMode::Delta),
            RoundScope::Full { head: "bbb".into() }
        );

        let entry = ledger.heads.get_mut("aaa").unwrap();
        entry
            .invariant_coverage
            .insert("invariant-a".into(), BTreeSet::from(["correctness".into()]));
        entry.unavailable_lenses.insert("correctness".into());
        assert_eq!(
            round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
            RoundScope::Full { head: "bbb".into() }
        );
    }

    #[test]
    fn delta_round_is_valid_when_no_findings_remain_open() {
        let mut ledger = FindingLedger::default();
        record_head(
            &mut ledger,
            "aaa",
            &[finding("F1", "low", "fixed")],
            BTreeMap::new(),
            BTreeSet::from(["correctness".into()]),
            BTreeMap::new(),
        );
        assert_eq!(
            round_scope(&ledger, "bbb", "base", Some("base"), RoundMode::Delta),
            RoundScope::Delta {
                base: "aaa".into(),
                head: "bbb".into()
            }
        );
    }

    #[test]
    fn base_move_widens_to_full() {
        let mut ledger = FindingLedger::default();
        record_head(
            &mut ledger,
            "aaa",
            &[finding("F1", "high", "open")],
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeMap::new(),
        );
        let moved = round_scope(&ledger, "bbb", "base2", Some("base1"), RoundMode::Delta);
        assert_eq!(moved, RoundScope::Full { head: "bbb".into() });
        let explicit = round_scope(&ledger, "bbb", "base1", Some("base1"), RoundMode::Full);
        assert_eq!(explicit, RoundScope::Full { head: "bbb".into() });
    }

    #[test]
    fn sole_unavailable_lens_leaves_invariant_uncovered() {
        let gaps = coverage_gaps(
            &BTreeSet::new(),
            &["correctness".to_owned()],
            &BTreeMap::new(),
            &["invariant-A".to_owned()],
        );
        assert!(gaps.iter().any(|gap| gap.contains("invariant-A")));
        assert!(gaps.iter().any(|gap| gap.contains("correctness")));
        // An uncovered invariant is INCONCLUSIVE, never an agreed PASS.
        assert!(!gaps.is_empty());
    }

    #[test]
    fn contradictory_blocking_findings_split_even_with_matching_verdicts() {
        let mut confirmed = finding("a:F1", "high", "open");
        confirmed.class = Some("custody".into());
        confirmed.triage = Some(FindingTriage {
            decision: "confirmed".into(),
            reason: "observed".into(),
            worth_fixing: true,
            root_cause: "race".into(),
            blocking_class: "boundary".into(),
            shared_flaw: false,
        });
        let mut rejected = confirmed.clone();
        rejected.id = "b:F1".into();
        rejected.triage.as_mut().unwrap().decision = "rejected".into();
        assert!(contradictory_blocking_findings(&[
            confirmed.clone(),
            rejected.clone()
        ]));
        confirmed.triage.as_mut().unwrap().blocking_class = "none".into();
        rejected.triage.as_mut().unwrap().blocking_class = "none".into();
        assert!(contradictory_blocking_findings(&[confirmed, rejected]));
    }

    #[test]
    fn split_goes_to_one_adjudicator_no_round() {
        use provenant_records::workflow::ReviewVerdict;
        assert!(split_needs_adjudicator(&[
            ReviewVerdict::Approve,
            ReviewVerdict::RequestChanges
        ]));
        assert!(!split_needs_adjudicator(&[
            ReviewVerdict::Approve,
            ReviewVerdict::Approve
        ]));
        assert!(!split_needs_adjudicator(&[
            ReviewVerdict::Approve,
            ReviewVerdict::Inconclusive
        ]));
        assert!(split_needs_adjudicator(&[
            ReviewVerdict::ApproveWithNits,
            ReviewVerdict::RequestChanges
        ]));
        assert!(!split_needs_adjudicator(&[]));
    }

    #[test]
    fn missing_evidence_rereviews_never_repairs() {
        assert_eq!(
            lens_outcome(Some("missing_evidence"), 0, 2, true),
            LensOutcome::Retry
        );
        assert_eq!(
            lens_outcome(Some("missing_evidence"), 2, 2, true),
            LensOutcome::Unavailable
        );
        assert_eq!(
            lens_outcome(Some("blocked_environment"), 0, 1, true),
            LensOutcome::Retry
        );
        assert_eq!(
            lens_outcome(Some("blocked_environment"), 1, 1, true),
            LensOutcome::Unavailable
        );
        assert_eq!(
            lens_outcome(Some("model_unavailable"), 1, 1, false),
            LensOutcome::Unavailable
        );
        assert_eq!(lens_outcome(None, 0, 1, true), LensOutcome::Completed);
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    #[test]
    fn delivery_review_requires_complete_nonblocking_evidence() {
        let mut outputs = NodeOutputs {
            verdict: Some("VERDICT: INCONCLUSIVE".into()),
            findings_available: Some(true),
            ..Default::default()
        };
        assert!(delivery_matches(
            AcceptBelow::Blocking,
            &outputs,
            "VERDICT: PASS"
        ));
        assert!(!delivery_matches(
            AcceptBelow::Off,
            &outputs,
            "VERDICT: PASS"
        ));
        outputs.findings_available = None;
        assert!(!delivery_matches(
            AcceptBelow::Blocking,
            &outputs,
            "VERDICT: PASS"
        ));
        outputs.findings_available = Some(true);
        outputs.partial = true;
        assert!(!delivery_matches(
            AcceptBelow::Blocking,
            &outputs,
            "VERDICT: PASS"
        ));
        outputs.partial = false;
        outputs.findings.push(
            serde_json::from_value(serde_json::json!({
                "id":"F1", "severity":"blocking", "file":"a.rs", "line":1,
                "summary":"defect", "status":"open"
            }))
            .unwrap(),
        );
        assert!(!delivery_matches(
            AcceptBelow::Blocking,
            &outputs,
            "VERDICT: PASS"
        ));
        outputs.findings.clear();
        outputs.verdict = Some("VERDICT: FAIL".into());
        assert!(!delivery_matches(
            AcceptBelow::Blocking,
            &outputs,
            "VERDICT: PASS"
        ));
    }

    #[test]
    fn delivery_review_evidence_parks_contract_errors_but_not_valid_failures() {
        assert!(malformed_delivery_evidence_error(Some(
            "review_verdict_invalid"
        )));
        assert!(malformed_delivery_evidence_error(Some(
            "review_findings_invalid"
        )));
        assert!(malformed_delivery_evidence_error(Some(
            "review_triage_missing"
        )));
        assert!(!malformed_delivery_evidence_error(None));
        assert!(!malformed_delivery_evidence_error(Some("review_failed")));
    }
}
