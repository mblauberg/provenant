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
            review_base: None,
            extra: Default::default(),
            integration_base: None,
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
fn bound_attempt_manifest_survives_file_journal_reopen_and_reattachment() {
    type Observation = (AttemptKey, Option<String>, Option<String>, Option<String>);
    #[derive(Default)]
    struct RecordingOwner {
        observed: Vec<Observation>,
    }
    impl AttemptOwner for RecordingOwner {
        type Error = Infallible;
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
        directory: base_dir.clone(),
        document: document.clone(),
        review_base: None,
        integration_base: None,
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
        directory: directory.clone(),
        document: document.clone(),
        review_base: None,
        integration_base: None,
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
