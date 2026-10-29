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
    pub name: String,
    pub url: Option<String>,
    pub excerpt: String,
}

pub(super) fn default_failed_checks_max_bytes() -> usize {
    16_384
}
pub(super) fn default_max_failed_checks() -> usize {
    20
}

impl CiObservation {
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
            if !valid_identifier(target)
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
}

pub(super) fn evaluate<O: AttemptOwner>(
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
    let resolved = match target {
        CiTarget::Ref(target) => Some(target.clone()),
        CiTarget::Node { node, .. } => checkpoint
            .nodes
            .iter()
            .find(|n| &n.id == node)
            .and_then(|n| n.outputs.as_ref())
            .and_then(|outputs| outputs.branch.as_ref())
            .map(|branch| {
                format!(
                    "refs/heads/{}",
                    branch.strip_prefix("refs/heads/").unwrap_or(branch)
                )
            }),
    };
    let prior = checkpoint.nodes[index].ci.clone();
    let mut host = owner.ci_status();
    let sampled = host
        .as_ref()
        .and_then(|host| host.host_stamp())
        .map(|mut stamp| {
            stamp.boot_identity = bounded(&stamp.boot_identity, HOST_ID_BYTES);
            stamp
        });
    let mut observation = match (&resolved, sampled.as_ref(), host.as_mut()) {
        (Some(target), Some(_), Some(host)) => {
            host.observe_ci(target, checks, max_bytes, max_checks)
        }
        _ => CiObservation {
            extra: Default::default(),
            state: CiState::Unavailable,
            summary: if resolved.is_none() {
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
    if observation
        .head_sha
        .as_ref()
        .is_none_or(|sha| sha.parse::<provenant_core::GitOid>().is_err())
        && matches!(observation.state, CiState::Passed | CiState::Failed)
    {
        observation.state = CiState::Unavailable;
        observation.summary = "CI observation has no valid head SHA".into();
        observation.failed_checks.clear();
    }
    observation.head_sha = observation.head_sha.map(|sha| bounded(&sha, 64));
    observation.bound_failed_checks(max_bytes, max_checks);
    let superseded = prior.as_ref().is_some_and(|prior| {
        prior.head_sha != observation.head_sha && observation.head_sha.is_some()
    });
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
    checkpoint.nodes[index].ci = Some(CiEvidence {
        wake_at: None,
        pending_observations,
        extra: [("failed_checks_policy".into(), serde_json::json!({
            "max_bytes": max_bytes,
            "max_bytes_source": if failed_checks_max_bytes.is_some() { "condition" } else { "default" },
            "max_checks": max_checks,
            "max_checks_source": if max_failed_checks.is_some() { "condition" } else { "default" },
        }))].into(),
        first_observed_stamp: anchor,
        clock_reanchors: reanchors,
        clock_reanchor_reason: reanchor_reason,
        state,
        summary: bounded(&observation.summary, OUTPUT_FIELD_BYTES),
        head_sha: observation.head_sha,
        failed_checks: observation.failed_checks,
        check_names: names,
        truncated: observation.truncated,
        elapsed_seconds,
    });
    if reanchors > 3 {
        return Ok(Evaluation::Settled(Some(
            "timeout: clock re-anchor limit (3) exceeded".into(),
        )));
    }
    if prior.is_some() && !superseded && elapsed_seconds >= *timeout_seconds {
        return Ok(Evaluation::Settled(Some("timeout".into())));
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
            let remaining = elapsed_micros.map(|elapsed| {
                timeout_seconds
                    .saturating_mul(1_000_000)
                    .saturating_sub(elapsed)
            });
            let duration = provenant_core::DurationMicros::from_micros(
                remaining.map_or(delay_micros, |remaining| delay_micros.min(remaining)),
            );
            let wake_at = now.checked_add(duration).map_err(|_| {
                format!(
                    "node {id}: CI wake deadline passes the last representable wall time; lower ci_poll_backoff"
                )
            })?;
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
    })
}

pub(super) fn outputs(passed: bool, ci: Option<&CiEvidence>) -> NodeOutputs {
    let mut outputs = gate_outputs(passed);
    if let Some(ci) = ci.filter(|_| !passed) {
        outputs.findings = ci
            .failed_checks
            .iter()
            .enumerate()
            .map(|(index, check)| Finding {
                extra: check
                    .url
                    .as_ref()
                    .map(|url| [("url".into(), serde_json::Value::String(url.clone()))].into())
                    .unwrap_or_default(),
                id: format!("ci-{}", index + 1),
                severity: "error".into(),
                file: String::new(),
                line: None,
                summary: format!("{}: {}", check.name, check.excerpt),
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
