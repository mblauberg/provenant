//! Pure graph state transitions. The scheduler observes host facts, applies one
//! transition and saves before admitting the next effect. This module deliberately
//! takes neither an `AttemptOwner` nor a `GraphJournal`.
use super::*;

/// Apply an accepted, nonterminal attempt observation without I/O.
pub(super) fn apply_pending_observation(
    attempt: &mut GraphAttempt,
    state: &mut NodeState,
    reference: Option<String>,
) {
    attempt.state = AttemptState::Running;
    attempt.attempt_ref = reference;
    *state = NodeState::Running;
}

/// Apply an unresolved attempt observation without authorising a new route.
pub(super) fn apply_unknown_observation(
    attempt: &mut GraphAttempt,
    state: &mut NodeState,
    reference: Option<String>,
) {
    attempt.state = AttemptState::Unresolved;
    attempt.attempt_ref = reference;
    *state = NodeState::Running;
}

/// Apply a settled gate using already-observed inputs, before checkpoint save.
pub(super) fn apply_gate_result(
    progress: &mut NodeProgress,
    reason: Option<String>,
    on_fail: Option<String>,
    is_ci: bool,
) {
    let passed = reason.is_none();
    progress.state = if passed {
        NodeState::Succeeded
    } else {
        NodeState::Failed
    };
    progress.gate = Some(GateResult {
        extra: Default::default(),
        passed,
        reason: reason.map(|reason| bounded(&reason, OUTPUT_FIELD_BYTES)),
        on_fail,
    });
    progress.outputs = Some(if is_ci {
        ci::outputs(passed, progress.ci.as_ref())
    } else {
        gate_outputs(passed)
    });
    if let Some(ci) = progress.ci.as_mut() {
        ci.wake_at = None;
    }
}

/// Apply redacted launch metadata from the owner without reading host state.
pub(super) fn apply_launch_metadata(
    checkpoint: &mut GraphCheckpoint,
    node_index: usize,
    warnings: Vec<String>,
) {
    if !warnings.is_empty() {
        checkpoint.nodes[node_index]
            .extra
            .insert("warnings".into(), serde_json::json!(warnings));
    }
}

pub(super) fn apply_route(progress: &mut NodeProgress, applied_route: Option<GraphRoute>) {
    if let Some(mut route) = applied_route {
        let clean = |value: &mut Option<String>| {
            if value.as_ref().is_some_and(|value| {
                value.is_empty()
                    || value.len() > HOST_ID_BYTES
                    || value.starts_with('/')
                    || !value.bytes().all(|b| {
                        b.is_ascii_alphanumeric()
                            || matches!(b, b'/' | b'-' | b'_' | b'.' | b':' | b'@' | b'+')
                    })
            }) {
                *value = None;
            }
        };
        for value in [
            &mut route.adapter,
            &mut route.name,
            &mut route.model,
            &mut route.alias,
            &mut route.effort,
            &mut route.account,
        ] {
            clean(value);
        }
        // An applied route identifies one selection. Keep its concrete
        // model when present, otherwise retain the alias before a name.
        // This also bounds the retained selection to four fields.
        if route.model.is_some() {
            route.alias = None;
            route.name = None;
        } else if route.alias.is_some() {
            route.name = None;
        }
        route.extra.clear();
        if let Some(manifest) = progress
            .attempts
            .last_mut()
            .and_then(|attempt| attempt.input_manifest.as_mut())
        {
            manifest.applied_route = Some(route);
        }
    }
}

/// Owner facts collected before applying an attempt observation. In particular,
/// a failed range lookup cannot leave a partly settled attempt to be saved.
pub(super) struct ObservationFacts {
    pub route_index: usize,
    pub usage: Option<AttemptUsage>,
    pub fallback_evidence: Option<PreparationFallbackEvidence>,
    pub evidence_unreadable: bool,
    pub fallback_cause: Option<provenant_records::RouteExclusionReason>,
    pub review_range: ReviewRange,
}

pub(super) struct AppliedObservation {
    pub settled_result: Option<Box<crate::result_capture::CapturedResult>>,
    pub retry_was_pending: bool,
    pub should_continue: bool,
}

/// Pure attempt and node settlement. No owner, clock, filesystem or journal is
/// available here; the caller saves the whole resulting transition atomically.
pub(super) fn apply_attempt_observation(
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
    node_index: usize,
    limits: &crate::config::GraphLimits,
    observation: AttemptObservation,
    facts: ObservationFacts,
) -> Result<AppliedObservation, &'static str> {
    let node = &plan.nodes[node_index];
    let ObservationFacts {
        route_index,
        usage,
        fallback_evidence,
        evidence_unreadable,
        fallback_cause,
        review_range,
    } = facts;
    // Synthesis retains qualified evidence even without a disposition contract.
    let upstream_findings: Vec<Finding> = if node.dispositions.is_empty()
        && node.kind != Some(NodeKind::Synthesise)
        && !template::review_verifier(node)
    {
        Default::default()
    } else {
        selected_findings(node, plan, checkpoint, positions)
            .into_iter()
            .map(|(source, finding)| upstream_finding(node, &source, finding))
            .collect()
    };
    let proof_dispositions = if node.dispositions.is_empty() && template::review_verifier(node) {
        vec![
            "upheld".to_owned(),
            "falsified".to_owned(),
            "unverifiable".to_owned(),
        ]
    } else {
        node.dispositions.clone()
    };
    let overflow_ids = if proof_dispositions.is_empty() {
        Vec::new()
    } else {
        truncated_finding_ids(node, plan, checkpoint, positions)
    };
    let marker_inputs = propagation_inputs(node, plan, checkpoint, positions);
    let partial_review = template::review_behaviour(node).is_some()
        && review::partial_inputs(node, checkpoint, positions);
    let mut settled_result = None;
    let retry_was_pending;
    let should_continue = {
        let progress = &mut checkpoint.nodes[node_index];
        let attempt = progress
            .attempts
            .last_mut()
            .ok_or("an observed attempt must have a checkpoint row")?;
        // Any observation answers the stopped launch, so its wait
        // is over and the checkpoint wake no longer owes it.
        retry_was_pending = progress.launch_retry.take().is_some();
        if retry_was_pending {
            progress.advance_owner_code = None;
        }
        match observation {
            AttemptObservation::Pending { attempt_ref } => {
                apply_pending_observation(attempt, &mut progress.state, attempt_ref);
                false
            }
            AttemptObservation::Unknown { attempt_ref } => {
                apply_unknown_observation(attempt, &mut progress.state, attempt_ref);
                false
            }
            AttemptObservation::Terminal {
                attempt_ref,
                outcome,
                launched,
                result,
                result_warning,
                result_path,
                branch,
                commit,
            } => {
                settled_result = result.clone();
                attempt.usage = usage;
                attempt.launched = launched;
                attempt.state = AttemptState::Terminal;
                attempt.attempt_ref = attempt_ref;
                // Provider success alone does not satisfy an explicit result contract.
                let warned = result_warning.is_some();
                let mut outputs = make_outputs_with_upstream(
                    result.as_deref(),
                    result_warning,
                    result_path,
                    branch,
                    commit,
                    &upstream_findings,
                );
                if !plan.nodes.iter().any(|consumer| {
                    consumer.extra.get("repair_scope_gate") == Some(&serde_json::Value::Bool(true))
                        && consumer.depends_on.contains(&node.id)
                }) {
                    outputs.extra.remove("repair_design");
                }
                if node.kind == Some(NodeKind::Synthesise) && node.dispositions.is_empty() {
                    retain_synthesis_findings(&mut outputs, &upstream_findings);
                }
                if node.access == AccessMode::ReadOnly
                    && let Some(lens) = node
                        .role
                        .as_deref()
                        .and_then(|role| role.strip_prefix("lens."))
                        .or_else(|| {
                            node.extra
                                .get("review_lens")
                                .and_then(serde_json::Value::as_str)
                        })
                {
                    for finding in &mut outputs.findings {
                        if !upstream_findings
                            .iter()
                            .any(|source| source.id == finding.id)
                        {
                            finding.extra.insert("lens".into(), lens.into());
                        }
                    }
                }
                let marker_warnings = propagation_warnings(&outputs, &marker_inputs);
                let propagation_error = template::review_behaviour(node).and_then(|_| {
                    marker_warnings
                        .first()
                        .map(|warning| ("review_propagation_invalid", warning.clone()))
                });
                if !marker_warnings.is_empty() {
                    outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(2));
                    outputs.warnings.extend(marker_warnings);
                }
                let review_contract_error =
                    review::contract_error(node, &upstream_findings, &outputs);
                if node.access == AccessMode::ReadOnly
                    && template::review_node(node)
                    && attempt.attempt_ref.is_some()
                {
                    match review_range {
                        ReviewRange::NoBase => {
                            outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(1));
                            outputs
                                .warnings
                                .push("review_base_unresolved: no base was resolved".into());
                        }
                        ReviewRange::Resolved { base, head } if base == head => {
                            outputs.verdict = Some("VERDICT: INCONCLUSIVE".into());
                            outputs.verdict_error = None;
                            outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(1));
                            outputs.warnings.push(format!("equal_base_and_head: review range is empty (base {base}, head {head})"));
                        }
                        _ => {}
                    }
                }
                outputs.warnings.truncate(MAX_WARNINGS);
                // A node that neither promises findings nor reviews has no block to miss.
                if !node.require_findings
                    && node.dispositions.is_empty()
                    && !template::review_node(node)
                    && node.kind != Some(NodeKind::Synthesise)
                    && !node
                        .inputs
                        .iter()
                        .any(|input| input.outputs.contains(&OutputKind::Findings))
                {
                    outputs
                        .warnings
                        .retain(|warning| !warning.starts_with(FINDINGS_INVALID));
                }
                if outcome == AttemptOutcome::UsageLimited {
                    outputs.findings_available = Some(false);
                    outputs
                        .warnings
                        .retain(|warning| !warning.starts_with(FINDINGS_INVALID));
                }
                let disposition_error = if node.require_findings
                    && (outputs.result.is_none()
                        || outputs
                            .warnings
                            .iter()
                            .any(|warning| warning.starts_with(FINDINGS_INVALID)))
                {
                    Some((
                        "findings_required",
                        outputs
                            .warnings
                            .iter()
                            .find(|warning| warning.starts_with(FINDINGS_INVALID))
                            .cloned()
                            .unwrap_or_else(|| {
                                "the findings block is missing or invalid".to_owned()
                            }),
                    ))
                } else {
                    // Overflow proof covers all findings from truncated composites;
                    // their structured subset may differ across selected inputs.
                    let required_findings: Vec<_> = upstream_findings
                        .iter()
                        .filter(|finding| !overflow_ids.iter().any(|(id, _)| id == &finding.id))
                        .cloned()
                        .collect();
                    disposition_error(&node.dispositions, &required_findings, &outputs)
                                    .or_else(|| review_verification_error(node, &upstream_findings, &required_findings, &overflow_ids, &outputs))
                                    .or_else(|| {
                                        (!overflow_ids.is_empty() && result.as_ref().is_none_or(|capture| !capture.disposition_proof.matches(&overflow_ids, &proof_dispositions))).then(|| (
                                            "disposition_overflow_incomplete",
                                            format!("all {} findings from truncated composites need exactly one DISPOSITION line with an allowed disposition and non-empty reason; composite limits are {MAX_FINDINGS} entries or {DISPOSITION_FINDINGS_BYTES} canonical bytes: read the staged full member results and emit a line for every finding", overflow_ids.len()),
                                        ))
                                    })
                                    .or_else(|| {
                                        let proof = &result.as_ref()?.disposition_proof;
                                        let verifier = node.dispositions.is_empty() && template::review_verifier(node);
                                        let mismatch = outputs.findings.iter().any(|finding| {
                                            if !overflow_ids.iter().any(|(id, _)| id == &finding.id) { return false; }
                                            let status = if verifier { finding.extra.get("verification").and_then(serde_json::Value::as_str) } else { finding.disposition.as_deref() };
                                            status.is_none_or(|status| !proof.agrees(&finding.id, status, &overflow_ids))
                                        });
                                        mismatch.then(|| ("disposition_overflow_inconsistent", "structured findings disagree with their DISPOSITION lines".to_owned()))
                                    })
                };
                let disposition_error = disposition_error
                    .or(propagation_error)
                    .or(review_contract_error);
                if template::review_behaviour(node).is_some() {
                    retain_synthesis_findings(&mut outputs, &upstream_findings);
                    if let Some((code, _)) = &disposition_error {
                        outputs.verdict_error = Some((*code).into());
                        outputs.extra.insert(review::PARTIAL.into(), true.into());
                        outputs.findings_available = Some(false);
                    } else if partial_review {
                        review::mark_partial(&mut outputs);
                        outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(1));
                        outputs
                            .warnings
                            .push("review_partial: selected review coverage is incomplete".into());
                    }
                }
                let review_result = node.access == AccessMode::ReadOnly
                    && template::review_node(node)
                    && outputs.verdict.is_some()
                    && outputs.verdict_error.is_none()
                    && outputs.result.is_some()
                    && !outputs
                        .warnings
                        .iter()
                        .any(|warning| warning.starts_with(FINDINGS_INVALID))
                    && disposition_error.is_none();
                let outcome = if matches!(
                    outcome,
                    AttemptOutcome::Succeeded
                        | AttemptOutcome::DeclaredStop
                        | AttemptOutcome::StoppedNoWork
                ) && disposition_error.is_some()
                {
                    AttemptOutcome::TaskFailed
                } else if outcome == AttemptOutcome::DeclaredStop && review_result && !warned {
                    AttemptOutcome::Succeeded
                } else if outcome == AttemptOutcome::DeclaredStop
                    && !warned
                    && launched
                    && plans::writer(node)
                    && result
                        .as_ref()
                        .is_some_and(|capture| plans::handoff(capture).ok().flatten().is_some())
                {
                    // A typed unfinished writer handoff is settled work whose
                    // continuation must be admitted (or returned to the chair).
                    AttemptOutcome::Succeeded
                } else if !warned
                    && launched
                    && node.access == AccessMode::WorktreeWrite
                    && outcome == AttemptOutcome::StoppedNoWork
                {
                    progress.advance_error_code = Some("stopped_no_work".into());
                    progress.advance_error =
                        Some("writer stopped with no work: no new commit".into());
                    AttemptOutcome::StoppedNoWork
                } else if warned || outcome == AttemptOutcome::DeclaredStop {
                    AttemptOutcome::OtherFailure
                } else if outcome == AttemptOutcome::Succeeded
                    && (!review_result
                        && node.required_verdict.is_some()
                        && !(plans::writer(node)
                            && result.as_ref().is_some_and(|capture| {
                                plans::handoff(capture).ok().flatten().is_some()
                            }))
                        && !result
                            .as_ref()
                            .is_some_and(|capture| capture.matched_required)
                        || disposition_error.is_some())
                {
                    AttemptOutcome::TaskFailed
                } else {
                    outcome
                };
                attempt.outcome = Some(outcome);
                if outcome == AttemptOutcome::TaskFailed
                    && let Some((code, reason)) = disposition_error
                {
                    progress.advance_error_code = Some(code.into());
                    progress.advance_error = Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
                }
                if node.access == AccessMode::WorktreeWrite {
                    progress.handoff = result
                        .as_ref()
                        .and_then(|capture| capture.handoff.as_ref().ok())
                        .and_then(|handoff| handoff.as_ref())
                        .filter(|handoff| handoff.unfinished())
                        .map(provenant_records::handoff::Handoff::fenced);
                }
                compact_findings(&mut outputs, result.as_deref(), &upstream_findings);
                progress.outputs = Some(outputs);
                match outcome {
                    AttemptOutcome::Succeeded => {
                        progress.state = NodeState::Succeeded;
                        false
                    }
                    AttemptOutcome::AuthRequired
                    | AttemptOutcome::UsageLimited
                    | AttemptOutcome::AdapterUnavailable => {
                        let review_limit_reached = review_preparation_substitution(node, attempt)
                            && progress
                                .attempts
                                .iter()
                                .filter(|attempt| attempt.review_substitution)
                                .count()
                                >= limits.review_substitutions;
                        match settlement_fallback_decision(
                            node,
                            route_index,
                            outcome,
                            launched,
                            FallbackEvidence {
                                preparation: fallback_evidence,
                                unreadable: evidence_unreadable,
                                review_limit_reached,
                                cause: fallback_cause.as_ref(),
                            },
                        ) {
                            SettlementFallback::Continue => true,
                            SettlementFallback::Fail { code, message } => {
                                fail_node(progress, code, message);
                                false
                            }
                        }
                    }
                    AttemptOutcome::DeadlineExceeded => {
                        progress.advance_error_code = Some("deadline_exceeded".into());
                        progress.advance_error =
                            Some("node deadline exceeded; lane was cancelled".into());
                        progress.state = NodeState::Failed;
                        false
                    }
                    AttemptOutcome::TaskFailed
                        if review_substitution(node, attempt)
                            && route_index + 1 < node.routes.len()
                            && progress
                                .attempts
                                .iter()
                                .filter(|attempt| attempt.review_substitution)
                                .count()
                                < limits.review_substitutions =>
                    {
                        progress.outputs = None;
                        progress.advance_error = None;
                        progress.advance_error_code = None;
                        progress.advance_owner_code = None;
                        progress.state = NodeState::Running;
                        true
                    }
                    AttemptOutcome::TaskFailed
                    | AttemptOutcome::OtherFailure
                    | AttemptOutcome::ReviewRequired
                    | AttemptOutcome::DeclaredStop
                    | AttemptOutcome::StoppedNoWork => {
                        progress.state = NodeState::Failed;
                        false
                    }
                }
            }
        }
    };
    Ok(AppliedObservation {
        settled_result,
        retry_was_pending,
        should_continue,
    })
}

/// Initialise durable progress solely from the admitted plan and host identity.
pub(super) fn initial_checkpoint(
    document: &GraphDocument,
    plan: &PlannedGraph,
    plan_digest: String,
    bound_review_base: Option<String>,
    bound_integration_base: Option<String>,
    instance_id: Option<String>,
    ask_actor: Option<String>,
) -> GraphCheckpoint {
    GraphCheckpoint {
        bound_review_base: bound_review_base.map(|base| base.to_string()),
        bound_integration_base: bound_integration_base.map(|base| base.to_string()),
        extra: Default::default(),
        expansions: Vec::new(),
        remaining: None,
        instance_id,
        asks: Vec::new(),
        ask_actor,
        wake_at: None,
        pause_asks: 0,
        delivery_required: document.delivery == Some(provenant_records::run::DeliveryMode::Pr),
        delivered: false,
        pull_request: None,
        schema_version: 1,
        cancelled: false,
        cancel_reason: None,
        integration_pin: None,
        pause_reason: None,
        pause_code: None,
        unknown_usage: Vec::new(),
        unknown_usage_omitted: 0,
        usage_events: Vec::new(),
        usage_events_omitted: 0,
        settlements: 0,
        extensions: Vec::new(),
        limit_notes: Vec::new(),
        graph_id: plan.graph_id.clone(),
        plan_digest,
        nodes: plan
            .nodes
            .iter()
            .map(|node| NodeProgress {
                extra: Default::default(),
                children: node.composite_members.clone(),
                min_successful: node.composite.as_ref().and_then(|spec| spec.min_successful),
                source_binding: node.source.clone(),
                source_commit: match &node.source {
                    Some(SourceBinding::Commit { commit }) => Some(commit.clone()),
                    _ => None,
                },
                writer_binding: None,
                writer_binding_pending: plans::writer(node),
                review_base: None,
                review_base_pending: node.access == AccessMode::ReadOnly
                    && node.extra.contains_key("review_base"),
                fragment: None,
                handoff: None,
                expansion_pause: None,
                expansion_cause: None,
                loop_pause: None,
                loop_pause_code: None,
                loop_cause: None,
                truncation_cause: None,
                round: node.round.clone(),
                gate: None,
                separation: None,
                ci: None,
                land: None,
                id: node.id.clone(),
                title: Some(node.title.as_deref().unwrap_or(&node.id).to_owned()),
                worktree: node.worktree.clone(),
                state: NodeState::Pending,
                cancelled_from: None,
                skip_reason: None,
                advance_error: None,
                advance_error_code: None,
                advance_owner_code: None,
                launch_retry: None,
                last_launch_retry: None,
                outputs: None,
                previous_attempts: Vec::new(),
                attempts: Vec::new(),
            })
            .collect(),
    }
}

/// A failed admission or source check has one typed node disposition. Callers
/// save it before returning or attempting any other launch.
pub(super) fn fail_with(progress: &mut NodeProgress, code: &str, message: String) {
    fail_node(progress, Some(code.into()), Some(message));
}

pub(super) fn writer_binding_failed(progress: &mut NodeProgress, _id: &str, reason: &str) {
    fail_with(progress, "writer_binding_unavailable", reason.to_owned());
}

pub(super) fn retry_exhausted(progress: &mut NodeProgress, reason: &str) {
    progress.advance_error_code = Some("launch_retry_exhausted".into());
    progress.advance_error = Some(bounded(
        &format!(
            "node {}: the launch failed: {reason}",
            provenant_records::diagnostic::safe_text(&progress.id)
        ),
        MAX_ADVANCE_ERROR_BYTES,
    ));
    progress.state = NodeState::Blocked;
    progress.last_launch_retry = progress.launch_retry.take().map(|mut retry| {
        retry.wake_at = None;
        retry
    });
}

pub(super) fn apply_writer_binding(
    node: &PlannedNode,
    progress: &mut NodeProgress,
    binding: Option<WriterWorktreeBinding>,
    required: bool,
) -> Result<(), String> {
    let Some(binding) = binding else {
        if required {
            return Err(format!(
                "node {}: host cannot bind the writer source",
                node.id
            ));
        }
        progress.writer_binding_pending = false;
        return Ok(());
    };
    if !std::path::Path::new(&binding.worktree).is_absolute()
        || binding.worktree.len() > 4096
        || binding.head.parse::<provenant_core::GitOid>().is_err()
    {
        return Err(format!("node {}: writer binding is invalid", node.id));
    }
    if node.source.is_none() && node.continues.is_none() && progress.source_commit.is_none() {
        progress.source_commit = Some(binding.head.clone());
    }
    progress.writer_binding = Some(binding);
    progress.writer_binding_pending = false;
    Ok(())
}

pub(super) fn apply_review_base(progress: &mut NodeProgress, base: String) {
    progress.review_base = Some(base);
    progress.review_base_pending = false;
}

pub(super) fn apply_source_commit(
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    index: usize,
    commit: Option<String>,
) -> bool {
    if commit
        .as_ref()
        .is_none_or(|commit| commit.parse::<provenant_core::GitOid>().is_err())
    {
        fail_with(
            &mut checkpoint.nodes[index],
            "source_commit_unavailable",
            format!("node {}: source commit unavailable or invalid", node.id,),
        );
        return false;
    }
    if matches!(node.source, Some(SourceBinding::Integration))
        && checkpoint.integration_pin.is_none()
    {
        checkpoint.integration_pin = commit.clone();
    }
    checkpoint.nodes[index].source_commit = commit;
    true
}

/// Admission is decided from one checkpoint view, before any input or launch
/// effect. Waiting and refusal states can be saved by the same driver path.
pub(super) enum Admission {
    Ready,
    WriterBusy,
    SlotWait { changed: bool },
    SaveAndWait,
}

pub(super) fn admit_attempt(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
    limits: &crate::config::GraphLimits,
    effective_budget: &crate::config::GraphBudget,
) -> Admission {
    let node = &plan.nodes[node_index];
    let cap = limits.node_prompt_bytes;
    if node.access == AccessMode::WorktreeWrite
        && checkpoint.nodes.iter().enumerate().any(|(index, other)| {
            index != node_index
                && other.state == NodeState::Running
                && !(relay::yields_slot(other) && node.id.starts_with(&format!("{}--", other.id)))
                && plan
                    .nodes
                    .iter()
                    .any(|peer| peer.id == other.id && plans::writers_conflict(node, peer))
        })
    {
        return Admission::WriterBusy;
    }
    if limits.concurrency.is_some_and(|cap| {
        checkpoint
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                *index != node_index
                    && node.state == NodeState::Running
                    && node.ci.is_none()
                    && !relay::yields_slot(node)
                    && plan.nodes[*index].composite.is_none()
                    && plan.nodes[*index].decide.is_none()
                    && plan.nodes[*index].land.is_none()
            })
            .count()
            >= cap
    }) {
        let changed = checkpoint.nodes[node_index]
            .extra
            .insert("slot_wait".into(), serde_json::Value::Bool(true))
            .is_none();
        return Admission::SlotWait { changed };
    }
    checkpoint.nodes[node_index].extra.remove("slot_wait");
    update_usage_warnings(checkpoint);
    if let Some(reason) = checkpoint
        .pause_reason
        .clone()
        .or_else(|| budget_reason(checkpoint, effective_budget))
    {
        checkpoint.pause_reason = Some(bounded(&reason, PAUSE_REASON_BYTES));
        checkpoint
            .pause_code
            .get_or_insert_with(|| "budget_exceeded".into());
        return Admission::SaveAndWait;
    }
    if let Some(missing) = required_input_unavailable(node, plan, checkpoint, positions) {
        fail_with(
            &mut checkpoint.nodes[node_index],
            "required_input_unavailable",
            format!(
                "node {}: required input {missing} is unavailable or invalid; repair the producer and rerun the graph",
                node.id
            ),
        );
        return Admission::SaveAndWait;
    }
    if !node.inputs.is_empty() && render_input(node, plan, checkpoint, positions, cap).1 {
        let (code, reason) = if node
            .prompt
            .len()
            .saturating_add(INPUT_OPEN.len())
            .saturating_add(INPUT_CLOSE.len())
            > cap
        {
            (
                "input_prompt_too_large",
                "node prompt leaves no room for upstream data",
            )
        } else {
            (
                "upstream_outputs_too_large",
                "selected upstream outputs do not fit",
            )
        };
        fail_with(
            &mut checkpoint.nodes[node_index],
            code,
            format!(
                "node {}: {reason}; increase node_prompt_bytes and restart the graph",
                node.id,
            ),
        );
        return Admission::SaveAndWait;
    }
    Admission::Ready
}

pub(super) struct LaunchFacts {
    pub input_cap: usize,
    pub placement_host_id: Option<String>,
    pub launch_actor: String,
    pub input_cap_source: InputCapSource,
    pub timing: Option<LaunchTiming>,
}

/// Record the idempotency identity, fixed inputs and placement before launch.
/// A saved Starting row is the fence that makes an uncertain handoff replayable.
pub(super) fn begin_attempt(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
    key: &AttemptKey,
    route: &GraphRoute,
    facts: LaunchFacts,
) {
    let node = &plan.nodes[node_index];
    let LaunchFacts {
        input_cap: cap,
        placement_host_id,
        launch_actor,
        input_cap_source,
        timing,
    } = facts;
    let mut initial_manifest = attempt_input_manifest(node, plan, checkpoint, positions, &[], None);
    for input in &mut initial_manifest.inputs {
        if matches!(input.kind, OutputKind::Result | OutputKind::Findings) {
            input.file_delivery = InputDelivery::Pending;
        }
    }
    let review_substitution = checkpoint.nodes[node_index]
        .attempts
        .last()
        .is_some_and(|attempt| {
            review_substitution(node, attempt) || review_preparation_substitution(node, attempt)
        });
    checkpoint.nodes[node_index].attempts.push(GraphAttempt {
        // Persist placement before the launch can lose its reply.
        // Recovery must not treat an absent local operation as
        // proof that a configured peer accepted nothing.
        extra: node
            .attempt
            .as_ref()
            .and_then(|spec| spec.host.as_ref())
            .map(|host| {
                let mut extra: provenant_records::ExtraFields =
                    [("placement_host".into(), serde_json::json!(host))].into();
                if let Some(stable) = placement_host_id {
                    extra.insert("placement_host_id".into(), stable.into());
                }
                extra
            })
            .unwrap_or_default(),
        review_substitution,
        key: key.clone(),
        route: route.clone(),
        state: AttemptState::Starting,
        attempt_ref: None,
        outcome: None,
        launched: true,
        launch_actor,
        settled: 0,
        input_cap: Some(cap),
        input_cap_source: Some(input_cap_source),
        input_manifest: Some(initial_manifest),
        usage: None,
    });
    checkpoint.nodes[node_index].state = NodeState::Running;
    if let Some(timing) = timing
        && checkpoint.nodes[node_index].launch_retry.is_none()
    {
        let deadline = timing.clock.and_then(|now| {
            DurationMicros::from_seconds(timing.admission_seconds)
                .ok()
                .and_then(|duration| now.checked_add(duration).ok())
        });
        checkpoint.nodes[node_index].launch_retry = Some(LaunchRetry {
            extra: Default::default(),
            policy: bounded_launch_retry_policy(timing.policy),
            failures: 0,
            deadline,
            wake_at: None,
            reason: deadline
                .is_none()
                .then(|| "wall clock unavailable; launch retry timing is unavailable".into()),
        });
    }
    // A retry must not replay the preceding attempt's family snapshot.
    checkpoint.nodes[node_index].separation = None;
}

pub(super) fn apply_input_manifest(
    progress: &mut NodeProgress,
    manifest: AttemptInputManifest,
) -> Result<(), &'static str> {
    progress
        .attempts
        .last_mut()
        .ok_or("an observed attempt must have a checkpoint row")?
        .input_manifest = Some(manifest);
    Ok(())
}

pub(super) struct ObservationRecord {
    pub node_index: usize,
    pub retry_was_pending: bool,
    pub recorded_separation: Option<FamilySeparation>,
    pub usage_error: Option<String>,
    pub cost_error: Option<String>,
}

pub(super) fn record_observation(
    checkpoint: &mut GraphCheckpoint,
    node: &PlannedNode,
    plan: &PlannedGraph,
    positions: &std::collections::BTreeMap<String, usize>,
    record: ObservationRecord,
) {
    let ObservationRecord {
        node_index,
        retry_was_pending,
        recorded_separation,
        usage_error,
        cost_error,
    } = record;
    if retry_was_pending {
        asks::refresh_wake(checkpoint);
    }
    if recorded_separation.as_ref().is_some_and(|record| {
        record.strength == FamilyStrength::Prefer
            && record.outcome == SeparationOutcome::Unavailable
    }) && node.access == AccessMode::WorktreeWrite
        && node.round.as_ref().is_some_and(|round| {
            round.node_id == "fix"
                && positions
                    .get(&round.loop_id)
                    .is_some_and(|&index| repair::is_repair(&plan.nodes[index]))
        })
        && let Some(outputs) = checkpoint.nodes[node_index].outputs.as_mut()
    {
        outputs.warnings.truncate(MAX_WARNINGS.saturating_sub(1));
        outputs.warnings.push("No separate model family was available for the repair writer; the repair continued on an available route.".into());
    }
    if recorded_separation.is_some() {
        checkpoint.nodes[node_index].separation = recorded_separation;
    }
    if checkpoint.nodes[node_index]
        .attempts
        .last()
        .is_some_and(|attempt| attempt.state == AttemptState::Terminal)
    {
        stamp_settlement(checkpoint, node_index);
        record_usage_events(checkpoint, node_index, usage_error, cost_error);
    }
    update_usage_warnings(checkpoint);
}

/// Redraw consumes the same-alias budget and archives the settled execution in
/// one checkpoint update; it never manufactures a new launch identity itself.
pub(super) fn redraw_quota_alias(
    document: &GraphDocument,
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    node_index: usize,
    limits: &crate::config::GraphLimits,
) -> Result<(), String> {
    // Same-alias redraw takes precedence over a different configured
    // route, and the retry patch requires the old execution settled.
    checkpoint.nodes[node_index].state = NodeState::Failed;
    let prior = checkpoint.nodes[node_index].clone();
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    patch::retry_node(
        &mut patch::LiveGraph {
            document,
            checkpoint,
        },
        &node.id,
        None,
        &patch::PatchContext {
            library: &library,
            resolvers: &resolvers,
            limits,
            scan: &patch::scan_for_secrets,
            journal: None,
        },
    )
    .map_err(|error| error.to_string())?;
    retain_automatic_retry_accounting(&prior, &mut checkpoint.nodes[node_index]);
    Ok(())
}

/// An automatic reset must retain the sealed allowance and all spent launches.
pub(super) fn retain_automatic_retry_accounting(prior: &NodeProgress, next: &mut NodeProgress) {
    let retry_policy = prior.extra.get("retry").cloned();
    let spent = prior
        .extra
        .get("quota_redraw_spent")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .saturating_add(
            prior
                .attempts
                .iter()
                .filter(|attempt| attempt.launched)
                .count() as u64,
        )
        .saturating_add(u64::from(
            prior
                .attempts
                .last()
                .is_some_and(|attempt| !attempt.launched),
        ));
    let archived = prior
        .extra
        .get("quota_redraw_archived")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .saturating_add(prior.attempts.len() as u64);
    if let Some(policy) = retry_policy {
        next.extra.insert("retry".into(), policy);
    }
    next.extra.insert("quota_redraw_spent".into(), spent.into());
    next.extra
        .insert("quota_redraw_archived".into(), archived.into());
}

fn attribute_blocker(
    node_index: usize,
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    // These nodes have their own settlement and checkpoint evidence rules.
    if node.condition.is_some()
        || node.composite.is_some()
        || node.land.is_some()
        || checkpoint.nodes[node_index].advance_error.is_some()
    {
        return false;
    }
    let blocker = node
        .after_expansion
        .iter()
        .chain(&node.depends_on)
        .map(|id| &checkpoint.nodes[positions[id]])
        .find(|parent| matches!(parent.state, NodeState::Failed | NodeState::Blocked));
    if let Some(parent) = blocker {
        let reason = format!("blocked by {} ({:?})", parent.id, parent.state);
        checkpoint.nodes[node_index].advance_error =
            Some(bounded(&reason, MAX_ADVANCE_ERROR_BYTES));
        checkpoint.nodes[node_index].advance_error_code = Some("upstream_blocked".into());
        true
    } else {
        false
    }
}

/// A failure target must declare its gate as a dependency. Other dependants
/// follow the success edge; inactive paths propagate a skipped state.
pub(super) fn dependency_state(
    node: &PlannedNode,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> Option<NodeState> {
    let mut waiting = false;
    let mut skipped = false;
    let mut blocked = false;
    if node
        .after_expansion
        .iter()
        .any(|parent| !checkpoint.nodes[positions[parent]].state.settled())
    {
        return Some(NodeState::Pending);
    }
    for parent in &node.after_expansion {
        let progress = &checkpoint.nodes[positions[parent]];
        if !checkpoint.node_successful(progress)
            && !(plan.nodes[positions[parent]].advisory
                && matches!(progress.state, NodeState::Failed | NodeState::Blocked))
        {
            return Some(NodeState::Blocked);
        }
    }
    for parent in &node.depends_on {
        let index = positions[parent];
        if loops::round_failure_edge(node, &plan.nodes[index])
            && checkpoint.nodes[index].state == NodeState::Failed
        {
            continue;
        }
        let recovery = plan.nodes[index].on_fail.as_ref().is_some_and(|target| {
            target == &node.id
                || node
                    .round
                    .as_ref()
                    .is_some_and(|round| &round.loop_id == target)
        });
        match checkpoint.nodes[index].state {
            NodeState::Failed | NodeState::Blocked if plan.nodes[index].advisory => {}
            // A failed CI gate that reported no failing check leaves a node
            // that consumes its findings nothing to act on, so it is skipped,
            // not launched. Other recovery nodes (notify, diagnose) still run.
            NodeState::Failed
                if recovery
                    && node.skips_without_findings(&plan.nodes[index].id)
                    && matches!(plan.nodes[index].condition, Some(GateCondition::Ci { .. }))
                    && checkpoint.nodes[index]
                        .outputs
                        .as_ref()
                        .is_some_and(|outputs| outputs.findings.is_empty()) =>
            {
                skipped = true
            }
            NodeState::Failed if recovery => {}
            NodeState::Failed if plan.nodes[index].on_fail.is_some() => skipped = true,
            NodeState::Failed | NodeState::Blocked => blocked = true,
            NodeState::Succeeded if recovery => skipped = true,
            NodeState::Skipped => skipped = true,
            NodeState::Pending | NodeState::Running => waiting = true,
            NodeState::Succeeded => {}
        }
    }
    if waiting {
        Some(NodeState::Pending)
    } else if blocked {
        Some(NodeState::Blocked)
    } else if skipped {
        Some(NodeState::Skipped)
    } else {
        None
    }
}

/// Node-kind dispatch is itself a pure transition. A blocked dependency or an
/// inactive round returns a save request; effect executors receive only runnable
/// nodes. Composite and loop owners precede dependency checks, as their joins
/// must observe partial children.
pub(super) enum NodeAction<'a> {
    Wait { changed: bool },
    Composite,
    Loop(&'a LoopSpec),
    Decide(&'a DecideSpec),
    Land(&'a LandSpec),
    Gate(&'a GateCondition),
    Attempt,
}

pub(super) fn node_action<'a>(
    index: usize,
    plan: &'a PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> NodeAction<'a> {
    let node = &plan.nodes[index];
    if checkpoint.nodes[index].state.settled() {
        let changed = checkpoint.nodes[index].state == NodeState::Blocked
            && dependency_state(node, plan, checkpoint, positions) == Some(NodeState::Blocked)
            && attribute_blocker(index, node, checkpoint, positions);
        return NodeAction::Wait { changed };
    }
    if checkpoint.nodes[index]
        .extra
        .get("retry_generation")
        .is_none_or(|_| {
            checkpoint.nodes[index].extra.get("container_retry")
                == Some(&serde_json::Value::Bool(true))
        })
        && (loops::inactive_round(node, plan, checkpoint, positions)
            || repair::inactive_lens(node, plan, checkpoint, positions))
    {
        checkpoint.nodes[index].state = NodeState::Skipped;
        return NodeAction::Wait { changed: true };
    }
    if node.composite.is_some() {
        return NodeAction::Composite;
    }
    if let Some(spec) = &node.loop_body {
        return NodeAction::Loop(spec);
    }
    if let Some(state) = dependency_state(node, plan, checkpoint, positions) {
        let mut changed = checkpoint.nodes[index].state != state;
        if state == NodeState::Blocked {
            changed |= attribute_blocker(index, node, checkpoint, positions);
        } else if changed
            && checkpoint.nodes[index].advance_error_code.as_deref() == Some("upstream_blocked")
        {
            checkpoint.nodes[index].advance_error = None;
            checkpoint.nodes[index].advance_error_code = None;
        }
        if changed {
            checkpoint.nodes[index].state = state;
        }
        return NodeAction::Wait { changed };
    }
    if let Some(spec) = &node.decide {
        return NodeAction::Decide(spec);
    }
    if let Some(spec) = &node.land {
        return NodeAction::Land(spec);
    }
    if let Some(condition) = &node.condition {
        return NodeAction::Gate(condition);
    }
    NodeAction::Attempt
}

/// Accepted extensions and reopened cancellation states share one save. Until
/// this pure transition succeeds, the stored cancellation fence remains intact.
pub(super) fn apply_resume(
    checkpoint: &mut GraphCheckpoint,
    extensions: &[String],
) -> Result<(), (String, &'static str)> {
    if checkpoint.cancelled {
        restore_cancelled_nodes(checkpoint)?;
        checkpoint.cancelled = false;
    }
    for extension in extensions {
        if !checkpoint.extensions.contains(extension) {
            checkpoint.extensions.push(extension.clone());
        }
    }
    checkpoint.deactivate_expansion_pauses();
    checkpoint.pause_reason = None;
    checkpoint.pause_code = None;
    Ok(())
}

pub(super) fn apply_loop_extensions(
    checkpoint: &mut GraphCheckpoint,
    extensions: &[String],
    extended: &PlannedGraph,
) -> Result<(), &'static str> {
    for extension in extensions.iter().filter(|e| e.starts_with("loop.")) {
        let name = extension
            .strip_prefix("loop.")
            .and_then(|e| e.split_once('=').map(|(key, _)| key))
            .and_then(|key| key.rsplit_once('.').map(|(name, _)| name))
            .ok_or("loop extension")?;
        if let Some(node) = checkpoint.nodes.iter_mut().find(|n| n.id == name) {
            node.loop_pause = None;
            node.loop_pause_code = None;
            node.loop_cause = None;
        }
    }
    loops::reconcile(checkpoint, extended);
    checkpoint.cancelled = false;
    Ok(())
}

pub(super) fn refresh_structure_pause(
    checkpoint: &mut GraphCheckpoint,
    expanded: &GraphDocument,
    plan: &PlannedGraph,
    limits: &crate::config::GraphLimits,
) -> Option<String> {
    if let Some((index, reason)) = checkpoint
        .nodes
        .iter()
        .enumerate()
        .find_map(|(index, node)| {
            node.expansion_pause
                .as_ref()
                .map(|reason| (index, reason.clone()))
        })
    {
        checkpoint.activate_expansion_pause(index);
        checkpoint.pause_reason = Some(reason);
        checkpoint.pause_code = Some("invalid_plan_expansion".into());
    }
    let structural_pause = if checkpoint.nodes.iter().all(|node| node.state.settled()) {
        None
    } else {
        structural_limit_reason(expanded, plan, limits)
    };
    if let Some(reason) = &structural_pause {
        checkpoint.deactivate_expansion_pauses();
        checkpoint.pause_reason = Some(bounded(reason, PAUSE_REASON_BYTES));
        checkpoint.pause_code = Some("structural_limit".into());
    }

    structural_pause
}

pub(super) fn reject_loop_decision(checkpoint: &mut GraphCheckpoint, node_id: &str, reason: &str) {
    for ask in &mut checkpoint.asks {
        if ask.node.as_deref() == Some(node_id) {
            ask.resolved = true;
            if let Some(answer) = ask.answer.as_mut() {
                answer
                    .rejection_code
                    .get_or_insert_with(|| "invalid_loop_decision".into());
                answer
                    .rejected
                    .get_or_insert_with(|| bounded(reason, MAX_ADVANCE_ERROR_BYTES));
            }
        }
    }
}

pub(super) fn refresh_limit_notes(
    checkpoint: &mut GraphCheckpoint,
    document: &GraphDocument,
) -> bool {
    let notes = document_limit_notes(document);
    if checkpoint.limit_notes == notes {
        return false;
    }
    checkpoint.limit_notes = notes;
    true
}

pub(super) fn clear_cancellation(checkpoint: &mut GraphCheckpoint) {
    checkpoint.cancelled = false;
}

pub(super) fn record_repair_growth(
    progress: &mut NodeProgress,
    before: &str,
    after: &str,
    grew: bool,
) {
    progress.extra.insert(
        "repair_growth".into(),
        serde_json::json!({"before":before,"after":after,"grew":grew}),
    );
}

pub(super) fn record_retry_allowance(progress: &mut NodeProgress, retries: u8, origin: &str) {
    progress.extra.insert(
        "retry".into(),
        serde_json::json!({"retries":retries,"origin":origin}),
    );
}

pub(super) fn record_provisioning(
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    node_index: usize,
    path: &str,
    source: &str,
) {
    checkpoint.nodes[node_index]
        .extra
        .insert("provisioned_source".into(), source.to_owned().into());
    if node.source.is_some() {
        checkpoint.nodes[node_index].source_commit = Some(source.to_owned());
    }
    if matches!(node.source, Some(SourceBinding::Integration))
        && checkpoint.integration_pin.is_none()
    {
        checkpoint.integration_pin = Some(source.to_owned());
    }
    relay::fence_provisioning(checkpoint, &node.id, path, source);
}

pub(super) fn record_creation_base(progress: &mut NodeProgress, base: &str) {
    progress
        .extra
        .insert("new_worktree_base".into(), base.into());
}

pub(super) fn record_separation(progress: &mut NodeProgress, record: FamilySeparation) {
    progress.separation = Some(record);
}

pub(super) fn clear_settled_pause(checkpoint: &mut GraphCheckpoint) -> bool {
    if checkpoint.nodes.iter().all(|node| node.state.settled())
        && checkpoint
            .nodes
            .iter()
            .all(|node| node.expansion_pause.is_none())
        && checkpoint.pause_reason.take().is_some()
    {
        checkpoint.deactivate_expansion_pauses();
        checkpoint.pause_code = None;
        return true;
    }
    false
}

pub(super) fn record_delivery(checkpoint: &mut GraphCheckpoint, url: String) -> Result<(), String> {
    if url.trim().is_empty()
        || url.chars().any(char::is_control)
        || encoded_len(&url) > OUTPUT_FIELD_BYTES
    {
        return Err("PR delivery identity is invalid or exceeds its bound; reconcile delivery before retrying".into());
    }
    checkpoint.pull_request = Some(url);
    checkpoint.delivered = true;
    Ok(())
}

pub(super) fn graph_run(checkpoint: GraphCheckpoint) -> GraphRun {
    let all_settled = checkpoint.nodes.iter().all(|node| node.state.settled());
    let status = if checkpoint.pause_reason.is_some() {
        GraphStatus::Paused
    } else if all_settled && checkpoint.cancelled {
        GraphStatus::Cancelled
    } else if all_settled {
        GraphStatus::Complete
    } else {
        GraphStatus::Pending
    };
    let advance_error = checkpoint
        .nodes
        .iter()
        .find_map(|node| node.advance_error.clone());
    GraphRun {
        status,
        outcome: (status == GraphStatus::Complete).then(|| checkpoint.outcome()),
        checkpoint,
        advance_error,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum IntegrationExpectation {
    Pinned(String),
    Delivered(String),
}

/// Request head observations only for source contracts that require them. The
/// order matters: a moved pinned subject refuses before any later loop check.
pub(super) fn integration_expectations(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &GraphCheckpoint,
) -> Vec<IntegrationExpectation> {
    let node = &plan.nodes[node_index];
    let mut expected = Vec::new();
    if (node.kind == Some(NodeKind::Synthesise) || plans::writer(node))
        && let Some(pin) = checkpoint.integration_pin.as_ref()
        && checkpoint.nodes[node_index].source_commit.as_ref() == Some(pin)
        && checkpoint.nodes[node_index].attempts.is_empty()
    {
        expected.push(IntegrationExpectation::Pinned(pin.clone()));
    }
    if node.round.is_some()
        && plans::writer(node)
        && matches!(
            node.delivery,
            Some(provenant_records::run::DeliveryMode::Integrate(_))
        )
        && checkpoint.nodes[node_index].attempts.is_empty()
        && let Some(delivered) = plan.nodes[..node_index]
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, earlier)| {
                plans::writer(earlier)
                    && earlier.worktree == node.worktree
                    && matches!(
                        earlier.delivery,
                        Some(provenant_records::run::DeliveryMode::Integrate(_))
                    )
            })
            .find_map(|(index, _)| {
                checkpoint.nodes[index]
                    .outputs
                    .as_ref()
                    .and_then(|outputs| outputs.commit.clone())
            })
    {
        expected.push(IntegrationExpectation::Delivered(delivered));
    }
    expected
}

pub(super) fn apply_integration_observation(
    node: &PlannedNode,
    progress: &mut NodeProgress,
    expected: &IntegrationExpectation,
    head: Option<&str>,
) -> bool {
    let Some(head) = head else {
        return true;
    };
    let message = match expected {
        IntegrationExpectation::Pinned(pin) if head != pin => format!(
            "node {}: the integration branch moved from the pinned {pin} to {head} after the review subject was pinned; rerun the graph to review the new head",
            node.id,
        ),
        IntegrationExpectation::Delivered(delivered) if head != delivered => format!(
            "node {}: the integration branch moved from the delivered {delivered} to {head}; a loop round only fast-forwards it, so rerun the graph",
            node.id,
        ),
        _ => return true,
    };
    fail_with(progress, "integration_head_moved", message);
    false
}

pub(super) fn continuation_ready(
    node: &PlannedNode,
    node_index: usize,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
) -> bool {
    if node.continues.is_none() || land::continued_branch(node, checkpoint, positions).is_some() {
        return true;
    }
    fail_with(
        &mut checkpoint.nodes[node_index],
        "continuation_delivery_missing",
        format!(
            "node {}: the writer it continues recorded no delivery branch",
            node.id,
        ),
    );
    false
}

pub(super) fn file_inputs_fit(
    node_index: usize,
    plan: &PlannedGraph,
    checkpoint: &mut GraphCheckpoint,
    positions: &std::collections::BTreeMap<String, usize>,
    cap: usize,
    file_notice_bytes: usize,
) -> bool {
    let node = &plan.nodes[node_index];
    let prompt_bytes = render_input(node, plan, checkpoint, positions, cap).0.len();
    if node.inputs.is_empty() || prompt_bytes.saturating_add(file_notice_bytes) <= cap {
        return true;
    }
    fail_with(
        &mut checkpoint.nodes[node_index],
        "upstream_outputs_too_large",
        format!(
            "node {}: selected upstream file notices do not fit; increase node_prompt_bytes and restart the graph",
            node.id,
        ),
    );
    false
}

pub(super) fn attempt_key(
    plan: &PlannedGraph,
    node_index: usize,
    checkpoint: &GraphCheckpoint,
    route_index: usize,
) -> Result<AttemptKey, &'static str> {
    let node = &plan.nodes[node_index];
    let mut attempt_extra = provenant_records::ExtraFields::default();
    if let Some(generation) = checkpoint.nodes[node_index].extra.get("retry_generation") {
        attempt_extra.insert("retry_generation".into(), generation.clone());
    }
    let predecessor = checkpoint.nodes[node_index]
        .recorded_attempts()
        .filter(|attempt| attempt.state == AttemptState::Terminal)
        .filter_map(|attempt| attempt.attempt_ref.as_ref())
        .next_back();
    if let Some(reference) = predecessor {
        attempt_extra.insert("previous_attempt_ref".into(), reference.clone().into());
    }
    let key = AttemptKey {
        extra: attempt_extra,
        graph_id: plan.graph_id.clone(),
        node_id: node.id.clone(),
        route_index: u16::try_from(route_index).map_err(|_| "route count")?,
    };
    let key = checkpoint.nodes[node_index]
        .attempts
        .last()
        .filter(|attempt| attempt.state != AttemptState::Terminal)
        .map_or(key, |attempt| attempt.key.clone());

    Ok(key)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
    use super::super::next_attempt::{
        NextAttemptDecision, NextAttemptFacts, PreparationObservation, next_attempt_transition,
    };
    use super::*;

    fn fixture() -> (
        PlannedGraph,
        std::collections::BTreeMap<String, usize>,
        GraphCheckpoint,
        crate::config::GraphLimits,
    ) {
        let document: GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "pure-transitions",
            "nodes": [{"id":"one", "access":"read_only", "prompt":"do the work",
                "routes":[{"adapter":"first", "model":"first/model"}, {"adapter":"second", "model":"second/model"}]}]
        })).unwrap();
        let limits = crate::config::GraphLimits::default();
        let (plan, positions) = validate_flat_plan(&document, &limits).unwrap();
        let checkpoint =
            initial_checkpoint(&document, &plan, "digest".into(), None, None, None, None);
        (plan, positions, checkpoint, limits)
    }

    fn start(
        plan: &PlannedGraph,
        positions: &std::collections::BTreeMap<String, usize>,
        checkpoint: &mut GraphCheckpoint,
        limits: &crate::config::GraphLimits,
    ) -> AttemptKey {
        let key = attempt_key(plan, 0, checkpoint, 0).unwrap();
        begin_attempt(
            0,
            plan,
            checkpoint,
            positions,
            &key,
            &plan.nodes[0].routes[0],
            LaunchFacts {
                input_cap: limits.node_prompt_bytes,
                placement_host_id: None,
                launch_actor: "original-actor".into(),
                input_cap_source: InputCapSource::Configured,
                timing: None,
            },
        );
        key
    }

    #[test]
    fn complete_attempt_transition_needs_no_owner_or_journal() {
        // Exercise the terminal contract and fallback decisions with no fake
        // process owner or storage implementation to accidentally depend on.
        for (outcome, contract, expected_state, expected_outcome, again) in [
            (
                AttemptOutcome::Succeeded,
                None,
                NodeState::Succeeded,
                AttemptOutcome::Succeeded,
                false,
            ),
            (
                AttemptOutcome::Succeeded,
                Some("VERDICT: PASS"),
                NodeState::Failed,
                AttemptOutcome::TaskFailed,
                false,
            ),
            (
                AttemptOutcome::AuthRequired,
                None,
                NodeState::Running,
                AttemptOutcome::AuthRequired,
                true,
            ),
            (
                AttemptOutcome::DeadlineExceeded,
                None,
                NodeState::Failed,
                AttemptOutcome::DeadlineExceeded,
                false,
            ),
        ] {
            let (mut plan, positions, mut checkpoint, limits) = fixture();
            plan.nodes[0].required_verdict = contract.map(str::to_owned);
            checkpoint.extra.insert("future_graph".into(), true.into());
            checkpoint.nodes[0]
                .extra
                .insert("future_node".into(), true.into());
            let key = start(&plan, &positions, &mut checkpoint, &limits);
            let recorded = checkpoint.nodes[0].attempts[0].clone();
            assert_eq!(recorded.launch_actor, "original-actor");
            assert!(recorded.input_manifest.is_some());
            let applied = apply_attempt_observation(
                &plan,
                &mut checkpoint,
                &positions,
                0,
                &limits,
                AttemptObservation::Terminal {
                    attempt_ref: Some("host/attempt".into()),
                    outcome,
                    launched: true,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                },
                ObservationFacts {
                    route_index: 0,
                    usage: None,
                    fallback_evidence: None,
                    evidence_unreadable: false,
                    fallback_cause: None,
                    review_range: ReviewRange::Unknown,
                },
            )
            .unwrap();
            assert_eq!(applied.should_continue, again);
            let progress = &checkpoint.nodes[0];
            assert_eq!(progress.state, expected_state);
            assert_eq!(progress.attempts[0].state, AttemptState::Terminal);
            assert_eq!(progress.attempts[0].outcome, Some(expected_outcome));
            assert_eq!(progress.attempts[0].key, key);
            assert_eq!(progress.attempts[0].launch_actor, recorded.launch_actor);
            assert_eq!(progress.attempts[0].input_manifest, recorded.input_manifest);
            assert_eq!(checkpoint.extra["future_graph"], true);
            assert_eq!(progress.extra["future_node"], true);
            if expected_outcome == AttemptOutcome::DeadlineExceeded {
                assert_eq!(
                    progress.advance_error_code.as_deref(),
                    Some("deadline_exceeded")
                );
            }
        }
    }

    #[test]
    fn next_attempt_transition_requests_only_the_facts_its_disposition_needs() {
        let (plan, positions, mut checkpoint, limits) = fixture();
        start(&plan, &positions, &mut checkpoint, &limits);
        let node = &plan.nodes[0];
        let progress = &mut checkpoint.nodes[0];
        let at = |seconds| provenant_core::WallTime::from_unix_seconds(seconds).unwrap();
        progress.launch_retry = Some(LaunchRetry {
            extra: Default::default(),
            policy: Default::default(),
            failures: 1,
            deadline: Some(at(10)),
            wake_at: Some(at(5)),
            reason: Some("busy".into()),
        });
        let mut observed = NextAttemptFacts::default();
        assert_eq!(
            next_attempt_transition(node, progress, &limits, Some(at(1)), &observed),
            NextAttemptDecision::Ready(NextAttempt::WaitRetryWake)
        );
        assert_eq!(
            next_attempt_transition(node, progress, &limits, Some(at(10)), &observed),
            NextAttemptDecision::ObserveLaunchAcceptance
        );
        observed.launch_accepted = Some(false);
        assert_eq!(
            next_attempt_transition(node, progress, &limits, Some(at(10)), &observed),
            NextAttemptDecision::Ready(NextAttempt::LaunchRetryExhausted {
                reason: "busy".into()
            })
        );
        observed.launch_accepted = Some(true);
        assert_eq!(
            next_attempt_transition(node, progress, &limits, Some(at(10)), &observed),
            NextAttemptDecision::Ready(NextAttempt::Candidate {
                route_index: 0,
                settlement_retries: None
            })
        );
        progress.launch_retry = None;
        let attempt = &mut progress.attempts[0];
        attempt.state = AttemptState::Terminal;
        attempt.launched = false;
        attempt.outcome = Some(AttemptOutcome::AuthRequired);
        attempt.attempt_ref = Some("host/attempt".into());
        assert_eq!(
            next_attempt_transition(node, progress, &limits, None, &observed),
            NextAttemptDecision::ObservePreparationFallback
        );
        observed.fallback = Some(PreparationObservation {
            evidence: None,
            unreadable: true,
            cause: None,
        });
        assert!(matches!(
            next_attempt_transition(node, progress, &limits, None, &observed),
            NextAttemptDecision::Ready(NextAttempt::Fail {
                code: Some("fallback_evidence_unreadable"),
                ..
            })
        ));
    }

    #[test]
    fn preparation_transitions_enforce_pinned_sources_and_input_capacity_without_io() {
        let (mut plan, positions, mut checkpoint, limits) = fixture();
        plan.nodes[0].kind = Some(NodeKind::Synthesise);
        checkpoint.integration_pin = Some("pinned".into());
        checkpoint.nodes[0].source_commit = Some("pinned".into());
        let expected = integration_expectations(0, &plan, &checkpoint);
        assert_eq!(
            expected,
            vec![IntegrationExpectation::Pinned("pinned".into())]
        );
        assert!(!apply_integration_observation(
            &plan.nodes[0],
            &mut checkpoint.nodes[0],
            &expected[0],
            Some("moved")
        ));
        assert_eq!(checkpoint.nodes[0].state, NodeState::Failed);
        assert_eq!(
            checkpoint.nodes[0].advance_error_code.as_deref(),
            Some("integration_head_moved")
        );
        plan.nodes[0].inputs.push(GraphInput {
            extra: Default::default(),
            node: "producer".into(),
            outputs: vec![OutputKind::Result],
            required_outputs: vec![],
        });
        assert!(!file_inputs_fit(
            0,
            &plan,
            &mut checkpoint,
            &positions,
            limits.node_prompt_bytes,
            limits.node_prompt_bytes
        ));
        assert_eq!(
            checkpoint.nodes[0].advance_error_code.as_deref(),
            Some("upstream_outputs_too_large")
        );
    }
}
