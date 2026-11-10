use super::tests::{call, clock, context};
use super::*;
use provenant_core::{CancelToken, Clock, OperationId, TaskId};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn dispatch_preserves_launch_fields_on_request_reopen() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let placement = provenant_records::run::CloudPlacement {
        target: "verify".into(),
        provider: "fake".into(),
        environment: "env".into(),
        repository: "owner/repo".into(),
        branch: "main".into(),
        model: "model".into(),
        effort: None,
        deadline_seconds: 3600,
        work_branch: None,
    };
    for (name, roots) in [
        ("read", vec![]),
        ("write", vec![temp.path().join("shared")]),
    ] {
        let mut request = submission("run a test");
        request.operation_id = OperationId::parse(name)?;
        request.tasks[0].cloud = Some(placement.clone());
        request.tasks[0].account = Some("account-a".into());
        request.tasks[0].build_heavy = true;
        request.tasks[0].access.write_roots = roots.clone();
        let run = Service::new(&store).dispatch(request, temp.path(), &call)?;
        let reopened = store.request(run.run_id, &call)?;
        let task = &reopened.tasks[0];
        assert_eq!(task.cloud, Some(placement.clone()));
        assert_eq!(task.account.as_deref(), Some("account-a"));
        assert!(task.build_heavy);
        assert_eq!(task.access.write_roots, roots);
    }
    Ok(())
}

#[test]
fn peer_wait_views_keep_terminal_and_show_custody_until_released() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let input = super::tests::input();
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    let custody = attempt.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    store.withdraw(
        id,
        AttemptState::Queued {
            extra: Default::default(),
        },
        Outcome::Cancelled,
        "fixture terminal publication",
        input.created_at,
        &call,
    )?;
    let terminal = store.record(id, &call)?;
    crate::artifacts::seal(
        &attempt,
        "merge-restored.json",
        &crate::db::bytes(&crate::writer::MergeRestored {
            schema_version: 1,
            scope: RunStore::scope(&terminal),
        })?,
        crate::RECORD_LIMIT,
    )?;
    let service = Service::new(&store);
    let query = LaneQuery {
        targets: vec![],
        all: true,
        limit: 10,
        after: 0,
        newest_first: false,
        owner: None,
        unseen_after: None,
    };
    assert!(matches!(
        service.status(id, &call)?.state,
        AttemptState::Terminal { .. }
    ));
    let waiting = service.status_for_wait(id, &call)?;
    assert!(matches!(waiting.state, AttemptState::Terminal { .. }));
    assert_eq!(waiting.custody_pending, Some(true));
    assert_eq!(
        service.lanes_for_wait(&query, &call)?.lanes[0].custody_pending,
        Some(true)
    );
    assert!(matches!(
        &service.lanes(&query, &call)?.lanes[0].state,
        AttemptState::Terminal { reason: Some(reason), .. }
            if reason.contains("merge index, parents and working files were retained")
    ));
    drop(custody);
    let observed = service.status_for_wait(id, &call)?;
    assert!(matches!(observed.state, AttemptState::Terminal { .. }));
    assert_eq!(observed.custody_pending, Some(false));
    assert!(store.custody_released(&observed, &call)?);
    assert!(matches!(
        service.lanes_for_wait(&query, &call)?.lanes[0].state,
        AttemptState::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn dispatch_validates_review_commits() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let git = |args: &[&str]| -> TestResult {
        assert!(
            provenant_os::fixture::git_command()
                .current_dir(&root)
                .args(args)
                .status()?
                .success()
        );
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&["commit", "--allow-empty", "-qm", "test"])?;
    let output = provenant_os::fixture::git_command()
        .current_dir(&root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    assert!(output.status.success());
    let head = String::from_utf8(output.stdout)?.trim().to_owned();
    git(&["branch", "-M", "main"])?;
    git(&[
        "remote",
        "add",
        "origin",
        root.to_str().ok_or("fixture path")?,
    ])?;
    git(&["update-ref", "refs/remotes/origin/main", &head])?;
    git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(&root)?, &call)?;
    let service = Service::new(&store);
    let mut valid = submission("review");
    valid.tasks[0].head = Some(head.clone());
    valid.tasks[0].base = Some(head.clone());
    let run = service.dispatch(valid, &root, &call)?;
    let task = &store.request(run.run_id, &call)?.tasks[0];
    assert_eq!(task.head.as_deref(), Some(head.as_str()));
    assert_eq!(task.base.as_deref(), Some(head.as_str()));
    let mut head_only = submission("head only");
    head_only.operation_id = OperationId::parse("head-only")?;
    head_only.tasks[0].head = Some(head.clone());
    let run = service.dispatch(head_only, &root, &call)?;
    assert_eq!(
        store.request(run.run_id, &call)?.tasks[0].base.as_deref(),
        Some(head.as_str())
    );
    for (head, base) in [
        ("a".repeat(40), None),
        (head.clone(), Some("b".repeat(40))),
        ("HEAD".into(), None),
    ] {
        let mut invalid = submission("review");
        invalid.operation_id = OperationId::parse("invalid-review")?;
        invalid.tasks[0].head = Some(head);
        invalid.tasks[0].base = base;
        assert!(service.dispatch(invalid, &root, &call).is_err());
    }
    Ok(())
}

pub(super) fn submission(prompt: &str) -> DispatchRequest {
    DispatchRequest {
        chair: None,
        operation_id: OperationId::parse("service-dispatch").unwrap(),
        tasks: vec![DispatchTask {
            cloud: None,
            context: None,
            build_heavy: false,
            title: None,
            task_id: TaskId::from_bytes([3; 16]),
            prompt: prompt.into(),
            base: None,
            pinned_base: None,
            head: None,
            source_head: None,
            route: RequestedRoute::Named {
                extra: Default::default(),
                name: "senior".into(),
                effort: None,
            },
            account: None,
            profile: HarnessProfile::default(),
            timeout_seconds: 60,
            delivery_id: None,
            deliver: None,
            worktree: None,
            access: LaneAccess::default(),
        }],
        concurrency: 1,
        lane_depth: 1,
        parent_attempt: None,
    }
}

#[test]
fn adapter_child_leaves_network_unset_so_the_parent_bound_applies() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut input = submission("command-child");
    input.tasks[0].route = RequestedRoute::Adapter {
        extra: Default::default(),
        provider: "command".into(),
        name: "fmt".into(),
    };
    input.parent_attempt = Some(provenant_core::AttemptId::from_bytes([9; 16]));
    input.lane_depth = 2;
    let record = Service::new(&store).dispatch(input, temp.path(), &call)?;
    let request = store.request(record.run_id, &call)?;
    assert_eq!(request.tasks[0].access.network, None);
    Ok(())
}

#[test]
fn child_dispatch_accepts_database_proposal_but_keeps_other_access_restricted() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    for case in ["database", "network", "read", "write"] {
        let mut input = submission(case);
        input.operation_id = OperationId::parse(&format!("child-{case}"))?;
        input.parent_attempt = Some(provenant_core::AttemptId::from_bytes([9; 16]));
        input.lane_depth = 2;
        match case {
            "database" => input.tasks[0].access.database_ipc = true,
            "network" => input.tasks[0].access.network = Some(NetworkAccess::Provider),
            "read" => input.tasks[0]
                .access
                .read_roots
                .push(temp.path().join("read")),
            "write" => input.tasks[0]
                .access
                .write_roots
                .push(temp.path().join("write")),
            _ => unreachable!(),
        }
        let result = Service::new(&store).dispatch(input, temp.path(), &call);
        assert_eq!(result.is_ok(), case == "database", "{case}: {result:?}");
    }
    Ok(())
}

#[test]
fn child_dispatch_replays_original_id_scheme() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let proposal = || {
        let mut input = submission("child");
        input.parent_attempt = Some(provenant_core::AttemptId::from_bytes([9; 16]));
        input.lane_depth = 2;
        input
    };
    let original = Service::new(&store).dispatch(proposal(), temp.path(), &call)?;
    let original_attempt = store.pending_attempts(original.run_id, &call)?[0].clone();
    assert_eq!(
        Service::new(&store)
            .dispatch(proposal(), temp.path(), &call)?
            .run_id,
        original.run_id
    );
    assert_eq!(
        store.pending_attempts(original.run_id, &call)?[0].attempt_id,
        original_attempt.attempt_id
    );
    let legacy_dir = temp.path().join("legacy");
    std::fs::create_dir(&legacy_dir)?;
    let legacy = RunStore::open(&context(&legacy_dir)?, &call)?;
    let mut request = store.request(original.run_id, &call)?;
    request.child_attempt_id_version = None;
    let accepted = legacy.create(
        &PreparedRun {
            run_id: original.run_id,
            operation_id: original.operation_id,
            request,
            attempts: vec![NewAttempt {
                attempt_id: original_attempt.attempt_id,
                task_id: original_attempt.task_id,
                prompt: "child".into(),
            }],
            created_at: original.created_at,
        },
        &call,
    )?;
    assert_eq!(
        Service::new(&legacy)
            .dispatch(proposal(), temp.path(), &call)?
            .run_id,
        accepted.run_id
    );
    assert_eq!(
        legacy
            .request(accepted.run_id, &call)?
            .child_attempt_id_version,
        None
    );
    Ok(())
}

#[test]
fn chair_binding_replays_the_original_submission() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut first = submission("review this code");
    first.chair = Some(ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([4; 16]),
        actor: provenant_core::ActorId::from_bytes([5; 16]),
    });
    let chair = first.chair.clone();
    let run = Service::new(&store).dispatch(first, temp.path(), &call)?;
    // A later harness identity does not reassign an accepted operation.
    let replay =
        Service::new(&store).dispatch(submission("review this code"), temp.path(), &call)?;
    assert_eq!(run.run_id, replay.run_id);
    assert_eq!(store.request(run.run_id, &call)?.chair, chair);
    Ok(())
}

#[test]
fn task_title_roundtrip() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let mut input = submission("review this code");
    input.tasks[0].title = Some("Review changes".into());
    let run = Service::new(&store).dispatch(input, temp.path(), &call)?;
    let attempt = store.request(run.run_id, &call)?.tasks[0].task_id;
    drop(store);
    let store = RunStore::open(&context, &call)?;
    let request = store.request(run.run_id, &call)?;
    assert_eq!(request.tasks[0].title.as_deref(), Some("Review changes"));
    let page = Service::new(&store).lanes(
        &LaneQuery {
            targets: vec![],
            all: true,
            limit: 10,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        },
        &call,
    )?;
    assert_eq!(page.lanes[0].task_id, attempt);
    assert_eq!(page.lanes[0].title.as_deref(), Some("Review changes"));
    for title in [
        "x".repeat(257),
        "line\nbreak".into(),
        "tab\there".into(),
        "escape\u{1b}".into(),
        "unicode\u{85}".into(),
    ] {
        let mut invalid = submission("review this code");
        invalid.operation_id = OperationId::parse("invalid-title")?;
        invalid.tasks[0].title = Some(title);
        let error = Service::new(&store)
            .dispatch(invalid, temp.path(), &call)
            .unwrap_err();
        assert!(matches!(error, RunError::InvalidTaskTitle(_)));
        assert_eq!(error.code(), "INVALID");
    }
    Ok(())
}

#[test]
fn dispatch_replay_output_and_cancel_share_the_service() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let service = Service::new(&store);
    let run = service.dispatch(submission("review this code"), temp.path(), &call)?;
    let replay = service.dispatch(submission("review this code"), temp.path(), &call)?;
    assert_eq!(run.run_id, replay.run_id);
    let page = service.lanes(
        &LaneQuery {
            targets: vec![],
            all: true,
            limit: 10,
            after: 0,
            newest_first: false,
            owner: None,
            unseen_after: None,
        },
        &call,
    )?;
    assert_eq!(page.lanes.len(), 1);
    let id = page.lanes[0].attempt_id;
    assert_eq!(
        service
            .output(id, ArtifactPart::Prompt, ByteRange::default(), &call)?
            .bytes,
        b"review this code"
    );
    let operation = OperationId::parse("service-cancel")?;
    service.bind_cancel(&operation, &id.to_string(), &call)?;
    service.cancel(id, "stop", &call)?;
    assert!(matches!(
        service.bind_cancel(&operation, "another target", &call),
        Err(RunError::OperationConflict)
    ));
    assert!(matches!(
        service.status(id, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    assert!(matches!(
        service.dispatch(submission("changed prompt"), temp.path(), &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}

#[test]
fn secret_input_is_refused_before_publication() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let secret = "sk-abcdefghijklmnopqrstuvwxyz123456";
    let input = submission(secret);
    let operation = input.operation_id.clone();
    let error = Service::new(&store)
        .dispatch(input, temp.path(), &call)
        .unwrap_err();
    assert!(matches!(
        error,
        RunError::RecordedRefusal(ref diagnostic) if diagnostic.code == ErrorKind::SecretInput.code()
    ));
    let saved = store
        .operation(&operation, &call)?
        .ok_or("missing refusal")?;
    assert!(!serde_json::to_string(&saved)?.contains(secret));
    assert!(matches!(
        Service::new(&store).dispatch(submission(secret), temp.path(), &call),
        Err(RunError::RecordedRefusal(_))
    ));
    assert!(matches!(
        Service::new(&store).dispatch(submission("changed"), temp.path(), &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}

#[test]
fn later_secret_is_refused_before_preparing_any_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut input = submission("ordinary first task");
    // A missing writer would fail if preparation ran before the later scan.
    input.tasks[0].worktree = Some(temp.path().join("missing-writer"));
    let mut later = submission("sk-abcdefghijklmnopqrstuvwxyz123456")
        .tasks
        .remove(0);
    later.task_id = TaskId::from_bytes([4; 16]);
    input.tasks.push(later);
    let operation = input.operation_id.clone();
    assert!(matches!(
        Service::new(&store).dispatch(input, temp.path(), &call),
        Err(RunError::RecordedRefusal(ref diagnostic)) if diagnostic.code == ErrorKind::SecretInput.code()
    ));
    assert!(store.operation(&operation, &call)?.is_some());
    Ok(())
}

#[test]
fn named_adapter_routes_preserve_configuration_name_without_a_model() -> TestResult {
    let route: RequestedRoute = serde_json::from_value(serde_json::json!({
        "kind": "adapter", "provider": "command", "name": "fmt-check",
        "future_context": {"source": "project"}
    }))?;
    let normalised = Service::route(route.clone())?;
    assert_eq!(normalised, route);
    let projection = normalised.projection();
    assert_eq!(projection.provider.as_deref(), Some("command"));
    assert!(projection.model.is_none());
    assert!(projection.effort.is_none());
    for field in ["model", "effort"] {
        let mut value = serde_json::to_value(&route)?;
        value[field] = "unavailable".into();
        assert!(matches!(
            Service::route(serde_json::from_value(value)?),
            Err(RunError::Input {
                kind: ErrorKind::Invalid,
                ..
            })
        ));
    }
    Ok(())
}

#[test]
fn command_write_is_refused_before_preparing_a_worktree() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut input = submission("run the configured command");
    let operation = input.operation_id.clone();
    input.tasks[0].route = RequestedRoute::Adapter {
        extra: Default::default(),
        provider: "command".into(),
        name: "fmt-check".into(),
    };
    input.tasks[0].worktree = Some(temp.path().join("not-a-worktree"));
    let error = Service::new(&store)
        .dispatch(input, temp.path(), &call)
        .unwrap_err();
    assert_eq!(ErrorKind::from(&error), ErrorKind::Unsupported);
    assert!(
        matches!(&error, RunError::RecordedRefusal(diagnostic) if diagnostic.code == ErrorKind::Unsupported.code())
    );
    let saved = store
        .operation(&operation, &call)?
        .ok_or("refusal receipt missing")?;
    assert_eq!(saved.state, OperationState::Refused);
    assert_eq!(
        saved
            .refusal
            .as_ref()
            .map(|diagnostic| diagnostic.code.as_str()),
        Some(ErrorKind::Unsupported.code())
    );
    assert!(!store.has_runs(&call)?);
    assert!(!temp.path().join("not-a-worktree").exists());
    Ok(())
}

#[test]
fn model_routes_are_canonical_before_replay_comparison() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let service = Service::new(&store);
    let model = |id: &str| RequestedRoute::Model {
        extra: Default::default(),
        provider: "fixture".into(),
        model: id.into(),
        effort: None,
    };
    let mut input = submission("review this code");
    input.tasks[0].route = model("model");
    let run = service.dispatch(input, temp.path(), &call)?;
    let mut input = submission("review this code");
    input.tasks[0].route = model("fixture/model");
    assert_eq!(
        service.dispatch(input, temp.path(), &call)?.run_id,
        run.run_id
    );
    assert_eq!(
        store.request(run.run_id, &call)?.tasks[0].route,
        model("fixture/model")
    );
    assert!(matches!(
        Service::route(model("other/model")),
        Err(RunError::Input {
            kind: ErrorKind::Invalid,
            ..
        })
    ));
    Ok(())
}

#[test]
fn unsupported_launch_is_refused_before_any_run_exists() -> TestResult {
    let model = |provider: &str, effort: Option<&str>| RequestedRoute::Model {
        extra: Default::default(),
        provider: provider.into(),
        model: format!("{provider}/m"),
        effort: effort.map(str::to_owned),
    };
    // (provider, effort, accepted): the effort column of the adapter table.
    let cases = [
        ("claude", Some("high"), true),
        ("codex", Some("high"), true),
        ("agy", Some("high"), true),
        ("opencode", Some("high"), true),
        ("cursor", Some("high"), false),
        ("cursor", None, true),
    ];
    for (provider, effort, accepted) in cases {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = submission("capabilities");
        let operation = input.operation_id.clone();
        input.tasks[0].route = model(provider, effort);
        let result = Service::new(&store).dispatch(input, temp.path(), &call);
        let recorded = store.operation(&operation, &call)?.is_some();
        assert_eq!(result.is_ok(), accepted, "{provider} {effort:?}");
        assert!(recorded, "{provider} {effort:?} left no operation");
        if let Err(error) = result {
            assert_eq!(
                error.to_string(),
                format!("{provider} does not support effort")
            );
        }
    }
    Ok(())
}

#[test]
fn preflight_refusal_replays_and_rejects_changed_inputs() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let make = |prompt| {
        let mut input = submission(prompt);
        input.chair = Some(ChairBinding {
            extra: Default::default(),
            host: provenant_core::HostId::from_bytes([7; 16]),
            actor: provenant_core::ActorId::from_bytes([8; 16]),
            session: Some("original".into()),
        });
        input.tasks[0]
            .profile
            .knobs
            .insert("chair.refusal_notice".into(), serde_json::json!("off"));
        input.tasks[0].route = RequestedRoute::Model {
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/m".into(),
            effort: Some("high".into()),
        };
        input
    };
    let first = Service::new(&store)
        .dispatch(make("first"), temp.path(), &call)
        .unwrap_err();
    let mut replay = make("first");
    replay.chair.as_mut().unwrap().session = Some("new-session".into());
    let second = Service::new(&store)
        .dispatch(replay, temp.path(), &call)
        .unwrap_err();
    assert_eq!(first.to_string(), second.to_string());
    assert!(
        matches!(second, RunError::RecordedRefusal(ref diagnostic) if diagnostic.code == "UNSUPPORTED")
    );
    assert!(matches!(
        Service::new(&store).dispatch(make("changed"), temp.path(), &call),
        Err(RunError::OperationConflict)
    ));
    assert_eq!(store.recent_refusals(10, &call)?.len(), 1);
    let saved = store.recent_refusals(10, &call)?.remove(0);
    assert_eq!(
        saved
            .refusal_chair
            .as_ref()
            .and_then(|chair| chair.session.as_deref()),
        Some("original")
    );
    assert_eq!(saved.extra["refusal_notice"], "off");
    assert!(!serde_json::to_string(&saved)?.contains("first"));
    Ok(())
}

#[test]
fn refusal_only_chair_leaves_accepted_identity_and_settlement_unbound() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let chair = ChairBinding {
        extra: Default::default(),
        host: provenant_core::HostId::from_bytes([7; 16]),
        actor: provenant_core::ActorId::from_bytes([8; 16]),
        session: Some("chair-session".into()),
    };
    let first = Service::new(&store).dispatch(submission("accepted"), temp.path(), &call)?;
    let original = store.request(first.run_id, &call)?;
    let repeated = Service::new(&store)
        .with_refusal_context(RefusalContext {
            chair: Some(chair.clone()),
            intent_digest: Some(provenant_core::Digest::from_bytes([6; 32])),
        })
        .dispatch(submission("accepted"), temp.path(), &call)?;
    assert_eq!(repeated, first);
    assert_eq!(store.request(first.run_id, &call)?, original);
    assert!(original.chair.is_none());
    // A fresh accepted operation keeps the same canonical request even when
    // the host supplies a destination reserved for a possible refusal.
    let mut input = submission("accepted");
    input.operation_id = OperationId::parse("accepted-with-refusal-chair")?;
    let accepted = Service::new(&store)
        .with_refusal_context(RefusalContext {
            chair: Some(chair.clone()),
            intent_digest: Some(provenant_core::Digest::from_bytes([6; 32])),
        })
        .dispatch(input, temp.path(), &call)?;
    assert_eq!(accepted.request_digest, first.request_digest);
    assert!(store.request(accepted.run_id, &call)?.chair.is_none());
    assert!(
        !store
            .operation(&accepted.operation_id, &call)?
            .ok_or("accepted operation")?
            .extra
            .contains_key("origin_intent_digest")
    );
    for nested in [false, true] {
        let make = || {
            let mut input = submission("refused");
            input.operation_id = OperationId::parse(if nested {
                "nested-refusal-chair"
            } else {
                "refusal-chair"
            })
            .unwrap();
            input.parent_attempt = nested.then_some(provenant_core::AttemptId::from_bytes([9; 16]));
            input.tasks[0].route = RequestedRoute::Model {
                extra: Default::default(),
                provider: "cursor".into(),
                model: "cursor/m".into(),
                effort: Some("high".into()),
            };
            input
        };
        let input = make();
        let operation = input.operation_id.clone();
        for _ in 0..2 {
            assert!(matches!(
                Service::new(&store)
                    .with_refusal_context(RefusalContext {
                        chair: Some(chair.clone()),
                        intent_digest: Some(provenant_core::Digest::from_bytes([6; 32]))
                    })
                    .dispatch(make(), temp.path(), &call),
                Err(RunError::RecordedRefusal(_))
            ));
        }
        let saved = store.operation(&operation, &call)?.ok_or("refusal")?;
        assert_eq!(saved.refusal_chair, Some(chair.clone()));
        assert_eq!(
            saved.extra["origin_intent_digest"],
            serde_json::json!(provenant_core::Digest::from_bytes([6; 32]))
        );
    }
    Ok(())
}

#[test]
fn nested_preflight_refusal_replays_and_rejects_changed_inputs() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let make = |prompt| {
        let mut input = submission(prompt);
        input.parent_attempt = Some(provenant_core::AttemptId::from_bytes([9; 16]));
        input.lane_depth = 2;
        input.tasks[0].route = RequestedRoute::Model {
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/m".into(),
            effort: Some("high".into()),
        };
        input
    };
    let operation = make("first").operation_id;
    let first = Service::new(&store)
        .dispatch(make("first"), temp.path(), &call)
        .unwrap_err();
    let saved = store
        .operation(&operation, &call)?
        .ok_or("missing refusal")?;
    assert!(matches!(first, RunError::RecordedRefusal(_)));
    assert!(matches!(
        Service::new(&store).dispatch(make("first"), temp.path(), &call),
        Err(RunError::RecordedRefusal(_))
    ));
    assert_eq!(
        store.operation(&operation, &call)?.unwrap().run_id,
        saved.run_id
    );
    assert!(matches!(
        Service::new(&store).dispatch(make("changed"), temp.path(), &call),
        Err(RunError::OperationConflict)
    ));
    Ok(())
}

#[test]
fn nested_secret_refusal_does_not_store_prompt() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let secret = concat!("sk-", "abcdefghijklmnopqrstuvwxyz123456");
    let mut input = submission(secret);
    input.parent_attempt = Some(provenant_core::AttemptId::from_bytes([9; 16]));
    input.lane_depth = 2;
    let operation = input.operation_id.clone();
    let result = Service::new(&store).dispatch(input, temp.path(), &call);
    assert!(
        matches!(result, Err(RunError::RecordedRefusal(_))),
        "{result:?}"
    );
    let saved = store
        .operation(&operation, &call)?
        .ok_or("missing refusal")?;
    assert!(!serde_json::to_string(&saved)?.contains(secret));
    Ok(())
}

#[test]
fn explicit_skill_location_is_rechecked_before_dispatch() -> TestResult {
    for (provider, required, accepted) in [
        ("command", false, true),
        ("command", true, false),
        ("codex", true, true),
    ] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = submission("skill location");
        let operation = input.operation_id.clone();
        input.tasks[0].profile.require_skill_location = required;
        input.tasks[0].route = if provider == "command" {
            RequestedRoute::Adapter {
                extra: Default::default(),
                provider: provider.into(),
                name: "fixture".into(),
            }
        } else {
            RequestedRoute::Model {
                extra: Default::default(),
                provider: provider.into(),
                model: "codex/m".into(),
                effort: None,
            }
        };
        let result = Service::new(&store).dispatch(input, temp.path(), &call);
        assert_eq!(
            result.is_ok(),
            accepted,
            "{provider} required={required}: {result:?}"
        );
        assert!(store.operation(&operation, &call)?.is_some());
        if let Err(error) = result {
            assert_eq!(error.to_string(), "command does not support skill location");
        }
    }
    Ok(())
}

#[test]
fn every_task_is_checked_before_any_writer_is_prepared() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut input = submission("two tasks");
    let operation = input.operation_id.clone();
    // The first task names a writer path that cannot resolve, so reaching its
    // preparation would fail with a different error than the second task's.
    input.tasks[0].worktree = Some(temp.path().join("no-such-writer"));
    let mut second = submission("second").tasks.remove(0);
    second.task_id = TaskId::from_bytes([4; 16]);
    second.worktree = None;
    second.route = RequestedRoute::Model {
        extra: Default::default(),
        provider: "cursor".into(),
        model: "cursor/m".into(),
        effort: Some("high".into()),
    };
    input.tasks.push(second);
    let error = Service::new(&store)
        .dispatch(input, temp.path(), &call)
        .unwrap_err();
    assert_eq!(error.to_string(), "cursor does not support effort");
    assert_eq!(
        store
            .operation(&operation, &call)?
            .ok_or("missing operation")?
            .state,
        OperationState::Refused
    );
    Ok(())
}

#[test]
fn each_refusal_has_its_own_stable_code() {
    use provenant_confinement::{permissions::PermissionError, worktrees::WorktreeError};
    for (error, code) in [
        (
            RunError::Writer(WorktreeError::InvalidRequest),
            "WRITER_SCOPE",
        ),
        (
            RunError::WriterProjectMismatch {
                worktree: "/project/.worktrees/writer".into(),
                project: "/other".into(),
                selected: "/project".into(),
            },
            "WRITER_PROJECT_MISMATCH",
        ),
        (
            RunError::Confinement(PermissionError::BroadRoot),
            "CONFINEMENT",
        ),
        (RunError::Refused, "REFUSED"),
        (RunError::GitUnavailable, "GIT"),
        (
            RunError::Admission(AdmissionError::Disk {
                free: 1,
                floor: 2,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            }),
            "DISK_FLOOR",
        ),
        (
            RunError::Admission(AdmissionError::MemoryPressure {
                level: provenant_records::diagnostic::PressureLevel::Critical,
                threshold: provenant_records::diagnostic::PressureThreshold::Critical,
                origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
            }),
            "MEMORY_PRESSURE",
        ),
        (
            RunError::Admission(AdmissionError::LaneCap {
                active: 3,
                limit: 3,
                kind: provenant_records::diagnostic::LaneLimit::HostWriters,
                origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
            }),
            "LANE_CAP",
        ),
        (
            RunError::Admission(AdmissionError::BuildSettings),
            "BUILD_SETTINGS",
        ),
        (
            RunError::Admission(AdmissionError::GuardianIdentity),
            "GUARDIAN_IDENTITY",
        ),
        (RunError::Admission(AdmissionError::Clock), "HOST_CLOCK"),
        (
            RunError::ObjectImportLimit("fixture"),
            "OBJECT_IMPORT_LIMIT",
        ),
    ] {
        assert_eq!(error.code(), code, "{error:?}");
    }
    let mut codes: Vec<_> = ErrorKind::ALL.iter().map(|kind| kind.code()).collect();
    let listed = codes.len();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), listed, "every code is distinct");
}

#[test]
fn work_context_never_widens_authority() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let service = Service::new(&store);
    let plain = service.dispatch(submission("same"), temp.path(), &call)?;
    let mut labelled = submission("same");
    labelled.operation_id = OperationId::parse("service-dispatch-labelled")?;
    // Every label, including ones that read like grants, is descriptive only.
    labelled.tasks[0].context = Some(provenant_records::run::WorkContext {
        work_item: Some("#1".into()),
        graph_id: Some("graph".into()),
        node_id: Some("node".into()),
        preset: Some("deliver".into()),
        pattern: Some("deliver".into()),
        role: Some("writer".into()),
        role_digest: Some("write-everything".into()),
        experiment_arm: Some("network=all".into()),
        selection_source: Some("delivery=merge".into()),
        ..Default::default()
    });
    let labelled = service.dispatch(labelled, temp.path(), &call)?;
    let (plain, labelled) = (
        store.request(plain.run_id, &call)?.tasks.remove(0),
        store.request(labelled.run_id, &call)?.tasks.remove(0),
    );
    assert!(plain.context.is_none() && labelled.context.is_some());
    assert_eq!(plain.authority_digest, labelled.authority_digest);
    let strip = |mut task: TaskRequest| {
        task.context = None;
        task
    };
    // Access, write scope, source and delivery are untouched.
    assert_eq!(strip(plain), strip(labelled));
    Ok(())
}

#[test]
fn unknown_work_context_fields_are_refused_at_the_request_edge() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let mut request = submission("unknown");
    let mut unknown = provenant_records::run::WorkContext::default();
    unknown
        .extra
        .insert("write_roots".into(), serde_json::json!(["/"]));
    request.tasks[0].context = Some(unknown);
    assert!(matches!(
        Service::new(&store).dispatch(request, temp.path(), &call),
        Err(RunError::Validation("work context"))
    ));
    Ok(())
}

#[test]
fn legacy_writer_replay_preserves_sealed_request_and_authority() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["worktree", "add", "-qb", "writer", ".worktrees/writer"],
    ] {
        assert!(
            provenant_os::fixture::git_command()
                .current_dir(&root)
                .args(args)
                .status()?
                .success()
        );
    }
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let mut context = context(&root)?;
    std::fs::create_dir_all(context.state_root.as_path())?;
    std::fs::set_permissions(
        context.state_root.as_path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    let mut roots = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    roots.state_dir = Some(context.state_root.as_path().to_owned());
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(root.join(".git"))?,
    )?;
    context.project_id = binding.project_id();
    let store = RunStore::open(&context, &call)?;
    let head = String::from_utf8(
        provenant_os::fixture::git_command()
            .current_dir(&root)
            .args(["rev-parse", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_owned();
    let make = || {
        let mut input = submission("writer task");
        input.tasks[0].worktree = Some(root.join(".worktrees/writer"));
        input.tasks[0].head = Some(head.clone());
        input.tasks[0].source_head = Some(head.clone());
        input.tasks[0].base = Some(head.clone());
        input.tasks[0].context = Some(WorkContext {
            graph_id: Some("legacy-graph".into()),
            node_id: Some("writer".into()),
            ..Default::default()
        });
        input
    };
    let run = Service::new(&store).dispatch(make(), &root, &call)?;
    let mut legacy = store.request(run.run_id, &call)?;
    let task = &mut legacy.tasks[0];
    let writer = task.writer.as_mut().ok_or("writer")?;
    assert_eq!(
        writer.extra.remove("custody_project"),
        Some(serde_json::json!(context.project_id))
    );
    task.authority_digest = provenant_records::CanonicalV1::record_digest(
        "run-authority",
        2,
        &serde_json::json!({"writer":writer,"cwd":root}),
    )?;
    let task_id = task.task_id;
    let prepared = PreparedRun {
        run_id: provenant_core::RunId::from_bytes([91; 16]),
        operation_id: OperationId::parse("legacy-writer")?,
        request: legacy.clone(),
        attempts: vec![NewAttempt {
            attempt_id: provenant_core::AttemptId::from_bytes([92; 16]),
            task_id,
            prompt: "writer task".into(),
        }],
        created_at: clock.wall_now()?,
    };
    store.create(&prepared, &call)?;
    let saved = store.run(prepared.run_id, &call)?;
    let mut replay = make();
    replay.operation_id = prepared.operation_id.clone();
    let reopened = Service::new(&store).dispatch(replay, &root, &call)?;
    assert_eq!(reopened.run_id, saved.run_id);
    assert_eq!(reopened.request_digest, saved.request_digest);
    assert_eq!(store.request(saved.run_id, &call)?, legacy);
    store.refuse_queued(saved.run_id, "fixture terminal predecessor", &call)?;
    let mut retry = make();
    retry.operation_id = OperationId::parse("legacy-graph-retry")?;
    retry.tasks[0]
        .context
        .as_mut()
        .ok_or("context")?
        .writer_predecessor = Some(prepared.attempts[0].attempt_id);
    let retried = Service::new(&store).dispatch(retry, &root, &call)?;
    let retry_request = store.request(retried.run_id, &call)?;
    let retry_task = &retry_request.tasks[0];
    let launch = prepare_writer(
        retry_task.writer.as_ref().ok_or("retry writer")?,
        context.state_root.as_path(),
        context.project_id,
    )?;
    let pending = store.pending_attempts(retried.run_id, &call)?.remove(0);
    store.check_launch_writer_lineage(&pending, retry_task, &launch.worktree, &call)?;
    assert_eq!(store.request(saved.run_id, &call)?, legacy);
    // Legacy predecessor identity defaults only custody to attribution. Paths,
    // other extension fields and a foreign custody namespace remain fenced.
    let saved_writer = legacy.tasks[0].writer.as_ref().ok_or("legacy writer")?;
    let mut resolved = resolve_writer(context.state_root.as_path(), &root, &saved_writer.worktree)?;
    assert!(equivalent_writer(
        saved_writer,
        &resolved,
        context.project_id
    ));
    resolved.extra.insert(
        "custody_project".into(),
        serde_json::json!(provenant_core::ProjectId::from_bytes([99; 16])),
    );
    assert!(!equivalent_writer(
        saved_writer,
        &resolved,
        context.project_id
    ));
    resolved = saved_writer.clone();
    resolved.extra.insert("unknown".into(), true.into());
    assert!(!equivalent_writer(
        saved_writer,
        &resolved,
        context.project_id
    ));
    resolved = saved_writer.clone();
    resolved.worktree = root.join("other");
    assert!(!equivalent_writer(
        saved_writer,
        &resolved,
        context.project_id
    ));
    Ok(())
}

#[test]
fn route_normalisation_preserves_required_family() -> TestResult {
    let invalid: RequestedRoute = serde_json::from_value(
        serde_json::json!({"kind":"named", "name":"senior", "effort":null, "required_family":42}),
    )?;
    assert!(Service::route(invalid).is_err());
    for value in [
        serde_json::json!({"kind":"named", "name":"senior", "effort":null, "required_family":"openai"}),
        serde_json::json!({"kind":"model", "provider":"codex", "model":"codex/gpt-5", "effort":null, "required_family":"openai"}),
        serde_json::json!({"kind":"review", "name":"senior", "effort":null, "excluded_families":[], "distinct_families":true, "required_family":"openai"}),
    ] {
        let route: RequestedRoute = serde_json::from_value(value)?;
        assert_eq!(Service::route(route.clone())?, route);
    }
    Ok(())
}
