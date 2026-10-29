//! Worst-case checkpoint size for a plan.
//!
//! A checkpoint holds the submitted document, its admitted expansions and one
//! progress row per expanded node. Every runtime string a row can gain is cut
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

pub(super) fn reason(
    document: &GraphDocument,
    expansions: &[Expansion],
    plan: &PlannedGraph,
) -> Option<String> {
    let bound = checkpoint_bound(document, expansions, plan);
    (bound > MAX_CHECKPOINT_BYTES as usize).then(|| {
        format!(
            "graph checkpoint could reach {bound} bytes, over its {MAX_CHECKPOINT_BYTES}-byte limit; use fewer nodes, routes or loop rounds, or smaller prompts"
        )
    })
}

pub(super) fn checkpoint_bound(
    document: &GraphDocument,
    expansions: &[Expansion],
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
        delivery_required: true,
        delivered: true,
        pull_request: Some(String::new()),
        integration_pin: Some(String::new()),
        cancelled: true,
        pause_reason: Some(String::new()),
        unknown_usage: Vec::new(),
        unknown_usage_omitted: usize::MAX,
        usage_events: Vec::new(),
        usage_events_omitted: u64::MAX,
        extensions: Vec::new(),
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
        // instance ID, PR URL, integration pin, pause reason and plan digest
        64 + OUTPUT_FIELD_BYTES + 64 + PAUSE_REASON_BYTES + 80,
        MAX_UNKNOWN_USAGE * (UNKNOWN_USAGE_TEXT_BYTES + longest_id + 1),
        MAX_USAGE_EVENTS * (canonical_len(&event) + 64 + NOTE_BYTES + 1),
        MAX_EXTENSIONS * (EXTENSION_BYTES + 3),
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
            .saturating_add(1)
        })
        .chain(fixed)
        .fold(0usize, usize::saturating_add)
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
        extra: Default::default(),
        children: node.composite_members.clone(),
        source_commit: node.source.as_ref().map(|_| String::new()),
        fragment: (node.plan.is_some() && !admitted).then(String::new),
        handoff: plans::writer(node).then(String::new),
        expansion_pause: expands.then(String::new),
        loop_asks: Vec::new(),
        loop_pause: node.loop_body.as_ref().map(|_| String::new()),
        loop_inbox_error: node.loop_body.as_ref().map(|_| String::new()),
        round: node.round.clone(),
        loop_decision_actor: node.loop_body.as_ref().map(|_| String::new()),
        gate: gate.then(|| GateResult {
            extra: Default::default(),
            passed: false,
            reason: Some(String::new()),
            on_fail: node.on_fail.clone(),
        }),
        ci: ci.then(|| CiEvidence {
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
        }),
        title: Some(node.title.clone().unwrap_or_else(|| node.id.clone())),
        worktree: node.worktree.clone(),
        id: node.id.clone(),
        state: NodeState::Succeeded,
        advance_error: Some(String::new()),
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
            }
        }),
        attempts: Vec::new(),
    };
    let mut bytes = canonical_len(&skeleton)
        + 64 // source commit
        + MAX_ADVANCE_ERROR_BYTES; // advance error
    if expands {
        bytes += MAX_ADVANCE_ERROR_BYTES; // expansion pause
    }
    if node.plan.is_some() && !admitted {
        bytes += plans::STORED_BLOCK_BYTES;
    }
    if plans::writer(node) {
        bytes += plans::STORED_NOTE_BYTES;
    }
    if let Some(spec) = &node.loop_body {
        let ask = LoopAsk {
            extra: Default::default(),
            rounds: usize::MAX,
            session: spec.decision_session.clone().unwrap_or_default(),
            resolved: true,
            closed: true,
        };
        bytes += MAX_ADVANCE_ERROR_BYTES
            + LOOP_PAUSE_TEXT_BYTES
            + 3 * node.id.len()
            + MAX_ADVANCE_ERROR_BYTES
            + HOST_ID_BYTES
            + loop_rounds * (canonical_len(&ask) + 1);
    }
    if gate {
        bytes += OUTPUT_FIELD_BYTES; // gate reason
    }
    if ci {
        bytes += HOST_ID_BYTES + 64 + OUTPUT_FIELD_BYTES;
    }
    if !boundary {
        let excerpt = if node.composite.is_some() {
            COMPOSITE_EXCERPT_BYTES
        } else {
            RESULT_EXCERPT_BYTES
        };
        // digest, excerpt, path, verdict, verdict error, findings, branch, commit, warnings
        bytes += 80
            + excerpt
            + OUTPUT_FIELD_BYTES
            + OUTPUT_FIELD_BYTES
            + 64
            + FINDINGS_BYTES
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
