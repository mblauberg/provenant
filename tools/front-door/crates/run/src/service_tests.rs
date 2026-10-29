use super::tests::{call, clock, context};
use super::*;
use provenant_core::{CancelToken, OperationId, TaskId};

type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn peer_wait_views_remain_settling_until_terminal_custody_is_released() -> TestResult {
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
    assert_eq!(
        service.status_for_wait(id, &call)?.state,
        AttemptState::Settling {
            publication_pending: false,
            extra: Default::default()
        }
    );
    assert_eq!(
        service.lanes_for_wait(&query, &call)?.lanes[0].state,
        AttemptState::Settling {
            publication_pending: false,
            extra: Default::default()
        }
    );
    drop(custody);
    let observed = service.status_for_wait(id, &call)?;
    assert!(matches!(observed.state, AttemptState::Terminal { .. }));
    assert!(store.custody_released(&observed, &call)?);
    assert!(matches!(
        service.lanes_for_wait(&query, &call)?.lanes[0].state,
        AttemptState::Terminal { .. }
    ));
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)]
fn dispatch_validates_review_commits() -> TestResult {
    use std::process::Command;
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let git = |args: &[&str]| -> TestResult {
        assert!(
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?
                .success()
        );
        Ok(())
    };
    git(&["init", "-q"])?;
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.org",
        "commit",
        "--allow-empty",
        "-qm",
        "test",
    ])?;
    let output = Command::new("git")
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
            context: None,
            build_heavy: false,
            title: None,
            task_id: TaskId::from_bytes([3; 16]),
            prompt: prompt.into(),
            base: None,
            pinned_base: None,
            head: None,
            route: RequestedRoute::Named {
                extra: Default::default(),
                name: "senior".into(),
                effort: None,
            },
            account: None,
            fallback: vec![],
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
    // A pre-upgrade crash may reserve rows without publishing request.json.
    let mut operation = legacy
        .operation(&accepted.operation_id, &call)?
        .ok_or("operation")?;
    operation.state = OperationState::Prepared;
    legacy.db.execute(
        "UPDATE operations SET record=?1",
        [crate::db::bytes(&operation)?],
    )?;
    std::fs::remove_file(
        legacy
            .context
            .state_root
            .as_path()
            .join("runs")
            .join(accepted.run_id.to_string())
            .join("request.json"),
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
        assert!(matches!(
            error,
            RunError::Input {
                kind: ErrorKind::Invalid,
                ..
            }
        ));
        assert!(error.to_string().contains("control characters"));
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
    let input = submission("sk-abcdefghijklmnopqrstuvwxyz123456");
    let operation = input.operation_id.clone();
    let error = Service::new(&store)
        .dispatch(input, temp.path(), &call)
        .unwrap_err();
    assert!(matches!(
        error,
        RunError::Input {
            kind: ErrorKind::SecretInput,
            ..
        }
    ));
    assert!(store.operation(&operation, &call)?.is_none());
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
    assert!(matches!(
        Service::new(&store).dispatch(input, temp.path(), &call),
        Err(RunError::Input {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    assert!(store.operation(&operation, &call)?.is_none());
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
        assert_eq!(
            recorded, accepted,
            "{provider} {effort:?} left an operation"
        );
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
    assert!(store.operation(&operation, &call)?.is_none());
    Ok(())
}
