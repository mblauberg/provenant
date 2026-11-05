use super::*;
use provenant_core::{Clock, ProjectId};
use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
use std::{convert::Infallible, os::unix::fs::PermissionsExt};
struct Owner;

#[test]
fn interrupted_marker_and_sentinel_publications_converge() {
    use super::journal::{CRASH_POINT, CrashPoint};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution.clone()).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    CRASH_POINT.with(|point| point.set(Some(CrashPoint::PendingMarker)));
    assert!(catch_unwind(AssertUnwindSafe(|| journal.save(&fixture.checkpoint))).is_err());
    drop(journal);
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.clear_stale_pending().unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    journal.save(&fixture.checkpoint).unwrap();
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    CRASH_POINT.with(|point| point.set(Some(CrashPoint::TerminalCheckpoint)));
    assert!(catch_unwind(AssertUnwindSafe(|| journal.save(&fixture.checkpoint))).is_err());
    drop(journal);
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.clear_stale_pending().unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    let sentinel = base
        .join("fabric/graphs")
        .join(project.to_string())
        .join("pending/complete");
    for stage in [CrashPoint::SentinelBefore, CrashPoint::SentinelAfter] {
        std::fs::remove_file(&sentinel).unwrap();
        CRASH_POINT.with(|point| point.set(Some(stage)));
        assert!(
            catch_unwind(AssertUnwindSafe(|| FileJournal::pending_graph_ids(
                &root, project
            )))
            .is_err()
        );
        assert_eq!(
            FileJournal::pending_graph_ids(&root, project).unwrap(),
            ["g-test"]
        );
    }
}

#[test]
fn manual_resume_publishes_marker_before_reopened_checkpoint() {
    use super::journal::{CRASH_POINT, CrashPoint};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"resume-test","nodes":[{"id":"one","prompt":"work",
            "access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
    }))
    .unwrap();
    let mut journal =
        FileJournal::open(&root, project, "resume-test", budget, &cancel, &clock).unwrap();
    journal
        .bind(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: None,
            directory: base.clone(),
            document: document.clone(),
        })
        .unwrap();
    let mut owner = Owner;
    let mut run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    run.checkpoint.cancelled = true;
    run.checkpoint.nodes[0].state = NodeState::Pending;
    run.checkpoint.nodes[0].attempts.clear();
    journal.save(&run.checkpoint).unwrap();
    let stopped = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(stopped.checkpoint.nodes[0].state, NodeState::Skipped);
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    CRASH_POINT.with(|point| point.set(Some(CrashPoint::PendingMarker)));
    let interrupted = catch_unwind(AssertUnwindSafe(|| {
        resume(&document, &mut owner, &mut journal)
    }));
    assert!(interrupted.is_err(), "{interrupted:?}");
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["resume-test"]
    );
    assert!(
        FileJournal::read_checkpoint(&root, project, "resume-test")
            .unwrap()
            .unwrap()
            .cancelled
    );
    let reopened = resume(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(reopened.checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["resume-test"]
    );
}

#[test]
fn answered_ask_publishes_marker_before_pending_checkpoint() {
    use super::journal::{CRASH_POINT, CrashPoint};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    fixture.checkpoint.asks.push(
        serde_json::from_value(serde_json::json!({
            "ask_id":"open","kind":"pause","session":"chair","title":"question","body":"waiting",
            "options":[],"resolved":false,"closed":false
        }))
        .unwrap(),
    );
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    fixture.checkpoint.asks[0].resolved = true;
    fixture.checkpoint.asks[0].answer = Some(
        serde_json::from_value(serde_json::json!({
            "source":"answered","actor":"chair","text":"continue"
        }))
        .unwrap(),
    );
    CRASH_POINT.with(|point| point.set(Some(CrashPoint::PendingMarker)));
    assert!(catch_unwind(AssertUnwindSafe(|| journal.save(&fixture.checkpoint))).is_err());
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    assert!(
        !FileJournal::read_checkpoint(&root, project, "g-test")
            .unwrap()
            .unwrap()
            .asks[0]
            .resolved
    );
    journal.save(&fixture.checkpoint).unwrap();
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    assert!(
        FileJournal::pending_execution(&root, project, "g-test")
            .unwrap()
            .is_some()
    );
}

#[test]
fn pending_index_reads_only_pending_checkpoints_after_rebuild() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    drop(FileJournal::open(&root, project, "initial", budget, &cancel, &clock).unwrap());
    let directory = base.join("fabric/graphs").join(project.to_string());
    let mut stored: StoredCheckpoint =
        serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json")).unwrap();
    for number in 0..5020 {
        let id = format!("graph-{number:04}");
        stored.checkpoint.graph_id = id.clone();
        stored.execution.document.graph_id = id.clone();
        stored.checkpoint.nodes[0].state = if number < 5000 {
            NodeState::Succeeded
        } else {
            NodeState::Running
        };
        stored.digest = CanonicalV1::record_digest(
            "fabric-graph-checkpoint",
            2,
            &(&stored.checkpoint, &stored.execution),
        )
        .unwrap()
        .to_string();
        let bytes = CanonicalV1::bytes(&stored).unwrap();
        std::fs::write(directory.join(format!("graph-{id}.json")), bytes.as_bytes()).unwrap();
    }
    let ids = FileJournal::pending_graph_ids(&root, project).unwrap();
    assert_eq!(ids.len(), 20);
    assert_eq!(
        ids,
        (5000..5020)
            .map(|number| format!("graph-{number:04}"))
            .collect::<Vec<_>>()
    );
    super::journal::CHECKPOINT_READS.with(|reads| reads.set(0));
    let ids = FileJournal::pending_graph_ids(&root, project).unwrap();
    for id in ids {
        assert!(
            FileJournal::read_checkpoint_for_tick(&root, project, &id)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(
        super::journal::CHECKPOINT_READS.with(|reads| reads.get()),
        20
    );
}

#[test]
fn checkpoint_collection_keeps_pending_and_recent_graphs() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution.clone()).unwrap();
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    journal.save(&fixture.checkpoint).unwrap();
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    fixture.checkpoint.asks.push(
        serde_json::from_value(serde_json::json!({
            "ask_id":"open", "kind":"pause", "session":"chair", "title":"question",
            "body":"waiting", "options":[], "resolved":false, "closed":false
        }))
        .unwrap(),
    );
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    fixture.checkpoint.asks.clear();
    let mut delivery = fixture.execution.clone();
    delivery.document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
    journal.bind(delivery).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, 0, &cancel, &clock).unwrap(),
        0
    );
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        1
    );
    assert!(
        FileJournal::read_checkpoint(&root, project, "g-test")
            .unwrap()
            .is_none()
    );
    assert!(
        !base
            .join("fabric/graphs")
            .join(project.to_string())
            .join("graph-g-test.lock")
            .exists()
    );
}

#[test]
fn checkpoint_collection_removes_matching_scratch_documents() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    fixture.execution.directory = base.join("checkout");
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    let scratch = fixture
        .execution
        .directory
        .join(".provenant/scratch/graphs");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bytes = serde_json::to_vec(&fixture.execution.document).unwrap();
    let digest = CanonicalV1::file_digest(&bytes).to_string();
    let generated = scratch.join(format!("preset-{}.json", &digest[7..]));
    std::fs::write(&generated, &bytes).unwrap();
    std::fs::set_permissions(&generated, std::fs::Permissions::from_mode(0o600)).unwrap();
    let operator = scratch.join("operator.json");
    std::fs::write(&operator, &bytes).unwrap();
    let revised = scratch.join("revised.json");
    std::fs::write(&revised, b"operator updated this file").unwrap();
    let mut other = fixture.execution.document.clone();
    other.graph_id = "another".into();
    let other_bytes = serde_json::to_vec(&other).unwrap();
    let other_hash = CanonicalV1::file_digest(&other_bytes).to_string();
    let other_path = scratch.join(format!("preset-{}.json", &other_hash[7..]));
    std::fs::write(&other_path, other_bytes).unwrap();
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    assert!(generated.exists());
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, 0, &cancel, &clock).unwrap(),
        0
    );
    assert!(generated.exists());
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        1
    );
    assert!(!generated.exists());
    assert_eq!(
        std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o077,
        0
    );
    assert!(!operator.exists());
    assert!(revised.exists());
    assert!(other_path.exists());
}

#[test]
fn pending_markers_follow_saves_and_rebuild_after_loss() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    journal.save(&fixture.checkpoint).unwrap();
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    let markers = base
        .join("fabric/graphs")
        .join(project.to_string())
        .join("pending");
    std::fs::remove_dir_all(&markers).unwrap();
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    journal.save(&fixture.checkpoint).unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    // A crash after publishing a marker but before publishing pending state
    // leaves a harmless marker, which is cleared under the journal lock.
    std::fs::write(markers.join("g-test.pending"), b"1").unwrap();
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    journal.clear_stale_pending().unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    drop(journal);
    std::fs::remove_file(markers.join("complete")).unwrap();
    std::fs::write(
        base.join("fabric/graphs")
            .join(project.to_string())
            .join("graph-g-test.json"),
        b"invalid",
    )
    .unwrap();
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["g-test"]
    );
    assert!(FileJournal::read_checkpoint_for_tick(&root, project, "g-test").is_err());
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_none()
    );
}
impl AttemptOwner for Owner {
    type Error = Infallible;
    fn actor(&self) -> String {
        "fixture-actor".into()
    }
    fn observe_or_start(
        &mut self,
        _: &AttemptKey,
        _: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        _: Option<&str>,
        _: Replay<'_>,
    ) -> Result<AttemptObservation, Infallible> {
        Ok(AttemptObservation::Pending {
            attempt_ref: Some("fixture-attempt".into()),
        })
    }
}

#[test]
fn launched_route_fallback_history_is_unreadable() {
    let mut stored: StoredCheckpoint =
        serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json")).unwrap();
    let node = &mut stored.checkpoint.nodes[0];
    node.attempts[0].launched = true;
    node.attempts[0].outcome = Some(AttemptOutcome::UsageLimited);
    let mut next = node.attempts[0].clone();
    next.key.route_index = 1;
    node.attempts.push(next);
    stored.digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&stored.checkpoint, &stored.execution),
    )
    .unwrap()
    .to_string();
    let error = match decode_checkpoint(&serde_json::to_vec(&stored).unwrap(), "g-test") {
        Err(error) => error,
        Ok(_) => panic!("old launched-route fallback must be unreadable"),
    };
    assert!(matches!(error, JournalError::RetiredFallbackCheckpoint));
    assert!(error.is_unreadable());
    assert!(error.to_string().contains("previous release"));
}
#[test]
fn delivery_retry_reopens_only_the_matching_failed_attempt() {
    for outcome in [AttemptOutcome::OtherFailure, AttemptOutcome::ReviewRequired] {
        assert_delivery_retry_reopens_matching_attempt(outcome);
    }
}

fn assert_delivery_retry_reopens_matching_attempt(outcome: AttemptOutcome) {
    let temp = crate::test_support::tempdir().expect("state fixture");
    let base = std::fs::canonicalize(temp.path()).expect("absolute state");
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).expect("private state");
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).expect("state root");
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).expect("budget");
    let mut legacy: StoredCheckpoint =
        serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json"))
            .expect("legacy fixture");
    let node = &mut legacy.checkpoint.nodes[0];
    node.state = NodeState::Failed;
    node.attempts[0].outcome = Some(outcome);
    {
        let mut journal =
            FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).expect("journal");
        journal.bind(legacy.execution).expect("bind execution");
        journal.save(&legacy.checkpoint).expect("failed delivery");
    }
    FileJournal::retry_delivered_attempt(
        &root,
        project,
        provenant_core::AttemptId::from_bytes([7; 16]),
        budget,
        &cancel,
        &clock,
    )
    .expect("unrelated pickup");
    assert_eq!(
        FileJournal::read_checkpoint(&root, project, "g-test")
            .expect("checkpoint")
            .expect("graph")
            .nodes[0]
            .state,
        NodeState::Failed
    );
    FileJournal::retry_delivered_attempt(
        &root,
        project,
        "22222222222222222222222222222222".parse().expect("attempt"),
        budget,
        &cancel,
        &clock,
    )
    .expect("successful pickup");
    let checkpoint = FileJournal::read_checkpoint(&root, project, "g-test")
        .expect("checkpoint")
        .expect("graph");
    assert_eq!(checkpoint.nodes[0].state, NodeState::Running);
    assert_eq!(
        checkpoint.nodes[0].attempts[0].state,
        AttemptState::Starting
    );
    assert_eq!(checkpoint.nodes[0].attempts[0].outcome, None);
    assert!(
        FileJournal::pending_execution(&root, project, "g-test")
            .expect("pending graph")
            .is_some()
    );
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).expect("pending marker"),
        ["g-test"]
    );
}
#[test]
fn tick_reports_undecodable_checkpoint_once_and_recovers() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let fixture: StoredCheckpoint =
        serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json")).unwrap();
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    let path = base
        .join("state/fabric/graphs")
        .join(project.to_string())
        .join("graph-g-test.json");
    let valid = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{} ").unwrap();
    // The ordinary read reproduces the same error on every tick.
    assert!(FileJournal::read_checkpoint(&root, project, "g-test").is_err());
    let first = FileJournal::read_checkpoint_for_tick(&root, project, "g-test").unwrap_err();
    assert!(first.to_string().contains("cancel it"));
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_none()
    );
    FileJournal::set_advance_error(&root, project, "g-test", Some("another error")).unwrap();
    FileJournal::set_advance_error(&root, project, "g-test", None).unwrap();
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_none()
    );
    std::fs::write(&path, &valid).unwrap();
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_some()
    );
    assert!(!path.with_file_name("graph-g-test.unreadable").exists());
    let mut bad_digest: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    bad_digest["digest"] = "sha256:bad".into();
    std::fs::write(&path, serde_json::to_vec(&bad_digest).unwrap()).unwrap();
    assert!(matches!(
        FileJournal::read_checkpoint(&root, project, "g-test"),
        Err(JournalError::BadDigest)
    ));
    assert!(FileJournal::read_checkpoint_for_tick(&root, project, "g-test").is_err());
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_none()
    );
    std::fs::write(&path, b"{} ").unwrap();
    assert!(FileJournal::read_checkpoint_for_tick(&root, project, "g-test").is_err());
}
#[test]
fn advance_error_is_recorded_replaced_and_cleared_and_rebind_moves_directory() {
    let root = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let document:GraphDocument=serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"errors","nodes":[{"id":"one","prompt":"test","access":"read_only","route":{"adapter":"fixture","model":"fixture"}}]})).unwrap();
    let read = || FileJournal::read_advance_error(&root, project, "errors").unwrap();
    assert_eq!(read(), None);
    let mut journal = FileJournal::open(&root, project, "errors", budget, &cancel, &clock).unwrap();
    for directory in [base.clone(), base.join("state")] {
        journal
            .bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory,
                document: document.clone(),
            })
            .unwrap();
    }
    // Recording takes no graph lock, so it works while the journal is held.
    let set = |error| FileJournal::set_advance_error(&root, project, "errors", error).unwrap();
    set(Some("first"));
    assert_eq!(read().as_deref(), Some("first"));
    set(Some("second"));
    assert_eq!(read().as_deref(), Some("second"));
    let long = "é".repeat(MAX_ADVANCE_ERROR_BYTES);
    set(Some(&long));
    assert_eq!(read().unwrap().len(), MAX_ADVANCE_ERROR_BYTES);
    set(None);
    assert_eq!(read(), None);
    set(None);
}
#[test]
fn version_two_checkpoint_without_v2_fields_reloads() {
    let root = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    // Version 2 is a superset of version 1, so a plain document saves and reloads.
    let document: GraphDocument = serde_json::from_value(serde_json::json!({"schema_version":2,"graph_id":"plain-v2","nodes":[{"id":"one","prompt":"test","access":"read_only","route":{"adapter":"fixture","model":"fixture"}}]})).unwrap();
    let checkpoint = {
        let mut journal =
            FileJournal::open(&root, project, "plain-v2", budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory: base.clone(),
                document: document.clone(),
            })
            .unwrap();
        advance(&document, &mut Owner, &mut journal)
            .unwrap()
            .checkpoint
    };
    assert_eq!(
        FileJournal::read_checkpoint(&root, project, "plain-v2").unwrap(),
        Some(checkpoint.clone())
    );
    let mut journal =
        FileJournal::open(&root, project, "plain-v2", budget, &cancel, &clock).unwrap();
    assert_eq!(journal.load("plain-v2").unwrap(), Some(checkpoint));
}
#[test]
fn checkpoint_reopens_with_project_isolation_and_detects_tampering() {
    let root = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::set_permissions(base.join("state"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let document:GraphDocument=serde_json::from_value(serde_json::json!({"schema_version":1,"graph_id":"reopen","nodes":[{"id":"one","prompt":"test","access":"read_only","route":{"adapter":"fixture","model":"fixture"}}]})).unwrap();
    assert!(
        FileJournal::read_checkpoint(&root, project, "reopen")
            .unwrap()
            .is_none()
    );
    assert!(!root.as_path().join("fabric/graphs").exists());
    let checkpoint = {
        let mut journal =
            FileJournal::open(&root, project, "reopen", budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
                directory: base.clone(),
                document: document.clone(),
            })
            .unwrap();
        let checkpoint = advance(&document, &mut Owner, &mut journal)
            .unwrap()
            .checkpoint;
        // Status must remain available while the writer holds its lock.
        assert_eq!(
            FileJournal::read_checkpoint(&root, project, "reopen").unwrap(),
            Some(checkpoint.clone())
        );
        checkpoint
    };
    assert_eq!(
        FileJournal::graph_ids(&root, project).unwrap(),
        vec!["reopen"]
    );
    assert_eq!(
        FileJournal::pending_execution(&root, project, "reopen").unwrap(),
        Some(GraphExecution {
            review_base: None,
            extra: Default::default(),
            integration_base: Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap()),
            directory: base.clone(),
            document: document.clone()
        })
    );
    {
        let mut journal =
            FileJournal::open(&root, project, "reopen", budget, &cancel, &clock).unwrap();
        assert_eq!(journal.load("reopen").unwrap(), Some(checkpoint));
        assert_eq!(
            journal.integration_base(),
            Some(provenant_core::GitOid::parse(&"a".repeat(40)).unwrap())
        );
        assert!(matches!(
            journal.bind(GraphExecution {
                review_base: None,
                extra: Default::default(),
                integration_base: None,
                directory: base.clone(),
                document: document.clone()
            }),
            Err(JournalError::GraphMismatch)
        ));
    }
    {
        let other_project = ProjectId::from_bytes([2; 16]);
        assert!(
            FileJournal::read_checkpoint(&root, other_project, "reopen")
                .unwrap()
                .is_none()
        );
        assert!(
            !root
                .as_path()
                .join(format!("fabric/graphs/{other_project}"))
                .exists()
        );
    }
    let lock = root
        .as_path()
        .join(format!("fabric/graphs/{project}/graph-reopen.lock"));
    std::fs::remove_file(&lock).unwrap();
    assert!(
        FileJournal::read_checkpoint(&root, project, "missing")
            .unwrap()
            .is_none()
    );
    assert!(
        FileJournal::read_checkpoint(&root, project, "reopen")
            .unwrap()
            .is_some()
    );
    assert!(!lock.exists());
    let path = root
        .as_path()
        .join(format!("fabric/graphs/{project}/graph-reopen.json"));
    let mut stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    stored["checkpoint"]["graph_id"] = serde_json::json!("changed");
    std::fs::write(path, serde_json::to_vec(&stored).unwrap()).unwrap();
    assert!(matches!(
        FileJournal::read_checkpoint(&root, project, "reopen"),
        Err(JournalError::BadDigest)
    ));
    assert!(matches!(
        FileJournal::open(&root, project, "reopen", budget, &cancel, &clock),
        Err(JournalError::BadDigest)
    ));
}

#[test]
fn retired_checkpoint_cleanup_is_verified_and_failure_keeps_record() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.clone());
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    drop(FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap());
    let path = base
        .join("fabric/graphs")
        .join(project.to_string())
        .join("graph-g-test.json");
    let mut stored: StoredCheckpoint =
        serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json")).unwrap();
    stored.checkpoint.nodes[0]
        .extra
        .insert("loop_asks".into(), serde_json::json!([]));
    stored.digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&stored.checkpoint, &stored.execution),
    )
    .unwrap()
    .to_string();
    {
        let mut journal =
            FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
        journal.bind(stored.execution.clone()).unwrap();
        assert!(matches!(
            journal.save(&stored.checkpoint),
            Err(JournalError::RetiredLoopCheckpoint)
        ));
        assert!(!path.exists());
    }
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let failed = FileJournal::retire_unreadable(
        &root,
        project,
        "g-test",
        budget,
        &cancel,
        &clock,
        |checkpoint| {
            assert_eq!(checkpoint.unwrap().graph_id, "g-test");
            Err("cleanup failed".into())
        },
    );
    assert!(failed.is_err());
    assert!(path.exists());
    assert!(
        FileJournal::retire_unreadable(
            &root,
            project,
            "g-test",
            budget,
            &cancel,
            &clock,
            |checkpoint| {
                assert_eq!(checkpoint.unwrap().nodes[0].attempts.len(), 1);
                Ok(())
            }
        )
        .unwrap()
    );
    assert!(!path.exists());
}
