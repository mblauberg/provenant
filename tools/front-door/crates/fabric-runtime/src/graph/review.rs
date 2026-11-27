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
    policy: &DeliveryReviewSpec,
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
    let threshold = policy.accept_below;
    let agrees = match outputs
        .verdict
        .as_deref()
        .and_then(ReviewVerdict::from_token)
    {
        Some(ReviewVerdict::Approve) => true,
        Some(ReviewVerdict::Inconclusive) => policy.accept_below != AcceptBelow::Off,
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
            (repair::blocks_acceptance(finding)
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
) -> Option<(&'static str, String)> {
    if !repair_writer(node) {
        return None;
    }
    let required: std::collections::BTreeSet<_> = upstream
        .iter()
        .filter(|finding| repair::repair_eligible(finding))
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

/// A malformed verdict gets one isolated, checkpointed correction. Expansion
/// history survives resets and restarts, so it is also the retry fence.
pub(super) fn correct_verdict(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
    worktree_layout: &provenant_config::WorktreeLayoutPort,
) -> Result<Option<GraphCheckpoint>, PatchError> {
    if checkpoint.cancelled || checkpoint.pause.is_some() {
        return Ok(None);
    }
    let Some((index, node)) = plan.nodes.iter().enumerate().find(|(index, node)| {
        template::review_behaviour(node) == Some(template::ReviewBehaviour::Lens)
            && checkpoint.nodes[*index].state == NodeState::Failed
            && checkpoint.nodes[*index]
                .review_verdict_correction_refused
                .is_none()
            && checkpoint.nodes[*index].advance_code.as_deref() == Some("review_verdict_invalid")
            && !checkpoint
                .expansions
                .iter()
                .filter_map(|expansion| expansion.patch.as_ref())
                .any(|patch| {
                    patch.reason.as_deref()
                        == Some(format!("correct review verdict {}", node.id).as_str())
                })
    }) else {
        return Ok(None);
    };
    let mut next = checkpoint.clone();
    let brief = format!(
        "{}\n\nThe previous reply had an invalid verdict. Return the findings. {VERDICT_LAST} This is the only automatic verdict correction.",
        node.prompt
    );
    if let Err(error) = patch::correct_review_verdict(
        &mut LiveGraph {
            document,
            checkpoint: &mut next,
        },
        &node.id,
        &brief,
        &PatchContext {
            catalogue: None,
            library: &TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            limits,

            journal: None,
            worktree_layout: worktree_layout.clone(),
        },
    ) {
        if let Some(PlanFailureFact::Structural(limit)) = &error.cause {
            next = checkpoint.clone();
            plans::set_expansion_pause(
                &mut next,
                index,
                limit.to_string(),
                false,
                None,
                Some(*limit),
            );
            return Ok(Some(next));
        }
        // Admission is atomic: keep the failed attempt and fence further
        // automatic corrections so coverage can settle on subsequent advances.
        next = checkpoint.clone();
        if next.nodes[index].expansion_pause.is_some() {
            plans::clear_expansion_pause(&mut next, index, true);
        }
        next.nodes[index].review_verdict_correction_refused =
            Some(bounded(&error.to_string(), MAX_ADVANCE_NOTE_BYTES));
        return Ok(Some(next));
    }
    transitions::retain_automatic_retry_accounting(
        &checkpoint.nodes[index],
        &mut next.nodes[index],
    );
    Ok(Some(next))
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
    let Some(ceiling) = threshold_severity(threshold) else {
        return findings.iter().find(|finding| is_open(finding)).map_or(
            Acceptance::Accept,
            |finding| {
                Acceptance::Reject(format!(
                    "finding {} is open but acceptance is off",
                    finding.id
                ))
            },
        );
    };
    for finding in findings.iter().filter(|finding| is_open(finding)) {
        if finding.has_blocking_class() {
            return Acceptance::Reject(format!(
                "finding {} has an open blocking class",
                finding.id
            ));
        }
        let (severity, _) = finding_severity(finding);
        if severity >= ceiling {
            return Acceptance::Reject(format!(
                "finding {} severity {severity:?} is not below {ceiling:?}",
                finding.id
            ));
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
/// findings. A moved base, an unattributable delta (no previous head), or no
/// open findings widens the round to the full head.
pub(super) fn round_scope(
    ledger: &FindingLedger,
    head: &str,
    base_head: &str,
    previous_base: Option<&str>,
    mode: RoundMode,
) -> RoundScope {
    let last = ledger.last_reviewed_head.as_deref();
    let base_moved = previous_base.is_some_and(|previous| previous != base_head);
    let has_open = last
        .and_then(|last_head| ledger.heads.get(last_head))
        .is_some_and(|entry| !entry.unresolved.is_empty());
    if mode == RoundMode::Delta
        && !base_moved
        && last.is_some_and(|last_head| last_head != head)
        && has_open
    {
        RoundScope::Delta {
            base: last.unwrap_or("").to_owned(),
            head: head.to_owned(),
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
        .filter(|finding| is_open(finding))
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
    verdicts
        .first()
        .is_some_and(|first| verdicts.iter().any(|verdict| verdict != first))
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
                finding.has_blocking_class()
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
    let mut ledger = FindingLedger::default();
    for (index, progress) in checkpoint.nodes.iter().enumerate().take(boundary) {
        let Some(settlement) = progress.review_settlement.as_ref() else {
            continue;
        };
        let Some(outputs) = progress.outputs.as_ref() else {
            continue;
        };
        if progress.state != NodeState::Succeeded
            || outputs.partial
            || outputs.verdict_error.is_some()
        {
            continue;
        }
        let head = settlement.head.to_string();
        let entry = ledger.heads.entry(head.clone()).or_default();
        if let Some(base) = settlement.base {
            entry
                .dispositions
                .insert("base".to_owned(), base.to_string());
        }
        entry.unresolved.extend(
            outputs
                .findings
                .iter()
                .filter(|finding| is_open(finding))
                .cloned(),
        );
        if let Some(lens) = plan
            .nodes
            .get(index)
            .and_then(|node| node.review_lens.as_deref())
        {
            entry.covered_lenses.insert(lens.to_owned());
        }
        let (invariants, lenses) = outputs::finding_coverage(&outputs.findings);
        for (invariant, covered) in invariants {
            entry
                .invariant_coverage
                .entry(invariant)
                .or_default()
                .extend(covered);
        }
        entry.covered_lenses.extend(lenses);
        if head != current_head {
            ledger.last_reviewed_head = Some(head);
        }
    }
    ledger
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
        assert!(matches!(
            acceptance_decision(std::slice::from_ref(&boundary), AcceptBelow::Blocking, true),
            Acceptance::Reject(_)
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
        let open = finding("F1", "high", "open");
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
        let mut policy = DeliveryReviewSpec::default();
        let mut outputs = NodeOutputs {
            verdict: Some("VERDICT: INCONCLUSIVE".into()),
            findings_available: Some(true),
            ..Default::default()
        };
        assert!(delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        policy.accept_below = AcceptBelow::Off;
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        policy.accept_below = AcceptBelow::Blocking;
        outputs.findings_available = None;
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        outputs.findings_available = Some(true);
        outputs.partial = true;
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        outputs.partial = false;
        outputs.findings.push(
            serde_json::from_value(serde_json::json!({
                "id":"F1", "severity":"blocking", "file":"a.rs", "line":1,
                "summary":"defect", "status":"open"
            }))
            .unwrap(),
        );
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
        outputs.findings.clear();
        outputs.verdict = Some("VERDICT: FAIL".into());
        assert!(!delivery_matches(&policy, &outputs, "VERDICT: PASS"));
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
