use super::*;
use crate::Mode;

fn reexec_test_in_lane(name: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if !crate::fabric::in_lane() {
        return Ok(false);
    }
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsStr;

    let argv = vec![
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        name.into(),
    ];
    let removed = [
        OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
        OsStr::new(run::LANE_DEPTH_ENV),
        OsStr::new(run::PARENT_ATTEMPT_ENV),
    ];
    let env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !removed.contains(&key.as_os_str()))
        .collect();
    let cwd = std::env::current_dir()?;
    let timeout = match name {
        "run::tests::wait_handles_a_stale_default_cursor" => 300,
        "run::tests::dispatch_reuses_only_free_lane_or_unowned_worktree" => 90,
        _ => 30,
    };
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(timeout));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65_536 };
    options.stderr = StreamPolicy::Capture { limit: 65_536 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
    Ok(true)
}

#[test]
fn cancel_wait_preserves_silence_as_non_success_without_timeout() {
    let wait = json!({
        "lanes": [{"state":"running"}],
        "condition_met": false,
        "timed_out": false,
        "reason": "provider silent; lane attempt-1 is still running"
    });
    let mut data = json!({});
    let mut status = Status::Ok;
    apply_cancel_wait(&mut data, &wait, false, &mut status);
    assert_eq!(status, Status::Failed);
    assert_eq!(data["timed_out"], false);
    assert_eq!(data["condition_met"], false);
    assert_eq!(data["reason"], wait["reason"]);
    assert_eq!(data["lanes"], wait["lanes"]);
}

/// A project with a linked worktree `writer` and a `nested` directory in it.
/// The fixture's Git runs without the lane's `GIT_*` variables.
struct LinkedFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    tree: PathBuf,
}

fn linked_fixture() -> Result<LinkedFixture, Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    let tree = root.join("writer");
    std::fs::create_dir_all(&project)?;
    let executor = SystemExecutor::new(1)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
        .collect();
    env.extend(provenant_os::fixture::fixture_git_settings());
    for args in [
        vec!["init", "-q", "-b", "fixture-base"],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec![
            "worktree",
            "add",
            "-qb",
            "writer",
            tree.to_str().ok_or("path")?,
        ],
    ] {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.into_iter().map(std::ffi::OsString::from))
            .collect();
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(10));
        options.env = Some(&env);
        let output = executor.execute(options)?;
        assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
    }
    std::fs::create_dir(tree.join("nested"))?;
    Ok(LinkedFixture {
        _temp: temp,
        root,
        project,
        tree,
    })
}

/// Runs `check` with the edge that `run` would see from `cwd`.
fn with_edge<T>(
    root: &Path,
    cwd: &Path,
    check: impl FnOnce(&Edge<'_>) -> T,
) -> Result<T, Box<dyn std::error::Error>> {
    use provenant_core::RootInputs;
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home)?;
    let mut inputs = RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    Ok(check(&Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd,
    }))
}

#[test]
fn lane_gitdir_resolves_the_registered_project_without_querying_primary()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane(
        "run::tests::lane_gitdir_resolves_the_registered_project_without_querying_primary",
    )? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let context = crate::fabric::Context::open(edge.roots.clone(), edge.home, edge.cwd)?;
            let gitdir = String::from_utf8(provenant_os::fixture::run_git(
                &fixture.tree,
                &["rev-parse", "--absolute-git-dir"],
            )?)?;
            let gitdir = PathBuf::from(gitdir.trim());
            assert_eq!(
                super::launch::project_id_for_lane_gitdir(edge.roots, &gitdir),
                Some(context.identity.project)
            );
            assert_eq!(
                super::launch::project_id_for_lane_gitdir(
                    edge.roots,
                    &fixture.project.join(".git")
                ),
                Some(context.identity.project)
            );
            assert_eq!(
                super::launch::project_id_for_lane_gitdir(
                    edge.roots,
                    &fixture.root.join("unrelated")
                ),
                None
            );
            Ok(())
        },
    )??;
    Ok(())
}

#[test]
fn graph_quality_reports_missing_checkpoint_without_failing()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, ProjectId, TaskId, WallTime};
    use provenant_records::{AttemptFact, AttemptOutcome, Usage};
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("project"));
    inputs.state_dir = Some(base.clone());
    let root = provenant_core::StateRoot::resolve(&inputs, &provenant_os::fs::SystemPathResolver)?;
    let project = ProjectId::from_bytes([3; 16]);
    let fact = AttemptFact {
        context: Some(provenant_records::run::WorkContext {
            graph_id: Some("retired".into()),
            ..Default::default()
        }),
        attempt_id: AttemptId::from_bytes([1; 16]),
        task_id: TaskId::from_bytes([2; 16]),
        attempt_number: 1,
        operation_id: OperationId::parse("quality-test")?,
        project_id: project,
        terminal_state: AttemptOutcome::Succeeded,
        exit_code: Some(0),
        queued_at: WallTime::from_micros(0)?,
        started_at: None,
        provider_ms: None,
        finished_at: WallTime::from_micros(1)?,
        resolved_route: None,
        usage: Usage::default(),
        reviewer: None,
        delivery_id: None,
        subject: None,
        head: None,
        output_refs: vec![],
        requested_route: None,
        applied_route: None,
        observed_route: None,
        verified_owner: None,
        lineage: vec![],
        tier: None,
        template: None,
        parent_node: None,
        slot_wait_ms: None,
        reason: None,
        trace: Default::default(),
        transcript: None,
    };
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let rows =
        graph_quality_rows(&[&fact], None, &call, &root, project).map_err(|_| "quality report")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["graph_id"], "retired");
    assert_eq!(rows[0]["checkpoint"], "missing");
    Ok(())
}

#[test]
fn graph_quality_counts_only_executed_repair_rounds_and_leaf_findings() {
    use provenant_fabric_runtime::graph::NodeProgress;
    let node = |value| serde_json::from_value::<NodeProgress>(value).unwrap();
    let findings = json!({
        "schema_version": 1,
        "findings": [{"id":"F1", "severity":"P2", "file":"a.rs", "line":1, "summary":"finding", "status":"open"}],
        "warnings": []
    });
    let checkpoint_nodes = vec![
        node(
            json!({"id":"composite", "state":"succeeded", "children":["child"], "attempts":[], "outputs":findings}),
        ),
        node(json!({"id":"child", "state":"succeeded", "attempts":[], "outputs":findings})),
        node(
            json!({"id":"initial", "state":"succeeded", "attempts":[], "round":{"loop_id":"loop","round":1,"node_id":"initial","until":false}, "gate":{"passed":true}}),
        ),
        node(
            json!({"id":"repair", "state":"succeeded", "attempts":[{"launched":true,"key":{"graph_id":"g","node_id":"repair","route_index":0},"route":{"alias":"utility"},"state":"terminal","launch_actor":"actor"}], "round":{"loop_id":"loop","round":2,"node_id":"repair","until":false}}),
        ),
        node(
            json!({"id":"configured_only", "state":"pending", "attempts":[], "round":{"loop_id":"loop","round":3,"node_id":"configured_only","until":false}}),
        ),
    ];
    assert_eq!(graph_quality_counts(&checkpoint_nodes), (1, 1, 1));
}

#[test]
fn host_native_attempts_do_not_resume_explicitly() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|error| error.message)?;
    let store = trace_store(&temp, &call)?;
    let input = trace_run(44, AttemptId::from_bytes([45; 16]), clock.wall_now()?)?;
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.applied_route = Some(run::RouteRecord {
        extra: [("host_native".into(), json!("ci-fixer"))].into(),
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5-5".into()),
        ..Default::default()
    });
    record.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: clock.wall_now()?,
        reason: None,
    };
    record.session = Some("native-session".into());
    let registry = Registry::default();
    let error = registry
        .check_continuable(&record)
        .err()
        .ok_or("host resume accepted")?;
    assert_eq!(error.kind, ErrorKind::ContinuationUnsupported);
    Ok(())
}

#[test]
fn unsafe_store_path_reason_survives_cli_conversion() {
    let failure = super::Fail::from(provenant_run::RunError::UnsafePath {
        path: "runs/example/process.json".into(),
        check: provenant_run::PathCheck::Links,
    });
    assert!(failure.message.contains("runs/example/process.json"));
    assert!(failure.message.contains("link count"));
}

#[test]
fn host_delivery_limit_keeps_its_text_as_typed_io_detail() {
    for (tool, command, text) in [
        (
            "git",
            "worktree list",
            "host writer delivery failed at Integrate: git worktree list output exceeded the 8388608-byte host limit",
        ),
        (
            "gh",
            "pr",
            "host writer delivery failed at Integrate: gh pr output exceeded the 8388608-byte host limit",
        ),
    ] {
        let failure = super::Fail::from(provenant_run::RunError::DeliveryLimit {
            stage: provenant_records::run::WriterDeliveryStage::Integrate,
            tool,
            command: command.into(),
            limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
        });
        assert_eq!(failure.message, text);
        assert_eq!(failure.kind, ErrorKind::Io);
        let detail = failure.projection.expect("typed detail");
        assert!(matches!(
            detail.fact,
            Some(provenant_records::diagnostic::DiagnosticFact::DeliveryLimit(_))
        ));
        assert_eq!(detail.code, "IO");
        assert_eq!(detail.detail.as_deref(), Some(text));
    }
    let failure = super::Fail::from(provenant_run::RunError::Delivery(
        provenant_records::run::WriterDeliveryStage::Prepare,
    ));
    let detail = failure.projection.expect("stage detail");
    assert!(matches!(
        detail.fact,
        Some(provenant_records::diagnostic::DiagnosticFact::DeliveryStage(_))
    ));
    assert_eq!(detail.detail.as_deref(), Some(failure.message.as_str()));
}

#[test]
fn cli_io_projection_omits_untrusted_sources_in_both_formats()
-> Result<(), Box<dyn std::error::Error>> {
    for source in [
        "TOKEN_SENTINEL\u{1b}".to_owned(),
        format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
    ] {
        let failure = super::Fail::from(run::RunError::Io(std::io::Error::other(source)));
        let reply = super::reply("run", Err(failure));
        for mode in [
            crate::render::RenderMode::Human {
                text: &reply.human,
                summary: None,
                read_only: false,
            },
            crate::render::RenderMode::Json,
        ] {
            let mut output = Vec::new();
            crate::render::render(&reply.result, mode, &mut output)?;
            let text = String::from_utf8(output)?;
            assert!(text.contains("IO"));
            assert!(!text.contains("TOKEN_SENTINEL"));
            assert!(!text.chars().any(|c| c.is_control() && c != '\n'));
            assert!(text.len() < 4096);
        }
        assert!(reply.result.error.as_ref().unwrap().repair.is_some());
    }
    Ok(())
}

#[test]
fn host_delivery_limit_cli_error_matches_golden() -> Result<(), Box<dyn std::error::Error>> {
    let failure = super::Fail::from(provenant_run::RunError::DeliveryLimit {
        stage: provenant_records::run::WriterDeliveryStage::Integrate,
        tool: "git",
        command: "worktree list".into(),
        limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
    });
    assert_eq!(failure.kind, ErrorKind::Io);
    let reply = super::reply("run", Err(failure));
    let mut rendered = Vec::new();
    crate::render::render(
        &reply.result,
        crate::render::RenderMode::Human {
            text: &reply.human,
            summary: reply.summary.as_ref(),
            read_only: reply.read_only,
        },
        &mut rendered,
    )?;
    let actual = String::from_utf8(rendered)?;
    for part in [
        "IO",
        "Integrate",
        "git",
        &provenant_os::exec::DEFAULT_CAPTURE_LIMIT.to_string(),
    ] {
        assert!(actual.contains(part), "{actual}");
    }
    Ok(())
}

#[test]
fn stats_summary_reports_cost_coverage() {
    use provenant_telemetry::{CostTotal, Money, Totals};
    let mut totals = Totals {
        attempts: 3,
        usage: Default::default(),
        cache: Default::default(),
        cost: CostTotal {
            cost_basis: "api_equivalent_estimate",
            subscription_attempts: 0,
            complete: None,
            known_lower_bound: None,
            priced_attempts: 0,
            missing_attempts: 3,
        },
        outcomes: Default::default(),
    };
    assert_eq!(totals_cache_read_share(&totals), None);
    totals.usage.uncached_input.known = 10;
    totals.usage.cache_read.known = 90;
    assert_eq!(totals_cache_read_share(&totals), Some(0.9));
    totals.usage.cache_write.missing = 1;
    assert_eq!(totals_cache_read_share(&totals), None);
    assert_eq!(
        cost_summary(&totals),
        "3 attempts, cost unknown (3 attempts missing cost data)\n"
    );
    totals.cost.known_lower_bound = Some(Money {
        microdollars: 2_000_000,
        usd: "2.000000".into(),
    });
    totals.cost.priced_attempts = 1;
    totals.cost.missing_attempts = 2;
    assert_eq!(
        cost_summary(&totals),
        "3 attempts, est. API-equivalent at least $2.000000 (1 of 3 attempts priced)\n"
    );
    totals.cost.complete = totals.cost.known_lower_bound.clone();
    assert_eq!(
        cost_summary(&totals),
        "3 attempts, est. API-equivalent $2.000000\n"
    );
}

#[test]
fn stats_text_lists_cache_share_only_where_a_provider_reported_it() {
    use provenant_telemetry::{ChairModelUsage, ConfiguredPrices, usage::ChairSession};
    let prices = provenant_telemetry::validate_prices(&ConfiguredPrices {
        schema_version: 1,
        id: "empty".into(),
        reviewed_at: "2026-10-29".into(), // timebomb: allow fixed price-table review date, never compared with the clock
        units: "usd_microdollars_per_million_tokens".into(),
        max_review_age_days: None,
        discrepancy_threshold_bps: None,
        models: Vec::new(),
    })
    .unwrap();
    let chair = |usage| ChairSession {
        adapter: "claude".into(),
        account: "work".into(),
        session: "s".into(),
        project: None,
        config: None,
        usage,
        unread: None,
    };
    let usage = |read| {
        vec![ChairModelUsage {
            model: "claude-opus-5-5".into(),
            usage: provenant_records::Usage {
                uncached_input: Some(10),
                cache_read: read,
                cache_write: Some(0),
                cache_write_1h: None,
                output_excluding_reasoning: Some(1),
                reasoning: None,
            },
        }]
    };
    let report = |usage| {
        let report =
            provenant_telemetry::stats_with_chair([], [chair(usage)], &prices, Default::default())
                .unwrap();
        stats_summary(&report)
    };
    let text = report(usage(Some(90)));
    assert!(
            text.contains(
                "cache read by source [chair]: 90.0% of 100 input tokens (1 records reported, 0 did not)\n"
            ),
            "{text}"
        );
    assert!(
        text.contains("cache read by model [chair] claude claude/claude-opus-5-5: 90.0%"),
        "{text}"
    );
    assert!(
        text.contains("chair: 1 sessions on this host (0 unread)"),
        "{text}"
    );
    let text = report(usage(None));
    assert!(!text.contains("cache read"), "{text}");
}

#[test]
fn stats_text_names_its_scope_and_reports_reasoning_only_when_present() {
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let scope = stats_scope_line(project, 2);
    assert!(
        scope.starts_with(&format!(
            "scope: project {project}, finished attempts only (2 queued or running not counted)"
        )),
        "{scope}"
    );
    assert!(scope.contains("other projects are excluded"), "{scope}");
    let mut totals = provenant_telemetry::Totals {
        attempts: 1,
        usage: Default::default(),
        cache: Default::default(),
        cost: provenant_telemetry::CostTotal {
            cost_basis: "api_equivalent_estimate",
            subscription_attempts: 0,
            complete: None,
            known_lower_bound: None,
            priced_attempts: 0,
            missing_attempts: 1,
        },
        outcomes: Default::default(),
    };
    assert_eq!(reasoning_line(&totals), None);
    totals.usage.reasoning.known = 42;
    assert_eq!(
        reasoning_line(&totals).as_deref(),
        Some("reasoning tokens: 42\n")
    );
    totals.usage.reasoning.missing = 3;
    assert_eq!(
        reasoning_line(&totals).as_deref(),
        Some("reasoning tokens: >=42 (3 unknown)\n")
    );
}

#[test]
fn displaced_fallback_reports_unavailable_history() {
    let history = super::fallback_history(Path::new(env!("CARGO_MANIFEST_DIR")), "HEAD", true);
    assert_eq!(
        history,
        "[history unavailable: prior worktree is missing or reassigned]"
    );
}

#[test]
fn submit_names_the_fix_for_a_bare_word_model_and_unsupported_effort()
-> Result<(), Box<dyn std::error::Error>> {
    let Err(bare) = super::requested(Some("junior"), None, None) else {
        panic!("bare-word model accepted");
    };
    assert_eq!(bare.status, Status::Invalid);
    assert!(
        bare.message.ends_with("did you mean --route junior?"),
        "{}",
        bare.message
    );
    Ok(())
}

#[test]
fn contention_reply_names_the_operation_to_retry() -> Result<(), Box<dyn std::error::Error>> {
    let id = OperationId::parse("retry-lane")?;
    let mut reply = super::reply(
        "run",
        Err(Fail::new(
            Status::Refused,
            ErrorKind::Contention,
            "the lane is being admitted",
        )),
    );
    super::operation_reply(&mut reply, &id);
    let repair = reply
        .result
        .error
        .as_ref()
        .and_then(|error| error.repair.as_deref());
    assert_eq!(
        repair,
        Some("retry the same command with --operation-id retry-lane")
    );
    Ok(())
}

#[test]
fn pruning_intent_replays_only_for_its_missing_worktree() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let tree = temp.path().join("owned");
    let receipt = serde_json::json!({"state": "pruning", "worktree": tree});
    assert!(super::worktree_was_pruned(&receipt, &tree));
    assert!(!super::worktree_was_pruned(
        &receipt,
        &temp.path().join("other")
    ));
    std::fs::create_dir(&tree)?;
    assert!(!super::worktree_was_pruned(&receipt, &tree));
    std::fs::remove_dir(&tree)?;
    std::os::unix::fs::symlink(temp.path().join("missing-target"), &tree)?;
    assert!(!super::worktree_was_pruned(&receipt, &tree));
    Ok(())
}

use provenant_run::Providers;
use provenant_supervisor::custody::CaptureTotals;
use provenant_supervisor::custody::PublicationState;
use provenant_supervisor::custody::Scope;
use provenant_supervisor::custody::SettlementEvidence;

#[test]
fn operation_replays_after_its_context_ref_moves_or_is_deleted()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{ActorId, RootInputs};
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::Executor;
    use provenant_os::exec::StreamPolicy;
    use provenant_os::exec::SystemExecutor;
    use provenant_os::exec::Termination;

    use std::ffi::{OsStr, OsString};
    let executor = SystemExecutor::new(1)?;
    // The regression exercises a top-level submission, even when the
    // contributor running it is itself inside a nested lane.
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
        || std::env::var_os(run::LANE_DEPTH_ENV).is_some()
        || std::env::var_os(run::PARENT_ATTEMPT_ENV).is_some()
    {
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::operation_replays_after_its_context_ref_moves_or_is_deleted".into(),
            "--nocapture".into(),
        ];
        let removed = [
            OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
            OsStr::new(run::LANE_DEPTH_ENV),
            OsStr::new(run::PARENT_ATTEMPT_ENV),
        ];
        let env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| !removed.contains(&key.as_os_str()))
            .collect();
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = executor.execute(options)?;
        assert!(
            out.termination == Termination::Exit(0) && !out.timed_out,
            "{out:?}"
        );
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let cwd = root.join("checkout");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&cwd)?;
    let mut inputs = RootInputs::new(home.clone(), cwd.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let command = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
        let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.iter().map(OsString::from))
            .collect();
        let env = provenant_os::fixture::fixture_git_settings();
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(10));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = executor.execute(options)?;
        assert!(
            out.termination == Termination::Exit(0) && !out.timed_out,
            "{out:?}"
        );
        Ok(String::from_utf8(out.stdout.bytes)?.trim().to_owned())
    };
    command(&["init", "-q"])?;
    command(&["commit", "--allow-empty", "-qm", "initial"])?;
    command(&["branch", "context-base"])?;
    let original_base = command(&["rev-parse", "context-base"])?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let context = run::ExecutionContext {
        state_root: roots.state.clone(),
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session {
        store: run::RunStore::open(&context, &call)?,
        context,
    };
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &cwd,
        cwd: &cwd,
    };
    let chair = Some(run::ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([4; 16]),
        actor: ActorId::from_bytes([5; 16]),
    });
    let submission = |base: &str| Submission {
        chair: chair.clone(),
        operation_id: OperationId::parse("context-ref-replay").unwrap(),
        tasks: vec![Submitted {
            attempt: Default::default(),
            require_skill_location: true,
            settlement_retry_limit: None,
            context: None,
            title: None,
            task_id: TaskId::from_bytes([3; 16]),
            prompt: "review".into(),
            base: Some(base.into()),
            pinned_base: None,
            head: None,
            source_head: None,
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "fixture".into(),
                model: "fixture/model".into(),
                effort: None,
            },
            provider_timeout: 10,
            delivery_id: None,
            deliver: None,
            worktree: None,
            access: Default::default(),
        }],
        concurrency: Some(1),
        knobs: vec![],
    };
    // The fake adapter refuses preparation after admission. Keep the durable
    // operation as the replay subject even though the initial launch fails.
    let initial = submit(&session, &edge, submission("context-base"), &call)
        .err()
        .ok_or("unknown fixture adapter must refuse its initial launch")?;
    assert_eq!(initial.status, Status::Refused);
    assert_eq!(initial.kind, ErrorKind::Refused);
    let operation = session
        .store
        .operation(&OperationId::parse("context-ref-replay")?, &call)?
        .ok_or("refused launch lost its admitted operation")?;
    assert_eq!(operation.state, run::OperationState::Terminal);
    let first = session.store.run(operation.run_id, &call)?;
    assert!(
        session.store.request(first.run_id, &call)?.tasks[0]
            .profile
            .require_skill_location
    );
    with_chair_index(&roots, &call, |index, index_call| {
        let binding = index
            .read(session.context.project_id, first.run_id, index_call)?
            .unwrap();
        assert_eq!(binding.chair, chair);
        assert_eq!(
            binding.request_digest,
            session
                .store
                .request(first.run_id, &call)
                .unwrap()
                .digest()
                .unwrap()
        );
        Ok(())
    })
    .map_err(|f| f.message)?;
    std::fs::rename(
        roots.state.as_path().join("fabric/host.sqlite3"),
        roots.state.as_path().join("fabric/interrupted.sqlite3"),
    )?;
    let mut forged = session.store.request(first.run_id, &call)?;
    forged.concurrency = 2;
    with_chair_index(&roots, &call, |index, c| {
        index.submit(session.context.project_id, first.run_id, &forged, c)
    })
    .map_err(|f| f.message)?;
    assert_eq!(
        launch(&session, first.run_id, &edge, &call)
            .unwrap_err()
            .status,
        Status::Refused
    );
    std::fs::rename(
        roots.state.as_path().join("fabric/host.sqlite3"),
        roots.state.as_path().join("fabric/forged.sqlite3"),
    )?;
    // Reusing an operation from another chair cannot rebind even a missing
    // host index row. Repair must use the immutable original request.
    let mut replay = submission("context-base");
    replay.chair = Some(run::ChairBinding {
        extra: Default::default(),
        session: None,
        host: provenant_core::HostId::from_bytes([6; 16]),
        actor: ActorId::from_bytes([7; 16]),
    });
    let (replayed, _) = submit(&session, &edge, replay, &call).map_err(|f| f.message)?;
    assert_eq!(replayed.run_id, first.run_id);
    let stopped = CancelToken::new();
    stopped.cancel();
    let cancelled = run::Request {
        cancel: &stopped,
        ..call
    };
    let failure = with_chair_index(&roots, &cancelled, |_, _| Ok(())).unwrap_err();
    assert_eq!(failure.status, Status::Error);
    with_chair_index(&roots, &call, |index, call| {
        assert_eq!(
            index
                .read(session.context.project_id, first.run_id, call)?
                .unwrap()
                .chair,
            chair
        );
        Ok(())
    })
    .map_err(|f| f.message)?;
    command(&["commit", "--allow-empty", "-qm", "moved"])?;
    command(&["branch", "-f", "context-base", "HEAD"])?;
    for deleted in [false, true] {
        if deleted {
            command(&["branch", "-D", "context-base"])?;
        }
        let (replayed, _) =
            submit(&session, &edge, submission("context-base"), &call).map_err(|f| f.message)?;
        assert_eq!(replayed.run_id, first.run_id);
        assert_eq!(
            session.store.request(first.run_id, &call)?.tasks[0]
                .base
                .as_deref(),
            Some(original_base.as_str())
        );
    }
    assert!(
        matches!(submit(&session, &edge, submission("HEAD"), &call), Err(fail) if fail.kind == ErrorKind::OperationConflict)
    );
    Ok(())
}

pub(super) fn trace_run(
    n: u8,
    parent_attempt: AttemptId,
    created_at: WallTime,
) -> Result<run::PreparedRun, Box<dyn std::error::Error>> {
    let task_id = TaskId::from_bytes([n; 16]);
    Ok(run::PreparedRun {
        run_id: RunId::from_bytes([n + 1; 16]),
        operation_id: OperationId::parse(&format!("trace-{n}"))?,
        created_at,
        request: run::CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![run::TaskRequest {
                cloud: None,
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
                task_id,
                prompt_digest: CanonicalV1::file_digest(b"p"),
                route: run::RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: None,
                },
                account: None,
                profile: Default::default(),
                authority_digest: provenant_core::Digest::from_bytes([5; 32]),
                provider_timeout_seconds: 3600,
                writer: None,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: Some(1),
            parent_attempt: Some(parent_attempt),
        },
        attempts: vec![run::NewAttempt {
            task_id,
            attempt_id: AttemptId::from_bytes([n + 2; 16]),
            prompt: "p".into(),
        }],
    })
}

fn trace_store(
    temp: &tempfile::TempDir,
    call: &run::Request<'_>,
) -> Result<run::RunStore, Box<dyn std::error::Error>> {
    let root = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir_all(root.join("home"))?;
    std::fs::create_dir_all(root.join("checkout"))?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: provenant_core::ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    Ok(run::RunStore::open(&context, call)?)
}

#[test]
fn retry_reuses_the_retained_brief_and_accepts_a_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    let prior = prepared.attempts[0].attempt_id;
    store.create(&prepared, &call)?;
    let mut roots = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    roots.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session { store, context };
    let mut command = crate::Resume {
        attempt: crate::Attempt(Target {
            selection: None,
            run: None,
            task: None,
            attempt: Some(prior),
        }),
        operation_id: OperationId::parse("retry-brief")?,
        node: None,
        brief: None,
        extend: vec![],
        no_fallback: false,
        prompt: crate::Prompt {
            prompt_file: None,
            prompt_stdin: false,
        },
        wait: None,
    };
    assert_eq!(
        resume::retry_prompt(&command, &session, prior, &call).map_err(|f| f.message)?,
        "p"
    );
    command.brief = Some("repair the parser".into());
    assert_eq!(
        resume::retry_prompt(&command, &session, prior, &call).map_err(|f| f.message)?,
        "repair the parser"
    );
    Ok(())
}

#[test]
fn refused_operations_are_rendered_without_an_attempt() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = trace_store(&temp, &call)?;
    let input = trace_run(
        30,
        AttemptId::from_bytes([2; 16]),
        now(&clock).map_err(|e| e.message)?,
    )?;
    store.record_refusal(
        &input,
        ErrorKind::Unsupported.diagnostic(),
        Some("writer".into()),
        &call,
    )?;
    let mut data = json!({"lanes": []});
    let mut human = String::new();
    assert!(
        status::append_refused_operations(&store, 20, &mut data, &mut human, &call)
            .map_err(|e| e.message)?
    );
    assert_eq!(
        data["refused_operations"][0]["operation_id"],
        input.operation_id.as_str()
    );
    assert_eq!(
        data["refused_operations"][0]["refusal"]["code"],
        "UNSUPPORTED"
    );
    assert!(human.contains(input.operation_id.as_str()));
    assert!(data["lanes"].as_array().is_some_and(Vec::is_empty));
    Ok(())
}

#[test]
fn cloud_job_survives_reopening_owned_attempt() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::cloud::{
        CloudProvider, CloudResult, CloudSubmission, RemoteJob, RemoteState,
    };
    struct Fake {
        running: bool,
    }
    impl CloudProvider for Fake {
        type Error = ();
        fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
            Ok(())
        }
        fn submit(&mut self, request: CloudSubmission<'_>) -> Result<String, ()> {
            assert_eq!(request.job.commit, "a".repeat(40));
            Ok("remote-id".into())
        }
        fn observe(&mut self, job: &RemoteJob) -> Result<RemoteState, ()> {
            assert_eq!(job.remote_id, "remote-id");
            assert_eq!(job.repository, "repository");
            assert_eq!(job.commit, "a".repeat(40));
            if self.running {
                return Ok(RemoteState::Running);
            }
            Ok(RemoteState::Finished {
                result: CloudResult {
                    job_id: job.job_id.clone(),
                    nonce: job.nonce.clone(),
                    commit: "a".repeat(40),
                    exit_status: 0,
                    tests_run: vec!["targeted".into()],
                    tests_failed: vec![],
                    log_location: "log".into(),
                    tracked_files_changed: false,
                    observed_model: Some("small".into()),
                },
            })
        }
        fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
            Ok(false)
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|e| e.message)?;
    let store = trace_store(&temp, &call)?;
    let mut input = trace_run(
        40,
        AttemptId::from_bytes([1; 16]),
        now(&clock).map_err(|e| e.message)?,
    )?;
    input.request.parent_attempt = None;
    input.request.tasks[0].head = Some("a".repeat(40));
    input.request.tasks[0].cloud = Some(provenant_records::run::CloudPlacement {
        target: "target".into(),
        provider: "fake".into(),
        environment: "env".into(),
        repository: "repository".into(),
        branch: "main".into(),
        model: "small".into(),
        effort: None,
        deadline_seconds: 3600,
    });
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let target = crate::domains::CloudTarget {
        provider: "fake".into(),
        environment: "env".into(),
        model: "small".into(),
        effort: None,
        repository: "repository".into(),
        branch: "main".into(),
        deadline_seconds: 3600,
    };
    let job = cloud_owner::prepare_job(id, "target", &target, &"a".repeat(40), &clock)?;
    assert_eq!(job.job_id, id.to_string());
    assert_eq!(job.nonce.len(), 32);
    assert_ne!(
        job.nonce,
        cloud_owner::prepare_job(id, "target", &target, &"a".repeat(40), &clock)?.nonce
    );
    let mut wrong_commit = job.clone();
    wrong_commit.commit = "b".repeat(40);
    assert!(
        cloud_owner::submit(
            &mut Fake { running: true },
            &store,
            id,
            wrong_commit,
            "run tests",
            &call
        )
        .is_err()
    );
    let submitted = cloud_owner::submit(
        &mut Fake { running: true },
        &store,
        id,
        job,
        "run tests",
        &call,
    )?;
    drop(store);
    let store = trace_store(&temp, &call)?;
    let cancelled = cloud_owner::cancel(&mut Fake { running: true }, &store, id, &call)?;
    assert_eq!(cancelled.remote_id, submitted.remote_id);
    assert_eq!(cancelled.state, RemoteState::CancellationUnconfirmed);
    drop(store);
    let store = trace_store(&temp, &call)?;
    let running = cloud_owner::reconcile(&mut Fake { running: true }, &store, id, &call)?;
    assert_eq!(running.state, RemoteState::CancellationUnconfirmed);
    // A crash can happen after the remote result is saved but before its run
    // event is recorded; reconciliation repairs that publication.
    cloud_store::AttemptCloudStore::with_attempt(
        &store,
        id,
        false,
        &call,
        |cloud_store| -> Result<(), String> {
            let mut job = cloud_store
                .load()
                .map_err(|error| error.to_string())?
                .ok_or("missing cloud job")?;
            provenant_providers::cloud::reconcile(
                &mut Fake { running: false },
                cloud_store,
                &mut job,
            )
            .map_err(|error| format!("{error:?}"))?;
            Ok(())
        },
    )??;
    assert!(!cloud_owner::finished_run(&store, input.run_id, &call)?);
    let finished = cloud_owner::reconcile(&mut Fake { running: false }, &store, id, &call)?;
    assert!(matches!(finished.state, RemoteState::Finished { .. }));
    assert!(cloud_owner::finished_run(&store, input.run_id, &call)?);
    let event = store.last_event_sequence(id, &call)?;
    let _ = cloud_owner::reconcile(&mut Fake { running: false }, &store, id, &call)?;
    assert_eq!(store.last_event_sequence(id, &call)?, event);
    let mut lanes = vec![store.record(id, &call)?];
    cloud_owner::hydrate(&store, &mut lanes, &call)?;
    assert_eq!(status::lane_display_state(&lanes[0]), "cloud_passed");
    assert!(condition_met(&lanes, Condition::All));
    let mut data = json!({});
    let mut human = String::new();
    status::attach_cloud_jobs(&store, &lanes, &mut data, &mut human, &call)
        .map_err(|error| error.message)?;
    assert_eq!(data["cloud_jobs"][0]["state"]["state"], "finished");
    assert_eq!(data["cloud_jobs"][0]["passed"], true);
    assert!(human.contains("advisory pass"));
    let mut setup_failed = finished;
    setup_failed.state = RemoteState::SetupFailed {
        detail: "environment setup failed".into(),
    };
    let diagnostic = cloud_owner::view(&setup_failed);
    assert_eq!(diagnostic["target"], "target");
    assert_eq!(diagnostic["environment"], "env");
    assert!(
        diagnostic["remedy"]
            .as_str()
            .is_some_and(|text| text.contains("repair"))
    );
    Ok(())
}

#[test]
fn graph_cloud_ticks_recover_one_submission_and_settle_remote_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::fabric::graph_cloud;
    use provenant_fabric_runtime::graph::{AttemptObservation, AttemptOutcome};
    use provenant_providers::cloud::{
        CloudProvider, CloudResult, CloudSubmission, RemoteJob, RemoteState,
    };
    struct Fake {
        submissions: usize,
        observations: usize,
        recoveries: usize,
        uncertain_submission: bool,
        state: u8,
        passed: bool,
        cancel_confirmed: bool,
    }
    impl CloudProvider for Fake {
        type Error = ();
        fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
            Ok(())
        }
        fn submit(&mut self, request: CloudSubmission<'_>) -> Result<String, ()> {
            assert_eq!(request.task, "p");
            assert_eq!(request.job.commit, "a".repeat(40));
            self.submissions += 1;
            if self.uncertain_submission {
                return Err(());
            }
            Ok("graph-remote-id".into())
        }
        fn recover(&mut self, job: &RemoteJob) -> Result<Option<String>, ()> {
            assert_eq!(job.state, RemoteState::Submitting);
            assert!(job.remote_id.is_empty());
            assert_eq!(self.submissions, 1);
            self.recoveries += 1;
            Ok(Some("graph-remote-id".into()))
        }
        fn observe(&mut self, job: &RemoteJob) -> Result<RemoteState, ()> {
            assert_eq!(job.remote_id, "graph-remote-id");
            self.observations += 1;
            Ok(match self.state {
                0 => RemoteState::Running,
                1 => RemoteState::Unknown {
                    detail: "temporarily unavailable".into(),
                },
                _ => RemoteState::Finished {
                    result: CloudResult {
                        job_id: job.job_id.clone(),
                        nonce: job.nonce.clone(),
                        commit: job.commit.clone(),
                        exit_status: if self.passed { 0 } else { 1 },
                        tests_run: vec!["targeted".into()],
                        tests_failed: if self.passed {
                            vec![]
                        } else {
                            vec!["targeted".into()]
                        },
                        log_location: "log".into(),
                        tracked_files_changed: false,
                        observed_model: Some(job.requested_model.clone()),
                    },
                },
            })
        }
        fn cancel(&mut self, job: &RemoteJob) -> Result<bool, ()> {
            assert_eq!(job.remote_id, "graph-remote-id");
            Ok(self.cancel_confirmed)
        }
    }
    for (expected_pass, finishes_after_cancel, uncertain_submission) in [
        (Some(false), false, false),
        (Some(true), false, false),
        (None, false, false),
        (None, true, false),
        (None, false, true),
    ] {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let store = trace_store(&temp, &call)?;
        let mut input = trace_run(44, AttemptId::from_bytes([1; 16]), clock.wall_now()?)?;
        input.request.parent_attempt = None;
        input.request.tasks[0].head = Some("a".repeat(40));
        input.request.tasks[0].cloud = Some(provenant_records::run::CloudPlacement {
            target: "target".into(),
            provider: "fake".into(),
            environment: "env".into(),
            repository: "repository".into(),
            branch: "main".into(),
            model: "small".into(),
            effort: None,
            deadline_seconds: 3600,
        });
        store.create(&input, &call)?;
        let id = input.attempts[0].attempt_id;
        let reference = format!("host/{id}");
        let passed = expected_pass.unwrap_or(false);
        let mut provider = Fake {
            submissions: 0,
            observations: 0,
            recoveries: 0,
            uncertain_submission,
            state: 0,
            passed,
            cancel_confirmed: uncertain_submission,
        };
        let observation =
            graph_cloud::advance(&mut provider, &store, id, "p", &reference, &call, false);
        if uncertain_submission {
            assert!(observation.is_err());
            let saved = cloud_owner::load(&store, id, &call)?.ok_or("missing submission intent")?;
            assert_eq!(saved.state, RemoteState::Submitting);
            drop(store);
            let store = trace_store(&temp, &call)?;
            let recovered =
                graph_cloud::advance(&mut provider, &store, id, "p", &reference, &call, true)?;
            assert!(matches!(
                recovered,
                Some(AttemptObservation::Terminal {
                    outcome: AttemptOutcome::OtherFailure,
                    launched: true,
                    ..
                })
            ));
            assert_eq!(provider.submissions, 1);
            assert_eq!(provider.recoveries, 1);
            assert_eq!(provider.observations, 1);
            continue;
        }
        let observation = observation?;
        assert!(
            matches!(observation, Some(AttemptObservation::Pending { attempt_ref: Some(ref id) }) if id == &reference)
        );
        assert_eq!(provider.submissions, 1);
        assert_eq!(provider.observations, 0);
        drop(store);
        if expected_pass.is_none() {
            let store = trace_store(&temp, &call)?;
            let unconfirmed =
                graph_cloud::advance(&mut provider, &store, id, "p", &reference, &call, true)?;
            assert!(matches!(
                unconfirmed,
                Some(AttemptObservation::Unknown { .. })
            ));
            drop(store);
            provider.cancel_confirmed = !finishes_after_cancel;
            provider.state = if finishes_after_cancel { 2 } else { 0 };
            let store = trace_store(&temp, &call)?;
            let confirmed =
                graph_cloud::advance(&mut provider, &store, id, "p", &reference, &call, true)?;
            let Some(AttemptObservation::Terminal {
                outcome, launched, ..
            }) = confirmed
            else {
                panic!("confirmed cancellation or observed completion must settle the node");
            };
            assert_eq!(
                outcome,
                if finishes_after_cancel {
                    AttemptOutcome::TaskFailed
                } else {
                    AttemptOutcome::OtherFailure
                }
            );
            assert!(launched);
            assert!(
                provider.observations > 0,
                "unconfirmed cancellation must observe the remote job"
            );
            assert_eq!(provider.submissions, 1);
            continue;
        }
        // Every tick reopens the store, as a recovered graph owner does.
        for state in [0, 1, 2, 2] {
            let store = trace_store(&temp, &call)?;
            provider.state = state;
            let observation =
                graph_cloud::advance(&mut provider, &store, id, "p", &reference, &call, false)?;
            match state {
                0 => assert!(matches!(
                    observation,
                    Some(AttemptObservation::Pending { .. })
                )),
                1 => assert!(matches!(
                    observation,
                    Some(AttemptObservation::Unknown { .. })
                )),
                _ => {
                    let Some(AttemptObservation::Terminal {
                        attempt_ref,
                        outcome,
                        launched,
                        result,
                        ..
                    }) = observation
                    else {
                        panic!("finished remote evidence must settle the graph node");
                    };
                    assert_eq!(attempt_ref.as_deref(), Some(reference.as_str()));
                    assert_eq!(
                        outcome,
                        if passed {
                            AttemptOutcome::Succeeded
                        } else {
                            AttemptOutcome::TaskFailed
                        }
                    );
                    assert!(launched);
                    assert!(result.is_some());
                }
            }
            assert_eq!(provider.submissions, 1);
        }
        assert_eq!(provider.observations, 3, "terminal evidence is reused");
    }
    Ok(())
}

#[test]
fn status_shows_recorded_capability_origins() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let mut input = trace_run(
        1,
        AttemptId::from_bytes([9; 16]),
        WallTime::from_micros(100)?,
    )?;
    input.request.tasks[0].access.database_ipc = true;
    input.request.tasks[0].access.extra.insert(
        "capability_sources".into(),
        json!([{"name":"postgres","source":"project default"}]),
    );
    store.create(&input, &call)?;
    let row = store.record(input.attempts[0].attempt_id, &call)?;
    let mut roots = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    roots.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: roots.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session { store, context };
    let (mut data, _) = render_lanes(std::slice::from_ref(&row), &[], None, &root, None);
    super::status::attach_capability_sources(
        &session,
        std::slice::from_ref(&row),
        &mut data,
        &call,
    );
    assert_eq!(
        data["lanes"][0]["capabilities"],
        json!([{"name":"postgres","source":"project default"}])
    );
    Ok(())
}

#[test]
fn a_nested_lane_reads_the_host_ledger_when_it_opens_read_only()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let host = root.join("state");
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(host.join("children").join("lane"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: provenant_core::ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    // The store must exist to be opened; the writable open creates it.
    run::RunStore::open(&context, &call)?;
    let store = read_only_store(&context, &host, &call)
        .map_err(|f| f.message)?
        .ok_or("nested store")?;
    assert_eq!(store.writer_registry(), host);
    assert_ne!(store.writer_registry(), context.state_root.as_path());
    Ok(())
}

#[test]
fn trace_chain_follows_parents_and_ends_at_a_parent_this_store_lacks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let missing = AttemptId::from_bytes([9; 16]);
    let prepared =
        |n: u8, parent_attempt: AttemptId| trace_run(n, parent_attempt, clock.wall_now()?);
    let first = prepared(10, missing)?;
    let second = prepared(20, first.attempts[0].attempt_id)?;
    store.create(&first, &call)?;
    store.create(&second, &call)?;
    let leaf = store.record(second.attempts[0].attempt_id, &call)?;
    let chain = trace_chain(&store, leaf, &call)?;
    let attempts: Vec<_> = chain.iter().map(|t| t.attempt).collect();
    assert_eq!(
        attempts,
        [
            Some(second.attempts[0].attempt_id),
            Some(first.attempts[0].attempt_id),
            Some(missing)
        ]
    );
    assert_eq!(chain[1].run, Some(first.run_id));
    assert_eq!(
        chain[2],
        provenant_records::Trace {
            attempt: Some(missing),
            ..Default::default()
        }
    );
    let line = trace_line(&chain[2]);
    assert_eq!(line.lines().count(), 1);
    assert!(line.contains("host=- ") && line.contains(&format!("attempt={missing} ")));
    Ok(())
}

#[test]
fn refused_provider_continuation_keeps_a_durable_attempt() -> Result<(), Box<dyn std::error::Error>>
{
    struct Refusing {
        prior: Option<AttemptId>,
    }
    impl run::Providers for Refusing {
        fn prepare(
            &self,
            _: &run::TaskRequest,
            _: &str,
            _: &run::LaunchPaths,
            resume: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            assert_eq!(resume.map(|prior| prior.attempt_id), self.prior);
            Err("fixture continuation refused; run provenant run".into())
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!("a refused launch never reaches a provider")
        }
    }
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let prior = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prior, &call)?;
    store.start(
        prior.run_id,
        &Refusing { prior: None },
        &|_, _| unreachable!(),
        &call,
    )?;
    let prior_id = prior.attempts[0].attempt_id;
    let mut resumed = trace_run(20, prior_id, clock.wall_now()?)?;
    resumed.request.parent = Some(prior_id);
    resumed.request.tasks[0].task_id = prior.attempts[0].task_id;
    resumed.attempts[0].task_id = prior.attempts[0].task_id;
    store.create(&resumed, &call)?;
    let started = store.start(
        resumed.run_id,
        &Refusing {
            prior: Some(prior_id),
        },
        &|_, _| unreachable!(),
        &call,
    )?;
    let [run::Started::Refused { attempt, .. }] = started.as_slice() else {
        return Err("continuation must be refused".into());
    };
    assert_eq!(*attempt, resumed.attempts[0].attempt_id);
    // Reopen the store: every ID printed by the refusal must remain
    // addressable after the command exits.
    drop(store);
    let reopened = trace_store(&temp, &call)?;
    let record = run::Service::new(&reopened).status(*attempt, &call)?;
    assert_eq!(record.run_id, resumed.run_id);
    assert!(matches!(
        record.state,
        run::AttemptState::Terminal { outcome: run::Outcome::Rejected, reason: Some(ref reason), .. }
            if reason.contains("fixture continuation refused")
    ));
    Ok(())
}

#[test]
fn grants_lines_list_each_applied_grant_and_stay_empty_for_older_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prepared, &call)?;
    let mut record = store.record(prepared.attempts[0].attempt_id, &call)?;
    assert_eq!(grants_lines(&record), "");
    record.write_grants = vec![run::WriteGrantRecord {
        extra: Default::default(),
        source: run::WriteSource::WriteRoot,
        path: provenant_records::AbsolutePath::new(std::path::Path::new("/work/shared"))?,
        scope: run::PathScope::Subtree,
    }];
    record.read_only_writable = vec!["docs".into()];
    record.git_grants_digest = Some(provenant_core::Digest::from_bytes([6; 32]));
    let lines = grants_lines(&record);
    let mut lines = lines.lines();
    assert_eq!(
        lines.next(),
        Some("grant write source=write_root scope=subtree path=/work/shared")
    );
    assert_eq!(lines.next(), Some("grant read_only_writable path=docs"));
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("grant git digest="))
    );
    assert_eq!(lines.next(), None);
    Ok(())
}

#[test]
fn grants_lines_report_incomplete_tool_closure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    store.create(&prepared, &call)?;
    let mut record = store.record(prepared.attempts[0].attempt_id, &call)?;
    record.applied_route = Some(run::RouteRecord {
        extra: [(
            "tool_closure".into(),
            serde_json::json!([{
                "tool": "rg", "outcome": "incomplete", "reason": "missing", "reasons": ["missing"]
            }]),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    assert!(
        grants_lines(&record).contains("warning tool_closure_incomplete tool=rg reason=missing\n")
    );
    Ok(())
}

#[test]
fn trace_chain_keeps_every_ancestor_of_a_long_chain() -> Result<(), Box<dyn std::error::Error>> {
    const LEN: u8 = 70;
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    let missing = AttemptId::from_bytes([9; 16]);
    let mut parent = missing;
    let mut ids = Vec::new();
    for link in 1..=LEN {
        // Each run uses three bytes of ID space, so step by three.
        let prepared = trace_run(3 * link, parent, clock.wall_now()?)?;
        store.create(&prepared, &call)?;
        parent = prepared.attempts[0].attempt_id;
        ids.push(parent);
    }
    let leaf = store.record(parent, &call)?;
    let chain = trace_chain(&store, leaf, &call)?;
    let attempts: Vec<_> = chain.iter().filter_map(|t| t.attempt).collect();
    ids.reverse();
    ids.push(missing);
    assert_eq!(attempts, ids);
    Ok(())
}

#[test]
fn trace_chain_fails_when_parents_loop() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|f| f.message)?;
    let store = trace_store(&temp, &call)?;
    // Attempt IDs are n + 2, so 12 and 22 name each other.
    let first = trace_run(10, AttemptId::from_bytes([22; 16]), clock.wall_now()?)?;
    let second = trace_run(20, AttemptId::from_bytes([12; 16]), clock.wall_now()?)?;
    store.create(&first, &call)?;
    store.create(&second, &call)?;
    let leaf = store.record(second.attempts[0].attempt_id, &call)?;
    let Err(error) = trace_chain(&store, leaf, &call) else {
        return Err("a looping chain must fail".into());
    };
    assert!(matches!(error, TraceError::Loop(attempt) if attempt == second.attempts[0].attempt_id));
    Ok(())
}

#[test]
fn check_continuable_refuses_nonterminal_before_session() -> Result<(), Box<dyn std::error::Error>>
{
    for state in ["running", "queued"] {
        let prior: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "resume-nonterminal",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": WallTime::from_micros(100)?,
            "state": {"state":state},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))?;
        let result = Registry::default().check_continuable(&prior);
        assert!(
            matches!(result, Err(fail) if fail.kind == ErrorKind::Nonterminal && fail.status == Status::Refused),
            "{state} attempt must be refused as nonterminal"
        );
    }
    Ok(())
}

#[test]
fn reconcile_reports_orphan_reservation_issues() -> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane("run::tests::reconcile_reports_orphan_reservation_issues")? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let slot = session
                .context
                .state_root
                .as_path()
                .join("worktrees")
                .join(session.context.project_id.to_string())
                .join("orphan");
            std::fs::create_dir_all(&slot)?;
            let path = slot.join("reservation.json");
            std::fs::write(&path, b"malformed retained reservation")?;
            let reply = reconcile(
                Reconcile {
                    id: Vec::new(),
                    abandon: None,
                    reason: None,
                },
                edge,
            );
            assert_eq!(reply.result.status, Status::Failed, "{}", reply.human);
            assert_eq!(
                reply.result.error.as_ref().ok_or("missing error")?.code,
                "RECONCILE_INCOMPLETE"
            );
            assert_eq!(reply.result.data["runs"], json!([]));
            assert_eq!(
                reply.result.data["writer_reservation_errors"][0]["reservation"],
                json!(path)
            );
            assert_eq!(std::fs::read(&path)?, b"malformed retained reservation");
            // A valid orphan whose destination cannot be published reports the
            // storage failure too, rather than only a generic retained fence.
            use provenant_supervisor::custody::{self, Scope};
            use std::os::unix::fs::PermissionsExt;
            std::fs::remove_file(&path)?;
            let directory = provenant_os::fs::Directory::open_anchor(&slot)?;
            let scope = Scope {
                run: RunId::from_bytes([6; 16]),
                attempt: AttemptId::from_bytes([7; 16]),
                generation: 1,
            };
            drop(custody::acquire_reader(
                directory.try_clone()?,
                scope,
                provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                &cancel,
                &clock,
            )?);
            let child_path = slot.join(scope.attempt.to_string());
            let child = child_path.join("reservation.json");
            let mut retained: Value = serde_json::from_slice(&std::fs::read(&child)?)?;
            let anchor = directory.metadata()?;
            retained["reservation_device"] = anchor.device.into();
            retained["reservation_inode"] = anchor.inode.into();
            std::fs::write(&path, serde_json::to_vec(&retained)?)?;
            std::fs::remove_file(&child)?;
            std::fs::set_permissions(&child_path, std::fs::Permissions::from_mode(0o500))?;
            let reply = reconcile(
                Reconcile {
                    id: Vec::new(),
                    abandon: None,
                    reason: None,
                },
                edge,
            );
            std::fs::set_permissions(&child_path, std::fs::Permissions::from_mode(0o700))?;
            assert_eq!(reply.result.status, Status::Failed, "{}", reply.human);
            assert!(
                reply.result.data["writer_reservation_errors"]
                    .as_array()
                    .ok_or("issues")?
                    .iter()
                    .any(|issue| issue["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("cut over writer reservations")))
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&std::fs::read(&path)?)?,
                retained
            );
            assert!(!child.exists());
            Ok(())
        },
    )??;
    Ok(())
}

#[test]
fn cancel_and_reconcile_report_leftover_paths_and_reasons_without_terminal_controls() {
    let state = run::Reconciled::TemporaryRetained {
        path: PathBuf::from("/state/runs/run/attempts/attempt/tmp"),
        reason: "permission denied\n\x1b[31mretry later\x1b[0m".into(),
    };
    let rendered = reconcile_summary(&state);
    assert!(rendered.contains("/state/runs/run/attempts/attempt/tmp"));
    assert!(rendered.contains("permission denied"));
    assert!(rendered.contains("retry later"));
    assert!(!rendered.chars().any(char::is_control));

    let state = run::Reconciled::Uncertain {
        reason: run::UncertainReason::ReconcileFailed {
            error: "writer fence listing failed".into(),
        },
    };
    assert!(reconcile_summary(&state).contains("writer fence listing failed"));

    let data =
        json!({"writer_reservation_errors": [{"reason": "legacy reservation has invalid anchor"}]});
    let blocked = reconcile_reply(
        Ok((
            data.clone(),
            "writer reservation: legacy reservation has invalid anchor\n".into(),
        )),
        true,
    );
    assert_eq!(blocked.result.status, Status::Failed);
    assert_eq!(blocked.result.data, data);
    let error = blocked
        .result
        .error
        .expect("failed reconcile must explain its blocker");
    assert_eq!(error.code, "RECONCILE_INCOMPLETE");
    assert!(error.message.contains("invalid anchor"));
}

fn render_lanes_without_cooldowns(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    root: &Path,
) -> (Value, String) {
    render_lanes(lanes, waits, next, root, None)
}

#[test]
fn local_lanes_read_and_show_capacity_cooldowns() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let route = run::RouteRecord {
        extra: Default::default(),
        provider: Some("codex".into()),
        model: Some("codex/model".into()),
        effort: None,
        account_label: Some("work".into()),
        account_key: Some("key".into()),
        source: None,
        pool_selection: None,
    };
    let now = SystemClock::new().wall_now()?.as_timestamp().as_second();
    crate::quota::persist_capacity_cooldown(&state, &route, now, 600)?;
    let cooldowns = read_lane_cooldowns(&state);
    let (data, human) = render_lanes(&[], &[], None, &state, lane_cooldown_view(&cooldowns));
    assert_eq!(data["capacity_cooldowns"][0]["model"], "model");
    assert_eq!(data["capacity_cooldowns"][0]["account"]["label"], "work");
    assert!(
        data["capacity_cooldowns"][0]["remaining_seconds"]
            .as_i64()
            .is_some_and(|seconds| seconds > 0 && seconds <= 600)
    );
    assert!(human.contains("capacity cooldown codex/model [work]:"));
    Ok(())
}

#[test]
fn lanes_show_only_live_capacity_cooldowns() {
    let account = provenant_quota::AccountScope {
        adapter: "codex".into(),
        account_key: "key".into(),
        label: "work".into(),
    };
    let mut record = provenant_quota::AccountRecord::empty(account.clone());
    record.capacity_cooldowns = vec![provenant_quota::CapacityCooldown {
        extra: Default::default(),
        account,
        model: "model".into(),
        observed_unix_seconds: 100,
        expires_unix_seconds: 700,
    }];
    let mut human = String::new();
    let shown = render_cooldowns(&[record.clone()], 400, &mut human);
    assert_eq!(shown[0]["remaining_seconds"], 300);
    assert!(human.contains("codex/model [work]: 300s remaining"));
    assert!(render_cooldowns(&[record.clone()], 700, &mut String::new()).is_empty());
    assert!(render_cooldowns(&[record], 99, &mut String::new()).is_empty());
}

#[test]
fn delivery_state_text_retains_known_details_without_extensions() {
    use provenant_records::run::{WriterDeliveryStage, WriterDeliveryState};
    for wire in [
        json!({"state":"not_attempted"}),
        json!({"state":"no_new_commits"}),
        json!({"state":"failed","stage":"push"}),
        json!({"state":"delivered","pull_request":null}),
        json!({"state":"delivered","pull_request":"https://example.invalid/pr/1"}),
        json!({"state":"delivered","pull_request":"pr\u{001b}\n1"}),
    ] {
        let mut wire = wire;
        wire["future_note"] = json!("future-secret");
        let state: WriterDeliveryState = serde_json::from_value(wire.clone()).unwrap();
        let text = delivery_state_text(&state);
        match state {
            WriterDeliveryState::NotAttempted { .. } | WriterDeliveryState::NoNewCommits { .. } => {
            }
            WriterDeliveryState::Failed { stage, .. } => {
                assert_eq!(stage, WriterDeliveryStage::Push);
            }
            WriterDeliveryState::Delivered { pull_request, .. } => {
                assert_eq!(pull_request.as_deref(), wire["pull_request"].as_str());
                if pull_request.is_some() {
                    assert!(text.contains("pr1") || text.contains("https://example.invalid/pr/1"));
                }
            }
        }
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(!text.contains("future-secret"));
    }
}

#[test]
fn render_lanes_removes_peer_terminal_controls() -> Result<(), Box<dyn std::error::Error>> {
    let reason = "\x1b[31mreason\x1b[0m";
    let account = "\x1b]0;account\x07";
    let detail = "TOKEN_SENTINEL /private/source\n\r\t\u{0008}\u{202e}detail";
    let remote = "or\x1bigin";
    let branch = "main\u{009b}line";
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "peer-lane-render",
        "attempt_number": 1,
        "title": "Review\nchanges",
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
        "applied_route": {"model":"fixture/\x1b[32mmodel","account_label":account},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "write_grants": [{"source":"write_root","path":{"relative_to_root":{"encoding":"utf8","path":"work/shared"}},"scope":"subtree"}],
        "read_only_writable": ["locks"],
        "git_grants_digest": provenant_core::Digest::from_bytes([6; 32]),
        "queued_at": now,
        "priority": 10,
        "state": {"state":"terminal","outcome":"failed","exit_code":1,"finished_at":now,"reason":reason},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        },
        "skipped_routes": [{"route":"fixture/\x1b[33mfallback","kind":"unavailable","detail":detail}],
        "delivery": {"schema_version":1,"remote":remote,"branch":branch,"state":{"state":"not_attempted","future_note":"future-secret"}}
    }))?;
    let (data, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    let mut stale = lane.clone();
    stale.skipped_routes[0].cause =
        Some(provenant_records::RouteExclusionReason::StaleAvailability);
    let (stale_data, stale_human) = render_lanes_without_cooldowns(
        std::slice::from_ref(&stale),
        &[],
        None,
        Path::new("/state"),
    );
    let stale_skip = &stale_data["lanes"][0]["skipped_routes"][0];
    assert!(stale_skip["detail"].as_str().unwrap().contains("stale"));
    assert!(
        stale_skip["next"]
            .as_str()
            .unwrap()
            .contains("provenant quota tick")
    );
    assert!(stale_human.contains("stale"));
    assert!(!stale_skip.to_string().contains("TOKEN_SENTINEL"));
    assert!(!stale_human.contains("/private/source"));
    assert_eq!(
        human,
        format!(
            "{}/{}/{} Reviewchanges failed fixture/[32mmodel account=]0;account (the lane failed; inspect its result and local evidence)\n  skipped fixture/[33mfallback Unavailable: the selected route is unavailable; check route readiness with `provenant quota tick`\n  delivery origin:mainline NotAttempted head=unknown\n",
            RunId::from_bytes([2; 16]),
            TaskId::from_bytes([4; 16]),
            AttemptId::from_bytes([1; 16])
        )
    );
    assert!(!human.chars().any(|c| c.is_control() && c != '\n'));
    assert!(!human.contains('\u{202e}'));
    assert_eq!(data["lanes"][0]["title"], "Review\nchanges");
    assert_eq!(data["lanes"][0]["priority"], 10);
    assert!(data["lanes"][0].get("requested_priority").is_none());
    lane.requested_priority = Some(10);
    lane.priority = Some(15);
    let (different, _) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert_eq!(different["lanes"][0]["priority"], 15);
    assert_eq!(different["lanes"][0]["requested_priority"], 10);
    assert!(
        data["lanes"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("lane failed")
    );
    assert!(
        !data["lanes"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("reason")
    );
    assert_eq!(data["lanes"][0]["write_grants"][0]["source"], "write_root");
    assert_eq!(data["lanes"][0]["write_grants"][0]["scope"], "subtree");
    assert_eq!(
        data["lanes"][0]["write_grants"][0]["path"]["relative"]["path"],
        "work/shared"
    );
    assert_eq!(
        data["lanes"][0]["read_only_writable"],
        json!([{"kind":"relative", "relative":{"encoding":"utf8", "path":"locks"}}])
    );
    assert_eq!(
        data["lanes"][0]["git_grants_digest"],
        json!(provenant_core::Digest::from_bytes([6; 32]))
    );
    assert_eq!(data["lanes"][0]["account"], json!(account));
    let skipped = &data["lanes"][0]["skipped_routes"][0];
    assert_eq!(skipped["code"], "REFUSED");
    assert!(skipped["detail"].as_str().unwrap().contains("unavailable"));
    assert!(
        skipped["next"]
            .as_str()
            .unwrap()
            .contains("provenant quota tick")
    );
    let skipped_wire = skipped.to_string();
    for text in [human.as_str(), skipped_wire.as_str()] {
        assert!(!text.contains("TOKEN_SENTINEL"));
        assert!(!text.contains("/private/source"));
    }
    assert_eq!(data["lanes"][0]["delivery"]["remote"], json!(remote));
    assert_eq!(data["lanes"][0]["delivery"]["branch"], json!(branch));
    assert_eq!(
        data["lanes"][0]["delivery"]["state"]["future_note"],
        "future-secret"
    );
    assert!(!human.contains("future-secret"));
    lane.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::ModelUnavailable,
        exit_code: Some(1),
        finished_at: now,
        reason: Some("provider capacity: Selected model is at capacity".into()),
    };
    lane.session = Some("session".into());
    let (data, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert_eq!(data["lanes"][0]["state"], "model_unavailable");
    assert!(human.contains("the model is unavailable"));
    assert!(!human.contains("Selected model is at capacity"));
    assert!(human.contains(&format!(
        "continue: provenant retry {} --prompt-file PROMPT",
        lane.attempt_id
    )));
    lane.session = None;
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(!human.contains("provenant retry"));
    assert!(human.contains("redispatch with another model"));
    lane.session = Some("session".into());
    if let run::AttemptState::Terminal { reason, .. } = &mut lane.state {
        *reason = Some("provider model unavailable: no route".into());
    }
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(!human.contains("provenant retry"));
    assert!(human.contains("redispatch with a corrected model"));
    lane.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::ReviewRequired,
        exit_code: Some(0),
        finished_at: now,
        reason: None,
    };
    lane.head = Some(provenant_core::GitOid::parse(&"a".repeat(40))?);
    lane.protected_paths = vec![provenant_records::PathWire::from_relative(
        &provenant_core::RelativePath::parse(".github/unsafe\npath.yml")?,
    )?];
    lane.delivery = None;
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert_eq!(data["lanes"][0]["state"], "review_required");
    assert_eq!(data["lanes"][0]["head"], "a".repeat(40));
    assert_eq!(
        data["lanes"][0]["protected_paths"][0]["relative"]["path"],
        ".github/unsafe\npath.yml"
    );
    assert!(human.contains(&format!(
        "review head={} paths=.github/unsafepath.yml",
        "a".repeat(40)
    )));
    Ok(())
}

#[test]
fn render_lanes_shows_review_line_when_review_required_without_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "review-no-paths",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state":"terminal","outcome":"failed","exit_code":1,"finished_at":now,"reason":null},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        }
    }))?;
    lane.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::ReviewRequired,
        exit_code: Some(0),
        finished_at: now,
        reason: None,
    };
    lane.protected_paths = vec![];
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(human.contains("  review head=unknown paths=\n"));
    assert!(human.contains(&format!(
        "  next: provenant lane deliver --accept-review {}\n",
        lane.attempt_id
    )));
    Ok(())
}

/// A lane for the lane-view tests: queued until a test sets its state.
fn lane_fixture(
    attempt: u8,
    task: u8,
    state: run::AttemptState,
) -> Result<run::AttemptRecord, Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([attempt; 16]),
        "run_id": RunId::from_bytes([9; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([task; 16]),
        "operation_id": format!("lane-{attempt}"),
        "attempt_number": attempt,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state":"queued"},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        }
    }))?;
    lane.state = state;
    Ok(lane)
}

#[test]
fn lanes_render_retry_lineage() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut failed = lane_fixture(
        1,
        4,
        run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::Failed,
            exit_code: Some(1),
            finished_at: now,
            reason: None,
        },
    )?;
    let mut retry = lane_fixture(
        2,
        4,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
    )?;
    retry.retry_of = Some(failed.attempt_id);
    failed.superseded_by = Some(retry.attempt_id);
    let (data, human) = render_lanes(
        &[failed.clone(), retry.clone()],
        &[],
        None,
        Path::new("/state"),
        None,
    );
    assert_eq!(data["lanes"][0]["superseded_by"], json!(retry.attempt_id));
    assert_eq!(data["lanes"][1]["retry_of"], json!(failed.attempt_id));
    assert_eq!(
        human,
        format!(
            "{}/{}/{} failed → retrying ({}) unresolved ({})\n{}/{}/{} queued (retry of {}) unresolved\n",
            failed.run_id,
            failed.task_id,
            failed.attempt_id,
            retry.attempt_id,
            run::Outcome::Failed.diagnostic_reason(),
            retry.run_id,
            retry.task_id,
            retry.attempt_id,
            failed.attempt_id,
        )
    );
    Ok(())
}

#[test]
fn lanes_give_a_settled_lane_its_settlement_reason() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let failed = lane_fixture(
        1,
        4,
        run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::Failed,
            exit_code: Some(1),
            finished_at: now,
            reason: Some("provider failed".into()),
        },
    )?;
    let queued = lane_fixture(
        2,
        4,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
    )?;
    let (data, _) =
        render_lanes_without_cooldowns(&[failed, queued], &[], None, Path::new("/state"));
    assert_eq!(
        data["lanes"][0]["settlement"]["reason"],
        run::Outcome::Failed.diagnostic_reason()
    );
    assert!(
        !data["lanes"][0]["settlement"]["reason"]
            .as_str()
            .unwrap()
            .contains("provider failed")
    );
    assert_eq!(data["lanes"][1]["settlement"], Value::Null);
    assert_eq!(data["lanes"][1]["reason"], Value::Null);
    Ok(())
}

#[test]
fn lanes_distinguish_exhausted_continuations_from_a_retry() -> Result<(), Box<dyn std::error::Error>>
{
    let now = WallTime::from_micros(100)?;
    let stopped = |reason: &str| run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Failed,
        exit_code: Some(0),
        finished_at: now,
        reason: Some(reason.into()),
    };
    let mut pending = lane_fixture(1, 4, stopped("model reported unfinished work"))?;
    let exhausted = lane_fixture(
        2,
        5,
        stopped("model reported unfinished work; continuations exhausted"),
    )?;
    let next = AttemptId::from_bytes([3; 16]);
    pending.superseded_by = Some(next);
    let (data, human) = render_lanes(&[pending, exhausted], &[], None, Path::new("/state"), None);
    assert_eq!(data["lanes"][0]["superseded_by"], json!(next));
    assert_eq!(data["lanes"][1]["state"], "failed");
    assert!(data["lanes"][1]["superseded_by"].is_null());
    assert_eq!(
        data["lanes"][1]["reason"],
        run::Outcome::Failed.diagnostic_reason()
    );
    assert!(human.contains("retrying"));
    assert!(!human.contains("model reported unfinished work"));
    assert!(!human.contains("continuations exhausted"));
    Ok(())
}

#[test]
fn newest_settled_of_names_the_settled_attempt_of_one_task()
-> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let settled = || run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Failed,
        exit_code: Some(1),
        finished_at: now,
        reason: None,
    };
    let running = || run::AttemptState::Running {
        extra: Default::default(),
    };
    // Newest first: a retry still running, then the attempt it replaced.
    let (retry, first) = (
        lane_fixture(2, 4, running())?,
        lane_fixture(1, 4, settled())?,
    );
    let (id, count, rule) =
        newest_settled_of(&[retry, first.clone()]).map_err(|fail| fail.message)?;
    assert_eq!((id, count, rule), (first.attempt_id, 2, "newest settled"));
    // Nothing has settled, so the newest attempt is the one named.
    let (newest, older) = (
        lane_fixture(3, 4, running())?,
        lane_fixture(2, 4, running())?,
    );
    let (id, _, rule) = newest_settled_of(&[newest.clone(), older]).map_err(|fail| fail.message)?;
    assert_eq!((id, rule), (newest.attempt_id, "newest"));
    // Attempts of two tasks are ambiguous, so RUN/TASK is asked for.
    let other = lane_fixture(4, 5, running())?;
    let error = newest_settled_of(&[newest, other])
        .err()
        .ok_or("two tasks were accepted")?;
    assert!(error.message.contains("2 tasks"), "{}", error.message);
    Ok(())
}

#[test]
fn render_lanes_points_at_the_provider_transcript() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "transcript-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": {"provider":"claude","model":"claude/claude-sonnet-5"},
        "session": "sess-1848",
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state": "terminal", "outcome": "rejected", "finished_at": now},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 0,
            "complete": true,
            "redacted": false
        }
    }))?;
    let root = tempfile::tempdir()?;
    let state = root.path().canonicalize()?;
    let attempt = artifact(&state, &lane, "");
    let transcript = attempt.join("provider-state/candidate-0/claude-config/projects/k");
    std::fs::create_dir_all(&transcript)?;
    std::fs::write(transcript.join("sess-1848.jsonl"), "{}\n")?;
    let (data, _) = render_lanes_without_cooldowns(&[lane.clone()], &[], None, &state);
    let found = &data["lanes"][0]["transcript"];
    assert_eq!(found["adapter"], "claude");
    assert_eq!(found["session"], "sess-1848");
    assert_eq!(
        found["path"],
        json!({"kind":"path", "root":"/", "relative":{"encoding":"utf8", "path":transcript.join("sess-1848.jsonl").strip_prefix("/")?.to_str().unwrap()}})
    );
    // Read-only Claude records its selected account's exact state-write path.
    // The lookup never follows links, so build the grant from the resolved temp root.
    let account = root
        .path()
        .canonicalize()?
        .join("accounts/claude/projects/project-key");
    std::fs::create_dir_all(&account)?;
    let path = account.join("sess-1848.jsonl");
    std::fs::write(&path, "{}\n")?;
    lane.write_grants = vec![run::WriteGrantRecord {
        extra: Default::default(),
        source: run::WriteSource::StateWrite,
        scope: run::PathScope::Exact,
        path: provenant_records::AbsolutePath::new(&path)?,
    }];
    let other = tempfile::tempdir()?;
    std::fs::create_dir_all(artifact(other.path(), &lane, ""))?;
    let (data, _) = render_lanes_without_cooldowns(&[lane], &[], None, other.path());
    assert_eq!(
        data["lanes"][0]["transcript"]["path"],
        json!({"kind":"path", "root":"/", "relative":{"encoding":"utf8", "path":path.strip_prefix("/")?.to_str().unwrap()}})
    );
    Ok(())
}

#[test]
fn render_lanes_omits_account_when_unresolved() -> Result<(), Box<dyn std::error::Error>> {
    let reason = "argv prompt exceeds 64 KiB";
    let now = WallTime::from_micros(100)?;
    let lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "prompt-refusal-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {
            "state": "terminal",
            "outcome": "rejected",
            "finished_at": now,
            "reason": reason
        },
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 65537,
            "complete": true,
            "redacted": false
        }
    }))?;
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert_eq!(
        human,
        format!(
            "{}/{}/{} rejected unresolved ({reason})\n",
            RunId::from_bytes([2; 16]),
            TaskId::from_bytes([4; 16]),
            AttemptId::from_bytes([1; 16]),
            reason = run::Outcome::Rejected.diagnostic_reason(),
        )
    );
    assert!(!human.contains("account="));
    assert!(data["lanes"][0].get("title").is_none());
    assert!(data["lanes"][0]["account"].is_null());
    assert_eq!(data["lanes"][0]["state"], "rejected");
    assert_eq!(
        data["lanes"][0]["reason"],
        json!(run::Outcome::Rejected.diagnostic_reason())
    );
    assert!(!human.contains(reason));
    assert!(data["lanes"][0]["applied_route"].is_null());
    Ok(())
}
#[test]
#[allow(clippy::disallowed_methods)] // The wait command has a real-time deadline.
fn wait_handles_a_stale_default_cursor() -> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane("run::tests::wait_handles_a_stale_default_cursor")? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("home");
    let project = root.join("checkout");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(root.join("state"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join("state"), std::fs::Permissions::from_mode(0o700))?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|e| e.message)?;
    let session = open(&edge, &call).map_err(|e| e.message)?;
    let make = |n: u64| -> Result<run::PreparedRun, Box<dyn std::error::Error>> {
        let mut prepared = trace_run(1, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        let mut bytes = [0; 16];
        bytes[8..].copy_from_slice(&n.to_be_bytes());
        prepared.run_id = RunId::from_bytes(bytes);
        bytes[0] = 1;
        let task = TaskId::from_bytes(bytes);
        bytes[0] = 2;
        prepared.operation_id = OperationId::parse(&format!("backlog-{n}"))?;
        prepared.request.tasks[0].task_id = task;
        prepared.attempts[0].task_id = task;
        prepared.attempts[0].attempt_id = AttemptId::from_bytes(bytes);
        Ok(prepared)
    };
    let seed = make(1)?;
    session.store.create(&seed, &call)?;
    session.store.withdraw(
        seed.attempts[0].attempt_id,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
        run::Outcome::Cancelled,
        "fixture",
        clock.wall_now()?,
        &call,
    )?;
    let cursor = session
        .store
        .last_event_sequence(seed.attempts[0].attempt_id, &call)?;
    session.store.ack_seen("default", cursor, &call)?;
    let active = make(2)?;
    session.store.create(&active, &call)?;
    let id = active.attempts[0].attempt_id;
    struct FetchHookGuard;
    impl Drop for FetchHookGuard {
        fn drop(&mut self) {
            wait::AFTER_FIRST_FETCH.with(|hook| *hook.borrow_mut() = None);
        }
    }
    let _hook_guard = FetchHookGuard;
    let context = session.context.clone();
    wait::AFTER_FIRST_FETCH.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let store = run::RunStore::open(&context, &call).unwrap();
            store
                .withdraw(
                    id,
                    run::AttemptState::Queued {
                        extra: Default::default(),
                    },
                    run::Outcome::Cancelled,
                    "fixture",
                    clock.wall_now().unwrap(),
                    &call,
                )
                .unwrap();
        }));
    });
    let picks = Picks {
        targets: &[],
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    let (data, _, timed_out) = wait_command(
        &session,
        &picks,
        Condition::Any,
        Instant::now() + Duration::from_secs(5),
        clock.wall_now()?,
        &edge,
    )
    .map_err(|e| e.message)?;
    assert!(!timed_out);
    assert!(
        data["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lane| lane["attempt_id"] == json!(id))
    );
    let (next, _, _) = wait_command(
        &session,
        &picks,
        Condition::Any,
        Instant::now() + Duration::from_secs(5),
        clock.wall_now()?,
        &edge,
    )
    .map_err(|e| e.message)?;
    assert!(next["lanes"].as_array().unwrap().is_empty());
    assert!(next.get("settled_backlog").is_none());
    Ok(())
}

#[test]
fn local_wait_observes_settlement_and_renders_pending_details()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let store = trace_store(&temp, &call)?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let context = run::ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: true,
    };
    let session = Session { store, context };
    let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
    session.store.create(&prepared, &call)?;
    let id = prepared.attempts[0].attempt_id;
    let targets = [Target {
        selection: None,
        run: Some(prepared.run_id),
        task: None,
        attempt: Some(id),
    }];
    let picks = Picks {
        targets: &targets,
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    let mut fetch = candidates(&session, &picks, None);
    let first = fetch(Duration::from_secs(1)).map_err(|fail| fail.message)?;
    assert_eq!(first.len(), 1);
    let (data, _) = render_settled(&session, &first, None).map_err(|fail| fail.message)?;
    assert_eq!(
        data["lanes"][0]["waiting_for"],
        "launch: not yet handed to a guardian"
    );
    let mut reads = 0;
    let finished_at = clock.wall_now()?;
    let attempt_path = session
        .context
        .state_root
        .as_path()
        .join("runs")
        .join(prepared.run_id.to_string())
        .join("attempts")
        .join(id.to_string());
    let directory = provenant_os::fs::Directory::open_anchor(&attempt_path)?;
    let mut custody = Some(directory.try_lock(
        std::ffi::OsStr::new("custody.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?);
    let waited = watch::wait(
        |remaining| {
            reads += 1;
            if reads == 2 {
                session.store.withdraw(
                    id,
                    run::AttemptState::Queued {
                        extra: Default::default(),
                    },
                    run::Outcome::Cancelled,
                    "fixture settlement",
                    finished_at,
                    &call,
                )?;
            }
            fetch(remaining)
        },
        picks.scope().map_err(|fail| fail.message)?,
        Condition::All,
        Duration::from_secs(1),
        Duration::from_millis(1),
    )
    .map_err(|fail| fail.message)?;
    assert!(waited.met);
    assert_eq!(waited.lanes.len(), 1);
    assert!(matches!(
        waited.lanes[0].state,
        run::AttemptState::Terminal {
            outcome: run::Outcome::Cancelled,
            ..
        }
    ));
    // The wait returns the sealed decision and reports retained custody
    // separately, instead of holding the terminal state back.
    assert_eq!(reads, 2, "a terminal decision ends the wait");
    assert_eq!(waited.lanes[0].custody_pending, Some(true));
    assert!(!session.store.custody_released(&waited.lanes[0], &call)?);
    drop(custody.take());
    assert!(session.store.custody_released(&waited.lanes[0], &call)?);
    Ok(())
}

#[test]
fn render_lanes_says_what_an_unstarted_lane_waits_for() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "wait-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 1,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state": "starting"},
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 1,
            "complete": true,
            "redacted": false
        }
    }))?;
    let wait = run::LaneWait {
        extra: Default::default(),
        attempt_id: lane.attempt_id,
        lane_class: run::LaneClass::ReadOnly,
        waiting_for: "read-only startup spacing: 1.2 s left of 2.0 s".into(),
    };
    let (preparing, _) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert_eq!(preparing["lanes"][0]["waiting_for"], "launch preparation");
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[wait], None, Path::new("/state"));
    assert!(
        human.contains("starting unresolved (read-only startup spacing: 1.2 s left of 2.0 s)"),
        "{human}"
    );
    let row = &data["lanes"][0];
    assert_eq!(row["lane_class"], "read_only");
    assert_eq!(
        row["waiting_for"],
        "read-only startup spacing: 1.2 s left of 2.0 s"
    );
    assert_eq!(row["reason"], row["waiting_for"]);
    Ok(())
}
#[test]
fn render_lanes_surfaces_provider_silence_while_running() -> Result<(), Box<dyn std::error::Error>>
{
    let now = WallTime::from_micros(100)?;
    let mut lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "silent-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 1,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {"state":"running"},
        "usage": {},
        "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"},"digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
    }))?;
    lane.provider_silent_for_ms.0 = Some(1_800_000);
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert!(human.contains("provider silent for 1800s; provider still running"));
    assert_eq!(
        data["lanes"][0]["reason"],
        "provider silent for 1800s; provider still running"
    );
    assert_eq!(
        data["lanes"][0]["waiting_for"],
        "provider silent for 1800s; provider still running"
    );
    assert_eq!(data["lanes"][0]["state"], "running");
    Ok(())
}
#[test]
fn render_lanes_reports_uncommitted_without_echoing_dirty_path_prose()
-> Result<(), Box<dyn std::error::Error>> {
    let reason = "writer left uncommitted edits and no new commit: src/a.rs, b.txt";
    let now = WallTime::from_micros(100)?;
    let lane: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1,
        "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]),
        "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]),
        "operation_id": "uncommitted-render",
        "attempt_number": 1,
        "revision": 1,
        "generation": 0,
        "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
        "applied_route": null,
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
        "queued_at": now,
        "state": {
            "state": "terminal",
            "outcome": "uncommitted",
            "finished_at": now,
            "reason": reason
        },
        "usage": {},
        "prompt": {
            "path": {"encoding":"utf8","path":"prompt.txt"},
            "digest": CanonicalV1::file_digest(b""),
            "bytes": 1,
            "complete": true,
            "redacted": false
        }
    }))?;
    assert_eq!(settled(&lane.state), (true, false));
    let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
    assert!(
        human.contains(&format!(
            " uncommitted unresolved ({})\n",
            run::Outcome::Uncommitted.diagnostic_reason()
        )),
        "{human}"
    );
    assert_eq!(data["lanes"][0]["state"], "uncommitted");
    assert_eq!(
        data["lanes"][0]["reason"],
        json!(run::Outcome::Uncommitted.diagnostic_reason())
    );
    assert!(!human.contains("src/a.rs"));
    assert!(!human.contains("b.txt"));
    assert!(!serde_json::to_string(&data)?.contains(reason));
    Ok(())
}
#[test]
fn lane_access_refuses_credential_and_enclosing_roots() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let credential = temp.path().join(".ssh").join("nested");
    std::fs::create_dir_all(&credential)?;
    let mut roots = vec![std::fs::canonicalize(&credential)?];
    // The home itself and its `.config` are enclosing roots; use the host's
    // real directories so the check sees a conventional home.
    let host_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_dir());
    if let Some(home) = &host_home {
        if home.join(".config").is_dir() {
            roots.push(home.join(".config"));
        }
        roots.push(home.clone());
    }
    assert!(roots.len() >= 2);
    let home = host_home
        .as_deref()
        .unwrap_or(Path::new("/fixture/host-home"));
    for root in roots {
        for (flag, read) in [("--read-root", true), ("--write-root", false)] {
            let access = crate::Access {
                attach: Vec::new(),
                mode: Some(Mode::ReadOnly),
                worktree: None,
                read_root: if read { vec![root.clone()] } else { vec![] },
                write_root: if read { vec![] } else { vec![root.clone()] },
                network: None,
                capability: vec![],
                no_capability: Vec::new(),
            };
            let fail = lane_access(&access, &temp.path().join("state"), home).unwrap_err();
            assert!(fail.message.contains(flag), "{}", fail.message);
            assert!(
                fail.message.contains(&root.display().to_string()),
                "{}",
                fail.message
            );
        }
    }
    Ok(())
}

#[test]
fn lane_access_canonicalises_attachment_files() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let image = temp.path().join("shot.png");
    std::fs::write(&image, b"image")?;
    let access = crate::Access {
        attach: vec![image.clone()],
        mode: None,
        worktree: None,
        read_root: vec![],
        write_root: vec![],
        network: None,
        capability: vec![],
        no_capability: vec![],
    };
    let granted = lane_access(
        &access,
        &temp.path().join("state"),
        Path::new("/fixture/home"),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(granted.attachments, vec![std::fs::canonicalize(image)?]);
    assert!(granted.read_roots.is_empty());
    Ok(())
}

#[test]
fn lane_access_refuses_a_root_enclosing_an_injected_nonconventional_home()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let team = temp.path().join("srv/team");
    let home = team.join("alice");
    let project = home.join("project");
    let config = home.join(".config");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&config)?;
    let team = std::fs::canonicalize(team)?;
    let home = std::fs::canonicalize(home)?;
    let project = std::fs::canonicalize(project)?;
    let config = std::fs::canonicalize(config)?;
    let state = temp.path().join("state");
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: vec![],
        no_capability: Vec::new(),
    };
    for root in [&team, &home, &config] {
        for (flag, read) in [("--read-root", true), ("--write-root", false)] {
            let fail = lane_access(
                &access(
                    if read { vec![root.clone()] } else { vec![] },
                    if read { vec![] } else { vec![root.clone()] },
                ),
                &state,
                &home,
            )
            .unwrap_err();
            assert!(fail.message.contains(flag), "{}", fail.message);
            assert!(
                fail.message.contains(&root.display().to_string()),
                "{}",
                fail.message
            );
        }
    }
    let granted = lane_access(
        &access(vec![project.clone()], vec![project.clone()]),
        &state,
        &home,
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(granted.read_roots, vec![project.clone()]);
    assert_eq!(granted.write_roots, vec![project]);
    Ok(())
}

/// Entry agrees with compile: a read root in a Codex standalone release
/// passes the credential-root refusal, while a write root there stays
/// refused with the flag and the root named.
#[test]
fn lane_access_allows_codex_standalone_read_roots() -> Result<(), Box<dyn std::error::Error>> {
    // The rule is lexical, so check it without the filesystem first.
    let shape = Path::new(
        "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin/bin",
    );
    assert!(
        !provenant_confinement::refuses_lane_root(shape, true, None),
        "reads pass entry"
    );
    assert!(
        provenant_confinement::refuses_lane_root(shape, false, None),
        "writes stay strict"
    );
    // Against a machine's real home: build a temp release and walk it
    // through `lane_access`. A lane can seal `~/.codex`; then the fixture
    // cannot exist and the filesystem part is skipped silently.
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(());
    };
    let releases = Path::new(&home).join(".codex/packages/standalone/releases");
    if std::fs::create_dir_all(&releases).is_err() {
        return Ok(());
    }
    let Ok(temp) = tempfile::tempdir_in(&releases) else {
        return Ok(());
    };
    let release = std::fs::canonicalize(temp.path())?;
    let state = tempfile::tempdir()?;
    let state = std::fs::canonicalize(state.path())?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: vec![],
        no_capability: Vec::new(),
    };
    let granted = lane_access(
        &access(vec![release.clone()], vec![]),
        &state.join("grants"),
        Path::new(&home),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(granted.read_roots, vec![release.clone()]);
    let fail = lane_access(
        &access(vec![], vec![release.clone()]),
        &state.join("writes"),
        Path::new(&home),
    )
    .unwrap_err();
    assert_eq!(
        fail.message,
        format!(
            "--write-root {} is or encloses a credential root",
            release.display()
        )
    );
    Ok(())
}

#[test]
fn entry_root_table_matches_confinement() {
    let standalone =
        "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
    let home = Path::new("/fixture/host-home");
    for (path, read_allowed, write_allowed) in [
        ("/Users/alice/.ssh", false, false),
        ("/Users/alice/.config", false, false),
        (standalone, true, false),
        ("/Users/alice/project", true, true),
    ] {
        for (flag, allowed) in [
            ("--read-root", read_allowed),
            ("--write-root", write_allowed),
        ] {
            let refused = refused_root(flag, Path::new(path), Path::new(path), home);
            assert_eq!(refused.is_none(), allowed, "{flag} {path}");
            if let Some(AccessError::CredentialRoot {
                flag: named,
                path: named_path,
            }) = refused
            {
                assert_eq!(named, flag);
                assert_eq!(named_path, Path::new(path));
            }
        }
    }
    // A host home outside /Users and /home is refused at entry, so entry
    // and compile agree without this test depending on the host's HOME.
    let home = Path::new("/srv/team/alice");
    for (path, read_allowed, write_allowed) in [
        ("/srv/team", false, false),
        ("/srv/team/alice", false, false),
        ("/srv/team/alice/.config", false, false),
        ("/srv/team/alice/project", true, true),
    ] {
        for (flag, allowed) in [
            ("--read-root", read_allowed),
            ("--write-root", write_allowed),
        ] {
            let refused = refused_root(flag, Path::new(path), Path::new(path), home);
            assert_eq!(refused.is_none(), allowed, "{flag} {path}");
        }
    }
}

#[test]
fn lane_access_maps_network_capability_and_roots() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let project = temp.path().join("project");
    provenant_os::fixture::private_state_dir(&state)?;
    std::fs::create_dir_all(&project)?;
    let access = |mode, network, capability: &[&str], write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(mode),
        worktree: None,
        read_root: Vec::new(),
        write_root,
        network,
        capability: capability.iter().map(|name| name.to_string()).collect(),
        no_capability: Vec::new(),
    };
    let network = |mode, switch, capability: &[&str]| {
        lane_access(
            &access(mode, switch, capability, Vec::new()),
            &state,
            Path::new("/fixture/host-home"),
        )
        .map(|access| access.network)
        .map_err(|fail| fail.message)
    };
    assert_eq!(network(Mode::WorktreeWrite, None, &[]), Ok(None));
    assert_eq!(network(Mode::ReadOnly, None, &["postgres"]), Ok(None));
    for mode in [Mode::ReadOnly, Mode::WorktreeWrite] {
        for (capability, expected) in [(Vec::new(), false), (vec!["postgres"], true)] {
            let declared = lane_access_checked(
                &access(mode, None, &capability, Vec::new()),
                &state,
                Path::new("/fixture/host-home"),
            )?;
            assert_eq!(declared.database_ipc, expected);
        }
    }
    assert_eq!(
        network(Mode::WorktreeWrite, Some(Switch::Off), &["postgres"]),
        Ok(Some(run::NetworkAccess::Local))
    );
    assert_eq!(
        network(Mode::ReadOnly, Some(Switch::Off), &["postgres"]),
        Ok(Some(run::NetworkAccess::Local))
    );
    assert_eq!(
        network(Mode::ReadOnly, Some(Switch::On), &[]),
        Ok(Some(run::NetworkAccess::Open))
    );
    let unknown = lane_access_checked(
        &access(Mode::ReadOnly, None, &["rust"], Vec::new()),
        &state,
        Path::new("/fixture/host-home"),
    )?;
    assert!(!unknown.database_ipc);
    let granted = lane_access(
        &access(Mode::WorktreeWrite, None, &[], vec![project.clone()]),
        &state,
        Path::new("/fixture/host-home"),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(
        granted.write_roots,
        [provenant_os::fs::canonicalize(&project)?]
    );
    for refused in [
        state.join("projects"),
        temp.path().to_path_buf(),
        temp.path().join("absent"),
    ] {
        std::fs::create_dir_all(state.join("projects"))?;
        assert!(
            lane_access(
                &access(Mode::WorktreeWrite, None, &[], vec![refused]),
                &state,
                Path::new("/fixture/host-home"),
            )
            .is_err()
        );
    }
    Ok(())
}
#[test]
fn lane_access_refuses_write_root_overlapping_the_worktree()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let worktree = temp.path().join("work").join("tree");
    let inside = worktree.join("src");
    provenant_os::fixture::private_state_dir(&state)?;
    std::fs::create_dir_all(&inside)?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::WorktreeWrite),
        worktree: Some(worktree.clone()),
        read_root,
        write_root,
        network: None,
        capability: Vec::new(),
        no_capability: Vec::new(),
    };
    for overlapping in [worktree.clone(), temp.path().join("work")] {
        let expected = provenant_os::fs::canonicalize(&overlapping)?;
        assert!(matches!(
            lane_access_checked(
                &access(Vec::new(), vec![overlapping]),
                &state,
                Path::new("/fixture/host-home"),
            ),
            Err(AccessError::WorktreeOverlap(path)) if path == expected
        ));
    }
    // A read root on the worktree and a write root beneath it stay valid.
    assert!(
        lane_access(
            &access(vec![worktree.clone()], vec![inside]),
            &state,
            Path::new("/fixture/host-home"),
        )
        .is_ok()
    );
    Ok(())
}
#[test]
fn lane_access_reads_installed_release_skills_only() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let releases = state.join("releases");
    let release = releases.join(provenant_records::SourceDigest::from_bytes([1; 32]).to_string());
    let skills = release.join("skills");
    std::fs::create_dir_all(&skills)?;
    std::fs::create_dir_all(state.join("projects"))?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        attach: Vec::new(),
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: Vec::new(),
        no_capability: Vec::new(),
    };
    // Preset roles read bundled skills from the installed release.
    let granted = lane_access(
        &access(vec![skills.clone()], Vec::new()),
        &state,
        Path::new("/fixture/host-home"),
    )
    .map_err(|fail| fail.message)?;
    assert_eq!(
        granted.read_roots,
        [provenant_os::fs::canonicalize(&skills)?]
    );
    assert!(
        lane_access(
            &access(vec![release.clone()], Vec::new()),
            &state,
            Path::new("/fixture/host-home"),
        )
        .is_ok()
    );
    for invalid in ["not-a-digest", "sha256:abc", ".provenant-staging.fixture"] {
        let root = releases.join(invalid).join("skills");
        std::fs::create_dir_all(&root)?;
        assert!(matches!(
            lane_access_checked(
                &access(vec![root], Vec::new()),
                &state,
                Path::new("/fixture/host-home"),
            ),
            Err(AccessError::StateOverlap { .. })
        ));
    }
    for refused in [
        access(vec![releases.clone()], Vec::new()),
        access(vec![state.join("projects")], Vec::new()),
        access(Vec::new(), vec![release]),
        access(Vec::new(), vec![skills.clone()]),
    ] {
        assert!(matches!(
            lane_access_checked(&refused, &state, Path::new("/fixture/host-home"),),
            Err(AccessError::StateOverlap { .. })
        ));
    }
    Ok(())
}
#[test]
fn paged_output_never_splits_a_character() {
    let bytes = "a☀b".as_bytes();
    let mut chunk = run::OutputChunk {
        bytes: bytes[..3].to_vec(),
        len: bytes.len() as u64,
        offset: 0,
        next_offset: 3,
        eof: false,
        clipped: true,
        complete: true,
        encoding: "utf8-fragment",
        digest: provenant_records::CanonicalV1::file_digest(b""),
    };
    align_utf8_tail(&mut chunk);
    assert_eq!(chunk.bytes, b"a");
    assert_eq!(chunk.next_offset, 1);
    assert_eq!(chunk.encoding, "utf8");
}
#[test]
fn quota_reason_uses_safe_fact_without_provider_message() {
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: RunId::from_bytes([1; 16]),
            attempt: AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        termination: Some(provenant_os::exec::Termination::Exit(1)),
        never_spawned: false,
        launch_refusal: None,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        cancel_delivered_before_exit: None,
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: false,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::NeverSpawned,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    };
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/a-free".into()),
        ..Default::default()
    };
    let registry = Registry {
        roots: None,
        home: None,
        assets: None,
        cwd: None,
        state_root: None,
        project_id: None,
        host: None,
        cancel: None,
        repo_facts: Default::default(),
    };
    for (message, _expected) in [
        (
            "Free allowance exhausted. See https://opencode.ai/go\\quota",
            "Free allowance exhausted. See https://opencode.ai/go\\quota",
        ),
        (
            "Free allowance exhausted TOKEN_SENTINEL /private/source \u{1b} ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "Free allowance exhausted for [REDACTED]",
        ),
    ] {
        let event = json!({"type":"provider.quota","response":{"body":{"error":{"type":"FreeUsageLimitError","message":message}}}});
        let decoded = registry
            .decode(
                &route,
                event.to_string().as_bytes(),
                &exit,
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
        assert_eq!(
            decoded.terminal_cause,
            Some(run::TerminalCause::FreePoolQuota)
        );
        let reason = decoded.reason.as_deref().unwrap();
        assert!(reason.contains("free tier exhausted"));
        assert!(reason.contains("wait"));
        assert!(!reason.contains(message));
        assert!(!reason.contains("ghp_"));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/source"));
        assert!(!reason.contains('\u{1b}'));
    }
    let event = json!({"type":"provider.quota","message":"☀".repeat(300_000)});
    let decoded = registry
        .decode(
            &route,
            event.to_string().as_bytes(),
            &exit,
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
    let reason = decoded.reason.unwrap();
    assert!(
        reason.len() <= 8192,
        "reason must leave room in the terminal record"
    );
    assert!(!reason.contains('☀'));
    assert!(reason.contains("quota"));
    assert!(reason.contains("wait"));
}

fn clean_exit() -> ExitObservation {
    ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: RunId::from_bytes([1; 16]),
            attempt: AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        termination: Some(provenant_os::exec::Termination::Exit(0)),
        never_spawned: false,
        launch_refusal: None,
        stop_reason: None,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        cancel_delivered_before_exit: None,
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: false,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: SettlementEvidence::NeverSpawned,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    }
}

#[test]
fn opencode_stderr_quota_settles_and_records_refusal() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use provenant_quota::Admission;
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    use std::os::unix::fs::PermissionsExt;
    if crate::fabric::in_lane() {
        // A lane suppresses the host quota writes this test observes.
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let provider_state = state.join("runs/run/attempts/attempt/provider-state");
    let argv = ["/bin/sh", "-c", "printf '%s\\n' 'AI.Error.QuotaExceeded: Go usage limit exceeded retryAfterMs=60000' >&2; exit 1"].map(std::ffi::OsString::from);
    let mut options = ExecOptions::new(&argv, temp.path(), Duration::from_secs(30));
    options.stdout = StreamPolicy::Capture { limit: 8192 };
    options.stderr = StreamPolicy::Capture { limit: 8192 };
    let output = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(output.termination, Termination::Exit(1));
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode-go/example".into()),
        account_key: Some("go".into()),
        account_label: Some("go".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry::default().decode_captures(
        &route,
        &output.stdout.bytes,
        &output.stderr.bytes,
        &exit,
        &provider_state,
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
    let records = crate::quota::read_cache_at(&state)?;
    assert_eq!(
        records.len(),
        1,
        "host refusal missing; lane environment suppresses host quota writes"
    );
    let refusal = &records[0].refusals[0];
    assert_eq!(refusal.pool.as_deref(), Some("opencode-go"));
    assert_eq!(
        refusal.resets_unix_seconds,
        Some(refusal.observed_unix_seconds + 60)
    );
    let resolved = ResolvedRoute {
        extra: Default::default(),
        adapter: "opencode".into(),
        model: "opencode-go/example".into(),
        effort: None,
        catalogue_revision: "fixture".into(),
        account_key: Some("go".into()),
        host: "host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: i64::MAX,
        },
        confidentiality: RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    };
    let pools = vec!["opencode-go".into()];
    let check = |now_unix_seconds| {
        Admission {
            policies: &[],
            now_unix_seconds,
            records: &records,
            pools: &pools,
        }
        .check(&resolved)
    };
    assert!(check(refusal.observed_unix_seconds).is_err());
    assert!(check(refusal.observed_unix_seconds + 60).is_ok());
    Ok(())
}

#[test]
fn opencode_stderr_quota_text_does_not_overturn_a_successful_run()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let provider_state = state.join("runs/run/attempts/attempt/provider-state");
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(0));
    let decoded = Registry::default().decode_captures(
        &opencode_decode_route(),
        include_bytes!("../../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"),
        b"AI.Error.QuotaExceeded: Go usage limit exceeded retryAfterMs=60000\n",
        &exit,
        &provider_state,
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert!(
        crate::quota::read_cache_at(&state)
            .map(|records| records.iter().all(|record| record.refusals.is_empty()))
            .unwrap_or(true),
        "a successful run must not persist a quota refusal"
    );
    Ok(())
}

#[test]
fn opencode_unavailable_model_reaches_the_decoded_outcome() {
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/glm-5.3-flas-free".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry::default()
        .decode(
            &route,
            include_bytes!("../../../../tests/fixtures/providers/opencode/model-unavailable.jsonl"),
            &exit,
            Path::new("/nonexistent"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    let reason = decoded.reason.as_deref().unwrap();
    assert!(reason.starts_with("provider model unavailable"), "{reason}");
    assert!(!reason.contains("opencode/misspelt-free"), "{reason}");
    assert!(reason.contains("nearest catalogued IDs:"));
    assert!(decoded.observed_route.is_none());
}

#[test]
fn unlisted_codex_generic_failure_does_not_suggest_catalogued_models() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-luna".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry::default()
        .decode(&route, b"", &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_ne!(decoded.outcome, run::Outcome::Succeeded);
    assert!(
        !decoded
            .reason
            .unwrap_or_default()
            .contains("nearest catalogued IDs")
    );
}

#[test]
fn codex_capacity_reaches_the_decoded_outcome() {
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        ..Default::default()
    };
    let stream = format!(
        "{}\n{}\n",
        json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
        json!({"type":"turn.failed","error":{"message":"Selected model is at capacity. TOKEN_SENTINEL /private/source","codex_error_info":"server_overloaded"}})
    );
    let decoded = Registry::default()
        .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    assert_eq!(
        decoded.session.as_deref(),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
}

#[test]
fn capacity_store_failure_does_not_change_settlement() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    let account_dir = state.join("quota/key");
    std::fs::create_dir_all(&account_dir)?;
    for dir in [&state, &state.join("quota"), &account_dir] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let bad_record = account_dir.join("codex.json");
    std::fs::write(&bad_record, b"invalid quota record")?;
    std::fs::set_permissions(&bad_record, std::fs::Permissions::from_mode(0o600))?;
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: temp.path().join("checkout"),
            state_root: state.clone(),
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    // The malformed on-disk record makes the store update fail after the
    // settlement path creates the lock. The lock proves it attempted the write.
    assert!(!state.join("quota/quota.lock").exists());
    let decoded = Registry::default().decode_captures(
        &route,
        stream,
        &[],
        &exit,
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert!(state.join("quota/quota.lock").is_file());
    assert_eq!(std::fs::read(&bad_record)?, b"invalid quota record");
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    Ok(())
}

#[test]
fn nested_capacity_settlement_does_not_write_shared_cooldown()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsString;
    const MARKER: &str = "PROVENANT_TEST_NESTED_CAPACITY_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::nested_capacity_settlement_does_not_write_shared_cooldown".into(),
            "--nocapture".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key != MARKER && key != provenant_core::RUN_STATE_DIRECTORY_ENV)
            .collect();
        env.push((OsString::from(MARKER), OsString::from("1")));
        env.push((
            OsString::from(provenant_core::RUN_STATE_DIRECTORY_ENV),
            OsString::from("nested"),
        ));
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65_536 };
        options.stderr = StreamPolicy::Capture { limit: 65_536 };
        let out = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    let registry = Registry {
        roots: Some(&roots),
        ..Default::default()
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(Termination::Exit(1));
    let decoded = registry.decode(&route, stream, &exit, Path::new("/nonexistent"))?;
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    assert!(!roots.state.as_path().join("quota").exists());
    Ok(())
}

#[test]
fn invalid_sealed_root_never_writes_to_registry_root() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsString;
    const MARKER: &str = "PROVENANT_TEST_INVALID_CAPACITY_ROOT";
    if std::env::var_os(MARKER).is_none() {
        let argv = vec![
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::invalid_sealed_root_never_writes_to_registry_root".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| key != MARKER && key != provenant_core::RUN_STATE_DIRECTORY_ENV)
            .collect();
        env.push((OsString::from(MARKER), OsString::from("1")));
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65_536 };
        options.stderr = StreamPolicy::Capture { limit: 65_536 };
        let out = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("registry"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let registry = Registry {
        roots: Some(&roots),
        ..Default::default()
    };
    let protected = temp.path().join("protected");
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: protected.clone(),
            state_root: protected,
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(Termination::Exit(1));
    let decoded = registry.decode_captures(
        &route,
        stream,
        &[],
        &exit,
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert!(
        decoded
            .reason
            .unwrap_or_default()
            .contains("capacity cooldown configuration unavailable")
    );
    assert!(!roots.state.as_path().join("quota").exists());
    Ok(())
}

/// Population of OpenCode's own session record: the synthetic v2
/// `session_message` database the dispatch tests pair with the fixtures.
fn seed_opencode_session(
    data: &Path,
    session: &str,
    provider: &str,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data)?;
    let db = rusqlite::Connection::open(data.join("opencode.db"))?;
    db.execute_batch("CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")?;
    db.execute(
        "INSERT INTO session_message VALUES (?1, 'assistant', ?2)",
        (
            session,
            serde_json::to_string(&json!({"model": {"providerID": provider, "id": model}}))?,
        ),
    )?;
    Ok(())
}

const OPENCODE_DECODE_XDG_MARKER: &str = "PROVENANT_TEST_OPENCODE_DECODE_XDG";

fn opencode_decode_route() -> run::RouteRecord {
    run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/glm-5.3-flash-free".into()),
        ..Default::default()
    }
}

fn opencode_decode_holder_context(
    temp: &Path,
) -> Result<run::HolderContext, Box<dyn std::error::Error>> {
    Ok(run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.join("home"),
            checkout: temp.join("checkout"),
            state_root: temp.join("state"),
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    })
}

/// Re-run `test` in a confined child process whose environment omits
/// `XDG_DATA_HOME` (when `xdg` is `None`) or pins it to `xdg`, so the
/// decode sees a controlled value without mutating this process's env.
fn run_opencode_decode_child(
    test: &str,
    xdg: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::ffi::OsString;
    let argv = vec![
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        format!("run::tests::{test}").into(),
        "--nocapture".into(),
    ];
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            key != OPENCODE_DECODE_XDG_MARKER
                && key != "XDG_DATA_HOME"
                && key != provenant_core::RUN_STATE_DIRECTORY_ENV
        })
        .collect();
    env.push((
        OsString::from(OPENCODE_DECODE_XDG_MARKER),
        OsString::from("1"),
    ));
    if let Some(xdg) = xdg {
        env.push(("XDG_DATA_HOME".into(), xdg.as_os_str().into()));
    }
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65_536 };
    options.stderr = StreamPolicy::Capture { limit: 65_536 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
    assert!(!out.timed_out, "{out:?}");
    Ok(())
}

#[test]
fn opencode_decode_with_context_reads_home_data_when_xdg_is_unset()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(OPENCODE_DECODE_XDG_MARKER).is_none() {
        run_opencode_decode_child(
            "opencode_decode_with_context_reads_home_data_when_xdg_is_unset",
            None,
        )?;
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let context = opencode_decode_holder_context(temp.path())?;
    seed_opencode_session(
        &context.roots.home.join(".local/share/opencode"),
        "ses_x",
        "opencode",
        "glm-5.3-flash-free",
    )?;
    // The success fixture's stream reports no model; the session record
    // beside the holder home supplies it through `decode_captures`.
    let decoded = Registry::default().decode_captures(
        &opencode_decode_route(),
        include_bytes!("../../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"),
        &[],
        &clean_exit(),
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.session.as_deref(), Some("ses_x"));
    assert_eq!(
        decoded
            .observed_route
            .as_ref()
            .and_then(|r| r.model.as_deref()),
        Some("opencode/opencode/glm-5.3-flash-free")
    );
    Ok(())
}

#[test]
fn absolute_xdg_data_home_wins_over_home_for_decode_with_context()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(OPENCODE_DECODE_XDG_MARKER).is_none() {
        let xdg = tempfile::tempdir()?;
        seed_opencode_session(
            &xdg.path().join("opencode"),
            "ses_x",
            "opencode",
            "beam-free",
        )?;
        run_opencode_decode_child(
            "absolute_xdg_data_home_wins_over_home_for_decode_with_context",
            Some(xdg.path()),
        )?;
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let context = opencode_decode_holder_context(temp.path())?;
    // A differing home record proves `XDG_DATA_HOME` took precedence.
    seed_opencode_session(
        &context.roots.home.join(".local/share/opencode"),
        "ses_x",
        "opencode",
        "home-decoy",
    )?;
    let decoded = Registry::default().decode_captures(
        &opencode_decode_route(),
        include_bytes!("../../../../tests/fixtures/providers/opencode/v2.2.0-success.jsonl"),
        &[],
        &clean_exit(),
        Path::new("/nonexistent"),
        Some(&context),
    )?;
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(
        decoded
            .observed_route
            .as_ref()
            .and_then(|r| r.model.as_deref()),
        Some("opencode/opencode/beam-free")
    );
    Ok(())
}

#[test]
fn capacity_guardian_context_records_and_reports_config_failure()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::DirBuilderExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().join("state");
    std::fs::DirBuilder::new().mode(0o700).create(&state)?;
    let context = run::HolderContext {
        schema_version: 1,
        roots: run::RootSeed {
            home: temp.path().join("home"),
            checkout: temp.path().join("checkout"),
            state_root: state.clone(),
        },
        project_id: provenant_core::ProjectId::from_bytes([1; 16]),
        actor_id: ActorId::from_bytes([2; 16]),
        run_id: RunId::from_bytes([3; 16]),
        attempt_id: AttemptId::from_bytes([4; 16]),
        host_delivery: None,
        launch: None,
        sealed: None,
    };
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-sol".into()),
        account_key: Some("key".into()),
        account_label: Some("label".into()),
        ..Default::default()
    };
    let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let registry = Registry::default();
    let decode = || {
        registry.decode_captures(
            &route,
            stream,
            &[],
            &exit,
            Path::new("/nonexistent"),
            Some(&context),
        )
    };
    std::fs::write(state.join("config.toml"), "invalid = [")?;
    let failed = decode()?;
    assert_eq!(failed.outcome, run::Outcome::ModelUnavailable);
    assert!(
        failed
            .reason
            .unwrap_or_default()
            .contains("capacity cooldown configuration unavailable")
    );
    std::fs::write(
        state.join("config.toml"),
        "schema_version = 1\nprofile = 'balanced'\n[domains.routing]\ncapacity_cooldown_seconds = 0\n",
    )?;
    let disabled = decode()?;
    assert_eq!(
        disabled.reason.as_deref(),
        Some(
            "provider capacity: the selected model is overloaded; wait or choose an eligible route"
        )
    );
    assert!(!state.join("quota").exists());
    std::fs::remove_file(state.join("config.toml"))?;
    decode()?;
    let record = provenant_quota::AccountRecord::decode(&std::fs::read(
        state.join("quota/key/codex.json"),
    )?)?;
    assert_eq!(record.capacity_cooldowns.len(), 1);
    let before = std::fs::read(state.join("quota/key/codex.json"))?;
    let mut nested = context.clone();
    nested.roots.state_root = state.join("children/child");
    nested.launch = Some(run::HostLaunchContext {
        configuration_root: state.clone(),
        cwd: temp.path().join("checkout"),
        executables: Default::default(),
        executable_search_path: Vec::new(),
        git: PathBuf::from("/usr/bin/git"),
        default_account_directories: Default::default(),
        cargo_build_jobs: None,
        #[cfg(feature = "fixture")]
        fixture_state_write: false,
    });
    let nested_result = registry.decode_captures(
        &route,
        stream,
        &[],
        &exit,
        Path::new("/nonexistent"),
        Some(&nested),
    )?;
    assert_eq!(nested_result.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(std::fs::read(state.join("quota/key/codex.json"))?, before);
    assert!(!nested.roots.state_root.join("quota").exists());
    Ok(())
}

#[test]
fn claude_and_opencode_overload_reach_the_decoded_outcome() {
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    for (provider, model, stream) in [
        (
            "claude",
            "claude/claude-sonnet-5-5",
            r#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"api_error_status":529,"result":"Overloaded"}"#,
        ),
        (
            "opencode",
            "opencode/anthropic/test",
            r#"{"type":"error","sessionID":"ses_x","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#,
        ),
    ] {
        let route = run::RouteRecord {
            provider: Some(provider.into()),
            model: Some(model.into()),
            ..Default::default()
        };
        let decoded = Registry::default()
            .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        assert!(
            decoded
                .reason
                .as_deref()
                .unwrap()
                .starts_with("provider capacity: the selected model is overloaded")
        );
        assert!(decoded.session.is_some());
    }
}

#[test]
fn codex_rate_limit_reaches_the_decoded_outcome() {
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6-luna".into()),
        ..Default::default()
    };
    let registry = Registry::default();
    for (message, _expected) in [
        (
            "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
            "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
        ),
        (
            "429 Too Many Requests TOKEN_SENTINEL /private/source \u{1b} ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
            "429 Too Many Requests for [REDACTED]".into(),
        ),
        (
            format!("429 Too Many Requests {}", "☀".repeat(3000)),
            format!("429 Too Many Requests {}", "☀".repeat(2723)),
        ),
    ] {
        let stream = [
            json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
            json!({"type":"error","message":message}),
            json!({"type":"turn.failed","error":{"message":message}}),
        ]
        .map(|frame| format!("{frame}\n"))
        .concat();
        let decoded = registry
            .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::RateLimited);
        let reason = decoded.reason.as_deref().unwrap();
        assert!(reason.contains("rate limited"));
        assert!(reason.contains("wait"));
        assert!(!reason.contains(&message));
        assert!(!reason.contains("ghp_"));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/source"));
        assert!(!reason.contains('\u{1b}'));
        assert!(reason.len() <= 1024);
    }
}

#[test]
fn agy_denials_preserve_answers_and_prior_failures() {
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        model: Some("agy/gemini-3.8-flash".into()),
        ..Default::default()
    };
    let fixture = include_str!("../../../../tests/fixtures/providers/agy/denied-result.jsonl")
        .replace("RunCommand", "TOKEN_SENTINEL/private/source");
    let recovered = fixture.replace("\"response\":\"\"", "\"response\":\"Recovered answer\"");
    let registry = Registry::default();
    let decoded = registry
        .decode(
            &route,
            recovered.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.result, "Recovered answer");
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider denied an action; check account access and inspect the result")
    );
    let decoded = registry
        .decode(
            &route,
            fixture.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Failed);
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider denied an action; check account access and inspect the result")
    );
    let mut failed_exit = clean_exit();
    failed_exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = registry
        .decode(
            &route,
            fixture.as_bytes(),
            &failed_exit,
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Failed);
    assert_eq!(decoded.reason.as_deref(), Some("provider failed"));
}

#[test]
fn codex_rollout_error_reaches_the_decoded_reason() {
    let exit = clean_exit();
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6-luna".into()),
        ..Default::default()
    };
    let registry = Registry {
        roots: None,
        home: None,
        assets: None,
        cwd: None,
        state_root: None,
        project_id: None,
        host: None,
        cancel: None,
        repo_facts: Default::default(),
    };
    let stream = [
        json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
        json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
    ]
    .map(|frame| format!("{frame}\n"))
    .concat();
    let decoded = registry
        .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
    assert_eq!(decoded.result, "OK");
    assert_eq!(
        decoded.session.as_deref(),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
    assert!(decoded.observed_route.is_none());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("Codex rollout identity could not be verified; inspect the local session evidence")
    );
}

#[test]
fn reviewer_receipts_cover_provenance_and_resolution_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut record: run::AttemptRecord = serde_json::from_value(json!({
        "schema_version": 1, "attempt_id": AttemptId::from_bytes([1; 16]),
        "run_id": RunId::from_bytes([2; 16]), "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
        "task_id": TaskId::from_bytes([4; 16]), "operation_id": "receipt-test",
        "attempt_number": 1, "revision": 1, "generation": 0,
        "requested_route": {"kind":"model","provider":"codex","model":"codex/gpt-6.1-sol"},
        "applied_route": {"provider":"codex","model":"codex/gpt-6.1-sol","effort":"high","account_label":null},
        "authority_digest": provenant_core::Digest::from_bytes([5; 32]), "queued_at": now,
        "state": {"state":"terminal","outcome":"lost","exit_code":null,"finished_at":now,"reason":null},
        "usage": {}, "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"}, "digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
    }))?;
    let (data, line) = reviewer_receipt(&record, false).unwrap();
    assert_eq!(data["resolved_from"], "explicit");
    assert_eq!(data["reviewer"]["provenance"], "unknown");
    assert_eq!(data["reviewer"]["actor_id"], "unknown");
    assert_eq!(
        data["receipt"],
        format!(
            "Reviewer: codex/gpt-6.1-sol@high (openai; resolved from explicit), run {}/{}/{}",
            record.run_id, record.task_id, record.attempt_id
        )
    );
    assert!(
        line.ends_with(
            "Reviewer provenance: unknown, actor unknown, model unknown, family unknown\n"
        )
    );
    record.verified_owner = Some(ActorId::from_bytes([6; 16]));
    let unobserved = reviewer_receipt(&record, false).unwrap().0;
    assert_eq!(unobserved["reviewer"]["provenance"], "unknown");
    assert_ne!(unobserved["reviewer"]["actor_id"], "unknown");
    record.observed_route = record.applied_route.clone();
    let observed_fact = run::fact(record.clone())?.reviewer.ok_or("reviewer fact")?;
    let observed = reviewer_receipt(&record, false).unwrap();
    assert_eq!(observed.0["reviewer"]["provenance"], "observed");
    assert_eq!(
        observed.0["reviewer"]["actor_id"],
        observed_fact.actor_id.to_string()
    );
    assert_eq!(observed.0["reviewer"]["family"], "openai");
    assert!(observed.1.contains(&format!(
        "Reviewer provenance: observed, actor {},",
        observed_fact.actor_id
    )));
    record.applied_route.as_mut().unwrap().model = Some("gpt-6.1-sol".into());
    let (writer_data, writer_line) = reviewer_receipt(&record, true).unwrap();
    assert_eq!(writer_data["role"], "writer");
    assert_eq!(writer_data["family"], "openai");
    assert!(writer_line.starts_with("Writer: codex/gpt-6.1-sol@high (openai;"));
    record.applied_route.as_mut().unwrap().effort = None;
    assert!(
        reviewer_receipt(&record, false)
            .unwrap()
            .1
            .contains("@unknown")
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/uncatalogued".into());
    let unknown_family = reviewer_receipt(&record, false).unwrap().0;
    assert_eq!(unknown_family["family"], "unknown");
    assert_eq!(unknown_family["resolved_from"], "unknown");
    record.skipped_routes.push(serde_json::from_value(
        json!({"route":"codex/gpt-6.1-sol","kind":"unavailable","detail":"unavailable"}),
    )?);
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["resolved_from"],
        "unknown"
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/gpt-6.1-sol".into());
    record.applied_route.as_mut().unwrap().pool_selection = Some(serde_json::from_value(
        json!({"pool":"junior","candidates":[],"roll":0,"total_weight":1,"pick":"codex/gpt-6.1-sol"}),
    )?);
    // An unavailable candidate is still a pool selection, not a fallback.
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["resolved_from"],
        "pool junior"
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/other".into());
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["resolved_from"],
        "unknown"
    );
    record.applied_route = None;
    record.observed_route = None;
    let unknown = reviewer_receipt(&record, false).unwrap().0;
    for field in ["adapter", "model", "effort", "family", "resolved_from"] {
        assert_eq!(unknown[field], "unknown", "{field}");
    }
    record.applied_route = Some(Default::default());
    assert_eq!(
        reviewer_receipt(&record, false).unwrap().0["model"],
        "unknown"
    );
    record.state = run::AttemptState::Running {
        extra: Default::default(),
    };
    let refused = reviewer_receipt(&record, false).unwrap_err();
    assert_eq!(refused.status, Status::Refused);
    assert_eq!(refused.kind, ErrorKind::Nonterminal);
    Ok(())
}

#[test]
fn opencode_without_a_session_record_observes_no_model() {
    let route = run::RouteRecord {
        provider: Some("opencode".into()),
        model: Some("opencode/opencode/glm-5.3-flash-free".into()),
        ..Default::default()
    };
    let registry = Registry::default();
    let stream = [
        json!({"type":"text","sessionID":"ses_a","part":{"text":"OK"}}),
        json!({"type":"step_finish","sessionID":"ses_a","part":{"reason":"stop"}}),
    ]
    .map(|frame| format!("{frame}\n"))
    .concat();
    let decoded = registry
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/nonexistent"),
        )
        .unwrap();
    assert_eq!(decoded.result, "OK");
    assert!(decoded.observed_route.is_none());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("OpenCode session identity could not be verified; inspect the local session evidence")
    );
}

#[test]
fn claude_session_reaches_the_decoded_attempt() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = include_str!("../../../../tests/fixtures/providers/claude/fresh.jsonl");
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(
        decoded.session.as_deref(),
        Some("FIXTURE_SESSION_IDENTIFIER_003")
    );
}

#[test]
fn agy_repeated_answer_is_captured_once() {
    let answer = "# Review\n\n```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"low\",\"file\":\"a.rs\",\"summary\":\"issue\",\"status\":\"open\"}]}\n```\nVERDICT: PASS\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        ..Default::default()
    };
    let decode = |step: &str, response: &str| {
        let stream = format!(
            "{}\n{}\n{}\n",
            json!({"event":"init","conversation_id":"s","init":{"model":"m"}}),
            json!({"event":"step_update","step_update":{"conversation_id":"s","step_type":"agent_response","text_delta":step}}),
            json!({"event":"result","result":{"conversation_id":"s","status":"SUCCESS","response":response,"usage":{"input_tokens":1,"output_tokens":1}}})
        );
        Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap()
    };
    for decoded in [
        decode(&format!("{answer}\n"), answer),
        decode("", &format!("  {answer}\n\n{answer}  ")),
    ] {
        assert_eq!(decoded.outcome, run::Outcome::Succeeded);
        assert_eq!(decoded.result, answer);
        let mut capture = provenant_records::result_capture::ResultCapture::new(None);
        capture.push(decoded.result.as_bytes());
        let captured = capture.finish();
        assert!(captured.handoff.unwrap().is_some());
        assert_eq!(captured.verdict.as_deref(), Some("VERDICT: PASS"));
        assert_eq!(captured.declarations, 1);
        assert_eq!(decoded.result.matches("```findings").count(), 1);
    }
    for response in [
        format!("{answer}\nextra"),
        format!(
            "{answer}\n{}",
            answer.replace("\"note\":\"done\"", "\"note\":\"different\"")
        ),
    ] {
        let decoded = decode("", &response);
        assert_eq!(decoded.result, response);
        assert!(provenant_records::handoff::Handoff::from_result(&decoded.result).is_err());
    }
}

#[test]
fn repeated_answer_from_non_agy_adapter_is_unchanged() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\\nanswer\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n"
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.result, "answer\nanswer");
}

#[test]
fn claude_usage_records_cache_read_and_cache_write() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":40,\"cache_creation\":{\"ephemeral_1h_input_tokens\":25}}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, Some(10));
    assert_eq!(decoded.usage.output_excluding_reasoning, Some(20));
    assert_eq!(decoded.usage.cache_read, Some(30));
    assert_eq!(decoded.usage.cache_write, Some(40));
    assert_eq!(decoded.usage.cache_write_1h, Some(25));
    assert_eq!(decoded.usage.source.as_deref(), Some("provider"));
}

#[test]
fn a_claude_success_that_hit_its_token_limit_is_flagged_not_failed() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"half\",\"stop_reason\":\"max_tokens\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.partial_signals, [run::PartialSignal::ProviderStop]);
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider stopped early; inspect the result and resume if needed")
    );
    assert_ne!(decoded.outcome, run::Outcome::Failed);
}

#[test]
fn streamed_deltas_stay_joined_without_a_message_boundary() {
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        ..Default::default()
    };
    let delta = |text: &str| {
        json!({"event":"step_update","step_update":{"conversation_id":"s","step_type":"agent_response","text_delta":text}})
            .to_string()
    };
    let stream = [
        json!({"event":"init","conversation_id":"s","init":{"model":"m"}}).to_string(),
        delta("VERDICT"),
        delta(": PA"),
        delta("SS"),
    ]
    .join("\n");
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.result, "VERDICT: PASS");
}

#[test]
fn a_handoff_in_its_own_codex_message_is_read_after_prose() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-terra".into()),
        ..Default::default()
    };
    let handoff = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"n\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
    let message = |text: &str| {
        serde_json::json!({"type":"item.completed","item":{"type":"agent_message","text":text}})
            .to_string()
    };
    let stream = [
        r#"{"type":"thread.started","thread_id":"t"}"#.to_owned(),
        message("Checking the commit."),
        message("VERDICT: PASS"),
        message(handoff),
        r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#.to_owned(),
    ]
    .join("\n");
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    let mut capture = provenant_records::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
    capture.push(decoded.result.as_bytes());
    let captured = capture.finish();
    let declared = captured
        .handoff
        .expect("handoff parses")
        .expect("handoff found");
    assert_eq!(declared.note, "n");
    assert_eq!(captured.verdict.as_deref(), Some("VERDICT: PASS"));
    assert!(
        decoded
            .result
            .starts_with("Checking the commit.\nVERDICT: PASS\n")
    );
}

#[test]
fn codex_usage_records_cache_read_apart_from_uncached_input() {
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-terra".into()),
        ..Default::default()
    };
    let stream = concat!(
        "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"answer\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":7}}\n",
    );
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, Some(20));
    assert_eq!(decoded.usage.cache_read, Some(80));
    assert_eq!(decoded.usage.cache_write, None);
    assert_eq!(decoded.usage.output_excluding_reasoning, Some(7));
}

#[test]
fn command_exit_keeps_requested_route() {
    let route = run::RouteRecord {
        provider: Some("command".into()),
        model: None,
        ..Default::default()
    };
    let decoded = Registry::default()
        .decode(&route, b"output", &clean_exit(), Path::new("/unused"))
        .unwrap();
    let observed = decoded.observed_route.expect("observed route");
    assert_eq!(observed.provider.as_deref(), Some("command"));
    assert_eq!(observed.source.as_deref(), Some("command_exit"));
}

#[test]
fn claude_absent_usage_names_its_reason() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5".into()),
        ..Default::default()
    };
    let stream = "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n";
    let decoded = Registry::default()
        .decode(
            &route,
            stream.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded.usage.uncached_input, None);
    assert_eq!(
        decoded.usage.source.as_deref(),
        Some("absent: no result event")
    );

    let stream_no_usage = concat!(
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
        "{\"type\" : \"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\"}\n",
    );
    let decoded_no_usage = Registry::default()
        .decode(
            &route,
            stream_no_usage.as_bytes(),
            &clean_exit(),
            Path::new("/unused"),
        )
        .unwrap();
    assert_eq!(decoded_no_usage.usage.uncached_input, None);
    assert_eq!(
        decoded_no_usage.usage.source.as_deref(),
        Some("absent: result event has no usage")
    );

    let unknown = run::RouteRecord {
        provider: Some("unregistered-fixture".into()),
        ..Default::default()
    };
    let decoded_unknown = Registry::default()
        .decode(&unknown, b"", &clean_exit(), Path::new("/unused"))
        .unwrap();
    assert_eq!(decoded_unknown.usage.source, None);
}

#[test]
fn wait_any_follows_a_superseded_failure() -> Result<(), Box<dyn std::error::Error>> {
    let now = WallTime::from_micros(100)?;
    let mut failed = lane_fixture(
        1,
        1,
        run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::Failed,
            exit_code: Some(1),
            finished_at: now,
            reason: None,
        },
    )?;
    let mut retry = lane_fixture(
        2,
        1,
        run::AttemptState::Queued {
            extra: Default::default(),
        },
    )?;
    failed.superseded_by = Some(retry.attempt_id);
    assert!(!condition_met(
        &[failed.clone(), retry.clone()],
        Condition::Any
    ));
    assert!(!condition_met(
        std::slice::from_ref(&failed),
        Condition::Any
    ));
    retry.state = run::AttemptState::Terminal {
        extra: Default::default(),
        outcome: run::Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: now,
        reason: None,
    };
    assert!(condition_met(&[failed, retry], Condition::Any));
    Ok(())
}

#[test]
fn admission_window_deadline_is_preserved_on_restart() -> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane("run::tests::admission_window_deadline_is_preserved_on_restart")? {
        return Ok(());
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.tree,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            crate::fabric::private_dir(edge.roots.state.as_path(), edge.roots.state.as_path())?;
            provenant_config::bind_project(
                edge.roots,
                provenant_config::ProjectLocation::new(fixture.project.join(".git"))?,
            )?;
            provenant_config::bind_host(edge.roots)?;

            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;

            let op: OperationId = "test-restart-op".parse().unwrap();
            let t0 = WallTime::from_micros(10_000_000).unwrap();
            let initial_window = run::AdmissionWindow {
                deadline: t0,
                wake_at: None,
            };
            let saved = session
                .store
                .save_admission_window(&op, initial_window, &call)?;
            assert_eq!(saved.deadline, t0);
            assert_eq!(saved.wake_at, None);

            // Later restart attempts to set a later deadline (t1) and a wake_at
            let t1 = WallTime::from_micros(20_000_000).unwrap();
            let wake = WallTime::from_micros(12_000_000).unwrap();
            let restarted_window = run::AdmissionWindow {
                deadline: t1,
                wake_at: Some(wake),
            };
            let reloaded = session
                .store
                .save_admission_window(&op, restarted_window, &call)?;
            // The deadline MUST NOT be renewed: it must remain t0!
            assert_eq!(reloaded.deadline, t0);
            // wake_at was updated
            assert_eq!(reloaded.wake_at, Some(wake));
            Ok(())
        },
    )??;
    Ok(())
}

#[test]
fn every_adapter_sign_in_failure_settles_auth_required() -> Result<(), Box<dyn std::error::Error>> {
    use run::Providers;
    let fixtures: &[(&str, &[u8])] = &[
        (
            "cursor",
            include_bytes!("../../../../tests/fixtures/providers/cursor/auth-required.stderr"),
        ),
        (
            "codex",
            include_bytes!("../../../../tests/fixtures/providers/codex/auth-required.stderr"),
        ),
        (
            "opencode",
            include_bytes!("../../../../tests/fixtures/providers/opencode/auth-required.stderr"),
        ),
        (
            "agy",
            include_bytes!("../../../../tests/fixtures/providers/agy/auth-required.stderr"),
        ),
    ];
    let temp = tempfile::tempdir()?;
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    for (adapter, stderr) in fixtures {
        let route = run::RouteRecord {
            provider: Some((*adapter).into()),
            model: Some(format!("{adapter}/example")),
            ..Default::default()
        };
        let decoded =
            Registry::default().decode_captures(&route, b"", stderr, &exit, temp.path(), None)?;
        assert_eq!(decoded.outcome, run::Outcome::AuthRequired, "{adapter}");
        assert!(
            decoded
                .reason
                .as_deref()
                .unwrap()
                .contains("provenant quota tick")
        );
    }
    Ok(())
}

#[test]
fn settled_auth_uses_only_the_selected_registered_account() -> Result<(), Box<dyn std::error::Error>>
{
    use run::Providers;
    let temp = tempfile::tempdir()?;
    let mut inputs =
        provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
    inputs.state_dir = Some(temp.path().join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_os::fixture::private_state_dir(roots.state.as_path())?;
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version = 1\n[domains.accounts.entries.work]\nadapter = 'codex'\nlabel = 'work'\nkey = 'key'\ndirectory = '/fixture/work'\n",
    )?;
    let route = run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/example".into()),
        account_key: Some("key".into()),
        ..Default::default()
    };
    let mut exit = clean_exit();
    exit.termination = Some(provenant_os::exec::Termination::Exit(1));
    let decoded = Registry {
        roots: Some(&roots),
        ..Default::default()
    }
    .decode_captures(
        &route,
        b"",
        include_bytes!("../../../../tests/fixtures/providers/codex/auth-required.stderr"),
        &exit,
        temp.path(),
        None,
    )?;
    assert_eq!(decoded.outcome, run::Outcome::AuthRequired);
    let reason = decoded.reason.unwrap();
    assert!(reason.contains("work [codex]"), "{reason}");
    assert!(reason.contains("codex login"), "{reason}");
    assert!(reason.contains("/fixture/work"), "{reason}");
    Ok(())
}

#[test]
fn successful_output_with_incidental_auth_stderr_stays_successful() {
    let route = run::RouteRecord {
        provider: Some("cursor".into()),
        model: Some("cursor/example".into()),
        ..Default::default()
    };
    let decoded = Registry::default()
        .decode_captures(
            &route,
            include_bytes!("../../../../tests/fixtures/providers/cursor/fresh.jsonl"),
            include_bytes!("../../../../tests/fixtures/providers/cursor/auth-required.stderr"),
            &clean_exit(),
            Path::new("/unused"),
            None,
        )
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::Succeeded);
}

#[cfg(feature = "fixture")]
#[test]
fn reconcile_reports_publish_error_and_explicitly_abandons_a_writer_fence()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane(
        "run::tests::reconcile_reports_publish_error_and_explicitly_abandons_a_writer_fence",
    )? {
        return Ok(());
    }
    use provenant_os::fs::{Directory, LockMode, PublishMode};
    use provenant_supervisor::custody::{self, Scope};
    use std::ffi::OsStr;
    for import_done in [false, true] {
        let reason = if import_done {
            "r".repeat(4096)
        } else {
            "worktree rebuilt by chair".into()
        };
        let phase = if import_done { "release" } else { "publish" };
        let blocker = if import_done {
            "release evidence missing"
        } else {
            "lane Git import failed"
        };
        let fixture = linked_fixture()?;
        with_edge(
            &fixture.root,
            &fixture.project,
            |edge| -> Result<(), Box<dyn std::error::Error>> {
                let clock = SystemClock::new();
                let cancel = CancelToken::new();
                let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
                let session = open(edge, &call).map_err(|fail| fail.message)?;
                let mut input = trace_run(30, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
                input.request.parent_attempt = None;
                input.request.lane_depth = None;
                input.request.tasks[0].writer = Some(run::WriterRequest {
                    extra: Default::default(),
                    project: fixture.project.clone(),
                    worktree: fixture.tree.clone(),
                });
                session.store.create(&input, &call)?;
                let id = input.attempts[0].attempt_id;
                let record = session.store.mark_starting_for_fixture(
                    id,
                    &run::RouteRecord::default(),
                    &call,
                )?;
                let scope = Scope {
                    run: input.run_id,
                    attempt: id,
                    generation: record.generation,
                };
                let attempt_path = edge
                    .roots
                    .state
                    .as_path()
                    .join(format!("runs/{}/attempts/{id}", input.run_id));
                let attempt = Directory::open_anchor(&attempt_path)?;
                let slot_path = edge.roots.state.as_path().join(format!(
                    "worktrees/{}/abandon-fixture",
                    session.context.project_id
                ));
                std::fs::create_dir_all(&slot_path)?;
                let lease = provenant_supervisor::fixture::acquire_writer(
                    Directory::open_anchor(&slot_path)?,
                    scope,
                    call.lock_budget(),
                    &cancel,
                    &clock,
                )?;
                let recovery = provenant_supervisor::fixture::bind_writer_attempt(lease, &attempt)?
                    .into_recovery()?;
                let fence = session
                    .store
                    .writer_fences()?
                    .into_iter()
                    .find(|f| f.scope == scope)
                    .ok_or("fence")?;
                let evidence = Directory::open_anchor(
                    fence.reservation.parent().ok_or("reservation parent")?,
                )?;
                evidence
                    .atomic_write(
                        OsStr::new("import-retained.json"),
                        ".test-",
                        &serde_json::to_vec(&json!({"schema_version":1,"scope":scope}))?,
                        PublishMode::NoReplace,
                    )?
                    .require_durable()?;
                let exit = ExitObservation {
                    scope,
                    terminal_observed: true,
                    settlement_evidence: custody::SettlementEvidence::DarwinCensusAndCustodyLease,
                    ..clean_exit()
                };
                let mut exit_json = serde_json::to_value(&exit)?;
                exit_json["schema_version"] = 1.into();
                attempt
                    .atomic_write(
                        OsStr::new("process-exit.json"),
                        ".test-",
                        &serde_json::to_vec(&exit_json)?,
                        PublishMode::NoReplace,
                    )?
                    .require_durable()?;
                session.store.publish_terminal(
                    id,
                    session.store.record(id, &call)?.revision,
                    &run::TerminalPublication {
                        terminal_cause: None,
                        terminal_note: None,
                        outcome: run::Outcome::Unsettled,
                        finished_at: clock.wall_now()?,
                        started_at: None,
                        provider_ms: None,
                        reason: Some("publish refused".into()),
                        result: "retained provider evidence",
                        complete: true,
                        redacted: false,
                        resolved_route: None,
                        applied_route: None,
                        observed_route: None,
                        usage: Default::default(),
                        session: None,
                        partial_signals: vec![],
                    },
                    &exit,
                    &call,
                )?;
                let import = if import_done {
                    run::EffectDisposition::Done
                } else {
                    run::EffectDisposition::Failed {
                        reason: blocker.into(),
                        retryable: true,
                    }
                };
                session.store.record_effect(
                    id,
                    scope.generation,
                    run::SettlementEffect::GitImport,
                    import.clone(),
                    &call,
                )?;
                attempt
                    .atomic_write(
                        OsStr::new("settlement-progress.json"),
                        ".test-",
                        &serde_json::to_vec(&json!({
                            "schema_version":1, "scope":scope, "phase":phase, "attempts":1,
                            "head":null, "base":null, "owner":null, "blocking_reason":blocker
                        }))?,
                        PublishMode::Replace,
                    )?
                    .require_durable()?;
                std::fs::create_dir_all(attempt_path.join("tmp/candidate-0/git-common"))?;
                std::fs::write(
                    attempt_path.join("tmp/candidate-0/git-common/evidence"),
                    "private objects",
                )?;
                let parse = |args: &[&str]| -> Result<Reconcile, Box<dyn std::error::Error>> {
                    let crate::ParseOutcome::Invocation(inv) =
                        crate::parse(args.iter().map(std::ffi::OsString::from))
                    else {
                        return Err("parse reconcile".into());
                    };
                    let crate::Command::Reconcile(command) = inv.command else {
                        return Err("reconcile command".into());
                    };
                    Ok(command)
                };
                let driver =
                    attempt.try_lock(OsStr::new("settlement-driver.lock"), LockMode::Exclusive)?;
                let run_id = input.run_id.to_string();
                let reported = reconcile(parse(&["reconcile", "--id", &run_id, "--json"])?, edge);
                let json = serde_json::to_value(&reported.result)?;
                if !import_done {
                    assert_eq!(
                        json["data"]["runs"][0]["attempts"][0]["found"]["phase"],
                        "publish"
                    );
                    assert_eq!(
                        json["data"]["runs"][0]["attempts"][0]["found"]["blocking_reason"],
                        "lane Git import failed"
                    );
                    assert!(reported.human.contains("lane Git import failed"));
                }
                let selection = format!("{run_id}/{id}");
                assert!(parse(&["reconcile", "--abandon", &selection]).is_err());
                assert!(parse(&["reconcile", "--reason", "missing attempt"]).is_err());
                assert!(
                    parse(&[
                        "reconcile",
                        "--id",
                        &run_id,
                        "--abandon",
                        &selection,
                        "--reason",
                        "ambiguous action"
                    ])
                    .is_err()
                );
                let args = [
                    "reconcile",
                    "--abandon",
                    &selection,
                    "--reason",
                    reason.as_str(),
                    "--json",
                ];
                assert_ne!(
                    reconcile(parse(&args)?, edge).result.status,
                    Status::Ok,
                    "live driver excludes abandonment"
                );
                drop(driver);
                assert_ne!(
                    reconcile(parse(&args)?, edge).result.status,
                    Status::Ok,
                    "held writer lock excludes abandonment"
                );
                drop(recovery);
                let abandoned = reconcile(parse(&args)?, edge);
                assert_eq!(
                    abandoned.result.status,
                    Status::Ok,
                    "{:?}",
                    abandoned.result.error
                );
                assert_eq!(abandoned.result.data["found"]["state"], "abandoned");
                assert_eq!(abandoned.result.data["found"]["reason"], reason);
                assert!(session.store.writer_fences()?.is_empty());
                session
                    .store
                    .reconcile_for_fixture(input.run_id, &Registry::default(), &call)?;
                assert_eq!(
                    session
                        .store
                        .record(id, &call)?
                        .settlement_receipts
                        .unwrap()
                        .git_import,
                    import
                );
                let journal: Value = serde_json::from_slice(&std::fs::read(
                    attempt_path.join("settlement-progress.json"),
                )?)?;
                assert_eq!(
                    journal["abandonment"]["actor"],
                    session.context.actor_id.to_string()
                );
                assert_eq!(journal["abandonment"]["phase"], phase);
                assert_eq!(journal["blocking_reason"], blocker);
                assert_eq!(
                    reconcile(parse(&args)?, edge).result.status,
                    Status::Ok,
                    "same reason is idempotent"
                );
                reconcile(parse(&["reconcile", "--id", &run_id, "--json"])?, edge);
                assert!(
                    attempt_path
                        .join("tmp/candidate-0/git-common/evidence")
                        .is_file()
                );
                let different = [
                    "reconcile",
                    "--abandon",
                    &selection,
                    "--reason",
                    "different explanation",
                    "--json",
                ];
                assert_ne!(
                    reconcile(parse(&different)?, edge).result.status,
                    Status::Ok,
                    "recorded reason cannot be replaced"
                );
                let state = Directory::open_anchor(edge.roots.state.as_path())?;
                let later =
                    WallTime::from_micros(clock.wall_now()?.as_micros() + 90 * 86_400_000_000)?;
                let collected = session
                    .store
                    .gc_provider_state(&state, 7, later, false, &call)?;
                assert!(collected.attempt_tmp.is_empty());
                assert!(
                    attempt_path
                        .join("tmp/candidate-0/git-common/evidence")
                        .is_file()
                );
                assert_eq!(
                    std::fs::read_to_string(attempt_path.join("result.md"))?,
                    "retained provider evidence"
                );
                Ok(())
            },
        )??;
    }
    Ok(())
}

#[test]
fn projected_cli_error_omits_unsafe_source_and_keeps_bounded_fields() {
    let failure = super::Fail::from(provenant_run::RunError::Io(std::io::Error::other(
        "TOKEN_SENTINEL\u{1b}",
    )));
    let reply = super::reply("run", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert_eq!(wire["error"]["code"], "IO");
    assert_eq!(wire["error"]["category"], "error");
    assert!(wire["error"].get("detail").is_none());
    assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    let mut rendered = Vec::new();
    crate::render::render(
        &reply.result,
        crate::render::RenderMode::Human {
            text: "",
            summary: None,
            read_only: false,
        },
        &mut rendered,
    )
    .unwrap();
    assert!(
        !String::from_utf8(rendered)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );
}

#[test]
fn malformed_claude_result_preserves_provider_error_in_settlement() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/fixture".into()),
        ..Default::default()
    };
    let message = "Permission mode forced to default — CLAUDE_CODE_SUBPROCESS_ENV_SCRUB is set";
    let result =
        serde_json::json!({"type":"result", "is_error":true, "errors":[message]}).to_string();
    let earlier = r#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"result":"earlier answer"}"#;
    for (result, reason) in [
        (
            result.as_str(),
            format!("provider protocol is malformed: {message}"),
        ),
        ("{", "provider protocol is malformed".into()),
    ] {
        for suffix in ["", "\n"] {
            let decoded = Registry::default()
                .decode(
                    &route,
                    format!("{earlier}\n{result}{suffix}").as_bytes(),
                    &clean_exit(),
                    Path::new("/unused"),
                )
                .unwrap();
            assert_eq!(decoded.outcome, run::Outcome::Failed);
            assert_eq!(decoded.reason.as_deref(), Some(reason.as_str()));
        }
    }
}

#[test]
fn lane_listing_omits_large_grants_but_id_retains_them() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::OsString;
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let grants: Vec<run::WriteGrantRecord> = (0..600).map(|index| {
            serde_json::from_value(json!({
                "source": "tool_write",
                "path": {"relative_to_root": {"encoding":"utf8", "path": format!("home/fixture/.cache/build/{index}/{}output", "directory/".repeat(8))}},
                "scope":"subtree"
            }))
        }).collect::<Result<_, _>>()?;
            let database = rusqlite::Connection::open(
                session.context.state_root.as_path().join("run.sqlite3"),
            )?;
            let mut selected = None;
            for number in 1..=25 {
                let prepared =
                    trace_run(number, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
                session.store.create(&prepared, &call)?;
                let mut lane = session
                    .store
                    .record(prepared.attempts[0].attempt_id, &call)?;
                lane.write_grants = grants.clone();
                // Seed retained launch evidence without starting 25 native providers.
                // The command still opens the store and reads its ordinary lane query.
                assert_eq!(
                    database.execute(
                        "UPDATE attempts SET record=?1 WHERE id=?2",
                        rusqlite::params![serde_json::to_vec(&lane)?, lane.attempt_id.to_string()]
                    )?,
                    1
                );
                selected = Some(Target {
                    selection: None,
                    run: Some(lane.run_id),
                    task: None,
                    attempt: Some(lane.attempt_id),
                });
            }
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(
                ["status", "--all", "--limit", "25"]
                    .into_iter()
                    .map(OsString::from),
            ) else {
                return Err("parse lanes failed".into());
            };
            let crate::Command::Status(mut command) = invocation.command else {
                return Err("not lanes".into());
            };
            let reply = status::status(command.clone(), true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let listing = reply.result.data;
            let rows = listing["rows"].as_array().ok_or("listing rows")?;
            assert_eq!(rows.len(), 25);
            assert!(rows.iter().all(|row| row.get("write_grants").is_none()));
            assert!(serde_json::to_vec(&listing)?.len() < 1024 * 1024);

            command.id = Some(
                selected
                    .ok_or("selected lane")?
                    .attempt
                    .ok_or("attempt")?
                    .to_string(),
            );
            let reply = status::status(command, true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let detail = reply.result.data;
            let rows = detail["rows"].as_array().ok_or("detail rows")?;
            assert_eq!(rows.len(), 1);
            let retained = rows[0]["write_grants"].as_array().ok_or("detail grants")?;
            assert_eq!(retained.len(), grants.len());
            assert!(retained.iter().all(|grant| grant["path"].is_object()));
            assert!(
                serde_json::to_vec(retained)?.len() * 25 > 1024 * 1024,
                "grant evidence alone must reproduce the oversized page"
            );
            Ok(())
        },
    )?
}

#[test]
fn lane_detail_and_output_retain_safe_refusal_remedies() -> Result<(), Box<dyn std::error::Error>> {
    struct Refusing {
        note: String,
    }
    impl run::Providers for Refusing {
        fn prepare(
            &self,
            _: &run::TaskRequest,
            _: &str,
            _: &run::LaunchPaths,
            _: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            Err(run::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                resource: None,
                terminal_cause: Some(provenant_records::run::TerminalCause::AccessPolicyRefused),
                terminal_note: Some(self.note.clone()),
                message: "PROVIDER_SENTINEL must not become a public diagnostic".into(),
                skipped_routes: Vec::new(),
                fallback_outcome: None,
            })
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!("a refused launch never reaches a provider")
        }
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let prepared = trace_run(10, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
            session.store.create(&prepared, &call)?;
            let note = format!(
                "configured access boundary: {} END_SAFE_NOTE",
                "fixture setting ".repeat(24)
            );
            assert!(note.len() > 160);
            session.store.start(
                prepared.run_id,
                &Refusing { note: note.clone() },
                &|_, _| unreachable!(),
                &call,
            )?;
            let target = Target {
                selection: None,
                run: Some(prepared.run_id),
                task: None,
                attempt: Some(prepared.attempts[0].attempt_id),
            };
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(
                ["status", "--all"]
                    .into_iter()
                    .map(std::ffi::OsString::from),
            ) else {
                return Err("parse lanes failed".into());
            };
            let crate::Command::Status(mut command) = invocation.command else {
                return Err("not lanes".into());
            };
            let reply = status::status(command.clone(), true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (listing, human) = (reply.result.data, reply.human);
            let expected = format!(
                "{}; {note}",
                provenant_records::run::TerminalCause::AccessPolicyRefused.detail()
            );
            assert_eq!(listing["rows"][0]["reason"], expected);
            assert!(
                human.contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{human}"
            );
            assert!(!human.contains(&note), "{human}");
            for text in [serde_json::to_string(&listing)?, human] {
                assert!(!text.contains("PROVIDER_SENTINEL"), "{text}");
            }
            let listing_reply = status::status(command.clone(), false, edge);
            let listing_summary = listing_reply
                .summary
                .as_ref()
                .ok_or("listing summary")?
                .lines
                .join("\n");
            assert!(
                listing_summary
                    .contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{listing_summary}"
            );
            assert!(!listing_summary.contains(&note), "{listing_summary}");
            let crate::ParseOutcome::Invocation(invocation) =
                crate::parse(["status"].into_iter().map(std::ffi::OsString::from))
            else {
                return Err("parse status failed".into());
            };
            let crate::Command::Status(status_command) = invocation.command else {
                return Err("not status".into());
            };
            let status_reply = status::status(status_command, true, edge);
            assert_eq!(status_reply.result.data["rows"][0]["reason"], expected);
            let status_summary = status_reply
                .summary
                .as_ref()
                .ok_or("status summary")?
                .lines
                .join("\n");
            assert!(
                status_summary
                    .contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{status_summary}"
            );
            assert!(!status_summary.contains(&note), "{status_summary}");
            assert!(
                !serde_json::to_string(&status_reply.result.data)?.contains("PROVIDER_SENTINEL")
            );
            command.id = Some(target.attempt.ok_or("attempt")?.to_string());
            let reply = status::status(command.clone(), true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (detail, human) = (reply.result.data, reply.human);
            assert_eq!(detail["rows"][0]["reason"], expected);
            assert!(
                human.contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{human}"
            );
            assert!(!human.contains(&note), "{human}");
            let explicit = status::status(command, false, edge);
            let explicit_summary = explicit
                .summary
                .as_ref()
                .ok_or("explicit summary")?
                .lines
                .join("\n");
            assert!(
                explicit_summary
                    .contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{explicit_summary}"
            );
            assert!(!explicit_summary.contains(&note), "{explicit_summary}");
            let output_command = || Output {
                attempt: crate::Attempt(target.clone()),
                host: None,
                part: Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65_536,
                trace: false,
            };
            let (data, text) =
                output::output_inner(output_command(), edge, None).map_err(|fail| fail.message)?;
            assert_eq!(data["reason"], expected);
            assert!(text.contains(&expected));
            assert!(!serde_json::to_string(&data)?.contains("PROVIDER_SENTINEL"));
            let reply = output::output(output_command(), edge);
            let mut bytes = Vec::new();
            crate::render(
                &reply.result,
                crate::RenderMode::Compact {
                    text: &reply.human,
                    read_only: true,
                    summary: reply.summary.as_ref(),
                },
                &mut bytes,
            )?;
            let default = String::from_utf8(bytes)?;
            assert!(default.contains(&expected), "{default}");
            assert_eq!(default.matches("END_SAFE_NOTE").count(), 1, "{default}");
            assert!(!default.contains("PROVIDER_SENTINEL"));
            Ok(())
        },
    )?
}

#[test]
fn disk_wait_rows_stay_compact_and_explicit_output_keeps_the_sample()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    let now = WallTime::from_micros(100)?;
    let reason = "host capacity: disk space: 23622320128 bytes available; gc.free_floor is 32212254720 bytes (source: host configuration)";
    let lanes: Vec<_> = (1..=20)
        .map(|number| {
            lane_fixture(
                number,
                number,
                run::AttemptState::Starting {
                    extra: Default::default(),
                },
            )
        })
        .collect::<Result<_, _>>()?;
    let waits: Vec<_> = lanes
        .iter()
        .map(|lane| run::LaneWait {
            extra: Default::default(),
            attempt_id: lane.attempt_id,
            lane_class: run::LaneClass::Writer,
            waiting_for: reason.into(),
        })
        .collect();
    let compact = status::compact_status(&lanes, &waits, now);
    assert!(compact.len() <= 2048, "{compact}");
    assert!(compact.contains("disk below gc.free_floor; provenant output ID"));
    assert!(!compact.contains("23622320128"));
    let (data, _) = render_lanes(&lanes, &waits, None, Path::new("/state"), None);
    assert!(
        data["lanes"]
            .as_array()
            .ok_or("rows")?
            .iter()
            .all(|row| row["waiting_for"] == reason)
    );
    let summary = status::lane_summary(&status::lane_lines(&data), None);
    let mut bytes = Vec::new();
    crate::render(
        &CommandResult::new("status", Status::Ok, data),
        crate::RenderMode::Compact {
            text: &compact,
            read_only: true,
            summary: Some(&summary),
        },
        &mut bytes,
    )?;
    assert!(bytes.len() <= 2048, "{}", String::from_utf8_lossy(&bytes));

    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            std::fs::write(
                edge.roots.state.as_path().join("config.toml"),
                "schema_version=1\n[gc]\nfree_floor='32212254720B'\n",
            )?;
            let mut prepared = trace_run(10, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
            prepared.request.tasks[0].writer = Some(run::WriterRequest {
                extra: Default::default(),
                project: fixture.project.clone(),
                worktree: fixture.tree.clone(),
            });
            session.store.create(&prepared, &call)?;
            let mut lane = session
                .store
                .record(prepared.attempts[0].attempt_id, &call)?;
            lane.state = run::AttemptState::Starting {
                extra: Default::default(),
            };
            let database = rusqlite::Connection::open(
                session.context.state_root.as_path().join("run.sqlite3"),
            )?;
            assert_eq!(
                database.execute(
                    "UPDATE attempts SET record=?1 WHERE id=?2",
                    rusqlite::params![serde_json::to_vec(&lane)?, lane.attempt_id.to_string()]
                )?,
                1
            );
            // Admission reads private guardian checkpoints. Match their mode
            // rather than inheriting the test runner's umask.
            let mut sample = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(artifact(
                    session.context.state_root.as_path(),
                    &lane,
                    "admission-disk.json",
                ))?;
            std::io::Write::write_all(
                &mut sample,
                &serde_json::to_vec(&json!({"generation":lane.generation,"free":23622320128_u64}))?,
            )?;
            drop(sample);
            let target = Target {
                selection: None,
                run: Some(lane.run_id),
                task: None,
                attempt: Some(lane.attempt_id),
            };
            let command = || Output {
                attempt: crate::Attempt(target.clone()),
                host: None,
                part: Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65_536,
                trace: false,
            };
            let (data, text) =
                output::output_inner(command(), edge, None).map_err(|fail| fail.message)?;
            let expected = format!("{reason}; free disk space or run `provenant gc --apply`");
            assert_eq!(data["reason"], expected);
            assert!(text.contains(&expected), "{text}");
            let selection = lane.attempt_id.to_string();
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(
                ["status", selection.as_str()]
                    .into_iter()
                    .map(std::ffi::OsString::from),
            ) else {
                return Err("parse lanes failed".into());
            };
            let crate::Command::Status(detail_command) = invocation.command else {
                return Err("not lanes".into());
            };
            let reply = status::status(detail_command, true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (detail, human) = (reply.result.data, reply.human);
            assert_eq!(detail["rows"][0]["reason"], expected);
            assert!(
                human.contains("disk below gc.free_floor; provenant output ID"),
                "{human}"
            );
            assert!(!human.contains("23622320128"), "{human}");
            for field in ["length", "digest", "complete", "clipped"] {
                assert!(data[field].is_null(), "{field}: {data}");
            }
            assert_eq!(data["bytes"]["data"], "");
            let destination = fixture.root.join("preserved.txt");
            std::fs::write(&destination, "existing content")?;
            let mut save = command();
            save.to = Some(destination.clone());
            let reply = output::output(save, edge);
            assert!(reply.result.error.is_some(), "{:?}", reply.result);
            assert_eq!(std::fs::read_to_string(destination)?, "existing content");
            Ok(())
        },
    )?
}
