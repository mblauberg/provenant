//! Worst-case checkpoint size for a plan.
//!
//! A checkpoint holds the submitted document, its admitted expansions and
//! extensions, its asks and one progress row per expanded node. Every runtime string a row can gain is cut
//! to a fixed canonical-byte share when it is stored, and every list it can
//! grow is either bounded by the plan or capped with an omitted count. The sum
//! below is therefore an upper bound for every checkpoint the plan can
//! produce, and a plan whose bound exceeds the envelope is refused before its
//! first save rather than stopping later on an oversized checkpoint.
use super::*;

/// Envelope keys, digest and pinned integration base around the checkpoint.
const ENVELOPE_KEYS_BYTES: usize = 512;
/// Host working directory, escaped in the worst case.
const DIRECTORY_BYTES: usize = 6 * 4096;
/// Fixed text around a loop pause reason, which names its loop three times.
const LOOP_PAUSE_TEXT_BYTES: usize = 256;
/// Fixed text and route index in an unknown-usage line, beside the node ID.
const UNKNOWN_USAGE_TEXT_BYTES: usize = 64;
/// Space for each runtime-owned refusal code, including future causes.
pub(super) const REFUSAL_CODE_BYTES: usize = 64;
/// Space for a deferred review node's expanded progress and serialized record.
/// A prospective expansion is also checked against this reservation.
const REVIEW_NODE_RESERVE: usize = 64 * 1024;
const REVIEW_RECORD_RESERVE: usize = 256 * 1024;

fn deferred_review_bound(node: &PlannedNode, review: &ReviewSpec) -> usize {
    let templates = node.extra.get("lens_templates").map_or(0, canonical_len);
    let panel_templates = node.extra.get("panel_templates").map_or(0, canonical_len);
    let inherited = canonical_len(&(&node.depends_on, &node.inputs, &node.read_roots));
    // A custom lens can have a disposition projection or CI evidence larger
    // than an ordinary review leaf. Reserve those declared additions too.
    let template_extra = node
        .extra
        .get("lens_templates")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|templates| templates.values())
        .fold(0usize, |total, template| {
            let dispositions = template
                .get("dispositions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty());
            let composite = template
                .get("composite")
                .is_some_and(|value| !value.is_null());
            let ci = template.get("condition").filter(|condition| {
                condition.get("type").and_then(serde_json::Value::as_str) == Some("ci")
            });
            let ci_bytes = ci.map_or(0, |condition| {
                let bytes = condition
                    .get("failed_checks_max_bytes")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_else(ci::default_failed_checks_max_bytes);
                let count = condition
                    .get("max_failed_checks")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_else(ci::default_max_failed_checks)
                    .min(bytes / 32);
                bytes
                    .saturating_mul(2)
                    .saturating_add(count.saturating_mul(256))
            });
            total
                .saturating_add(if dispositions {
                    DISPOSITION_FINDINGS_BYTES
                } else {
                    0
                })
                .saturating_add(if composite {
                    COMPOSITE_EXCERPT_BYTES
                } else {
                    0
                })
                .saturating_add(ci_bytes)
        });
    // Explicit tiers may provide longer route lists than the two ordinary
    // read-only retries; each extra attempt has its own bounded progress row.
    let route_extra = review
        .lenses
        .iter()
        .filter_map(|lens| lens.tier.as_array())
        .fold(0usize, |total, routes| {
            total.saturating_add(routes.len().saturating_sub(2).saturating_mul(2_048))
        });
    review
        .max_nodes
        .saturating_sub(1)
        .saturating_mul(REVIEW_NODE_RESERVE)
        .saturating_add(REVIEW_RECORD_RESERVE)
        .saturating_add(templates.saturating_mul(2))
        // Verifier templates can be instantiated more than once.
        .saturating_add(
            panel_templates
                .saturating_mul(review.max_nodes)
                .saturating_mul(2),
        )
        .saturating_add(if panel_templates > 0 {
            DISPOSITION_FINDINGS_BYTES + COMPOSITE_EXCERPT_BYTES
        } else {
            0
        })
        .saturating_add(template_extra)
        .saturating_add(route_extra)
        // Explicit lens tiers can carry route lists copied into both the
        // expansion record and the eventual attempt rows.
        .saturating_add(canonical_len(review).saturating_mul(2))
        .saturating_add(inherited.saturating_mul(review.max_nodes).saturating_mul(2))
}

pub(super) fn reason(
    document: &GraphDocument,
    expansions: &[Expansion],
    extensions: &[String],
    plan: &PlannedGraph,
) -> Option<String> {
    let bound = checkpoint_bound(document, expansions, extensions, plan);
    (bound > MAX_CHECKPOINT_BYTES as usize).then(|| {
        format!(
            "graph checkpoint could reach {bound} bytes, over its {MAX_CHECKPOINT_BYTES}-byte limit; split the graph into smaller batches, or use fewer nodes, routes, loop rounds or extensions, or smaller prompts"
        )
    })
}

pub(super) fn latest() -> provenant_core::WallTime {
    provenant_core::WallTime::parse("9999-12-30T22:00:00.999999Z")
        .or_else(|_| provenant_core::WallTime::from_micros(0))
        .unwrap_or_else(|_| unreachable!("the epoch is a valid wall time"))
}

/// Worst-case ask: its rendered text, deadline, answer and inbox error.
fn ask_bound(mut ask: NodeAsk, variable: usize) -> usize {
    ask.wake_at = Some(latest());
    ask.answer = Some(AskAnswer {
        extra: Default::default(),
        source: AnswerSource::Answered,
        option: Some(String::new()),
        text: Some(String::new()),
        actor: Some(String::new()),
        at: Some(latest()),
        rejected: Some(String::new()),
        rejection_code: Some("c".repeat(REFUSAL_CODE_BYTES)),
    });
    ask.inbox_error = Some(String::new());
    canonical_len(&ask)
        + variable
        + asks::OPTION_BYTES
        + asks::ANSWER_BYTES
        + HOST_ID_BYTES
        + 2 * MAX_ADVANCE_ERROR_BYTES
        + 1
}

pub(super) fn checkpoint_bound(
    document: &GraphDocument,
    expansions: &[Expansion],
    extensions: &[String],
    plan: &PlannedGraph,
) -> usize {
    let skeleton = GraphCheckpoint {
        extra: Default::default(),
        expansions: expansions.to_vec(),
        remaining: Some(RemainingBudgets {
            extra: Default::default(),
            nodes: usize::MAX,
            edges: usize::MAX,
            continuations: usize::MAX,
        }),
        instance_id: Some(String::new()),
        asks: Vec::new(),
        ask_actor: Some(String::new()),
        wake_at: Some(latest()),
        pause_asks: u64::MAX,
        delivery_required: true,
        delivered: true,
        pull_request: Some(String::new()),
        integration_pin: Some(String::new()),
        cancelled: true,
        cancel_reason: Some(String::new()),
        pause_reason: Some(String::new()),
        pause_code: Some("c".repeat(REFUSAL_CODE_BYTES)),
        unknown_usage: Vec::new(),
        unknown_usage_omitted: usize::MAX,
        usage_events: Vec::new(),
        usage_events_omitted: u64::MAX,
        settlements: u64::MAX,
        extensions: extensions.to_vec(),
        limit_notes: document_limit_notes(document),
        schema_version: 1,
        graph_id: plan.graph_id.clone(),
        plan_digest: String::new(),
        nodes: Vec::new(),
    };
    let longest_id = plan
        .nodes
        .iter()
        .map(|node| node.id.len())
        .max()
        .unwrap_or(0);
    let event = UnknownUsageEvent {
        extra: Default::default(),
        node_id: "n".repeat(longest_id),
        route_index: u16::MAX,
        metric: "wall_time_seconds".into(),
        reason: Some(String::new()),
    };
    let fixed = [
        ENVELOPE_KEYS_BYTES,
        DIRECTORY_BYTES,
        canonical_len(document),
        canonical_len(&skeleton),
        // instance ID, ask actor, PR URL, integration pin, pause reason and plan digest
        64 + HOST_ID_BYTES
            + OUTPUT_FIELD_BYTES
            + 64
            + PAUSE_REASON_BYTES
            + 80
            + 6 * MAX_CANCEL_REASON_BYTES,
        MAX_UNKNOWN_USAGE * (UNKNOWN_USAGE_TEXT_BYTES + longest_id + 1),
        MAX_USAGE_EVENTS * (canonical_len(&event) + 64 + NOTE_BYTES + 1),
        // Pause asks: the kept history, one open and one awaiting its close.
        plan.decision_session.as_deref().map_or(0, |session| {
            (asks::MAX_PAUSE_ASKS + 2)
                * ask_bound(
                    asks::pause_ask(&plan.graph_id, u64::MAX, session, "", Some("")),
                    PAUSE_REASON_BYTES + MAX_ADVANCE_ERROR_BYTES,
                )
        }),
    ];
    let mut loop_rounds = std::collections::BTreeMap::<&str, usize>::new();
    for node in &plan.nodes {
        if let Some(round) = &node.round {
            let rounds = loop_rounds.entry(round.loop_id.as_str()).or_default();
            *rounds = (*rounds).max(round.round);
        }
    }
    plan.nodes
        .iter()
        .map(|node| {
            // An admitted planner never stores its fragment again.
            let admitted = expansions.iter().any(|e| e.source == node.id);
            node_bound(
                plan,
                node,
                loop_rounds.get(node.id.as_str()).copied().unwrap_or(0),
                admitted,
            )
            .saturating_add(if admitted {
                0
            } else {
                node.review
                    .as_ref()
                    .map_or(0, |review| deferred_review_bound(node, review))
            })
            .saturating_add(1)
        })
        .chain(fixed)
        .fold(0usize, usize::saturating_add)
}

fn land_skeleton(tier: provenant_records::workflow::RiskTier) -> LandRecord {
    LandRecord {
        extra: Default::default(),
        tier,
        policy: crate::config::LandPolicy::Auto,
        policy_source: crate::config::LandSource::Project,
        branch: String::new(),
        head: String::new(),
        authorised_by: Some(LandAuthority::Chair {
            ask_id: String::new(),
        }),
        pull_request: Some(u64::MAX),
        note: Some(String::new()),
        wake_at: Some(latest()),
        pending_observations: u64::MAX,
    }
}

fn node_bound(
    plan: &PlannedGraph,
    node: &PlannedNode,
    loop_rounds: usize,
    admitted: bool,
) -> usize {
    let expands = node.plan.is_some() || plans::writer(node);
    let gate = node.condition.is_some();
    let ci = matches!(node.condition, Some(GateCondition::Ci { .. }));
    let boundary = gate || node.loop_body.is_some();
    let skeleton = NodeProgress {
        extra: [(
            "retry".into(),
            serde_json::json!({"retries":u8::MAX,"origin":"shipped_default"}),
        )]
        .into_iter()
        .collect(),
        children: node.composite_members.clone(),
        source_commit: node.source.as_ref().map(|_| String::new()),
        fragment: (node.plan.is_some() && !admitted).then(String::new),
        handoff: plans::writer(node).then(String::new),
        expansion_pause: expands.then(String::new),
        loop_pause: node.loop_body.as_ref().map(|_| String::new()),
        loop_pause_code: node
            .loop_body
            .as_ref()
            .map(|_| "c".repeat(REFUSAL_CODE_BYTES)),
        round: node.round.clone(),
        gate: gate.then(|| GateResult {
            extra: Default::default(),
            passed: false,
            reason: Some(String::new()),
            on_fail: node.on_fail.clone(),
        }),
        separation: (node.distinct_from.is_some() || node.family_group.is_some()).then(|| {
            FamilySeparation {
                extra: Default::default(),
                strength: FamilyStrength::Require,
                excluded: Vec::new(),
                preferred: Vec::new(),
                applied: Some(String::new()),
                outcome: SeparationOutcome::Unconstrained,
            }
        }),
        ci: ci.then(|| CiEvidence {
            wake_at: Some(latest()),
            pending_observations: u64::MAX,
            extra: Default::default(),
            first_observed_stamp: Some(CiHostStamp {
                extra: Default::default(),
                boot_identity: String::new(),
                host_monotonic_micros: u64::MAX,
            }),
            clock_reanchors: u8::MAX,
            clock_reanchor_reason: Some(String::new()),
            state: CiState::Unavailable,
            summary: String::new(),
            head_sha: Some(String::new()),
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: true,
            elapsed_seconds: u64::MAX,
        }),
        land: node.land.as_ref().map(|spec| land_skeleton(spec.tier)),
        title: Some(node.title.clone().unwrap_or_else(|| node.id.clone())),
        worktree: node.worktree.clone(),
        id: node.id.clone(),
        state: NodeState::Succeeded,
        cancelled_from: Some(NodeState::Running),
        skip_reason: Some("graph cancelled".into()),
        advance_error: Some(String::new()),
        advance_error_code: Some("c".repeat(REFUSAL_CODE_BYTES)),
        outputs: Some(if boundary {
            gate_outputs(false)
        } else {
            NodeOutputs {
                extra: Default::default(),
                verdict_error: Some(String::new()),
                schema_version: 2,
                result: Some(ResultOutput {
                    extra: Default::default(),
                    digest: String::new(),
                    excerpt: String::new(),
                    truncated: true,
                    path: Some(String::new()),
                }),
                verdict: Some(String::new()),
                findings: Vec::new(),
                branch: Some(String::new()),
                commit: Some(String::new()),
                warnings: Vec::new(),
                plan: node.plan.as_ref().and_then(|plan| {
                    plan.fallback.as_ref().map(|_| PlanOutput {
                        extra: Default::default(),
                        fallback_reason: String::new(),
                    })
                }),
                remaining: Some(Vec::new()),
            }
        }),
        attempts: Vec::new(),
        launch_retry: None,
    };
    let mut bytes = canonical_len(&skeleton)
        + 64 // source commit
        + MAX_ADVANCE_ERROR_BYTES; // advance error
    if node
        .plan
        .as_ref()
        .is_some_and(|plan| plan.fallback.is_some())
    {
        bytes += NOTE_BYTES;
    }
    // The node's own bounded launch retry wait, kept while it is running.
    if !boundary && node.composite.is_none() && node.decide.is_none() && node.land.is_none() {
        bytes += MAX_ADVANCE_ERROR_BYTES + 128; // launch retry reason and deadline
    }
    if expands {
        bytes += MAX_ADVANCE_ERROR_BYTES; // expansion pause
    }
    if node.plan.is_some() && !admitted {
        bytes += plans::STORED_BLOCK_BYTES;
    }
    if plans::writer(node) {
        bytes += plans::STORED_HANDOFF_BYTES;
    }
    if let Some(spec) = &node.loop_body {
        bytes += MAX_ADVANCE_ERROR_BYTES + LOOP_PAUSE_TEXT_BYTES + 3 * node.id.len();
        if let Some(session) = &spec.decision_session {
            let ask = asks::loop_ask(&plan.graph_id, &node.id, usize::MAX, session);
            bytes += loop_rounds * ask_bound(ask, 0);
        }
    }
    if let Some(spec) = &node.decide {
        // One ask per decide node: a withdrawn ask is replaced, not kept.
        let session = asks::decide_session(spec, plan.decision_session.as_deref()).unwrap_or("");
        let ask = asks::decide_ask(
            &plan.graph_id,
            node,
            spec,
            session,
            u64::MAX,
            None,
            Some(""),
        );
        bytes += ask_bound(ask, MAX_ADVANCE_ERROR_BYTES);
    }
    if let Some(spec) = &node.land {
        // branch, head, ask ID, actor and note; one ask, whose body names the
        // branch and head twice.
        bytes +=
            2 * OUTPUT_FIELD_BYTES + node.id.len() + 32 + HOST_ID_BYTES + MAX_ADVANCE_ERROR_BYTES;
        let session = land::session(spec, plan).unwrap_or("");
        let ask = land::land_ask(&plan.graph_id, node, &land_skeleton(spec.tier), session);
        bytes += ask_bound(ask, 4 * OUTPUT_FIELD_BYTES + MAX_ADVANCE_ERROR_BYTES);
    }
    if gate {
        bytes += OUTPUT_FIELD_BYTES; // gate reason
    }
    if node.distinct_from.is_some() || node.family_group.is_some() {
        // Excluded and preferred families and the applied family
        bytes += (2 * SEPARATION_FAMILIES + 1) * (FAMILY_BYTES + 3) + 16;
    }
    if ci {
        bytes += HOST_ID_BYTES + 64 + OUTPUT_FIELD_BYTES;
        if let Some(GateCondition::Ci {
            failed_checks_max_bytes,
            max_failed_checks,
            ..
        }) = &node.condition
        {
            // Evidence array and its findings projection, including per-finding keys.
            // A failed-check object costs at least 32 bytes, even with no text.
            let max_bytes =
                failed_checks_max_bytes.unwrap_or_else(ci::default_failed_checks_max_bytes);
            let count = max_failed_checks
                .unwrap_or_else(ci::default_max_failed_checks)
                .min(max_bytes / 32);
            bytes = bytes
                .saturating_add(max_bytes.saturating_mul(2))
                .saturating_add(count.saturating_mul(256))
                .saturating_add(OUTPUT_FIELD_BYTES + 512);
        }
    }
    if !boundary {
        let excerpt = if node.composite.is_some() {
            COMPOSITE_EXCERPT_BYTES
        } else {
            RESULT_EXCERPT_BYTES
        };
        // digest, excerpt, path, verdict, verdict error, findings, branch,
        // commit, warnings, remaining
        bytes += 80
            + excerpt
            + REMAINING_BYTES
            + OUTPUT_FIELD_BYTES
            + OUTPUT_FIELD_BYTES
            + 64
            + if node.dispositions.is_empty() {
                FINDINGS_BYTES
            } else {
                DISPOSITION_FINDINGS_BYTES
            }
            + 2 * OUTPUT_FIELD_BYTES
            + MAX_WARNINGS * (WARNING_BYTES + 3);
    }
    for route in &node.routes {
        let attempt = GraphAttempt {
            extra: Default::default(),
            launched: false,
            key: AttemptKey {
                extra: Default::default(),
                graph_id: plan.graph_id.clone(),
                node_id: node.id.clone(),
                route_index: u16::MAX,
            },
            route: route.clone(),
            state: AttemptState::Unresolved,
            attempt_ref: Some(String::new()),
            outcome: Some(AttemptOutcome::AdapterUnavailable),
            launch_actor: String::new(),
            input_cap: Some(usize::MAX),
            settled: u64::MAX,
            usage: Some(AttemptUsage {
                extra: Default::default(),
                tokens: Some(u64::MAX),
                cost_micro_usd: Some(u64::MAX),
                wall_time_seconds: Some(u64::MAX),
                tokens_partial: true,
                cost_partial: true,
                cost_unknown_reason: Some(String::new()),
            }),
        };
        bytes += canonical_len(&attempt) + 2 * HOST_ID_BYTES + NOTE_BYTES + 1;
    }
    bytes
}
