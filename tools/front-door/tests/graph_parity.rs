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
    Crash::LaunchUnknown {
        operation_id: provenant_core::OperationId::parse("op").expect("operation id"),
    }
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
}
enum Step {
    Pending(Option<&'static str>),
    Done(AttemptOutcome, bool, &'static str),
    Crash,
}
impl AttemptOwner for Provider {
    type Error = Crash;
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
        _: graph::NodeInput<'_>,
        _: &graph::GraphRoute,
        _: graph::AccessMode,
        _: Option<&str>,
        replay: graph::Replay<'_>,
    ) -> std::result::Result<AttemptObservation, Crash> {
        self.calls
            .push((key.clone(), replay.stored_ref.map(str::to_owned)));
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
                branch: None,
                commit: None,
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
        let rig = TestRig::new()?;
        let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let state = inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state;
        let document: GraphDocument =
            serde_json::from_value(json!({"schema_version":1,"graph_id":id,"nodes":[node]}))?;
        Ok(Self {
            rig,
            state,
            project: ProjectId::from_bytes([7; 16]),
            document,
            id: id.into(),
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
    let settled = node.attempts.iter().filter(|a| a.outcome.is_some()).count();
    assert_eq!(
        codes(events, GraphEventKind::Attempt)
            .iter()
            .filter(|code| **code == "settled")
            .count(),
        settled,
        "{events:?}"
    );
    let limited = node
        .attempts
        .iter()
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
    assert_eq!(
        codes(events, GraphEventKind::Checkpoint).is_empty(),
        false,
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
    assert_eq!(node.attempts.len(), 2);
    assert_eq!(node.attempts[0].outcome, Some(AttemptOutcome::UsageLimited));
    assert_eq!(node.attempts[0].key.route_index, 0);
    assert_eq!(node.attempts[1].key.route_index, 1);
    assert_eq!(node.attempts[1].outcome, Some(AttemptOutcome::Succeeded));
    let events = h.events()?;
    assert_eq!(codes(&events, GraphEventKind::Wait), ["quota"]);
    assert_same_story(&events, &checkpoint);
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
