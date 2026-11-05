use super::*;
use provenant_core::{Clock, ProjectId};
use provenant_os::{clock::SystemClock, fs::SystemPathResolver};
use std::{convert::Infallible, os::unix::fs::PermissionsExt};
struct Owner;
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
