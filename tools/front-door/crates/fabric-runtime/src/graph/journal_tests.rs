use super::*;
use provenant_core::{Clock, ProjectId};
use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
use std::{convert::Infallible, os::unix::fs::PermissionsExt};
struct Owner;

fn stored_fixture() -> StoredCheckpoint {
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
        "schema_version": 2,
        "graph_id": "g-test",
        "nodes": [{
            "id": "reader",
            "prompt": "work for reader",
            "access": "read_only",
            "route": {"adapter": "fixture", "model": "fixture/model"}
        }]
    }))
    .unwrap();
    let execution = GraphExecution {
        extra: Default::default(),
        bases: None,
        directory: base.clone(),
        document: document.clone(),
    };
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(execution).unwrap();
    let mut checkpoint = advance(&document, &mut Owner, &mut journal)
        .unwrap()
        .checkpoint;
    checkpoint.nodes[0].state = NodeState::Succeeded;
    let attempt = &mut checkpoint.nodes[0].attempts[0];
    attempt.state = AttemptState::Terminal;
    attempt.outcome = Some(AttemptOutcome::Succeeded);
    attempt.launched = true;
    attempt.attempt_ref = Some(format!(
        "{}/{}",
        "11111111111111111111111111111111", "22222222222222222222222222222222"
    ));
    attempt.launch_actor = "33333333333333333333333333333333".into();
    attempt.settled = 1;
    journal.save(&checkpoint).unwrap();
    let path = base
        .join("fabric/graphs")
        .join(project.to_string())
        .join("graph-g-test.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn landing_review_query_reads_typed_settlement_on_only_the_exact_head() {
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base);
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let mut fixture: StoredCheckpoint = stored_fixture();
    let head = provenant_core::GitOid::parse(&"a".repeat(40)).unwrap();
    let review = ReviewSettlement {
        attempt_id: provenant_core::AttemptId::from_bytes([7; 16]),
        head,
        base: None,
        verdict: provenant_records::workflow::ReviewVerdict::Approve,
        independent: true,
    };
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    fixture.checkpoint.nodes[0].review_settlement = Some(review.clone());
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    assert_eq!(landing_reviews(&root, project, head).unwrap(), vec![review]);
    let changed = provenant_core::GitOid::parse(&"b".repeat(40)).unwrap();
    assert!(landing_reviews(&root, project, changed).unwrap().is_empty());
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    journal.save(&fixture.checkpoint).unwrap();
    assert!(landing_reviews(&root, project, head).unwrap().is_empty());
}

#[test]
fn interrupted_peer_origin_publication_never_reuses_the_launch_name() {
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
    let origin = provenant_records::WorkKey::new(project, "origin", "writer", 1).unwrap();
    let mut fixture: StoredCheckpoint = stored_fixture();
    fixture
        .execution
        .document
        .extra
        .insert("peer_origin_work_key".into(), serde_json::json!(origin));
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution.clone()).unwrap();
    CRASH_POINT.with(|point| point.set(Some(CrashPoint::PeerOrigin)));
    assert!(catch_unwind(AssertUnwindSafe(|| journal.save(&fixture.checkpoint))).is_err());
    drop(journal);
    assert_eq!(
        FileJournal::peer_origin(&root, project, "g-test").unwrap(),
        Some(origin)
    );
    assert!(
        FileJournal::reserved_graph_ids(&root, project)
            .unwrap()
            .iter()
            .any(|id| id == "g-test")
    );
    fixture.execution.document.nodes[0].prompt = "changed input".into();
    let mut retry = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    assert!(matches!(
        retry.bind(fixture.execution),
        Err(JournalError::NameReserved)
    ));
}

#[test]
fn terminal_checkpoint_with_superseded_header_is_retired() {
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
    let mut stored = stored_fixture();
    stored.schema_version = super::journal::CHECKPOINT_SCHEMA_VERSION - 1;
    std::fs::write(&path, CanonicalV1::bytes(&stored).unwrap().as_bytes()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        FileJournal::read_status_checkpoint(&root, project, "g-test")
            .unwrap_err()
            .is_unreadable()
    );
    assert!(
        FileJournal::retire_unreadable(
            &root,
            project,
            "g-test",
            budget,
            &cancel,
            &clock,
            |checkpoint| {
                assert!(checkpoint.is_none());
                Ok(())
            }
        )
        .unwrap()
    );
    assert!(!path.exists());
}

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
    let mut fixture: StoredCheckpoint = stored_fixture();
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
            extra: Default::default(),
            bases: None,
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
fn an_ask_link_publishes_marker_before_pending_checkpoint() {
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
    let mut fixture: StoredCheckpoint = stored_fixture();
    fixture.checkpoint.nodes[0].state = NodeState::Succeeded;
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
    // A link to an open request makes the graph pending, so the tick reads the
    // inbox for its answer.
    fixture.checkpoint.asks.push(
        serde_json::from_value(serde_json::json!({
            "ask_id":"open","kind":"pause","session":"chair","request":"hrq-open"
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
        FileJournal::read_checkpoint(&root, project, "g-test")
            .unwrap()
            .unwrap()
            .asks
            .is_empty()
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
    // Applying the answer drops the link, and the graph is no longer pending.
    fixture.checkpoint.asks.clear();
    journal.save(&fixture.checkpoint).unwrap();
    assert!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .is_empty()
    );
}

fn checkpoint_scale_fixture() -> (tempfile::TempDir, StateRoot, ProjectId) {
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
    let mut stored: StoredCheckpoint = stored_fixture();
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
    (temp, root, project)
}

#[test]
fn pending_index_reads_only_pending_checkpoints_after_rebuild() {
    let (_temp, root, project) = checkpoint_scale_fixture();
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
#[ignore = "manual checkpoint read timing"]
#[allow(clippy::print_stderr)]
fn checkpoint_read_timing_at_scale() {
    let (_temp, root, project) = checkpoint_scale_fixture();
    // Build the markers before timing the steady-state pending path.
    assert_eq!(
        FileJournal::pending_graph_ids(&root, project)
            .unwrap()
            .len(),
        20
    );

    let clock = SystemClock::new();
    let started = clock.mono_now();
    let all = FileJournal::graph_ids(&root, project).unwrap();
    assert_eq!(all.len(), 5020);
    for id in &all {
        assert!(
            FileJournal::read_checkpoint_for_tick(&root, project, id)
                .unwrap()
                .is_some()
        );
    }
    let all_elapsed = clock
        .mono_now()
        .as_micros()
        .saturating_sub(started.as_micros());

    let started = clock.mono_now();
    let pending = FileJournal::pending_graph_ids(&root, project).unwrap();
    assert_eq!(pending.len(), 20);
    for id in &pending {
        assert!(
            FileJournal::read_checkpoint_for_tick(&root, project, id)
                .unwrap()
                .is_some()
        );
    }
    let pending_elapsed = clock
        .mono_now()
        .as_micros()
        .saturating_sub(started.as_micros());
    eprintln!("checkpoint reads: all 5020: {all_elapsed} us; pending 20: {pending_elapsed} us");
}

#[test]
fn checkpoint_collection_keeps_pending_graphs_and_reserves_collected_names() {
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
    let mut fixture: StoredCheckpoint = stored_fixture();
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
            "ask_id":"open", "kind":"pause", "session":"chair", "request":"hrq-open"
        }))
        .unwrap(),
    );
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    // Bind the delivery document to its own journal: a resumed journal keeps
    // the original execution rather than accepting a replacement document.
    let mut delivery = fixture.execution.clone();
    delivery.document.graph_id = "g-delivery".into();
    delivery.document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
    let mut delivery_checkpoint = fixture.checkpoint.clone();
    delivery_checkpoint.graph_id = "g-delivery".into();
    delivery_checkpoint.asks.clear();
    let mut journal =
        FileJournal::open(&root, project, "g-delivery", budget, &cancel, &clock).unwrap();
    journal.bind(delivery).unwrap();
    journal.save(&delivery_checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        0
    );
    let mut journal =
        FileJournal::open(&root, project, "g-delivery", budget, &cancel, &clock).unwrap();
    delivery_checkpoint.delivered = true;
    journal.save(&delivery_checkpoint).unwrap();
    drop(journal);
    assert_eq!(
        FileJournal::collect_terminal(&root, project, i128::MAX, &cancel, &clock).unwrap(),
        1
    );
    fixture.checkpoint.asks.clear();
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
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
    let reserved = FileJournal::reserved_graph_ids(&root, project).unwrap();
    assert!(reserved.iter().any(|id| id == "g-test"));
    assert!(matches!(
        crate::graph::template::decide_graph_name("g-test", "changed", true, &reserved, |_| None),
        Ok(crate::graph::template::NameDecision::Create { id, .. }) if id == "g-test-2"
    ));
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    assert!(matches!(
        journal.bind(fixture.execution.clone()),
        Err(JournalError::NameReserved)
    ));
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
    let mut fixture: StoredCheckpoint = stored_fixture();
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
fn rebuild_keeps_healthy_graph_pending_beside_oversized_checkpoint() {
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
    let mut fixture: StoredCheckpoint = stored_fixture();
    fixture.checkpoint.nodes[0].state = NodeState::Running;
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    let directory = base.join("fabric/graphs").join(project.to_string());
    std::fs::remove_dir_all(directory.join("pending")).unwrap();
    std::fs::File::create(directory.join("graph-bad.json"))
        .unwrap()
        .set_len(16_777_217)
        .unwrap();

    assert_eq!(
        FileJournal::pending_graph_ids(&root, project).unwrap(),
        ["bad", "g-test"]
    );
    assert!(directory.join("pending/complete").is_file());
    let error = FileJournal::read_checkpoint_for_tick(&root, project, "bad").unwrap_err();
    assert!(matches!(
        error.downcast_ref::<JournalError>(),
        Some(JournalError::TooLarge)
    ));
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_some()
    );
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
    let mut fixture: StoredCheckpoint = stored_fixture();
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
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }
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

struct PinnedOwner(GraphBaseBinding);

impl AttemptOwner for PinnedOwner {
    type Error = Infallible;

    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }

    fn actor(&self) -> String {
        "pinned-fixture-actor".into()
    }

    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        (self.0.review, self.0.integration)
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
fn launched_route_fallback_history_is_readable() {
    let mut stored: StoredCheckpoint = stored_fixture();
    let node = &mut stored.checkpoint.nodes[0];
    node.attempts[0].launched = true;
    node.attempts[0].outcome = Some(AttemptOutcome::UsageLimited);
    let mut next = node.attempts[0].clone();
    next.key.route_index = 1;
    next.key.attempt = 2;
    node.attempts.push(next);
    stored.digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&stored.checkpoint, &stored.execution),
    )
    .unwrap()
    .to_string();
    let decoded = decode_checkpoint(&serde_json::to_vec(&stored).unwrap(), "g-test").unwrap();
    assert_eq!(decoded.checkpoint.nodes[0].attempts.len(), 2);
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
    let mut legacy: StoredCheckpoint = stored_fixture();
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
    let mut fixture: StoredCheckpoint = stored_fixture();
    fixture.checkpoint.nodes[0].state = NodeState::Running;
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
    assert!(first.to_string().contains("provenant stop -- g-test"));
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
fn tick_reports_undecodable_undelivered_pr_with_skipped_node() {
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
    let mut fixture: StoredCheckpoint = stored_fixture();
    fixture.execution.document.delivery = Some(provenant_records::run::DeliveryMode::Pr);
    fixture.checkpoint.nodes[0].state = NodeState::Skipped;
    fixture.checkpoint.delivered = false;
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);
    let path = base
        .join("fabric/graphs")
        .join(project.to_string())
        .join("graph-g-test.json");
    let mut checkpoint: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    checkpoint["digest"] = "sha256:bad".into();
    std::fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert!(FileJournal::read_checkpoint_for_tick(&root, project, "g-test").is_err());
    assert!(
        FileJournal::read_checkpoint_for_tick(&root, project, "g-test")
            .unwrap()
            .is_none()
    );
}
#[test]
fn advance_error_is_recorded_replaced_and_cleared_and_rebind_keeps_directory() {
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
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "graph_id": "errors",
        "nodes": [{
            "id": "one",
            "prompt": "test",
            "access": "read_only",
            "route": {"adapter": "fixture", "model": "fixture"}
        }]
    }))
    .unwrap();
    let read = || FileJournal::read_advance_error(&root, project, "errors").unwrap();
    assert_eq!(read(), None);
    let mut journal = FileJournal::open(&root, project, "errors", budget, &cancel, &clock).unwrap();
    for directory in [base.clone(), base.join("state")] {
        journal
            .bind(GraphExecution {
                extra: Default::default(),
                bases: None,
                directory,
                document: document.clone(),
            })
            .unwrap();
    }
    advance(&document, &mut Owner, &mut journal).unwrap();
    let execution = FileJournal::execution(&root, project, "errors")
        .unwrap()
        .unwrap();
    assert_eq!(execution.directory, base);
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
    drop(journal);
    let mut journal = FileJournal::open(&root, project, "errors", budget, &cancel, &clock).unwrap();
    journal
        .bind(GraphExecution {
            extra: Default::default(),
            bases: None,
            directory: base.join("state"),
            document: document.clone(),
        })
        .unwrap();
    advance(&document, &mut Owner, &mut journal).unwrap();
    assert_eq!(
        FileJournal::execution(&root, project, "errors")
            .unwrap()
            .unwrap()
            .directory,
        base
    );
}
#[test]
fn schema_two_document_with_optional_fields_absent_reloads() {
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
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "plain-v2",
        "nodes": [{
            "id": "one",
            "prompt": "test",
            "access": "read_only",
            "route": {"adapter": "fixture", "model": "fixture"}
        }]
    }))
    .unwrap();
    let checkpoint = {
        let mut journal =
            FileJournal::open(&root, project, "plain-v2", budget, &cancel, &clock).unwrap();
        journal
            .bind(GraphExecution {
                extra: Default::default(),
                bases: None,
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
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "graph_id": "reopen",
        "integration_branch": "integration",
        "nodes": [{
            "id": "one",
            "prompt": "test",
            "access": "read_only",
            "route": {"adapter": "fixture", "model": "fixture"}
        }]
    }))
    .unwrap();
    let integration = provenant_core::GitOid::parse(&"a".repeat(40)).unwrap();
    let bases = GraphBaseBinding::new(None, Some(integration)).unwrap();
    let execution = GraphExecution {
        extra: Default::default(),
        bases: Some(bases),
        directory: base.clone(),
        document: document.clone(),
    };
    assert!(
        FileJournal::read_checkpoint(&root, project, "reopen")
            .unwrap()
            .is_none()
    );
    assert!(!root.as_path().join("fabric/graphs").exists());
    let checkpoint = {
        let mut journal =
            FileJournal::open(&root, project, "reopen", budget, &cancel, &clock).unwrap();
        journal.bind(execution.clone()).unwrap();
        let checkpoint = advance(&document, &mut PinnedOwner(bases), &mut journal)
            .unwrap()
            .checkpoint;
        assert_eq!(checkpoint.bases, Some(bases));
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
        Some(execution.clone())
    );
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            root.as_path()
                .join(format!("fabric/graphs/{project}/graph-reopen.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(stored["checkpoint"].get("bases").is_none());
    let integration_text = integration.to_string();
    assert_eq!(
        stored["execution"]["bases"]["integration"].as_str(),
        Some(integration_text.as_str())
    );
    {
        let mut journal =
            FileJournal::open(&root, project, "reopen", budget, &cancel, &clock).unwrap();
        let hydrated = journal.load("reopen").unwrap().unwrap();
        assert_eq!(hydrated, checkpoint);
        assert_eq!(hydrated.bases, Some(bases));
        assert_eq!(journal.integration_base(), Some(integration));
        assert!(matches!(
            journal.bind(GraphExecution {
                bases: None,
                ..execution.clone()
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
fn bound_attempt_manifest_survives_file_journal_reopen_and_reattachment() {
    type Observation = (AttemptKey, Option<String>, Option<String>, Option<String>);
    #[derive(Default)]
    struct RecordingOwner {
        observed: Vec<Observation>,
    }
    impl AttemptOwner for RecordingOwner {
        type Error = Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            "fixture-actor".into()
        }
        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            node: NodeInput<'_>,
            _: &GraphRoute,
            _: AccessMode,
            _: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Infallible> {
            self.observed.push((
                key.clone(),
                node.source_commit.map(str::to_owned),
                node.review_base.map(str::to_owned),
                replay.stored_ref.map(str::to_owned),
            ));
            Ok(AttemptObservation::Pending {
                attempt_ref: Some("fixture-attempt".into()),
            })
        }
    }
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base_dir = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs =
        provenant_core::RootInputs::new(base_dir.join("home"), base_dir.join("checkout"));
    inputs.state_dir = Some(base_dir.join("state"));
    std::fs::create_dir(base_dir.join("state")).unwrap();
    std::fs::set_permissions(
        base_dir.join("state"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let source = "1".repeat(40);
    let review = "2".repeat(40);
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":2,"graph_id":"pinned-replay","nodes":[{
            "id":"review","prompt":"review","access":"read_only",
            "source":{"type":"commit","commit":source}, "review_base":review,
            "route":{"adapter":"fixture","model":"fixture/model"}
        }]
    }))
    .unwrap();
    let execution = GraphExecution {
        extra: Default::default(),
        bases: None,
        directory: base_dir.clone(),
        document: document.clone(),
    };
    let mut owner = RecordingOwner::default();
    let first = {
        let mut journal =
            FileJournal::open(&root, project, "pinned-replay", budget, &cancel, &clock).unwrap();
        journal.bind(execution.clone()).unwrap();
        let mut checkpoint = advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint;
        checkpoint.nodes[0].attempts[0]
            .input_manifest
            .as_mut()
            .unwrap()
            .extra
            .insert("future".into(), serde_json::json!({"preserved":true}));
        journal.save(&checkpoint).unwrap();
        checkpoint
    };
    let replay = {
        let mut journal =
            FileJournal::open(&root, project, "pinned-replay", budget, &cancel, &clock).unwrap();
        let mut rebound = execution;
        rebound.directory = base_dir.join("state");
        journal.bind(rebound).unwrap();
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint
    };
    assert_eq!(first, replay);
    assert_eq!(owner.observed.len(), 2);
    assert_eq!(owner.observed[0].0, owner.observed[1].0);
    assert_eq!(owner.observed[1].1.as_deref(), Some(source.as_str()));
    assert_eq!(owner.observed[1].2.as_deref(), Some(review.as_str()));
    assert_eq!(owner.observed[1].3.as_deref(), Some("fixture-attempt"));
    assert_eq!(
        replay.nodes[0].attempts[0]
            .input_manifest
            .as_ref()
            .unwrap()
            .extra["future"]["preserved"],
        true
    );
    assert_eq!(
        FileJournal::execution(&root, project, "pinned-replay")
            .unwrap()
            .unwrap()
            .directory,
        base_dir
    );
}

#[test]
fn implicit_writer_binding_reattaches_without_resolving_the_moved_head() {
    struct BoundOwner {
        head: String,
        bindings: usize,
        sources: Vec<String>,
    }
    impl AttemptOwner for BoundOwner {
        type Error = Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            "fixture".into()
        }
        fn bind_writer_worktree(
            &mut self,
            _: &str,
        ) -> Result<Option<WriterWorktreeBinding>, Infallible> {
            self.bindings += 1;
            Ok(Some(WriterWorktreeBinding {
                extra: Default::default(),
                worktree: "/fixture/lane".into(),
                head: self.head.clone(),
            }))
        }
        fn observe_or_start(
            &mut self,
            _: &AttemptKey,
            node: NodeInput<'_>,
            _: &GraphRoute,
            _: AccessMode,
            worktree: Option<&str>,
            _: Replay<'_>,
        ) -> Result<AttemptObservation, Infallible> {
            assert_eq!(worktree, Some("/fixture/lane"));
            self.sources.push(node.source_commit.unwrap().to_owned());
            Ok(AttemptObservation::Pending {
                attempt_ref: Some("fixture-attempt".into()),
            })
        }
    }
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = std::fs::canonicalize(temp.path()).unwrap();
    let mut inputs =
        provenant_core::RootInputs::new(directory.join("home"), directory.join("checkout"));
    inputs.state_dir = Some(directory.join("state"));
    std::fs::create_dir(directory.join("state")).unwrap();
    std::fs::set_permissions(
        directory.join("state"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
    let project = ProjectId::from_bytes([1; 16]);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":2, "graph_id":"implicit-reattach", "nodes":[{
            "id":"write", "prompt":"write", "access":"worktree_write", "worktree":"lane",
            "route":{"adapter":"fixture","model":"fixture/model"}
        }]
    }))
    .unwrap();
    let execution = GraphExecution {
        extra: Default::default(),
        bases: None,
        directory: directory.clone(),
        document: document.clone(),
    };
    let first_head = "1".repeat(40);
    let mut owner = BoundOwner {
        head: first_head.clone(),
        bindings: 0,
        sources: Vec::new(),
    };
    let first = {
        let mut journal =
            FileJournal::open(&root, project, "implicit-reattach", budget, &cancel, &clock)
                .unwrap();
        journal.bind(execution.clone()).unwrap();
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint
    };
    owner.head = "2".repeat(40);
    let replay = {
        let mut journal =
            FileJournal::open(&root, project, "implicit-reattach", budget, &cancel, &clock)
                .unwrap();
        journal.bind(execution).unwrap();
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint
    };
    assert_eq!(owner.bindings, 1);
    assert_eq!(owner.sources, vec![first_head.clone(), first_head]);
    assert_eq!(
        first.nodes[0].attempts[0].key,
        replay.nodes[0].attempts[0].key
    );
    assert_eq!(
        first.nodes[0].writer_binding,
        replay.nodes[0].writer_binding
    );
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
    let mut stored: StoredCheckpoint = stored_fixture();
    stored.digest =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".into();
    std::fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
    let failed = FileJournal::retire_unreadable(
        &root,
        project,
        "g-test",
        budget,
        &cancel,
        &clock,
        |checkpoint| {
            assert!(checkpoint.is_none());
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
                assert!(checkpoint.is_none());
                Ok(())
            }
        )
        .unwrap()
    );
    assert!(!path.exists());
}

#[test]
fn prompts_are_stored_by_digest_and_collected_with_the_graph() {
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
    let mut fixture: StoredCheckpoint = stored_fixture();
    fixture.execution.directory = base.clone();
    let node = fixture.execution.document.nodes[0].clone();
    let state = fixture.checkpoint.nodes[0].clone();
    fixture.execution.document.nodes.clear();
    fixture.checkpoint.nodes.clear();
    for index in 0..8 {
        let mut node = node.clone();
        node.id = format!("n{index}");
        node.depends_on.clear();
        node.prompt = format!("{index}{}", "x".repeat(16 * 1024));
        fixture.execution.document.nodes.push(node);
        let mut state = state.clone();
        state.id = format!("n{index}");
        fixture.checkpoint.nodes.push(state);
    }
    let inline = CanonicalV1::bytes(&fixture).unwrap().as_bytes().len();
    let mut journal = FileJournal::open(&root, project, "g-test", budget, &cancel, &clock).unwrap();
    journal.bind(fixture.execution.clone()).unwrap();
    journal.save(&fixture.checkpoint).unwrap();
    drop(journal);

    let directory = graph_directory(&root, project).unwrap();
    let stored = read_checkpoint_bytes(&directory, "g-test")
        .unwrap()
        .unwrap();
    assert!(stored.len() < inline / 4, "{} vs {inline}", stored.len());
    assert!(!String::from_utf8(stored).unwrap().contains("xxxx"));
    let loaded = FileJournal::read_checkpoint(&root, project, "g-test")
        .unwrap()
        .unwrap();
    assert_eq!(loaded, fixture.checkpoint);
    let execution = FileJournal::execution(&root, project, "g-test")
        .unwrap()
        .unwrap();
    assert_eq!(
        execution.document.nodes[3].prompt,
        fixture.execution.document.nodes[3].prompt
    );

    // A missing blob makes the checkpoint unreadable rather than silently empty.
    let blobs = |directory: &Directory| {
        directory
            .entries()
            .unwrap()
            .map(|name| name.unwrap().to_string_lossy().into_owned())
            .filter(|name| name.contains(".prompt-"))
            .collect::<Vec<_>>()
    };
    assert_eq!(blobs(&directory).len(), 8);
    assert!(prompt_store::collect(&directory, "g-test").unwrap());
    assert!(blobs(&directory).is_empty());
    assert!(load_checkpoint(&directory, "g-test").is_err());
}

#[test]
fn inline_prompt_checkpoints_still_decode() {
    let fixture: StoredCheckpoint = stored_fixture();
    let temp = crate::test_support::tempdir().unwrap();
    let directory = Directory::open_anchor(temp.path()).unwrap();
    let bytes = CanonicalV1::bytes(&fixture).unwrap();
    let stored = decode_checkpoint_in(&directory, bytes.as_bytes(), "g-test").unwrap();
    assert_eq!(stored.checkpoint, fixture.checkpoint);
}

/// Use the real journal and a durable-operation stand-in at the process edge.
/// A recovered supervisor has a new actor, but must replay the original key
/// and launch actor rather than accepting another provider process.
mod launch_recovery {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};

    const ATTEMPT: &str = "22222222222222222222222222222222";

    struct Fixture {
        _temp: tempfile::TempDir,
        root: StateRoot,
        project: ProjectId,
        execution: GraphExecution,
    }

    impl Fixture {
        fn new(nodes: serde_json::Value) -> Self {
            let temp = crate::test_support::tempdir().unwrap();
            let base = std::fs::canonicalize(temp.path()).unwrap();
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mut inputs =
                provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
            inputs.state_dir = Some(base.clone());
            let root = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
            Self {
                _temp: temp,
                root,
                project: ProjectId::from_bytes([1; 16]),
                execution: GraphExecution {
                    extra: Default::default(),
                    bases: None,
                    directory: base,
                    document: serde_json::from_value(serde_json::json!({
                        "schema_version":2, "graph_id":"launch-recovery", "nodes":nodes
                    }))
                    .unwrap(),
                },
            }
        }

        fn single() -> Self {
            Self::new(serde_json::json!([{
                "id":"reader", "prompt":"work", "access":"read_only",
                "route":{"adapter":"fixture", "model":"fixture/model"}
            }]))
        }

        fn open(&self) -> FileJournal {
            let clock = SystemClock;
            let mut journal = FileJournal::open(
                &self.root,
                self.project,
                &self.execution.document.graph_id,
                WaitBudget::cli(clock.mono_now(), 5).unwrap(),
                &CancelToken::new(),
                &clock,
            )
            .unwrap();
            journal.bind(self.execution.clone()).unwrap();
            journal
        }

        fn checkpoint(&self) -> GraphCheckpoint {
            FileJournal::read_checkpoint(
                &self.root,
                self.project,
                &self.execution.document.graph_id,
            )
            .unwrap()
            .unwrap()
        }
    }

    #[derive(Default)]
    struct Ledger {
        starts: Vec<(AttemptKey, String)>,
        calls: Vec<(AttemptKey, String, Option<String>)>,
    }

    struct ProcessOwner {
        actor: &'static str,
        ledger: Arc<Mutex<Ledger>>,
        terminal: bool,
        failed_node: Option<&'static str>,
        barrier: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
        crash: bool,
    }

    impl ProcessOwner {
        fn new(actor: &'static str, ledger: Arc<Mutex<Ledger>>) -> Self {
            Self {
                actor,
                ledger,
                terminal: false,
                failed_node: None,
                barrier: None,
                crash: false,
            }
        }
    }

    impl AttemptOwner for ProcessOwner {
        type Error = Infallible;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }

        fn actor(&self) -> String {
            self.actor.into()
        }

        fn observe_or_start(
            &mut self,
            key: &AttemptKey,
            _: NodeInput<'_>,
            _: &GraphRoute,
            _: AccessMode,
            _: Option<&str>,
            replay: Replay<'_>,
        ) -> Result<AttemptObservation, Infallible> {
            {
                let mut ledger = self.ledger.lock().unwrap();
                let operation = (key.clone(), replay.launch_actor.to_owned());
                if !ledger.starts.contains(&operation) {
                    ledger.starts.push(operation);
                }
                ledger.calls.push((
                    key.clone(),
                    replay.launch_actor.to_owned(),
                    replay.stored_ref.map(str::to_owned),
                ));
            }
            // No timing guess: the observer confirms that the owner is inside
            // this call and keeps its journal lock until explicitly released.
            if let Some((entered, release)) = self.barrier.take() {
                entered.send(()).unwrap();
                release
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
            }
            assert!(
                !self.crash,
                "injected loss before saving the accepted reference"
            );
            let reference = format!(
                "fixture/{}",
                if key.node_id == "other" {
                    "33333333333333333333333333333333"
                } else {
                    ATTEMPT
                }
            );
            if self.terminal || self.failed_node == Some(key.node_id.as_str()) {
                Ok(AttemptObservation::Terminal {
                    attempt_ref: Some(reference),
                    outcome: if self.failed_node == Some(key.node_id.as_str()) {
                        AttemptOutcome::OtherFailure
                    } else {
                        AttemptOutcome::Succeeded
                    },
                    launched: true,
                    result: None,
                    result_warning: None,
                    result_path: None,
                    branch: None,
                    commit: None,
                    settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
                    stop: None,
                })
            } else {
                Ok(AttemptObservation::Pending {
                    attempt_ref: Some(reference),
                })
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum InterruptAt {
        BeforeLaunch,
        ReferenceSave,
        BeforeSettlementSave,
        AfterSettlementSave,
    }

    struct InterruptedJournal {
        inner: FileJournal,
        point: InterruptAt,
    }

    impl GraphJournal for InterruptedJournal {
        type Error = JournalError;

        fn load(&mut self, id: &str) -> Result<Option<GraphCheckpoint>, JournalError> {
            self.inner.load(id)
        }

        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), JournalError> {
            let attempt = checkpoint.nodes[0].attempts.last();
            let selected = attempt.is_some_and(|attempt| match self.point {
                InterruptAt::BeforeLaunch => {
                    attempt.state == AttemptState::Starting && attempt.attempt_ref.is_none()
                }
                InterruptAt::ReferenceSave => {
                    attempt.attempt_ref.is_some() && attempt.outcome.is_none()
                }
                InterruptAt::BeforeSettlementSave | InterruptAt::AfterSettlementSave => {
                    attempt.outcome.is_some()
                }
            });
            if selected
                && matches!(
                    self.point,
                    InterruptAt::ReferenceSave | InterruptAt::BeforeSettlementSave
                )
            {
                panic!("injected interruption before {:?}", self.point);
            }
            self.inner.save(checkpoint)?;
            assert!(!selected, "injected interruption after {:?}", self.point);
            Ok(())
        }
    }

    #[test]
    fn durable_launch_boundaries_recover_one_process_under_the_original_actor() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        for point in [
            InterruptAt::BeforeLaunch,
            InterruptAt::ReferenceSave,
            InterruptAt::BeforeSettlementSave,
            InterruptAt::AfterSettlementSave,
        ] {
            let fixture = Fixture::single();
            let ledger = Arc::new(Mutex::new(Ledger::default()));
            let mut first = ProcessOwner::new("original-actor", ledger.clone());
            if matches!(
                point,
                InterruptAt::BeforeSettlementSave | InterruptAt::AfterSettlementSave
            ) {
                advance(&fixture.execution.document, &mut first, &mut fixture.open()).unwrap();
                first.terminal = true;
            }
            let interrupted = catch_unwind(AssertUnwindSafe(|| {
                let mut journal = InterruptedJournal {
                    inner: fixture.open(),
                    point,
                };
                advance(&fixture.execution.document, &mut first, &mut journal).unwrap();
            }));
            assert!(interrupted.is_err(), "{point:?}");
            let saved = fixture.checkpoint();
            let attempt = &saved.nodes[0].attempts[0];
            assert_eq!(attempt.launch_actor, "original-actor", "{point:?}");
            assert_eq!(attempt.key.route_index, 0);
            assert_eq!(
                ledger.lock().unwrap().starts.len(),
                usize::from(point != InterruptAt::BeforeLaunch),
                "{point:?}"
            );
            if matches!(
                point,
                InterruptAt::BeforeLaunch | InterruptAt::ReferenceSave
            ) {
                assert_eq!(attempt.attempt_ref, None, "{point:?}");
            }
            let before_recovery = ledger.lock().unwrap().calls.len();
            let mut recovered = ProcessOwner::new("recovery-actor", ledger.clone());
            recovered.terminal = true;
            let completed = resume(
                &fixture.execution.document,
                &mut recovered,
                &mut fixture.open(),
            )
            .unwrap();
            assert_eq!(completed.status, GraphStatus::Complete, "{point:?}");
            let saved = fixture.checkpoint();
            assert_eq!(saved.nodes[0].state, NodeState::Succeeded);
            assert_eq!(saved.nodes[0].attempts.len(), 1);
            assert_eq!(saved.nodes[0].attempts[0].key, attempt.key);
            assert_eq!(saved.nodes[0].attempts[0].launch_actor, "original-actor");
            let calls = {
                let ledger = ledger.lock().unwrap();
                assert_eq!(
                    ledger.starts,
                    [(attempt.key.clone(), "original-actor".into())]
                );
                for (key, actor, _) in &ledger.calls[before_recovery..] {
                    assert_eq!(key, &attempt.key);
                    assert_eq!(actor, "original-actor");
                }
                if point == InterruptAt::AfterSettlementSave {
                    assert_eq!(ledger.calls.len(), before_recovery);
                }
                ledger.calls.len()
            };
            advance(
                &fixture.execution.document,
                &mut recovered,
                &mut fixture.open(),
            )
            .unwrap();
            assert_eq!(ledger.lock().unwrap().calls.len(), calls);
        }
    }

    fn assert_lock_timeout<T>(result: Result<T, JournalError>) {
        assert!(matches!(
            result,
            Err(JournalError::Filesystem(FsError::LockTimeout { .. }))
        ));
    }

    #[test]
    fn concurrent_advance_tick_and_resume_keep_one_launch_and_the_bound_execution() {
        for crash in [false, true] {
            let fixture = Fixture::single();
            let ledger = Arc::new(Mutex::new(Ledger::default()));
            std::thread::scope(|scope| {
                let (entered, at_owner) = mpsc::channel();
                let (release, released) = mpsc::channel();
                let holder_fixture = &fixture;
                let holder_ledger = ledger.clone();
                let holder = scope.spawn(move || {
                    let mut owner = ProcessOwner::new("original-actor", holder_ledger);
                    owner.barrier = Some((entered, released));
                    owner.crash = crash;
                    let mut journal = holder_fixture.open();
                    advance(&holder_fixture.execution.document, &mut owner, &mut journal).unwrap()
                });
                at_owner
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                let before = fixture.checkpoint();
                let clock = SystemClock;
                let cancel = CancelToken::new();
                let open = |budget| {
                    FileJournal::open(
                        &fixture.root,
                        fixture.project,
                        &fixture.execution.document.graph_id,
                        budget,
                        &cancel,
                        &clock,
                    )
                };
                // Settlement waits are bounded; a held owner cannot be bypassed.
                assert_lock_timeout(open(WaitBudget::up_to(
                    clock.mono_now(),
                    DurationMicros::from_micros(20_000),
                )));
                // This is the nonblocking open used by tick_graphs. Its caller
                // treats exactly this timeout as a skip, never an advance error.
                assert_lock_timeout(open(WaitBudget::nonblocking(clock.mono_now())));
                // Manual resume must acquire custody before bind or resume.
                assert_lock_timeout(open(WaitBudget::up_to(
                    clock.mono_now(),
                    DurationMicros::from_micros(20_000),
                )));
                assert_eq!(fixture.checkpoint(), before);
                assert_eq!(ledger.lock().unwrap().starts.len(), 1);
                release.send(()).unwrap();
                assert_eq!(holder.join().is_err(), crash);
            });
            let saved = fixture.checkpoint();
            let original = &saved.nodes[0].attempts[0];
            assert_eq!(original.attempt_ref.is_none(), crash);
            let mut journal = fixture.open();
            let mut from_other_checkout = fixture.execution.clone();
            from_other_checkout.directory = fixture.root.as_path().join("other-checkout");
            journal.bind(from_other_checkout).unwrap();
            let mut resumed = ProcessOwner::new("manual-resume-actor", ledger.clone());
            resumed.terminal = true;
            let run = resume(&fixture.execution.document, &mut resumed, &mut journal).unwrap();
            assert_eq!(run.status, GraphStatus::Complete);
            assert_eq!(run.checkpoint.nodes[0].attempts.len(), 1);
            assert_eq!(run.checkpoint.nodes[0].attempts[0].key, original.key);
            assert_eq!(
                journal.execution.as_ref().unwrap().directory,
                fixture.execution.directory
            );
            let ledger = ledger.lock().unwrap();
            assert_eq!(
                ledger.starts,
                [(original.key.clone(), "original-actor".into())]
            );
            let (key, actor, reference) = ledger.calls.last().unwrap();
            assert_eq!(key, &original.key);
            assert_eq!(actor, "original-actor");
            assert_eq!(reference, &original.attempt_ref);
        }
    }

    #[test]
    fn delivery_retry_cannot_reopen_a_failed_node_during_a_held_advance() {
        let fixture = Fixture::new(serde_json::json!([
            {"id":"delivered", "prompt":"work", "access":"read_only",
                "route":{"adapter":"fixture", "model":"fixture/model"}},
            {"id":"other", "prompt":"other work", "access":"read_only",
                "route":{"adapter":"fixture", "model":"fixture/model"}}
        ]));
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        // Seed both real attempts, with a delivery failure on the first node.
        let mut seed = ProcessOwner::new("original-actor", ledger.clone());
        seed.failed_node = Some("delivered");
        advance(&fixture.execution.document, &mut seed, &mut fixture.open()).unwrap();
        assert_eq!(fixture.checkpoint().nodes[0].state, NodeState::Failed);
        std::thread::scope(|scope| {
            let (entered, at_owner) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let holder_fixture = &fixture;
            let holder_ledger = ledger.clone();
            let holder = scope.spawn(move || {
                let mut owner = ProcessOwner::new("settlement-actor", holder_ledger);
                owner.barrier = Some((entered, released));
                advance(
                    &holder_fixture.execution.document,
                    &mut owner,
                    &mut holder_fixture.open(),
                )
                .unwrap()
            });
            at_owner
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            let before = fixture.checkpoint();
            let clock = SystemClock;
            assert_lock_timeout(FileJournal::retry_delivered_attempt(
                &fixture.root,
                fixture.project,
                ATTEMPT.parse().unwrap(),
                WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(20_000)),
                &CancelToken::new(),
                &clock,
            ));
            assert_eq!(fixture.checkpoint(), before);
            assert_eq!(ledger.lock().unwrap().starts.len(), 2);
            release.send(()).unwrap();
            holder.join().unwrap();
        });
        let clock = SystemClock;
        FileJournal::retry_delivered_attempt(
            &fixture.root,
            fixture.project,
            ATTEMPT.parse().unwrap(),
            WaitBudget::cli(clock.mono_now(), 5).unwrap(),
            &CancelToken::new(),
            &clock,
        )
        .unwrap();
        let reopened = fixture.checkpoint();
        assert_eq!(reopened.nodes[0].state, NodeState::Running);
        assert_eq!(reopened.nodes[0].attempts[0].state, AttemptState::Starting);
        assert_eq!(reopened.nodes[0].attempts[0].outcome, None);
        assert_eq!(reopened.nodes[1].attempts[0].outcome, None);
        let mut final_owner = ProcessOwner::new("retry-actor", ledger.clone());
        final_owner.terminal = true;
        let complete = advance(
            &fixture.execution.document,
            &mut final_owner,
            &mut fixture.open(),
        )
        .unwrap();
        assert_eq!(complete.status, GraphStatus::Complete);
        assert!(
            complete
                .checkpoint
                .nodes
                .iter()
                .all(|node| { node.state == NodeState::Succeeded && node.attempts.len() == 1 })
        );
        assert_eq!(ledger.lock().unwrap().starts.len(), 2);
    }
}
