//! Identifier-only observations written before the authoritative checkpoint.
use super::*;
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
}

/// Values that may contain free text are represented by their digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEvent {
    pub kind: GraphEventKind,
    pub subject: String,
    pub code: String,
    pub digest: String,
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
        kind,
        subject: digest(&subject)?,
        code: code.into(),
        digest: digest(value)?,
    })
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
) -> Result<(), JournalError> {
    let mut events = Vec::new();
    for node in &current.nodes {
        let old = previous.and_then(|previous| previous.nodes.iter().find(|old| old.id == node.id));
        if old.is_none_or(|old| {
            old.state != node.state || old.advance_error_code != node.advance_error_code
        }) {
            events.push(event(
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
            )?);
        }
        for attempt in &node.attempts {
            let old_attempt =
                old.and_then(|old| old.attempts.iter().find(|old| old.key == attempt.key));
            if old_attempt != Some(attempt) {
                events.push(event(
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
                )?);
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
            events.push(event(
                GraphEventKind::Join,
                &node.id,
                "changed",
                &(&node.children, node.state),
            )?);
        }
        if node.outputs.is_some() && old.is_none_or(|old| old.outputs != node.outputs) {
            if let Some(outputs) = &node.outputs {
                if outputs.branch.is_some() {
                    events.push(event(
                        GraphEventKind::Effect,
                        &node.id,
                        "branch",
                        &outputs.branch,
                    )?);
                }
                if outputs.commit.is_some() {
                    events.push(event(
                        GraphEventKind::Effect,
                        &node.id,
                        "commit",
                        &outputs.commit,
                    )?);
                }
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
        events.push(event(
            GraphEventKind::Budget,
            id,
            "changed",
            &current.remaining,
        )?);
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
    if previous.is_none_or(|old| old.asks != current.asks) && !current.asks.is_empty() {
        events.push(event(GraphEventKind::Wait, id, "ask", &current.asks)?);
    }
    if previous.is_none_or(|old| old.pull_request != current.pull_request)
        && current.pull_request.is_some()
    {
        events.push(event(
            GraphEventKind::Effect,
            id,
            "pull_request",
            &current.pull_request,
        )?);
    }
    events.push(event(GraphEventKind::Checkpoint, id, "save", current)?);
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
        append_changes(&directory, "g", None, &checkpoint).unwrap();
        let first = read(&directory, "g").unwrap();
        assert_eq!(first.len(), 2);
        let mut file = directory.open_append(&name("g")).unwrap();
        file.write_all(b"{\"kind\":").unwrap();
        file.sync().unwrap();
        assert_eq!(read(&directory, "g").unwrap(), first);
        let previous = checkpoint.clone();
        checkpoint.nodes[0].state = NodeState::Running;
        append_changes(&directory, "g", Some(&previous), &checkpoint).unwrap();
        let records = read(&directory, "g").unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0], first[0]);
        assert_eq!(records[2].code, "running");
        assert!(
            !String::from_utf8(bytes(&directory, "g").unwrap())
                .unwrap()
                .contains("private-node-id")
        );
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
}
