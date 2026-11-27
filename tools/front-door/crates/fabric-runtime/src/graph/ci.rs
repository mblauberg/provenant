//! Head-bound, attempt-less CI evidence and repair inputs.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CiTarget {
    Ref(String),
    Node {
        node: String,
        #[serde(flatten)]
        extra: provenant_records::ExtraFields,
    },
}

impl From<&str> for CiTarget {
    fn from(value: &str) -> Self {
        Self::Ref(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiFailedCheck {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    /// GitHub check-run ID for the failed job, when the provider reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub name: String,
    pub url: Option<String>,
    pub excerpt: String,
}

/// Mergeability observed before the graph waits for CI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictState {
    Clear,
    Conflicting,
    Pending,
    Unavailable,
}

/// One mergeability observation tied to the PR head that was read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictObservation {
    pub state: ConflictState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub summary: String,
}

/// Persisted gate observation and bounded wake state for replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEvidence {
    pub state: ConflictState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<provenant_core::WallTime>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub pending_observations: u64,
    /// CI conflict events already followed by an exact-head mergeability recheck.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub ci_rechecks: u64,
}

pub(super) fn default_failed_checks_max_bytes() -> usize {
    16_384
}
pub(super) fn default_max_failed_checks() -> usize {
    20
}

impl CiObservation {
    /// Carry a forge quota reset independently of the human-readable summary.
    pub fn set_quota_reset(&mut self, reset: provenant_core::WallTime) {
        self.quota_reset_at = Some(reset);
    }

    fn quota_reset(&self) -> Option<provenant_core::WallTime> {
        self.quota_reset_at
    }

    /// Cap the complete canonical JSON array, including names, URLs and escapes.
    /// Both host probes and checkpoint admission use this same boundary.
    pub fn bound_failed_checks(&mut self, max_bytes: usize, max_checks: usize) {
        let mut selected = Vec::new();
        for mut check in std::mem::take(&mut self.failed_checks) {
            if selected.len() == max_checks {
                self.truncated = true;
                break;
            }
            check.extra = Default::default();
            if check
                .job_id
                .as_deref()
                .is_some_and(|job_id| !valid_job_id(job_id))
            {
                check.job_id = None;
                self.truncated = true;
            }
            let original = check.clone();
            let used = canonical_len(&selected);
            // Reserve the object keys, quotes, null URL and array comma first.
            check.name.clear();
            check.url = None;
            check.excerpt.clear();
            let overhead = canonical_len(&check).saturating_add(usize::from(!selected.is_empty()));
            let Some(mut remaining) = max_bytes.checked_sub(used.saturating_add(overhead)) else {
                self.truncated = true;
                break;
            };
            check.name = bounded(&original.name, remaining);
            remaining = remaining.saturating_sub(encoded_len(&check.name) - 2);
            if let Some(url) = &original.url {
                // Replacing null with a string releases two bytes.
                let url = bounded(url, remaining.saturating_add(2));
                remaining = remaining
                    .saturating_add(2)
                    .saturating_sub(encoded_len(&url) - 2);
                check.url = Some(url);
            }
            check.excerpt = bounded(&original.excerpt, remaining);
            self.truncated |= check != original;
            selected.push(check);
        }
        self.failed_checks = selected;
    }
}

fn valid_job_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn validate_target(
    target: &CiTarget,
    node: &GraphNode,
    document: &GraphDocument,
) -> Result<(), &'static str> {
    match target {
        CiTarget::Ref(target) => {
            if target.trim().is_empty()
                || target.trim() != target
                || target.starts_with('-')
                || target.contains(['\n', '\r'])
                || target.len() > 1024
            {
                return Err("invalid CI gate target or timeout");
            }
        }
        CiTarget::Node { node: target, .. } => {
            let mut seen = std::collections::BTreeSet::new();
            let mut pending: Vec<_> = node.dependencies().collect();
            while let Some(parent) = pending.pop() {
                if seen.insert(parent)
                    && let Some(parent) = document.nodes.iter().find(|n| &n.id == parent)
                {
                    pending.extend(parent.dependencies());
                }
            }
            if !valid_node_identifier(target)
                || target == &node.id
                || !seen.contains(target)
                || !document.nodes.iter().any(|node| &node.id == target)
            {
                return Err("CI target node must be an upstream dependency");
            }
        }
    }
    Ok(())
}

pub(super) enum Evaluation {
    Pending,
    Settled(Option<String>),
    ParkUnavailable,
}

pub(super) fn evaluate<O: AttemptOwner>(
    delivery_queue: bool,
    index: usize,
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut O,
    wall_now: Option<provenant_core::WallTime>,
    backoff: CiPollBackoff,
) -> Result<Evaluation, String> {
    let Some(GateCondition::Ci {
        target,
        timeout_seconds,
        checks,
        failed_checks_max_bytes,
        max_failed_checks,
        ..
    }) = &node.condition
    else {
        unreachable!()
    };
    let max_bytes = failed_checks_max_bytes.unwrap_or_else(default_failed_checks_max_bytes);
    let max_checks = max_failed_checks.unwrap_or_else(default_max_failed_checks);
    let binding = resolve_target(target, checkpoint, delivery_queue);
    let expected_published_head = binding
        .as_ref()
        .and_then(|binding| binding.published_head.as_deref());
    let queue_published_head_missing = delivery_queue && expected_published_head.is_none();
    let prior = checkpoint.nodes[index].ci.clone();
    let mut host = owner.ci_status();
    let sampled = host
        .as_ref()
        .and_then(|host| host.host_stamp())
        .map(|mut stamp| {
            stamp.boot_identity = bounded(&stamp.boot_identity, HOST_ID_BYTES);
            stamp
        });
    let mut observation = match (&binding, sampled.as_ref(), host.as_mut()) {
        (Some(binding), Some(_), Some(host)) => host.observe_ci(
            &binding.target,
            checks,
            delivery_queue,
            max_bytes,
            max_checks,
        ),
        _ => CiObservation {
            quota_reset_at: None,
            extra: Default::default(),
            state: CiState::Unavailable,
            summary: if binding.is_none() {
                "CI target node has no delivered branch"
            } else {
                "CI status or host clock unavailable"
            }
            .into(),
            head_sha: None,
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: false,
        },
    };
    // A node target may still be waiting for its writer to publish a branch.
    // It is not an unavailable CI provider and must not settle as a failure.
    if binding.is_none() && matches!(target, CiTarget::Node { .. }) {
        observation.state = CiState::Pending;
    }
    // An invalid SHA is an incomplete observation, like a missing one.
    observation.head_sha = observation
        .head_sha
        .filter(|sha| sha.parse::<provenant_core::GitOid>().is_ok());
    if delivery_queue && observation.head_sha.is_none() {
        observation.state = CiState::Pending;
        observation.summary =
            "CI target head is unavailable; waiting to resolve the published head".into();
        observation.failed_checks.clear();
    } else if observation.head_sha.is_none()
        && matches!(observation.state, CiState::Passed | CiState::Failed)
    {
        observation.state = CiState::Unavailable;
        observation.summary = "CI observation has no valid head SHA".into();
        observation.failed_checks.clear();
    }
    if delivery_queue
        && matches!(
            observation.state,
            CiState::Unavailable | CiState::NotScheduled
        )
    {
        observation.state = CiState::Pending;
        observation.failed_checks.clear();
    }
    if queue_published_head_missing {
        observation.state = CiState::Pending;
        observation.summary =
            "published commit is unavailable; waiting to resolve the exact head".into();
        observation.head_sha = None;
        observation.failed_checks.clear();
        observation.check_names.clear();
    } else if delivery_queue
        && let Some(expected_head) = expected_published_head
        && observation.head_sha.as_deref() != Some(expected_head)
    {
        let observed_head = observation.head_sha.as_deref().unwrap_or("unavailable");
        observation.state = CiState::Pending;
        observation.summary = format!(
            "published head {expected_head} is not the observed branch head {observed_head}; waiting for publication reconciliation"
        );
        observation.head_sha = Some(expected_head.to_owned());
        observation.failed_checks.clear();
        observation.check_names.clear();
    }
    // Inspect raw CI text before applying caps so warnings see the complete
    // source; the checkpoint and repair input retain the original bytes.
    observation = prompt_text::normalise(&observation, prompt_text::Context::CiObservation)?;
    observation.bound_failed_checks(max_bytes, max_checks);
    if delivery_queue && observation.state == CiState::Passed && observation.check_names.is_empty()
    {
        observation.state = CiState::Pending;
        observation.summary =
            "CI reported green without checks for this head; waiting for check registration".into();
    }
    if delivery_queue
        && observation.state == CiState::Failed
        && observation.failed_checks.is_empty()
    {
        observation.state = CiState::Pending;
        observation.summary =
            "CI reported failure without bounded job evidence; waiting for usable check details"
                .into();
    }
    let quota_reset = observation
        .quota_reset()
        .or_else(|| prior.as_ref().and_then(|evidence| evidence.quota_reset_at))
        .filter(|reset| {
            delivery_queue
                && observation.state == CiState::Pending
                && wall_now.is_none_or(|now| *reset > now)
        });
    // The gate stays bound to the last known head: an observation without a
    // SHA keeps it, and only a different known SHA supersedes it.
    let bound = prior.as_ref().and_then(|prior| prior.head_sha.clone());
    let superseded = bound
        .as_ref()
        .zip(observation.head_sha.as_ref())
        .is_some_and(|(bound, observed)| bound != observed);
    let head_sha = if queue_published_head_missing {
        None
    } else {
        observation.head_sha.clone().or(bound)
    };
    let mut anchor = prior.as_ref().and_then(|e| e.first_observed_stamp.clone());
    let mut reanchors = prior.as_ref().map_or(0, |e| e.clock_reanchors);
    let mut reanchor_reason = prior.as_ref().and_then(|e| e.clock_reanchor_reason.clone());
    // A newer head restarts both the timeout and the poll backoff.
    let mut pending_observations = prior.as_ref().map_or(0, |e| e.pending_observations);
    if superseded {
        anchor = sampled.clone();
        pending_observations = 0;
    }
    if let Some(now) = &sampled {
        let reason = match &anchor {
            Some(first) if first.boot_identity != now.boot_identity => Some("host boot changed"),
            Some(first) if now.host_monotonic_micros < first.host_monotonic_micros => {
                Some("host monotonic clock moved backwards")
            }
            _ => None,
        };
        if let Some(reason) = reason {
            anchor = Some(now.clone());
            reanchors = reanchors.saturating_add(1);
            reanchor_reason = Some(reason.into());
        } else if anchor.is_none() {
            anchor = Some(now.clone());
            if prior.is_some() {
                reanchor_reason = Some("previous checkpoint lacked a host stamp".into());
            }
        }
    }
    let elapsed_micros = sampled.as_ref().zip(anchor.as_ref()).map(|(now, first)| {
        now.host_monotonic_micros
            .saturating_sub(first.host_monotonic_micros)
    });
    let elapsed_seconds = elapsed_micros.map_or(0, |micros| micros / 1_000_000);
    let mut names = Vec::new();
    let mut remaining = OUTPUT_FIELD_BYTES;
    for name in observation.check_names {
        let selected = bounded(&name, remaining.saturating_sub(3));
        observation.truncated |= selected != name;
        let cost = encoded_len(&selected).saturating_add(1);
        if cost > remaining {
            observation.truncated = true;
            break;
        }
        remaining -= cost;
        names.push(selected);
    }
    let state = observation.state;
    // Pending with no registered check means CI never started for this head.
    let unscheduled = state == CiState::Pending && names.is_empty();
    checkpoint.nodes[index].ci = Some(CiEvidence {
        wake_at: None,
        pending_observations,
        extra: Default::default(),
        quota_reset_at: quota_reset,
        failed_checks_policy: Some(CiFailedChecksPolicy {
            max_bytes,
            max_bytes_source: if failed_checks_max_bytes.is_some() {
                Layer::Template
            } else {
                Layer::Default
            },
            max_checks,
            max_checks_source: if max_failed_checks.is_some() {
                Layer::Template
            } else {
                Layer::Default
            },
        }),
        first_observed_stamp: anchor,
        clock_reanchors: reanchors,
        clock_reanchor_reason: reanchor_reason,
        state,
        summary: bounded(&observation.summary, OUTPUT_FIELD_BYTES),
        head_sha,
        failed_checks: observation.failed_checks,
        check_names: names,
        truncated: observation.truncated,
        elapsed_seconds,
    });
    let timed_out = prior.is_some() && !superseded && elapsed_seconds >= *timeout_seconds;
    if !delivery_queue && timed_out {
        return Ok(Evaluation::Settled(Some(if unscheduled {
            format!("timeout: {NOT_SCHEDULED}")
        } else {
            "timeout".into()
        })));
    }
    if delivery_queue && timed_out && state == CiState::Pending && quota_reset.is_none() {
        let Some(evidence) = checkpoint.nodes[index].ci.as_mut() else {
            return Err(format!(
                "node {}: CI timeout lost its persisted evidence",
                node.id
            ));
        };
        evidence.summary = bounded(
            &format!(
                "CI remained pending through the {} second gate timeout: {}",
                timeout_seconds, evidence.summary
            ),
            OUTPUT_FIELD_BYTES,
        );
        evidence.wake_at = None;
        return Ok(Evaluation::ParkUnavailable);
    }
    Ok(match state {
        CiState::Pending => {
            let id = &node.id;
            let now = wall_now.ok_or_else(|| {
                format!("node {id}: no wall clock to set its CI wake deadline; retry the advance")
            })?;
            let delay_micros = backoff
                .delay_seconds(pending_observations)
                .saturating_mul(1_000_000);
            let timeout_micros = timeout_seconds.saturating_mul(1_000_000);
            let remaining = elapsed_micros.map(|elapsed| timeout_micros.saturating_sub(elapsed));
            let duration = provenant_core::DurationMicros::from_micros(
                remaining.map_or(delay_micros, |remaining| delay_micros.min(remaining)),
            );
            let wake_at = now.checked_add(duration).map_err(|_| {
                format!(
                    "node {id}: CI wake deadline passes the last representable wall time; lower ci_poll_backoff"
                )
            })?;
            let wake_at = quota_reset.map_or(wake_at, |reset| wake_at.max(reset));
            let progress = &mut checkpoint.nodes[index];
            progress.state = NodeState::Running;
            if let Some(evidence) = progress.ci.as_mut() {
                evidence.wake_at = Some(wake_at);
                evidence.pending_observations = pending_observations.saturating_add(1);
            }
            Evaluation::Pending
        }
        CiState::Passed => Evaluation::Settled(None),
        CiState::Failed => Evaluation::Settled(Some("failed".into())),
        CiState::Unavailable => Evaluation::Settled(Some("unavailable".into())),
        CiState::Conflict => Evaluation::Settled(Some(format!(
            "conflict; next: {}",
            super::CONFLICT_GUIDANCE
        ))),
        CiState::NotScheduled => Evaluation::Settled(Some(NOT_SCHEDULED.into())),
    })
}

const NOT_SCHEDULED: &str =
    "not_scheduled; next: check the workflow triggers or push an empty commit to start CI";

pub(super) fn evaluate_conflict<O: AttemptOwner>(
    delivery_queue: bool,
    index: usize,
    node: &PlannedNode,
    checkpoint: &mut GraphCheckpoint,
    owner: &mut O,
    wall_now: Option<provenant_core::WallTime>,
    backoff: CiPollBackoff,
) -> Result<Evaluation, String> {
    let Some(GateCondition::Conflict { target, .. }) = &node.condition else {
        unreachable!()
    };
    let binding = resolve_target(target, checkpoint, delivery_queue);
    let mut host = owner.ci_status();
    let observation = match (&binding, host.as_mut()) {
        (Some(binding), Some(host)) => host.observe_conflict(&binding.target),
        _ => ConflictObservation {
            state: ConflictState::Unavailable,
            head_sha: None,
            summary: if binding.is_none() {
                "conflict target node has no delivered branch"
            } else {
                "mergeability status unavailable"
            }
            .into(),
        },
    };
    let mut observed_head = observation
        .head_sha
        .filter(|sha| sha.parse::<provenant_core::GitOid>().is_ok());
    let expected_head = binding
        .as_ref()
        .and_then(|binding| binding.published_head.as_deref());
    let queue_published_head_missing = delivery_queue && expected_head.is_none();
    let head_mismatch =
        expected_head.is_some_and(|expected| observed_head.as_deref() != Some(expected));
    let state = if queue_published_head_missing {
        observed_head = None;
        ConflictState::Pending
    } else {
        match (observation.state, observed_head.as_deref()) {
            (ConflictState::Clear, Some(head))
                if expected_head.is_none_or(|expected| expected == head) =>
            {
                ConflictState::Clear
            }
            (ConflictState::Conflicting, Some(head))
                if expected_head.is_none_or(|expected| expected == head) =>
            {
                ConflictState::Conflicting
            }
            _ => ConflictState::Pending,
        }
    };
    if let Some(expected_head) = expected_head
        && observed_head.as_deref() != Some(expected_head)
    {
        observed_head = Some(expected_head.to_owned());
    }
    let prior = checkpoint.nodes[index].conflict.as_ref();
    let ci_rechecks = prior
        .filter(|prior| prior.head_sha.as_ref() == observed_head.as_ref())
        .map_or(0, |prior| prior.ci_rechecks);
    let pending_observations =
        if prior.and_then(|prior| prior.head_sha.as_ref()) == observed_head.as_ref() {
            prior.map_or(0, |prior| prior.pending_observations)
        } else {
            0
        };
    let mut evidence = ConflictEvidence {
        state,
        head_sha: observed_head,
        summary: bounded(
            &if queue_published_head_missing {
                "published commit is unavailable; waiting for the exact head".to_owned()
            } else if head_mismatch {
                format!(
                    "published head {} is not the observed PR head; waiting for publication reconciliation",
                    expected_head.unwrap_or_default()
                )
            } else {
                observation.summary
            },
            OUTPUT_FIELD_BYTES,
        ),
        wake_at: None,
        pending_observations,
        ci_rechecks,
    };
    checkpoint.nodes[index].conflict = Some(evidence.clone());
    Ok(match state {
        ConflictState::Pending | ConflictState::Unavailable => {
            evidence.state = ConflictState::Pending;
            let id = &node.id;
            let now = wall_now.ok_or_else(|| {
                format!("node {id}: no wall clock to set its mergeability wake deadline")
            })?;
            let delay = backoff
                .delay_seconds(pending_observations)
                .saturating_mul(1_000_000);
            evidence.wake_at = Some(
                now.checked_add(provenant_core::DurationMicros::from_micros(delay))
                    .map_err(|_| {
                        format!("node {id}: mergeability wake deadline exceeds representable time")
                    })?,
            );
            evidence.pending_observations = pending_observations.saturating_add(1);
            checkpoint.nodes[index].state = NodeState::Running;
            checkpoint.nodes[index].conflict = Some(evidence);
            Evaluation::Pending
        }
        ConflictState::Clear => Evaluation::Settled(None),
        ConflictState::Conflicting => Evaluation::Settled(Some(format!(
            "conflicting; next: {}",
            super::CONFLICT_GUIDANCE
        ))),
    })
}

struct TargetBinding {
    target: String,
    published_head: Option<String>,
}

pub(super) fn published_head(
    target: &CiTarget,
    checkpoint: &GraphCheckpoint,
    delivery_queue: bool,
) -> Option<String> {
    resolve_target(target, checkpoint, delivery_queue).and_then(|binding| binding.published_head)
}

fn resolve_target(
    target: &CiTarget,
    checkpoint: &GraphCheckpoint,
    bind_published_head: bool,
) -> Option<TargetBinding> {
    match target {
        CiTarget::Ref(target) if !bind_published_head => Some(TargetBinding {
            target: target.clone(),
            published_head: None,
        }),
        CiTarget::Ref(_) => None,
        CiTarget::Node { node, .. } => checkpoint
            .nodes
            .iter()
            .find(|n| &n.id == node)
            .and_then(|n| n.outputs.as_ref())
            .and_then(|outputs| {
                let branch = outputs.branch.as_ref()?;
                let commit = outputs.commit.as_ref();
                if bind_published_head && commit.is_none() {
                    return None;
                }
                Some(TargetBinding {
                    target: format!(
                        "refs/heads/{}",
                        branch.strip_prefix("refs/heads/").unwrap_or(branch)
                    ),
                    published_head: commit.cloned(),
                })
            }),
    }
}

pub(super) fn conflict_details_requested(plan: &PlannedGraph, gate: &str) -> bool {
    plan.nodes.iter().any(|node| {
        node.inputs.iter().any(|input| {
            input.node == gate
                && input
                    .outputs
                    .iter()
                    .any(|kind| matches!(kind, OutputKind::Result | OutputKind::Commit))
        })
    })
}

/// Expose bounded mergeability evidence as an ordinary graph input so a
/// recovery writer can distinguish a conflict from CI or review failure.
pub(super) fn conflict_outputs(passed: bool, conflict: Option<&ConflictEvidence>) -> NodeOutputs {
    let mut outputs = gate_outputs(passed);
    if let Some(conflict) = conflict {
        let text = format!(
            "Untrusted mergeability observation: {:?}\nPublished head: {}\n{}",
            conflict.state,
            conflict.head_sha.as_deref().unwrap_or("unknown"),
            conflict.summary,
        );
        let excerpt = bounded(&text, OUTPUT_FIELD_BYTES);
        outputs.result = Some(ResultOutput {
            extra: Default::default(),
            digest: CanonicalV1::file_digest(text.as_bytes()).to_string(),
            truncated: excerpt.len() < text.len(),
            excerpt,
            path: None,
        });
        outputs.commit = conflict.head_sha.clone();
    }
    outputs
}

pub(super) fn outputs(passed: bool, ci: Option<&CiEvidence>, delivery_queue: bool) -> NodeOutputs {
    let mut outputs = gate_outputs(passed);
    outputs.findings_available = Some(ci.is_some_and(|evidence| !evidence.truncated));
    if delivery_queue {
        outputs.commit = ci.and_then(|evidence| evidence.head_sha.clone());
    }
    if let Some(ci) = ci.filter(|_| !passed) {
        outputs.findings = ci
            .failed_checks
            .iter()
            .enumerate()
            .map(|(index, check)| Finding {
                extra: Default::default(),
                url: check.url.clone(),
                trigger: None,
                triage: None,
                class: None,
                verification: None,
                evidence: None,
                proposed_fix: None,
                kind: None,
                lens: None,
                propagates: None,
                design_defect: None,
                risk_grew: None,
                scope_grew: None,
                id: format!("ci-{}", index + 1),
                severity: "error".into(),
                file: String::new(),
                line: None,
                summary: match &check.job_id {
                    Some(job_id) => format!("{} (job {job_id}): {}", check.name, check.excerpt),
                    None => format!("{}: {}", check.name, check.excerpt),
                },
                status: "open".into(),
                disposition: None,
                reason: None,
            })
            .collect();
        if ci.truncated {
            outputs
                .warnings
                .push("CI evidence truncated; follow check URLs for complete output".into());
        }
    }
    outputs
}

pub(super) fn valid_evidence(ci: &CiEvidence, condition: &GateCondition) -> bool {
    let GateCondition::Ci {
        failed_checks_max_bytes,
        max_failed_checks,
        ..
    } = condition
    else {
        return false;
    };
    ci.failed_checks.len() <= max_failed_checks.unwrap_or_else(default_max_failed_checks)
        && ci
            .failed_checks
            .iter()
            .all(|check| check.job_id.as_deref().is_none_or(valid_job_id))
        && (ci.failed_checks.is_empty()
            || canonical_len(&ci.failed_checks)
                <= failed_checks_max_bytes.unwrap_or_else(default_failed_checks_max_bytes))
        && encoded_len(&ci.summary) <= OUTPUT_FIELD_BYTES + 2
        && canonical_len(&ci.check_names) <= OUTPUT_FIELD_BYTES + 2
        && ci
            .head_sha
            .as_ref()
            .is_none_or(|sha| sha.parse::<provenant_core::GitOid>().is_ok())
        && (ci.state != CiState::Passed || ci.head_sha.is_some())
}
