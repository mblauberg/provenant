//! Parity corpus for the lane-to-node cutover (ADR 0039): a plain lane is one
//! graph node, so each behaviour a lane had must hold for that node. Each case
//! drives the real kernel and `FileJournal` through a scripted fake provider
//! owner, then checks final state, cause, attempt count and that the event log
//! (`graphs/<id>.events.jsonl`) tells the same story as the checkpoint.
//! No native confinement is needed, so these run on Linux and macOS.
mod support;
use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, WaitBudget};
use provenant_fabric_runtime::graph::{
    self, AttemptKey, AttemptObservation, AttemptOutcome, AttemptOwner, AttemptState, FileJournal,
    GraphCheckpoint, GraphDocument, GraphEvent, GraphEventKind, GraphExecution, GraphJournal,
    GraphStatus, NodeState,
};
use provenant_os::clock::SystemClock;
use serde_json::json;
use std::collections::VecDeque;
use support::{Result, TestRig};

/// A supervisor dying mid-launch leaves the host unable to say whether the
/// launch was accepted, which is what `LaunchUnknown` reports.
type Crash = provenant_fabric_runtime::coordination::facade::FacadeError;

fn crash() -> Crash {
    provenant_core::OperationId::parse("op")
        .map(|operation_id| Crash::LaunchUnknown { operation_id })
        .unwrap_or(Crash::StateCorrupt)
}

/// Fake provider host. Script entries are consumed per observation of a route;
/// `Pending` is returned once the script is empty. `sessions` records the
/// provider session each key resumed, so replays are visible.
#[derive(Default)]
struct Provider {
    script: VecDeque<Step>,
    /// Every observation: key, route index and the checkpointed attempt ref.
    calls: Vec<(AttemptKey, Option<String>)>,
    /// Distinct launches (first observation of a key).
    launches: Vec<AttemptKey>,
    cancelled_settled: bool,
    stops: usize,
    /// What the host's durable acceptance lookup says about a failed launch.
    accepted: Option<bool>,
    /// The host's wall clock; retry waits are measured against it.
    now: Option<provenant_core::WallTime>,
    quota_redraw: bool,
    retry_limit: Option<u8>,
    prior_attempts: Vec<Vec<String>>,
    permissions: Vec<(graph::AccessMode, Option<String>)>,
    writer_head: Option<String>,
    writer_bindings: usize,
    terminal_commit: Option<String>,
}
enum Step {
    Pending(Option<&'static str>),
    Done(AttemptOutcome, bool, &'static str),
    Crash,
}
impl AttemptOwner for Provider {
    type Error = Crash;
    fn retry_allowance(
        &self,
        _: &provenant_records::ExtraFields,
        _: &provenant_records::ExtraFields,
    ) -> std::result::Result<Option<(u8, String)>, Crash> {
        Ok(self.retry_limit.map(|limit| (limit, "fixture".into())))
    }
    fn quota_alias_redraw(&mut self, _: &AttemptKey, _: &str) -> std::result::Result<bool, Crash> {
        Ok(self.quota_redraw)
    }
    fn attempt_models(&mut self, attempts: &[String]) -> std::result::Result<Vec<String>, Crash> {
        Ok(attempts
            .iter()
            .filter(|reference| reference.as_str() == "h/quota")
            .map(|_| "member0/model".into())
            .collect())
    }
    fn bind_writer_worktree(
        &mut self,
        worktree: &str,
    ) -> std::result::Result<Option<graph::WriterWorktreeBinding>, Crash> {
        self.writer_bindings += 1;
        Ok(self
            .writer_head
            .as_ref()
            .map(|head| graph::WriterWorktreeBinding {
                extra: Default::default(),
                worktree: worktree.into(),
                head: head.clone(),
            }))
    }
    fn actor(&self) -> String {
        "fixture-actor".into()
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.now
    }
    fn stop_cancelled_attempt(
        &mut self,
        _: &graph::GraphAttempt,
    ) -> std::result::Result<(), Crash> {
        self.stops += 1;
        Ok(())
    }
    fn launch_accepted(&mut self, _: &graph::GraphAttempt) -> std::result::Result<bool, Crash> {
        Ok(self.accepted == Some(true))
    }
    fn launch_refused_before_acceptance(
        &mut self,
        _: &graph::GraphAttempt,
    ) -> std::result::Result<bool, Crash> {
        Ok(self.accepted == Some(false))
    }
    fn cancelled_attempt_settled(
        &mut self,
        _: &graph::GraphAttempt,
    ) -> std::result::Result<bool, Crash> {
        Ok(self.cancelled_settled)
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        node: graph::NodeInput<'_>,
        _: &graph::GraphRoute,
        access: graph::AccessMode,
        worktree: Option<&str>,
        replay: graph::Replay<'_>,
    ) -> std::result::Result<AttemptObservation, Crash> {
        self.calls
            .push((key.clone(), replay.stored_ref.map(str::to_owned)));
        self.prior_attempts.push(node.prior_attempts.to_vec());
        self.permissions.push((access, worktree.map(str::to_owned)));
        if !self.launches.contains(key) {
            self.launches.push(key.clone());
        }
        match self.script.pop_front() {
            Some(Step::Done(outcome, launched, reference)) => Ok(AttemptObservation::Terminal {
                attempt_ref: Some(reference.into()),
                outcome,
                launched,
                result: None,
                result_warning: None,
                result_path: None,
                branch: self.terminal_commit.as_ref().map(|_| "lane/parity".into()),
                commit: self.terminal_commit.clone(),
                stop: None,
            }),
            Some(Step::Pending(reference)) => Ok(AttemptObservation::Pending {
                attempt_ref: reference.map(str::to_owned),
            }),
            Some(Step::Crash) => Err(crash()),
            None => Ok(AttemptObservation::Pending {
                attempt_ref: replay.stored_ref.map(str::to_owned),
            }),
        }
    }
}

struct Harness {
    rig: TestRig,
    state: provenant_core::StateRoot,
    project: ProjectId,
    document: GraphDocument,
    id: String,
}
impl Harness {
    fn new(id: &str, node: serde_json::Value) -> Result<Self> {
        Self::with_document(serde_json::from_value(
            json!({"schema_version":1,"graph_id":id,"nodes":[node]}),
        )?)
    }
    fn with_document(document: GraphDocument) -> Result<Self> {
        let rig = TestRig::new()?;
        let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let state = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
        let id = document.graph_id.clone();
        Ok(Self {
            rig,
            state,
            project: ProjectId::from_bytes([7; 16]),
            document,
            id,
        })
    }
    /// Opening takes the graph lock; the previous handle must be dropped first,
    /// which is also how a crashed supervisor releases it.
    fn open(&self) -> Result<FileJournal> {
        let clock = SystemClock::new();
        let mut journal = FileJournal::open(
            &self.state,
            self.project,
            &self.id,
            WaitBudget::cli(clock.mono_now(), 10)?,
            &CancelToken::new(),
            &clock,
        )?;
        // Binding is per handle: a new supervisor rebinds the same execution.
        journal.bind(GraphExecution {
            extra: Default::default(),
            integration_base: None,
            review_base: None,
            directory: self.rig.project.clone(),
            document: self.document.clone(),
        })?;
        Ok(journal)
    }
    fn advance(&self, provider: &mut Provider) -> Result<graph::GraphRun> {
        let mut journal = self.open()?;
        Ok(graph::advance_with_budget(
            &self.document,
            provider,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?)
    }
    fn resume(&self, provider: &mut Provider) -> Result<graph::GraphRun> {
        let mut journal = self.open()?;
        Ok(graph::resume_with_budget(
            &self.document,
            provider,
            &mut journal,
            &Default::default(),
            &Default::default(),
            &[],
        )?)
    }
    fn checkpoint(&self) -> Result<GraphCheckpoint> {
        FileJournal::read_checkpoint(&self.state, self.project, &self.id)?
            .ok_or_else(|| "no checkpoint".into())
    }
    fn events(&self) -> Result<Vec<GraphEvent>> {
        Ok(FileJournal::read_events(
            &self.state,
            self.project,
            &self.id,
        )?)
    }
    /// Cancel as `src/fabric.rs` does: fence first, then stop and settle.
    fn cancel(&self, provider: &mut Provider) -> Result {
        let mut journal = self.open()?;
        let mut checkpoint = journal.load(&self.id)?.ok_or("no checkpoint")?;
        checkpoint.cancelled = true;
        checkpoint.cancel_reason = Some("operator stop".into());
        graph::mark_cancelled_nodes_without_live_attempts(&mut checkpoint);
        journal.save(&checkpoint)?;
        graph::settle_cancelled_nodes(&mut checkpoint, provider, &mut journal)?;
        Ok(())
    }
}

fn codes(events: &[GraphEvent], kind: GraphEventKind) -> Vec<&str> {
    events
        .iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.code.as_str())
        .collect()
}

/// The invariants every case shares: the log never contradicts the checkpoint.
fn assert_same_story(events: &[GraphEvent], checkpoint: &GraphCheckpoint) {
    let node = &checkpoint.nodes[0];
    let states = codes(events, GraphEventKind::NodeState);
    let expected = match node.state {
        NodeState::Pending => "pending",
        NodeState::Running => "running",
        NodeState::Succeeded => "succeeded",
        NodeState::Failed => "failed",
        NodeState::Blocked => "blocked",
        NodeState::Skipped => "skipped",
    };
    assert_eq!(states.last().copied(), Some(expected), "{events:?}");
    let settled = node
        .recorded_attempts()
        .filter(|a| a.outcome.is_some())
        .count();
    assert_eq!(
        codes(events, GraphEventKind::Attempt)
            .iter()
            .filter(|code| **code == "settled")
            .count(),
        settled,
        "{events:?}"
    );
    let limited = node
        .recorded_attempts()
        .filter(|a| a.outcome == Some(AttemptOutcome::UsageLimited))
        .count();
    assert_eq!(
        codes(events, GraphEventKind::Wait)
            .iter()
            .filter(|c| **c == "quota")
            .count(),
        limited
    );
    // The log is append-only: a resumed graph clears the flag, not the record.
    if checkpoint.cancelled {
        assert_eq!(codes(events, GraphEventKind::Cancellation), ["cancelled"]);
    }
    assert!(
        !codes(events, GraphEventKind::Checkpoint).is_empty(),
        "every save is logged"
    );
}

fn lane(routes: usize) -> serde_json::Value {
    let all: Vec<_> = (0..routes)
        .map(|i| json!({"adapter":format!("member{i}"),"model":format!("member{i}/model")}))
        .collect();
    json!({"id":"lane","prompt":"do the work","access":"read_only","routes":all})
}

#[test]
fn success() -> Result {
    let h = Harness::new("parity-success", lane(1))?;
    let mut p = Provider {
        script: [Step::Done(AttemptOutcome::Succeeded, true, "h/1")].into(),
        ..Default::default()
    };
    let run = h.advance(&mut p)?;
    assert_eq!(run.status, GraphStatus::Complete);
    let checkpoint = h.checkpoint()?;
    let node = &checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Succeeded);
    assert_eq!(node.attempts.len(), 1);
    assert_eq!(node.attempts[0].outcome, Some(AttemptOutcome::Succeeded));
    assert_eq!(node.advance_error_code, None);
    let events = h.events()?;
    assert_eq!(
        codes(&events, GraphEventKind::Attempt),
        ["started", "settled"]
    );
    assert_same_story(&events, &checkpoint);
    Ok(())
}

/// A changed launch is a new graph after either terminal outcome, while the
/// same launch replays its graph. The graph name is the graph id; keep both
/// runs in one journal store so an accidentally reused id would return the
/// earlier checkpoint.
#[test]
fn rerun_after_success_or_cancellation_launches_unless_the_launch_is_replayed() -> Result {
    for cancelled in [false, true] {
        for replayed in [false, true] {
            let first_id = "parity-run".to_string();
            let mut h = Harness::new(&first_id, lane(1))?;
            let first_step = if cancelled {
                Step::Pending(Some("h/first"))
            } else {
                Step::Done(AttemptOutcome::Succeeded, true, "h/first")
            };
            let mut p = Provider {
                script: [first_step].into(),
                cancelled_settled: true,
                ..Default::default()
            };
            h.advance(&mut p)?;
            if cancelled {
                h.cancel(&mut p)?;
            }
            let first = h.checkpoint()?;
            assert_eq!(first.cancelled, cancelled);
            assert_eq!(p.launches.len(), 1);
            let calls_before = p.calls.len();
            let rerun_id = if replayed {
                first_id.clone()
            } else {
                "parity-run-2".to_string()
            };
            h.id = rerun_id.clone();
            h.document.graph_id = rerun_id;
            p.script = [Step::Done(AttemptOutcome::Succeeded, true, "h/second")].into();
            let rerun = h.advance(&mut p)?;
            let checkpoint = h.checkpoint()?;
            if replayed {
                assert_eq!(p.calls.len(), calls_before);
                assert_eq!(p.launches.len(), 1);
                assert_eq!(checkpoint.cancelled, cancelled);
                assert_eq!(checkpoint.nodes[0].state, first.nodes[0].state);
            } else {
                assert_eq!(rerun.status, GraphStatus::Complete);
                assert_eq!(p.launches.len(), 2);
                assert_ne!(p.launches[0], p.launches[1]);
                assert!(!checkpoint.cancelled);
                assert_eq!(checkpoint.nodes[0].state, NodeState::Succeeded);
                assert_eq!(checkpoint.nodes[0].attempts.len(), 1);
                assert_same_story(&h.events()?, &checkpoint);
                let previous = FileJournal::read_checkpoint(&h.state, h.project, &first_id)?
                    .ok_or("previous checkpoint disappeared")?;
                assert_eq!(previous.cancelled, cancelled);
                assert_eq!(previous.nodes[0].state, first.nodes[0].state);
            }
        }
    }
    Ok(())
}

#[test]
fn failure() -> Result {
    let h = Harness::new("parity-failure", lane(2))?;
    let mut p = Provider {
        script: [Step::Done(AttemptOutcome::TaskFailed, true, "h/1")].into(),
        ..Default::default()
    };
    h.advance(&mut p)?;
    let checkpoint = h.checkpoint()?;
    let node = &checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Failed);
    // A launched task failure is the task's answer: no second member is tried.
    assert_eq!(node.attempts.len(), 1);
    assert_eq!(node.attempts[0].outcome, Some(AttemptOutcome::TaskFailed));
    assert_eq!(p.launches.len(), 1);
    assert_same_story(&h.events()?, &checkpoint);
    Ok(())
}

#[test]
fn quota_limit_retries_on_another_member() -> Result {
    let h = Harness::new("parity-quota", lane(2))?;
    let mut p = Provider {
        script: [
            Step::Done(AttemptOutcome::UsageLimited, true, "h/1"),
            Step::Done(AttemptOutcome::Succeeded, true, "h/2"),
        ]
        .into(),
        ..Default::default()
    };
    let run = h.advance(&mut p)?;
    assert_eq!(run.status, GraphStatus::Complete);
    let checkpoint = h.checkpoint()?;
    let node = &checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Succeeded);
    // Superseded attempts are history; the retry is the node's current one.
    let attempts: Vec<_> = node.recorded_attempts().collect();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].outcome, Some(AttemptOutcome::UsageLimited));
    // The retry runs on the other member, whatever its key's route index.
    assert_eq!(attempts[0].route.adapter.as_deref(), Some("member0"));
    assert_eq!(attempts[1].route.adapter.as_deref(), Some("member1"));
    assert_eq!(attempts[1].outcome, Some(AttemptOutcome::Succeeded));
    let events = h.events()?;
    assert_eq!(codes(&events, GraphEventKind::Wait), ["quota"]);
    assert_same_story(&events, &checkpoint);
    Ok(())
}

/// Medium: compile the plain `task` submission and a composite's task member
/// through the same patch admission used by the front door. Quota redraw must
/// retain identical predecessor evidence, including for a committed writer,
/// after another supervisor reopens the on-disk journal.
#[test]
fn quota_stopped_task_and_composite_member_keep_the_same_lineage() -> Result {
    use graph::patch::{self, Patch, PatchContext};
    use graph::template::TemplateLibrary;

    for writer in [false, true] {
        let mut lineages = Vec::new();
        for composite in [false, true] {
            let mut task = json!({"id":"task", "use":"task", "brief":"do the work"});
            if writer {
                task["access"] = json!("worktree_write");
                task["worktree"] = json!("/fixture/parity-writer");
                task["write_scope"] = json!(["src"]);
            }
            let library = TemplateLibrary::bundled();
            let mut patch = Patch {
                schema_version: patch::PATCH_SCHEMA_VERSION,
                nodes: vec![task.clone()],
                ..Default::default()
            };
            if composite {
                patch.templates.insert(
                    "parity-composite".into(),
                    serde_json::from_value(json!({"nodes":[task]}))?,
                );
                patch.nodes = vec![json!({"id":"group", "use":"parity-composite"})];
            }
            let id = format!("parity-quota-{writer}-{composite}");
            let document = patch::submit(
                &id,
                &patch,
                &PatchContext {
                    library: &library,
                    resolvers: &Default::default(),
                    scan: &patch::scan_for_secrets,
                    journal: None,
                    limits: &Default::default(),
                },
                None,
            )?;
            let h = Harness::with_document(document)?;
            let node_id = if composite { "group--task" } else { "task" };
            let initial_head = "1".repeat(40);
            let partial_commit = "2".repeat(40);
            let mut first = Provider {
                script: [
                    Step::Done(AttemptOutcome::UsageLimited, true, "h/quota"),
                    Step::Pending(Some("h/continuation")),
                ]
                .into(),
                quota_redraw: true,
                retry_limit: Some(1),
                writer_head: writer.then(|| initial_head.clone()),
                terminal_commit: writer.then(|| partial_commit.clone()),
                ..Default::default()
            };
            assert_eq!(h.advance(&mut first)?.status, GraphStatus::Pending);
            let waiting = h.checkpoint()?;
            let node = waiting
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .unwrap();
            assert_eq!(node.state, NodeState::Running);
            assert_eq!(node.previous_attempts.len(), 1);
            assert_eq!(
                node.previous_attempts[0].outcome,
                Some(AttemptOutcome::UsageLimited)
            );
            assert_eq!(
                node.previous_attempts[0].attempt_ref.as_deref(),
                Some("h/quota")
            );
            assert_eq!(node.attempts.len(), 1);
            let successor = &node.attempts[0];
            assert_eq!(successor.key.extra["previous_attempt_ref"], "h/quota");
            assert_eq!(successor.key.extra["retry_generation"], 1);
            assert_eq!(
                successor.key.route_index, 0,
                "redraw keeps the task's tier alias"
            );
            assert_eq!(successor.route.alias.as_deref(), Some("senior"));
            assert_ne!(successor.key, node.previous_attempts[0].key);
            assert_eq!(
                first.prior_attempts,
                [vec![], vec!["member0/model".to_owned()]]
            );
            assert_eq!(first.launches.len(), 2);
            if writer {
                assert_eq!(
                    first.writer_bindings, 1,
                    "continuation retains the bound worktree"
                );
                assert_eq!(node.source_commit.as_deref(), Some(initial_head.as_str()));
                assert_eq!(node.writer_binding.as_ref().unwrap().head, initial_head);
                assert!(first.permissions.iter().all(|permission| {
                    permission
                        == &(
                            graph::AccessMode::WorktreeWrite,
                            Some("/fixture/parity-writer".into()),
                        )
                }));
            }
            // Fresh supervisor observes the admitted successor, with its
            // predecessor and original launch actor preserved in the row.
            let mut resumed = Provider {
                script: [Step::Done(
                    AttemptOutcome::Succeeded,
                    true,
                    "h/continuation",
                )]
                .into(),
                terminal_commit: writer.then(|| partial_commit.clone()),
                ..Default::default()
            };
            let run = h.advance(&mut resumed)?;
            assert_eq!(run.status, GraphStatus::Complete);
            let checkpoint = h.checkpoint()?;
            let node = checkpoint
                .nodes
                .iter()
                .find(|node| node.id == node_id)
                .unwrap();
            assert_eq!(node.state, NodeState::Succeeded);
            assert_eq!(
                resumed.calls,
                [(successor.key.clone(), Some("h/continuation".into()))]
            );
            assert_eq!(
                node.previous_attempts,
                waiting
                    .nodes
                    .iter()
                    .find(|node| node.id == node_id)
                    .unwrap()
                    .previous_attempts
            );
            assert_eq!(node.attempts.len(), 1);
            assert_eq!(node.attempts[0].outcome, Some(AttemptOutcome::Succeeded));
            if writer {
                let output = node.outputs.as_ref().unwrap();
                assert_eq!(output.branch.as_deref(), Some("lane/parity"));
                assert_eq!(output.commit.as_deref(), Some(partial_commit.as_str()));
                assert_eq!(resumed.writer_bindings, 0);
            }
            assert_eq!(codes(&h.events()?, GraphEventKind::Wait), ["quota"]);
            lineages.push(
                node.recorded_attempts()
                    .map(|attempt| {
                        json!({
                            "reference":attempt.attempt_ref,
                            "predecessor":attempt.key.extra.get("previous_attempt_ref"),
                            "generation":attempt.key.extra.get("retry_generation"),
                            "route_index":attempt.key.route_index,
                            "actor":attempt.launch_actor,
                            "outcome":attempt.outcome,
                        })
                    })
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(
            lineages[0], lineages[1],
            "plain task and composite member have identical lineage"
        );
    }
    Ok(())
}

#[test]
fn interrupted_run_resumes_the_same_attempt() -> Result {
    let h = Harness::new("parity-resume", lane(2))?;
    let mut first = Provider {
        script: [Step::Pending(Some("h/1"))].into(),
        ..Default::default()
    };
    let run = h.advance(&mut first)?;
    assert_ne!(run.status, GraphStatus::Complete);
    let mid = h.checkpoint()?;
    assert_eq!(mid.nodes[0].state, NodeState::Running);
    assert_eq!(mid.nodes[0].attempts[0].attempt_ref.as_deref(), Some("h/1"));
    // A fresh supervisor observes the same key and the stored reference; it
    // must not start a second attempt or consume the next member.
    let mut second = Provider {
        script: [Step::Done(AttemptOutcome::Succeeded, true, "h/1")].into(),
        ..Default::default()
    };
    let run = h.advance(&mut second)?;
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(second.calls[0].0, first.calls[0].0);
    assert_eq!(second.calls[0].1.as_deref(), Some("h/1"), "same session");
    let checkpoint = h.checkpoint()?;
    assert_eq!(checkpoint.nodes[0].attempts.len(), 1);
    assert_eq!(checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_same_story(&h.events()?, &checkpoint);
    Ok(())
}

#[test]
fn cancel_while_running() -> Result {
    let h = Harness::new("parity-cancel-running", lane(1))?;
    let mut p = Provider {
        script: [Step::Pending(Some("h/1"))].into(),
        cancelled_settled: true,
        ..Default::default()
    };
    h.advance(&mut p)?;
    h.cancel(&mut p)?;
    let checkpoint = h.checkpoint()?;
    assert!(checkpoint.cancelled);
    assert_eq!(checkpoint.cancel_reason.as_deref(), Some("operator stop"));
    let node = &checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Skipped);
    assert_eq!(node.skip_reason.as_deref(), Some("graph cancelled"));
    assert_eq!(node.attempts.len(), 1);
    assert_same_story(&h.events()?, &checkpoint);
    Ok(())
}

#[test]
fn cancel_while_waiting_for_a_slot() -> Result {
    let h = Harness::new("parity-cancel-waiting", lane(2))?;
    // The slot is never granted: the provider reports no accepted attempt.
    let mut p = Provider {
        script: [Step::Pending(None)].into(),
        cancelled_settled: true,
        ..Default::default()
    };
    h.advance(&mut p)?;
    let waiting = h.checkpoint()?;
    assert_eq!(waiting.nodes[0].attempts[0].attempt_ref, None);
    h.cancel(&mut p)?;
    let checkpoint = h.checkpoint()?;
    assert!(checkpoint.cancelled);
    assert_eq!(checkpoint.nodes[0].state, NodeState::Skipped);
    // Cancelling must not release the next member into the freed slot.
    assert_eq!(p.launches.len(), 1);
    assert_eq!(checkpoint.nodes[0].attempts.len(), 1);
    let before = p.calls.len();
    let run = h.advance(&mut p)?;
    assert_eq!(p.calls.len(), before, "a cancelled graph admits nothing");
    assert_ne!(run.status, GraphStatus::Complete);
    assert_same_story(&h.events()?, &h.checkpoint()?);
    Ok(())
}

#[test]
fn stop_then_retry_observes_the_same_attempt() -> Result {
    let h = Harness::new("parity-stop-retry", lane(2))?;
    let mut p = Provider {
        script: [Step::Pending(Some("h/1"))].into(),
        cancelled_settled: false,
        ..Default::default()
    };
    h.advance(&mut p)?;
    h.cancel(&mut p)?;
    assert!(p.stops >= 1, "stop reaches the live attempt");
    let stopped = h.checkpoint()?;
    assert!(stopped.cancelled);
    assert_eq!(
        stopped.nodes[0].state,
        NodeState::Running,
        "stop is not yet settled"
    );
    // Retry reopens admission; the same key is observed, not relaunched.
    let mut retry = Provider {
        script: [Step::Done(AttemptOutcome::Succeeded, true, "h/1")].into(),
        ..Default::default()
    };
    let run = h.resume(&mut retry)?;
    assert_eq!(run.status, GraphStatus::Complete);
    let checkpoint = h.checkpoint()?;
    assert!(!checkpoint.cancelled);
    assert_eq!(checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(checkpoint.nodes[0].attempts.len(), 1);
    assert_eq!(retry.calls[0].0, p.calls[0].0);
    assert_same_story(&h.events()?, &checkpoint);
    Ok(())
}

/// The supervisor dies as it launches. A transport-class failure is a bounded
/// wait, not a block: after the wake, a fresh supervisor observes the same
/// attempt key and no second member is started.
#[test]
fn supervisor_crash_then_recovery() -> Result {
    let h = Harness::new("parity-crash", lane(2))?;
    let t0 = SystemClock::new().wall_now()?;
    let mut crashing = Provider {
        script: [Step::Crash].into(),
        now: Some(t0),
        ..Default::default()
    };
    // The kernel records the owner failure for status rather than aborting.
    h.advance(&mut crashing)?;
    let waiting = h.checkpoint()?;
    let node = &waiting.nodes[0];
    assert_ne!(node.state, NodeState::Blocked, "{node:#?}");
    assert!(node.launch_retry.is_some(), "a wake is recorded");
    assert_eq!(node.attempts.len(), 1);
    // Before the wake nothing is launched again.
    let mut early = Provider {
        now: Some(t0),
        ..Default::default()
    };
    h.advance(&mut early)?;
    assert!(early.calls.is_empty(), "still waiting for its wake");
    let mut recovered = Provider {
        script: [Step::Done(AttemptOutcome::Succeeded, true, "h/1")].into(),
        now: Some(provenant_core::WallTime::from_micros(
            t0.as_micros() + 6_000_000,
        )?),
        ..Default::default()
    };
    let run = h.advance(&mut recovered)?;
    assert_eq!(run.status, GraphStatus::Complete);
    let checkpoint = h.checkpoint()?;
    let node = &checkpoint.nodes[0];
    assert_eq!(node.state, NodeState::Succeeded, "{node:#?}");
    assert_eq!(node.attempts.len(), 1, "{node:?}");
    assert_eq!(node.attempts[0].state, AttemptState::Terminal);
    assert_eq!(node.attempts[0].key.route_index, 0);
    assert_eq!(recovered.launches, crashing.launches);
    let events = h.events()?;
    // Setting and clearing the wait both log `launch_retry`; the log does not
    // say which, so only its presence is asserted.
    assert!(codes(&events, GraphEventKind::Wait).contains(&"launch_retry"));
    assert_same_story(&events, &checkpoint);
    // Recovery is idempotent: advancing a settled graph launches nothing.
    let mut idle = Provider::default();
    h.advance(&mut idle)?;
    assert!(idle.calls.is_empty());
    Ok(())
}
