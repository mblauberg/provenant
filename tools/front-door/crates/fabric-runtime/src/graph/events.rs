//! Identifier-only observations written before the authoritative checkpoint.
use super::*;
use serde_json::Value;
use std::io::{Seek, SeekFrom, Write};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEventKind {
    Checkpoint,
    NodeState,
    Attempt,
    Wait,
    Gate,
    Join,
    Cancellation,
    Budget,
    Effect,
    Expansion,
    /// An admitted graph patch: revision, digests and node identifiers only.
    Patch,
}

/// Readable identifiers and closed codes; free text remains a digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEvent {
    pub timestamp_micros: i64,
    pub kind: GraphEventKind,
    pub subject: String,
    pub code: String,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptEvent>,
}

/// Retained, non-prompt attempt measurements for reporting after collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptEvent {
    pub identity: String,
    pub tier: String,
    pub template: String,
    pub tokens: Option<u64>,
    pub cost_micro_usd: Option<u64>,
    pub wall_time_seconds: Option<u64>,
    pub has_usage: bool,
    pub cache_read_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub queue_time_ms: Option<u64>,
    pub outcome: Option<String>,
    pub route: Value,
}

fn digest(value: &impl Serialize) -> Result<String, JournalError> {
    Ok(CanonicalV1::record_digest("fabric-graph-event", 1, value)?.to_string())
}

fn event(
    kind: GraphEventKind,
    subject: &str,
    code: &str,
    value: &impl Serialize,
) -> Result<GraphEvent, JournalError> {
    Ok(GraphEvent {
        timestamp_micros: provenant_os::clock::SystemClock::new()
            .wall_now()
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .as_micros(),
        kind,
        subject: subject.into(),
        code: code.into(),
        digest: digest(value)?,
        attempt: None,
        details: None,
    })
}

fn effect_identifier(kind: &str, value: Option<&str>) -> Option<Value> {
    let value = value?;
    if value.len() > 512
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
        || super::patch::scan_for_secrets(value).is_err()
    {
        return None;
    }
    Some(serde_json::json!({"effect":kind,"identifier":value}))
}

fn name(id: &str) -> OsString {
    format!("{id}.events.jsonl").into()
}

fn bytes(directory: &Directory, id: &str) -> Result<Vec<u8>, JournalError> {
    let mut file = match directory.open_read(&name(id)) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn parse(bytes: &[u8]) -> Result<Vec<GraphEvent>, JournalError> {
    let mut result = Vec::new();
    // A record is committed by its newline. An unfinished tail is ignored.
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let record = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        result.push(record);
    }
    Ok(result)
}

pub(super) fn read(directory: &Directory, id: &str) -> Result<Vec<GraphEvent>, JournalError> {
    parse(&bytes(directory, id)?)
}

pub(super) fn append_changes(
    directory: &Directory,
    id: &str,
    previous: Option<&GraphCheckpoint>,
    current: &GraphCheckpoint,
    document: Option<&GraphDocument>,
) -> Result<(), JournalError> {
    let expanded = document.and_then(|document| {
        super::plans::expand(
            document,
            &current.extensions,
            &current.expansions,
            &super::plans::generous(),
        )
        .ok()
    });
    let mut events = Vec::new();
    for node in &current.nodes {
        let old = previous.and_then(|previous| previous.nodes.iter().find(|old| old.id == node.id));
        if old.is_none_or(|old| {
            old.state != node.state
                || old.advance_error_code != node.advance_error_code
                || old.loop_pause_code != node.loop_pause_code
        }) {
            let mut change = event(
                GraphEventKind::NodeState,
                &node.id,
                match node.state {
                    NodeState::Pending => "pending",
                    NodeState::Running => "running",
                    NodeState::Succeeded => "succeeded",
                    NodeState::Failed => "failed",
                    NodeState::Blocked => "blocked",
                    NodeState::Skipped => "skipped",
                },
                &node.advance_error_code,
            )?;
            change.details = Some(
                serde_json::json!({"cause": node.advance_error_code.as_deref().or(node.loop_pause_code.as_deref())}),
            );
            events.push(change);
        }
        for attempt in node.recorded_attempts() {
            let old_attempt = old.and_then(|old| {
                old.recorded_attempts().find(|old| {
                    old.attempt_ref == attempt.attempt_ref
                        && old.key == attempt.key
                        && old.settled == attempt.settled
                })
            });
            if old_attempt != Some(attempt) {
                let mut observation = event(
                    GraphEventKind::Attempt,
                    &node.id,
                    if attempt.outcome.is_some() {
                        "settled"
                    } else {
                        "started"
                    },
                    &(
                        attempt.key.route_index,
                        &attempt.route,
                        &attempt.attempt_ref,
                        &attempt.outcome,
                    ),
                )?;
                observation.details = Some(serde_json::json!({
                    "route_index": attempt.key.route_index,
                    "state": attempt.state,
                    "outcome": attempt.outcome,
                }));
                if attempt.state != AttemptState::Terminal && !attempt.launched {
                    events.push(event(GraphEventKind::Wait, &node.id, "slot", &attempt.key)?);
                }
                // A live reservation is not countable until its reference arrives.
                // A terminal refusal may never receive one: retain a stable identity
                // from the graph key and settlement order so its outcome is counted.
                let identity = match &attempt.attempt_ref {
                    Some(reference) => Some(reference.clone()),
                    None if attempt.state == AttemptState::Terminal => Some(format!(
                        "unreferenced:{}",
                        digest(&(&attempt.key, attempt.settled))?
                    )),
                    None => None,
                };
                let metrics = attempt.usage.as_ref().map(|usage| &usage.extra);
                let template = expanded
                    .as_ref()
                    .and_then(|graph| graph.nodes.iter().find(|candidate| candidate.id == node.id))
                    .and_then(|node| node.extra.get("template"))
                    .and_then(Value::as_str)
                    .unwrap_or("raw");
                observation.attempt = identity.map(|identity| AttemptEvent {
                    identity,
                    tier: metrics.and_then(|m| m.get("tier")).and_then(Value::as_str).map(str::to_owned).or_else(|| attempt.route.alias.clone()).unwrap_or_else(|| "unclassified".into()),
                    template: metrics.and_then(|m| m.get("template")).and_then(Value::as_str).unwrap_or(template).into(),
                    tokens: attempt.usage.as_ref().and_then(|usage| usage.tokens),
                    cost_micro_usd: attempt
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.cost_micro_usd),
                    wall_time_seconds: attempt
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.wall_time_seconds),
                    has_usage: attempt.usage.is_some(),
                    cache_read_tokens: metrics.and_then(|m| m.get("cache_read_tokens")).and_then(Value::as_u64),
                    input_tokens: metrics.and_then(|m| m.get("input_tokens")).and_then(Value::as_u64),
                    queue_time_ms: metrics.and_then(|m| m.get("queue_time_ms")).and_then(Value::as_u64),
                    outcome: metrics.and_then(|m| m.get("outcome")).and_then(Value::as_str).map(str::to_owned).or_else(|| serde_json::to_value(attempt.outcome).ok()?.as_str().map(str::to_owned)),
                    route: metrics.and_then(|m| m.get("resolved_route")).cloned().unwrap_or_else(|| serde_json::json!({"adapter":attempt.route.adapter,"model":attempt.route.model,"name":attempt.route.name,"alias":attempt.route.alias,"effort":attempt.route.effort})),
                });
                events.push(observation);
                if attempt.outcome == Some(AttemptOutcome::UsageLimited) {
                    events.push(event(
                        GraphEventKind::Wait,
                        &node.id,
                        "quota",
                        &attempt.key.route_index,
                    )?);
                }
            }
        }
        if old.and_then(|node| node.extra.get("slot_wait")) != node.extra.get("slot_wait") {
            events.push(event(
                GraphEventKind::Wait,
                &node.id,
                if node.extra.contains_key("slot_wait") {
                    "slot"
                } else {
                    "slot_acquired"
                },
                &node.state,
            )?);
        }
        if super::relay::yields_slot(node) != old.is_some_and(super::relay::yields_slot) {
            events.push(event(
                GraphEventKind::Wait,
                &node.id,
                if super::relay::yields_slot(node) {
                    "slot_yielded"
                } else {
                    "slot_reclaimed"
                },
                &node.state,
            )?);
        }
        if old.map_or(node.launch_retry.is_some() || node.ci.is_some(), |old| {
            old.launch_retry != node.launch_retry || old.ci != node.ci
        }) {
            events.push(event(
                GraphEventKind::Wait,
                &node.id,
                if node.ci.is_some() {
                    "ci"
                } else {
                    "launch_retry"
                },
                &(&node.launch_retry, &node.ci),
            )?);
        }
        if node.gate.is_some() && old.is_none_or(|old| old.gate != node.gate) {
            events.push(event(
                GraphEventKind::Gate,
                &node.id,
                if node.gate.as_ref().is_some_and(|gate| gate.passed) {
                    "passed"
                } else {
                    "failed"
                },
                &node.gate,
            )?);
        }
        if !node.children.is_empty()
            && old.is_none_or(|old| old.children != node.children || old.state != node.state)
        {
            let mut join = event(
                GraphEventKind::Join,
                &node.id,
                "changed",
                &(&node.children, node.state),
            )?;
            join.details = Some(serde_json::json!({"children":node.children,"state":node.state}));
            events.push(join);
        }
        if old.is_none_or(|old| old.outputs != node.outputs)
            && let Some(outputs) = &node.outputs
        {
            if outputs.branch.is_some() {
                let mut effect =
                    event(GraphEventKind::Effect, &node.id, "branch", &outputs.branch)?;
                effect.details = effect_identifier("branch", outputs.branch.as_deref());
                events.push(effect);
            }
            if outputs.commit.is_some() {
                let mut effect =
                    event(GraphEventKind::Effect, &node.id, "commit", &outputs.commit)?;
                effect.details = effect_identifier("commit", outputs.commit.as_deref());
                events.push(effect);
            }
        }
    }
    if previous.is_none_or(|old| old.cancelled != current.cancelled) && current.cancelled {
        events.push(event(
            GraphEventKind::Cancellation,
            id,
            "cancelled",
            &current.cancelled,
        )?);
    }
    if previous.is_none_or(|old| old.remaining != current.remaining) && current.remaining.is_some()
    {
        let mut budget = event(GraphEventKind::Budget, id, "changed", &current.remaining)?;
        budget.details = current.remaining.as_ref().map(|remaining| serde_json::json!({
            "nodes":remaining.nodes,"edges":remaining.edges,"continuations":remaining.continuations
        }));
        events.push(budget);
    }
    if previous.is_none_or(|old| old.expansions != current.expansions)
        && !current.expansions.is_empty()
    {
        events.push(event(
            GraphEventKind::Expansion,
            id,
            "admitted",
            &current.expansions,
        )?);
    }
    // Patches `admit_patch` journaled are already written; the rest are written here.
    let known = previous.map_or(0, |old| old.expansions.len());
    for record in current
        .expansions
        .iter()
        .skip(known)
        .filter_map(|expansion| expansion.patch.as_ref())
        .filter(|record| !record.journaled)
    {
        let mut observation = patch_event(record)?;
        if let Some(details) = observation.details.as_mut().and_then(Value::as_object_mut) {
            let author = record
                .parent
                .as_ref()
                .and_then(|id| current.nodes.iter().find(|node| &node.id == id))
                .and_then(|node| node.attempts.last())
                .and_then(|attempt| attempt.attempt_ref.as_ref());
            details.insert("author_attempt".into(), serde_json::json!(author));
        }
        events.push(observation);
    }
    if previous.is_none_or(|old| old.asks != current.asks) && !current.asks.is_empty() {
        events.push(event(GraphEventKind::Wait, id, "ask", &current.asks)?);
    }
    if previous.is_none_or(|old| old.pull_request != current.pull_request)
        && current.pull_request.is_some()
    {
        let mut effect = event(
            GraphEventKind::Effect,
            id,
            "pull_request",
            &current.pull_request,
        )?;
        effect.details = effect_identifier("pull_request", current.pull_request.as_deref());
        events.push(effect);
    }
    let mut saved = event(GraphEventKind::Checkpoint, id, "save", current)?;
    saved.details = Some(serde_json::json!({"pause_code": current.pause_code,
        "cancelled":current.cancelled,"outcome":current.outcome()}));
    events.push(saved);
    append(directory, id, events)
}

/// Journal one admitted patch. Called by `admit_patch` before its commit.
#[cfg(test)]
pub(super) fn append_patch(
    directory: &Directory,
    id: &str,
    record: &super::patch::PatchRecord,
) -> Result<(), JournalError> {
    append(directory, id, vec![patch_event(record)?])
}

/// The journal line for an admitted patch: digests and identifiers only.
fn patch_event(record: &super::patch::PatchRecord) -> Result<GraphEvent, JournalError> {
    let mut observation = event(
        GraphEventKind::Patch,
        record.parent.as_deref().unwrap_or("host"),
        "admitted",
        &record.digest,
    )?;
    observation.details = Some(serde_json::json!({
        "revision": record.revision, "patch_digest": record.digest, "author_node": record.parent,
        "cause": if record.parent.is_some() { "node_patch" } else { "host_patch" },
        "reason_digest": record.reason.as_ref().map(digest).transpose()?,
        "skipped": record.skipped,
        "replaced": record.replaced.iter().map(|node| &node.id).collect::<Vec<_>>(),
    }));
    Ok(observation)
}

/// Append committed lines, repairing an unfinished tail first.
fn append(directory: &Directory, id: &str, events: Vec<GraphEvent>) -> Result<(), JournalError> {
    let torn = match directory.open_read(&name(id)) {
        Ok(mut file) => {
            let len = file.seek(SeekFrom::End(0))?;
            if len == 0 {
                false
            } else {
                file.seek(SeekFrom::End(-1))?;
                let mut last = [0];
                file.read_exact(&mut last)?;
                last[0] != b'\n'
            }
        }
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.into()),
    };
    if torn {
        let old = bytes(directory, id)?;
        parse(&old)?;
        let complete = old
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |pos| pos + 1);
        directory
            .atomic_write(
                &name(id),
                ".event-repair.",
                &old[..complete],
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(JournalError::DurabilityUnconfirmed)?;
    }
    let mut file = directory.open_append(&name(id))?;
    for event in events {
        let line = CanonicalV1::bytes(&event)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
    }
    file.sync()?;
    directory.sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_survive_a_torn_tail_and_keep_order_on_next_save() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "g", "plan_digest": "digest",
            "nodes": [{"id":"private-node-id", "state":"pending", "worktree":null, "attempts":[]}]
        }))
        .unwrap();
        append_changes(&directory, "g", None, &checkpoint, None).unwrap();
        let first = read(&directory, "g").unwrap();
        assert_eq!(first.len(), 2);
        let mut file = directory.open_append(&name("g")).unwrap();
        file.write_all(b"{\"kind\":").unwrap();
        file.sync().unwrap();
        assert_eq!(read(&directory, "g").unwrap(), first);
        let previous = checkpoint.clone();
        checkpoint.nodes[0].state = NodeState::Running;
        append_changes(&directory, "g", Some(&previous), &checkpoint, None).unwrap();
        let records = read(&directory, "g").unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0], first[0]);
        assert_eq!(records[2].code, "running");
        assert_eq!(records[0].subject, "private-node-id");
        assert!(records[0].timestamp_micros > 0);
    }

    #[test]
    fn trace_explains_failure_and_slot_wait_without_brief_text() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let before: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"g","plan_digest":"digest",
            "nodes":[{"id":"build","state":"pending","worktree":null,"attempts":[],"slot_wait":true}]
        })).unwrap();
        append_changes(&directory, "g", None, &before, None).unwrap();
        let mut after = before.clone();
        after.nodes[0].extra.remove("slot_wait");
        after.nodes[0].state = NodeState::Failed;
        after.nodes[0].advance_error_code = Some("required_input_unavailable".into());
        after.nodes[0].advance_error = Some("private brief or secret text".into());
        append_changes(&directory, "g", Some(&before), &after, None).unwrap();
        let records = read(&directory, "g").unwrap();
        assert!(
            records
                .iter()
                .any(|event| event.subject == "build" && event.code == "slot")
        );
        assert!(
            records
                .iter()
                .any(|event| event.subject == "build" && event.code == "slot_acquired")
        );
        let failure = records.iter().find(|event| event.code == "failed").unwrap();
        assert_eq!(
            failure.details.as_ref().unwrap()["cause"],
            "required_input_unavailable"
        );
        assert!(
            !String::from_utf8(bytes(&directory, "g").unwrap())
                .unwrap()
                .contains("private brief")
        );
    }

    #[test]
    fn terminal_refusals_without_a_reference_are_counted_once() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let mut checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"g","plan_digest":"digest",
            "nodes":[{"id":"task","state":"running","worktree":null,"attempts":[{
                "key":{"graph_id":"g","node_id":"task","route_index":0},
                "route":{"alias":"senior"},"state":"starting","launched":false,
                "attempt_ref":null,"outcome":null,"launch_actor":"actor"
            }]}]
        })).unwrap();
        append_changes(&directory, "g", None, &checkpoint, None).unwrap();
        assert!(read(&directory, "g").unwrap().iter().all(|event| event.attempt.is_none()));
        let before = checkpoint.clone();
        checkpoint.nodes[0].state = NodeState::Failed;
        let attempt = &mut checkpoint.nodes[0].attempts[0];
        attempt.state = AttemptState::Terminal;
        attempt.outcome = Some(AttemptOutcome::AdapterUnavailable);
        attempt.settled = 1;
        append_changes(&directory, "g", Some(&before), &checkpoint, None).unwrap();
        append_changes(&directory, "g", Some(&checkpoint), &checkpoint, None).unwrap();
        let attempts = read(&directory, "g").unwrap().into_iter()
            .filter_map(|event| event.attempt).collect::<Vec<_>>();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].outcome.as_deref(), Some("adapter_unavailable"));
        assert_eq!(attempts[0].tier, "senior");
        assert!(!attempts[0].has_usage);
    }

    #[test]
    fn terminal_collection_keeps_events() {
        use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        let mut fixture: StoredCheckpoint =
            serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json")).unwrap();
        fixture.execution.directory = base;
        let mut journal =
            FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
        journal.bind(fixture.execution).unwrap();
        journal.save(&fixture.checkpoint).unwrap();
        drop(journal);
        let before = FileJournal::read_events(&root, project, "g-test").unwrap();
        assert!(!before.is_empty());
        assert_eq!(
            FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
            1
        );
        assert_eq!(
            FileJournal::read_events(&root, project, "g-test").unwrap(),
            before
        );
        assert!(
            FileJournal::read_checkpoint(&root, project, "g-test")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn admitted_patches_keep_identifiers_and_hash_free_text() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let before: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "g", "plan_digest": "digest", "nodes": []
        }))
        .unwrap();
        let mut after = before.clone();
        after.expansions.push(Expansion {
            extra: Default::default(),
            source: "planner".into(),
            continuation: false,
            nodes: Vec::new(),
            uses: Vec::new(),
            patch: Some(super::super::patch::PatchRecord {
                extra: Default::default(),
                revision: 2,
                digest: "sha256:abc".into(),
                key: Some("secret-key".into()),
                parent: Some("planner".into()),
                reason: Some("secret reason".into()),
                skipped: Vec::new(),
                replaced: Vec::new(),
                journaled: false,
            }),
        });
        append_changes(&directory, "g", Some(&before), &after, None).unwrap();
        let records = read(&directory, "g").unwrap();
        assert!(
            records.iter().any(|record| {
                record.kind == GraphEventKind::Patch && record.code == "admitted"
            })
        );
        let text = String::from_utf8(bytes(&directory, "g").unwrap()).unwrap();
        assert!(!text.contains("secret"));
        assert!(text.contains("planner"));
    }

    #[test]
    fn a_journaled_patch_is_written_once_and_not_again_at_save() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let record = super::super::patch::PatchRecord {
            extra: Default::default(),
            revision: 1,
            digest: "sha256:abc".into(),
            key: None,
            parent: None,
            reason: None,
            skipped: Vec::new(),
            replaced: Vec::new(),
            journaled: true,
        };
        append_patch(&directory, "g", &record).unwrap();
        let before: GraphCheckpoint = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "graph_id": "g", "plan_digest": "digest", "nodes": []
        }))
        .unwrap();
        let mut after = before.clone();
        after.expansions.push(Expansion {
            extra: Default::default(),
            source: "host".into(),
            continuation: false,
            nodes: Vec::new(),
            uses: Vec::new(),
            patch: Some(record),
        });
        append_changes(&directory, "g", Some(&before), &after, None).unwrap();
        let patches = read(&directory, "g")
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == GraphEventKind::Patch)
            .count();
        assert_eq!(patches, 1);
    }
}
