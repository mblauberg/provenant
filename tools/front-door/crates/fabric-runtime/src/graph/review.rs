//! Deterministic contract for review template results and coverage.
use super::*;

pub(super) const CORRECTION_REFUSED: &str = "review_verdict_correction_refused";

pub(super) const PARTIAL: &str = "review_partial";

pub(super) fn partial(outputs: &NodeOutputs) -> bool {
    outputs.extra.get(PARTIAL) == Some(&serde_json::Value::Bool(true))
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
                && progress
                    .attempts
                    .last()
                    .is_some_and(|attempt| attempt.outcome == Some(AttemptOutcome::TaskFailed))))
        && progress
            .outputs
            .as_ref()
            .is_some_and(|outputs| !outputs.findings.is_empty())
}

pub(super) fn mark_partial(outputs: &mut NodeOutputs) {
    outputs.extra.insert(PARTIAL.into(), true.into());
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
    template::review_behaviour(node)?;
    // ResultCapture has already validated uniqueness and final position while
    // allowing the shared trailing handoff fence. `verdict_error` also carries
    // source truncation, which is diagnosed by findings availability below;
    // do not misreport a captured verdict as malformed for that reason.
    if !matches!(
        outputs.verdict.as_deref(),
        Some("VERDICT: PASS" | "VERDICT: FAIL" | "VERDICT: INCONCLUSIVE")
    ) {
        return Some((
            "review_verdict_invalid",
            "review needs exactly one final verdict: PASS, FAIL or INCONCLUSIVE".into(),
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
            (
                "evidence",
                finding
                    .extra
                    .get("evidence")
                    .and_then(serde_json::Value::as_str),
            ),
            (
                "proposed_fix",
                finding
                    .extra
                    .get("proposed_fix")
                    .and_then(serde_json::Value::as_str),
            ),
        ] {
            if value.is_none_or(|value| value.trim().is_empty()) {
                return Some((
                    "review_findings_invalid",
                    format!("finding {} needs non-empty string {field}", finding.id),
                ));
            }
        }
    }
    if outputs.verdict.as_deref() == Some("VERDICT: PASS")
        && outputs.findings.iter().any(|finding| {
            (repair::blocking(finding)
                && !(upstream.iter().any(|source| source.id == finding.id)
                    && finding
                        .extra
                        .get("verification")
                        .and_then(serde_json::Value::as_str)
                        == Some("falsified")))
                || finding.disposition.as_deref() == Some("accepted")
                || finding
                    .extra
                    .get("verification")
                    .and_then(serde_json::Value::as_str)
                    == Some("unverifiable")
        })
    {
        return Some((
            "review_verdict_conflict",
            "PASS contradicts unresolved blocking, accepted or unverifiable findings".into(),
        ));
    }
    None
}

/// A malformed verdict gets one isolated, checkpointed correction. Expansion
/// history survives resets and restarts, so it is also the retry fence.
pub(super) fn correct_verdict(
    document: &GraphDocument,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    limits: &crate::config::GraphLimits,
) -> Result<Option<GraphCheckpoint>, PatchError> {
    if checkpoint.cancelled || checkpoint.pause_reason.is_some() {
        return Ok(None);
    }
    let Some((index, node)) = plan.nodes.iter().enumerate().find(|(index, node)| {
        template::review_behaviour(node) == Some(template::ReviewBehaviour::Lens)
            && checkpoint.nodes[*index].state == NodeState::Failed
            && !checkpoint.nodes[*index]
                .extra
                .contains_key(CORRECTION_REFUSED)
            && checkpoint.nodes[*index].advance_error_code.as_deref()
                == Some("review_verdict_invalid")
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
        "{}\n\nThe previous reply had an invalid final verdict. Return the findings and end with exactly one final line: VERDICT: PASS, VERDICT: FAIL or VERDICT: INCONCLUSIVE. Only the shared closed handoff block may follow. This is the only automatic verdict correction.",
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
            library: &TemplateLibrary::bundled(),
            resolvers: &Default::default(),
            limits,
            scan: &patch::scan_for_secrets,
            journal: None,
        },
    ) {
        // Admission is atomic: keep the failed attempt and fence further
        // automatic corrections so coverage can settle on subsequent advances.
        next = checkpoint.clone();
        next.nodes[index].extra.insert(
            CORRECTION_REFUSED.into(),
            bounded(&error.to_string(), MAX_ADVANCE_ERROR_BYTES).into(),
        );
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
