use super::*;

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
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
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
    env.push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
    env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
    for args in [
        vec!["init", "-q", "-b", "fixture-base"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
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

fn parsed_run(args: &[&str]) -> Result<Dispatch, Box<dyn std::error::Error>> {
    let argv = std::iter::once("run")
        .chain(args.iter().copied())
        .map(std::ffi::OsString::from);
    let crate::ParseOutcome::Invocation(invocation) = crate::parse(argv) else {
        return Err("parse failed".into());
    };
    let crate::Command::Run(command) = invocation.command else {
        return Err("not run".into());
    };
    Ok(*command)
}

#[test]
fn run_in_linked_worktree_requires_explicit_target() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = linked_fixture()?;
    let nested = fixture.tree.join("nested");
    let command = parsed_run(&["--prompt-stdin"])?;
    let result = with_edge(&fixture.root, &nested, |edge| super::run(command, edge))?;
    let error = result.result.error.ok_or("expected refusal")?;
    assert_eq!(error.code, "REFUSED");
    assert_eq!(
        error.message,
        format!(
            "run from a linked worktree needs an explicit target: use --worktree {} for a writer, or --mode read-only for a reader",
            fixture.tree.display()
        )
    );
    assert!(super::require_worktree_target(&fixture.project).is_ok());
    Ok(())
}

#[test]
fn explicit_target_or_mode_passes_the_linked_worktree_guard()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = linked_fixture()?;
    let nested = fixture.tree.join("nested");
    let tree = fixture.tree.to_string_lossy().into_owned();
    for args in [
        vec!["--prompt-stdin", "--worktree", tree.as_str()],
        vec!["--prompt-stdin", "--mode", "read-only"],
        vec!["--prompt-stdin", "--branch", "feature"],
        vec!["--prompt-stdin", "--worktree-branch", "feature"],
    ] {
        let command = parsed_run(&args)?;
        assert!(
            super::require_explicit_target(&command, &nested).is_ok(),
            "{args:?}"
        );
    }
    let refused = parsed_run(&["--prompt-stdin"])?;
    assert!(super::require_explicit_target(&refused, &nested).is_err());
    Ok(())
}

#[test]
fn explicit_read_only_from_a_linked_worktree_dispatches() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = linked_fixture()?;
    let nested = fixture.tree.join("nested");
    let prompt = fixture.root.join("prompt.md");
    std::fs::write(&prompt, "Summarise the repository.")?;
    let prompt = prompt.to_string_lossy().into_owned();
    // Without a model or route, dispatch stops at task validation before
    // any provider is launched.
    let command = parsed_run(&["--prompt-file", prompt.as_str(), "--mode", "read-only"])?;
    let result = with_edge(&fixture.root, &nested, |edge| super::run(command, edge))?;
    // The guard refuses with no data. Dispatch always assigns an operation.
    assert!(
        result.result.data.get("operation_id").is_some(),
        "{:?}",
        result.result.error
    );
    assert!(
        result
            .result
            .error
            .as_ref()
            .is_none_or(|error| error.code != "REFUSED"),
        "{:?}",
        result.result.error
    );
    Ok(())
}

#[test]
fn run_explain_warns_once_for_existing_brief_paths_outside_read_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::write(project.join("inside.md"), "in-root")?;
    let outside = root.join("rubric.md");
    std::fs::write(&outside, "rubric")?;
    let missing = root.join("missing.md");
    let home = root.join("home");
    std::fs::create_dir_all(&home)?;
    std::fs::write(home.join("home-rubric.md"), "home rubric")?;
    let brief = root.join("brief.md");
    std::fs::write(
        &brief,
        format!(
            "Read `{}` and `{}` plus ~/home-rubric.md; also see `{}` and `/` and `{}`.",
            outside.display(),
            outside.display(),
            project.join("inside.md").display(),
            root.display()
        ),
    )?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let brief_arg = brief.to_string_lossy().into_owned();
    let command = parsed_run(&[
        "--model",
        "codex/gpt-6-luna",
        "--prompt-file",
        &brief_arg,
        "--explain",
    ])?;
    let reply = super::run(command, &edge);
    assert_eq!(reply.result.status, Status::Ok, "{:?}", reply.result.error);
    assert_eq!(reply.human.matches("warning: brief references").count(), 2);
    assert!(reply.human.contains(&outside.display().to_string()));
    assert!(
        reply
            .human
            .contains(&home.join("home-rubric.md").display().to_string())
    );
    assert!(reply.human.contains("add it to --read-root"));
    let carried = reply.result.data["warnings"]
        .as_array()
        .ok_or("warnings must travel as data")?;
    assert_eq!(carried.len(), 2);
    assert!(
        carried
            .iter()
            .filter_map(Value::as_str)
            .any(|text| text.contains(&outside.display().to_string()))
    );

    let warnings = super::brief_read_root_warnings(
        &format!(
            "{} {}",
            project.join("inside.md").display(),
            missing.display()
        ),
        &project,
        None,
        &[],
        &home,
    );
    assert!(warnings.is_empty());
    Ok(())
}

#[test]
fn brief_read_root_warnings_skips_root_ancestors_and_home_and_warns_on_outside_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    std::fs::create_dir_all(&project)?;
    let outside = root.join("rubric.md");
    std::fs::write(&outside, "rubric")?;
    let home = root.join("home");
    std::fs::create_dir_all(&home)?;

    // Acceptance 1: A brief containing `/` and the parent of the worktree gives no warning.
    let worktree_parent = project.parent().ok_or("worktree parent")?;
    let brief = format!(
        "Inspect `/` and `{}` and `~` and `~/`; do not touch them.",
        worktree_parent.display()
    );
    let warnings = super::brief_read_root_warnings(&brief, &project, None, &[], &home);
    assert!(
        warnings.is_empty(),
        "expected no warnings for `/`, `~` and worktree parent, got: {warnings:?}"
    );

    // Acceptance 2: A brief citing an existing file outside the roots still warns.
    let citing_outside = format!("Review `{}`.", outside.display());
    let warnings = super::brief_read_root_warnings(&citing_outside, &project, None, &[], &home);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(&outside.display().to_string()));

    let explicit_parent = root.join("explicit-parent");
    let explicit_root = explicit_parent.join("allowed");
    std::fs::create_dir_all(&explicit_root)?;
    let warnings = super::brief_read_root_warnings(
        &format!("Inspect `{}`.", explicit_parent.display()),
        &project,
        None,
        &[explicit_root],
        &home,
    );
    assert!(
        warnings.is_empty(),
        "explicit root ancestor warned: {warnings:?}"
    );

    Ok(())
}

#[test]
fn brief_read_root_warning_switch_turns_off_warnings() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    std::fs::create_dir_all(&project)?;
    let outside = root.join("rubric.md");
    std::fs::write(&outside, "rubric")?;
    let home = root.join("home");
    std::fs::create_dir_all(&home)?;

    let state = root.join("state");
    std::fs::create_dir_all(&state)?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version = 1\n[domains.run]\nbrief_read_root_warnings = false\n",
    )?;

    let brief = root.join("brief.md");
    std::fs::write(&brief, format!("Read `{}`.", outside.display()))?;

    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state);
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let brief_arg = brief.to_string_lossy().into_owned();
    let command = parsed_run(&[
        "--model",
        "codex/gpt-6-luna",
        "--prompt-file",
        &brief_arg,
        "--explain",
    ])?;
    let reply = super::run(command, &edge);
    assert_eq!(reply.result.status, Status::Ok, "{:?}", reply.result.error);
    assert!(
        !reply.human.contains("warning: brief references"),
        "expected no warnings with switch off, got: {}",
        reply.human
    );
    assert!(
        reply.result.data.get("warnings").is_none(),
        "expected no carried warnings with switch off"
    );

    Ok(())
}

#[test]
fn brief_read_root_warning_project_override_applies_to_explain()
-> Result<(), Box<dyn std::error::Error>> {
    if reexec_test_in_lane(
        "run::tests::brief_read_root_warning_project_override_applies_to_explain",
    )? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let outside = root.join("outside.md");
    std::fs::write(&outside, "outside")?;
    let brief = root.join("brief.md");
    std::fs::write(&brief, format!("Read `{}`.", outside.display()))?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let context = crate::fabric::Context::open(roots.clone(), &home, &project)?;
    let id = context.identity.project;
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let brief_arg = brief.to_string_lossy().into_owned();
    for (host, project_setting) in [(true, false), (false, true)] {
        std::fs::write(
            state.join("config.toml"),
            format!(
                "schema_version = 1\n[domains.run]\nbrief_read_root_warnings = {host}\n[domains.run.projects.\"{id}\"]\nbrief_read_root_warnings = {project_setting}\n"
            ),
        )?;
        let command = parsed_run(&[
            "--model",
            "codex/gpt-6-luna",
            "--prompt-file",
            &brief_arg,
            "--explain",
        ])?;
        let reply = super::run(command, &edge);
        assert_eq!(reply.result.status, Status::Ok, "{:?}", reply.result.error);
        assert_eq!(
            reply.human.contains("warning: brief references"),
            project_setting,
            "{}",
            reply.human
        );
        assert_eq!(reply.result.data.get("warnings").is_some(), project_setting);
    }
    Ok(())
}

#[test]
fn render_run_renders_warnings_from_data_and_does_not_scrape_text()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = &std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    std::fs::create_dir_all(&project)?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &root.join("home"),
        assets: &project,
        cwd: &project,
    };
    let result = super::reply(
        "dispatch",
        Ok((
            json!({
                "lanes": [{"attempt_id":"01010101010101010101010101010101", "state":"succeeded"}],
                "warnings": ["brief references /elsewhere/rubric.md outside the lane's read roots"]
            }),
            "warning: brief references /scraped outside the lane's read roots\n".into(),
        )),
    );
    let report = super::render_run(result, &edge);
    assert!(
        report
            .human
            .contains("warning: brief references /elsewhere/rubric.md")
    );
    assert!(!report.human.contains("/scraped"));
    assert_eq!(
        report.result.data["warnings"].as_array().map(Vec::len),
        Some(1)
    );
    Ok(())
}

#[test]
fn submodule_gitfile_is_not_a_linked_worktree() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let submodule = root.join("super").join("vendor");
    let nested = submodule.join("src");
    std::fs::create_dir_all(&nested)?;
    // A submodule's gitfile names its own administrative directory.
    std::fs::write(
        submodule.join(".git"),
        "gitdir: ../../.git/modules/vendor\n",
    )?;
    assert!(super::require_worktree_target(&nested).is_ok());
    let command = parsed_run(&["--prompt-stdin"])?;
    assert!(super::require_explicit_target(&command, &nested).is_ok());
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

fn handoff_result(status: &str) -> String {
    format!(
        "```handoff\n{}\n```",
        serde_json::json!({"schema_version":2,"status":status,"note":"Parser complete; renderer remains.","remaining":["Renderer"],"decisions":[],"verify":["Parser tests pass"]})
    )
}
fn model_assessment(
    policy: &provenant_routing::ModelPolicy,
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    policy_attempt: u64,
) -> Result<run::ResultAssessment, String> {
    let handoff = provenant_records::handoff::Handoff::from_result(&decoded.result)
        .ok()
        .flatten();
    super::assess_model_result(
        policy,
        task,
        record,
        decoded,
        handoff.as_ref(),
        policy_attempt,
    )
}

#[test]
fn model_policy_results_check_sections_and_bound_continuations()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|error| error.message)?;
    let store = trace_store(&temp, &call)?;
    let input = trace_run(40, AttemptId::from_bytes([41; 16]), clock.wall_now()?)?;
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    let task = &input.request.tasks[0];
    let policies = provenant_routing::ModelPolicies::default();
    let mut decoded = run::Decoded {
        outcome: run::Outcome::Succeeded,
        result: "   \n".into(),
        complete: true,
        observed_route: None,
        usage: Default::default(),
        session: Some("fixture".into()),
        reason: None,
        partial_signals: Vec::new(),
    };
    let gemini = policies
        .effective(
            &ModelId::parse("agy/gemini-3.8-flash")?,
            None,
            provenant_core::WallTime::from_unix_seconds(0).expect("valid policy instant"),
        )
        .policy;
    let decision = model_assessment(&gemini, task, &record, &decoded, record.attempt_number)?;
    assert!(decision.refusal.is_some());
    let fallback = decision.retry.ok_or("missing fallback")?;
    assert!(!fallback.resume);
    assert_eq!(fallback.max_attempts, 2);
    record.continuation = Some("policy_fallback".into());
    assert!(
        model_assessment(&gemini, task, &record, &decoded, record.attempt_number)?
            .retry
            .is_none()
    );
    record.continuation = None;
    decoded.result = "unstructured answer".into();
    let decision = model_assessment(&gemini, task, &record, &decoded, record.attempt_number)?;
    assert!(decision.refusal.is_none() && decision.retry.is_none());
    let mut with_sections = gemini.clone();
    with_sections.result.sections = Some(vec!["## Findings".into(), "## Evidence".into()]);
    let decision = model_assessment(
        &with_sections,
        task,
        &record,
        &decoded,
        record.attempt_number,
    )?;
    assert!(decision.refusal.is_some());
    decoded.result = "## Findings\nClean.\n## Evidence\nChecked.".into();
    let decision = model_assessment(
        &with_sections,
        task,
        &record,
        &decoded,
        record.attempt_number,
    )?;
    assert!(decision.refusal.is_none() && decision.retry.is_none());
    // Typed quota and stop outcomes retain their own recovery semantics,
    // even when their result is empty.
    decoded.result.clear();
    for outcome in [
        run::Outcome::QuotaLimited,
        run::Outcome::Cancelled,
        run::Outcome::TimedOut,
        run::Outcome::Lost,
    ] {
        decoded.outcome = outcome;
        let decision = model_assessment(&gemini, task, &record, &decoded, 1)?;
        assert!(decision.refusal.is_none() && decision.retry.is_none());
    }
    decoded.outcome = run::Outcome::Succeeded;
    let terra = policies
        .effective(
            &ModelId::parse("codex/gpt-6.1-terra")?,
            None,
            provenant_core::WallTime::from_unix_seconds(0).expect("valid policy instant"),
        )
        .policy;
    record.applied_route = Some(run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6.1-terra".into()),
        effort: Some("high".into()),
        ..Default::default()
    });
    decoded.result = handoff_result("stopped_early");
    let continuation = model_assessment(&terra, task, &record, &decoded, record.attempt_number)?
        .retry
        .ok_or("missing continuation")?;
    assert!(continuation.resume);
    assert_eq!(continuation.max_attempts, 3);
    decoded.session = None;
    record.session = None;
    assert!(
        !model_assessment(&terra, task, &record, &decoded, 1)?
            .retry
            .ok_or("missing fresh continuation")?
            .resume
    );
    decoded.session = Some("fixture".into());
    // Trailing blank lines do not hide the marker.
    decoded.result = format!("{}\n\n", handoff_result("stopped_early"));
    assert!(
        model_assessment(&terra, task, &record, &decoded, record.attempt_number)?
            .retry
            .is_some()
    );
    decoded.result = handoff_result("stopped_early");
    decoded.outcome = run::Outcome::Uncommitted;
    assert!(
        model_assessment(&terra, task, &record, &decoded, 1)?
            .retry
            .is_some()
    );
    decoded.outcome = run::Outcome::Succeeded;
    record.attempt_number = 3;
    let decision = model_assessment(&terra, task, &record, &decoded, record.attempt_number)?;
    assert!(decision.refusal.is_some() && decision.retry.is_none());
    decoded.result = handoff_result("done");
    assert!(
        model_assessment(&terra, task, &record, &decoded, record.attempt_number)?
            .retry
            .is_none()
    );
    record.attempt_number = 1;
    for answer in [
        "Status: incomplete".into(),
        "Work remains".into(),
        handoff_result("blocked"),
        handoff_result("done"),
        "```handoff\n{}\n```".into(),
    ] {
        decoded.result = answer;
        let handoff = provenant_records::handoff::Handoff::from_result(&decoded.result)
            .ok()
            .flatten();
        let assessment =
            super::assess_model_result(&terra, task, &record, &decoded, handoff.as_ref(), 1)?;
        assert!(assessment.retry.is_none() && assessment.refusal.is_none());
    }
    Ok(())
}
#[test]
fn an_alternate_account_is_spent_only_by_routing_rule_or_a_live_refusal()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus, QuotaWindow};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let state = temp.path().canonicalize()?.join("state");
    std::fs::create_dir_all(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let model = ModelId::parse("codex/gpt-6-luna")?;
    let now = 1_800_000_000;
    let scope = |key: &str| AccountScope {
        adapter: "codex".into(),
        account_key: key.into(),
        label: key.into(),
    };
    let exhausted = |key: &str, resets: i64| {
        let mut record = AccountRecord::empty(scope(key));
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account: scope(key),
            revision: 0,
            status: QuotaStatus::Exhausted,
            observed_unix_seconds: now - 7_200,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![QuotaWindow {
                pool: "seven_day".into(),
                window_seconds: Some(604_800),
                used_basis_points: 10_000,
                resets_unix_seconds: Some(resets),
            }],
            details: None,
        });
        record
    };
    let keys = ["work", "other"];
    let catalogue = provenant_routing::Catalogue::bundled()?;
    let policies = provenant_providers::quota_policies(provenant_providers::REGISTRY);
    let alternative = |quota: &[AccountRecord]| {
        unspent_alternative(keys, quota, &policies, "work", &model, &catalogue, now)
    };
    // Unknown is not spent.
    assert!(alternative(&[]));
    // An exhausted reading binds until its full windows reset, as routing has it.
    assert!(!alternative(&[exhausted("other", now + 3_600)]));
    assert!(alternative(&[exhausted("other", now - 60)]));
    // A live refusal recorded by a settlement, read back from the cache.
    crate::quota::persist_provider_refusal(
        &state,
        &run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6-luna".into()),
            account_key: Some("other".into()),
            account_label: Some("other".into()),
            ..Default::default()
        },
        None,
        now - 60,
        None,
    )?;
    let cache = crate::quota::read_cache_at(&state)?;
    assert!(!alternative(&cache));
    // It holds only that model, and only for its hour.
    let other = ModelId::parse("codex/gpt-6.1-sol")?;
    assert!(unspent_alternative(
        keys, &cache, &policies, "work", &other, &catalogue, now
    ));
    assert!(unspent_alternative(
        keys,
        &cache,
        &policies,
        "work",
        &model,
        &catalogue,
        now + 3_600
    ));
    // A persisted account mismatch is spent too, as admission has it.
    let mut mismatched = AccountRecord::empty(scope("other"));
    mismatched.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
    assert!(!alternative(&[mismatched]));
    // The spent account itself never counts.
    assert!(!unspent_alternative(
        ["work"],
        &[],
        &policies,
        "work",
        &model,
        &catalogue,
        now
    ));
    Ok(())
}
#[test]
fn settlement_alternates_read_the_configured_polling_interval_like_admission()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::{
        AccountRecord, AccountScope, PollingIntervals, QuotaSnapshot, QuotaStatus, QuotaWindow,
        Refusal, Settings,
    };
    let model = ModelId::parse("agy/gemini-3.8-flash")?;
    let now = 1_800_000_000;
    let scope = AccountScope {
        adapter: "agy".into(),
        account_key: "other".into(),
        label: "other".into(),
    };
    // A refusal, then a comparable reading with headroom that is older than
    // agy's default 1200 s timer interval but inside a configured 3600 s one.
    let mut record = AccountRecord::empty(scope.clone());
    record.refusals.push(Refusal {
        extra: Default::default(),
        account: scope.clone(),
        model: model.model().into(),
        pool: Some("gemini".into()),
        observed_unix_seconds: now - 2_000,
        resets_unix_seconds: None,
    });
    record.snapshot = Some(QuotaSnapshot {
        schema_version: 1,
        account: scope,
        revision: 0,
        status: QuotaStatus::Available,
        observed_unix_seconds: now - 1_500,
        source_ref: "fixture".into(),
        authority_ref: None,
        windows: vec![QuotaWindow {
            pool: "gemini".into(),
            window_seconds: Some(604_800),
            used_basis_points: 1_000,
            resets_unix_seconds: Some(now + 3_600),
        }],
        details: None,
    });
    let catalogue = provenant_routing::Catalogue::bundled()?;
    let pools = catalogue.admission_pools(&model);
    let mut configured = Settings::default();
    configured.polling_intervals.insert(
        "agy".into(),
        PollingIntervals {
            timer_seconds: Some(3_600),
            panel_seconds: None,
        },
    );
    for (settings, cleared) in [(Settings::default(), false), (configured, true)] {
        let (records, policies) =
            crate::route::account_evidence(&settings, vec![record.clone()], now);
        let blocked = provenant_quota::Admission {
            policies: &policies,
            now_unix_seconds: now,
            records: &records,
            pools: &pools,
        }
        .account_block("agy", "other", model.model())?
        .is_some();
        let alternative = unspent_alternative(
            ["work", "other"],
            &records,
            &policies,
            "work",
            &model,
            &catalogue,
            now,
        );
        assert_eq!(blocked, !cleared);
        assert_eq!(alternative, cleared, "assess must agree with admission");
    }
    Ok(())
}
#[test]
fn host_native_attempts_do_not_resume_implicitly_or_explicitly()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp.path().canonicalize()?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nstop_early_ok=false\nmax_continuations=2\n",
    )?;
    let registry = Registry {
        roots: Some(&roots),
        ..Default::default()
    };
    let error = registry
        .check_continuable(&record)
        .err()
        .ok_or("host resume accepted")?;
    assert_eq!(error.kind, ErrorKind::ContinuationUnsupported);
    let mut decoded = run::Decoded {
        outcome: run::Outcome::ModelUnavailable,
        result: String::new(),
        complete: false,
        observed_route: None,
        usage: Default::default(),
        session: record.session.clone(),
        reason: Some("provider capacity: Selected model is at capacity".into()),
        partial_signals: Vec::new(),
    };
    let assessment = run::Providers::assess_result(
        &registry,
        &input.request.tasks[0],
        &record,
        &decoded,
        None,
        None,
        1,
    )?;
    assert!(assessment.retry.is_none());
    decoded.outcome = run::Outcome::Succeeded;
    decoded.complete = true;
    let handoff = provenant_records::handoff::Handoff::from_json(br#"{"schema_version":2,"status":"stopped_early","note":"CI remains red","remaining":["Repair tests"],"decisions":[],"verify":[]}"#)?;
    let assessment = run::Providers::assess_result(
        &registry,
        &input.request.tasks[0],
        &record,
        &decoded,
        Some(&handoff),
        None,
        1,
    )?;
    assert_eq!(
        assessment.refusal.as_deref(),
        Some("model reported unfinished work")
    );
    assert!(assessment.retry.is_none());
    Ok(())
}

#[test]
fn capacity_and_quota_settlements_retry_within_their_bounds()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|error| error.message)?;
    let store = trace_store(&temp, &call)?;
    let input = trace_run(42, AttemptId::from_bytes([43; 16]), clock.wall_now()?)?;
    store.create(&input, &call)?;
    let mut record = store.record(input.attempts[0].attempt_id, &call)?;
    record.applied_route = Some(run::RouteRecord {
        provider: Some("codex".into()),
        model: Some("codex/gpt-6-luna".into()),
        effort: Some("low".into()),
        account_key: Some("work".into()),
        ..Default::default()
    });
    let mut task = input.request.tasks[0].clone();
    task.route = run::RequestedRoute::Named {
        extra: Default::default(),
        name: "implement".into(),
        effort: None,
    };
    let config = provenant_routing::RoutingConfig::default();
    let decoded = |outcome, session: Option<&str>, reason: &str| run::Decoded {
        outcome,
        result: String::new(),
        complete: false,
        observed_route: None,
        usage: Default::default(),
        session: session.map(str::to_owned),
        reason: Some(reason.into()),
        partial_signals: Vec::new(),
    };
    let capacity = decoded(
        run::Outcome::ModelUnavailable,
        Some("thread"),
        "provider capacity: Selected model is at capacity",
    );
    // Recovery has no holder context; the worker's registry must supply
    // the real roots so settlement honors the user's retry limit.
    let root = temp.path().canonicalize()?;
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    std::fs::write(
        roots.state.as_path().join("config.toml"),
        "schema_version=1\n[domains.routing]\nsettlement_retries=0\n",
    )?;
    let rooted = Registry {
        roots: Some(&roots),
        ..Default::default()
    };
    let assess_recovered = |registry: &Registry<'_>| {
        run::Providers::assess_result(registry, &task, &record, &capacity, None, None, 1)
    };
    assert!(assess_recovered(&rooted)?.retry.is_none());
    assert!(assess_recovered(&Registry::default())?.retry.is_some());
    const NOW: i64 = 1_800_000_000;
    let assess_at = |record: &run::AttemptRecord,
                     task: &run::TaskRequest,
                     decoded: &run::Decoded,
                     attempt: u64,
                     others: bool| {
        assess_settlement_retry(&config, |_, _| others, task, record, decoded, attempt, NOW)
    };
    let assess = |task: &run::TaskRequest, decoded: &run::Decoded, attempt: u64, others: bool| {
        assess_at(&record, task, decoded, attempt, others)
    };
    let wait = |retry: &run::PolicyRetry| retry.not_before_unix_seconds.map(|t| t - NOW);
    // Capacity with a session resumes it on the applied model, waiting
    // longer on each retry, with up to a quarter of jitter.
    let first = assess(&task, &capacity, 1, false);
    let retry = first.retry.ok_or("capacity retry")?;
    assert!(retry.resume && first.refusal.is_none());
    assert_eq!(retry.max_attempts, 3);
    let waited = wait(&retry).ok_or("not before")?;
    assert!((30..=37).contains(&waited), "{waited}");
    assert!(
        matches!(&retry.route, run::RequestedRoute::Model { model, .. } if model == "codex/gpt-6-luna")
    );
    assert_eq!(
        first.note.as_deref(),
        Some(format!("retrying this session in {waited} s (attempt 2 of 3)").as_str())
    );
    let second =
        wait(&assess(&task, &capacity, 2, false).retry.ok_or("second")?).ok_or("second wait")?;
    assert!((60..=75).contains(&second), "{second}");
    // The jitter differs between lanes, so they do not retry in step.
    let draws: std::collections::BTreeSet<_> = (0..8u8)
        .map(|byte| {
            let mut other = record.clone();
            other.attempt_id = AttemptId::from_bytes([byte.wrapping_mul(37); 16]);
            assess_at(&other, &task, &capacity, 1, false)
                .retry
                .and_then(|retry| wait(&retry))
        })
        .collect();
    assert!(draws.len() > 1, "{draws:?}");
    assert!(assess(&task, &capacity, 3, false).retry.is_none());
    // Without a session a pool route is drawn again, fresh.
    let sessionless = decoded(
        run::Outcome::ModelUnavailable,
        None,
        "provider capacity: Selected model is at capacity",
    );
    let redraw = assess(&task, &sessionless, 1, false)
        .retry
        .ok_or("redraw")?;
    assert!(!redraw.resume);
    assert_eq!(redraw.route, task.route);
    // A read-only lane takes its first fallback; a writer keeps its route.
    let mut fallback = task.clone();
    fallback.fallback = vec![run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/claude-sonnet-5-5".into()),
        ..Default::default()
    }];
    let read_only = assess(&fallback, &sessionless, 1, false)
        .retry
        .ok_or("fallback")?;
    assert!(
        matches!(&read_only.route, run::RequestedRoute::Model { model, .. } if model == "claude/claude-sonnet-5-5")
    );
    fallback.access.write_roots.push(temp.path().to_owned());
    let writer = assess(&fallback, &sessionless, 1, false)
        .retry
        .ok_or("writer")?;
    assert_eq!(writer.route, fallback.route);
    assert!(writer.instruction.contains("git status"));
    assert!(writer.instruction.contains("already pushed"));
    // Other model failures are not capacity.
    let unavailable = decoded(
        run::Outcome::ModelUnavailable,
        Some("thread"),
        "provider model unavailable: no route",
    );
    assert!(assess(&task, &unavailable, 1, true).retry.is_none());
    // A spent account redraws at once; an exact model needs another
    // registered account, and a pinned account is never swapped.
    let quota = decoded(
        run::Outcome::QuotaLimited,
        None,
        "You've hit your weekly limit",
    );
    // A named route on its only account, or every other account spent,
    // has nowhere to go: a redraw could reach the spent account again
    // through another model.
    assert!(assess(&task, &quota, 1, false).retry.is_none());
    let spent = assess(&task, &quota, 1, true);
    let retry = spent.retry.ok_or("quota retry")?;
    assert!(!retry.resume && retry.not_before_unix_seconds.is_none());
    // It keeps the applied model, so admission refuses the spent account
    // and resolution falls through to another for that model.
    assert!(
        matches!(&retry.route, run::RequestedRoute::Model { model, .. } if model == "codex/gpt-6-luna")
    );
    assert_eq!(
        spent.note.as_deref(),
        Some("retrying on the next eligible account (attempt 2 of 3)")
    );
    let mut pinned = task.clone();
    pinned.account = Some("work".into());
    assert!(assess(&pinned, &quota, 1, true).retry.is_none());
    let mut exact = task.clone();
    exact.route = input.request.tasks[0].route.clone();
    assert!(assess(&exact, &quota, 1, false).retry.is_none());
    assert!(
        matches!(&assess(&exact, &quota, 1, true).retry.ok_or("exact")?.route, run::RequestedRoute::Model { model, .. } if model == "codex/gpt-6-luna")
    );
    exact.account = Some("work".into());
    assert!(assess(&exact, &quota, 1, true).retry.is_none());
    // Zero retries turns the behaviour off.
    let off = provenant_routing::RoutingConfig {
        settlement_retries: 0,
        ..Default::default()
    };
    assert!(
        assess_settlement_retry(&off, |_, _| true, &task, &record, &quota, 1, NOW)
            .retry
            .is_none()
    );
    // The limit, base, cap and ceiling are all configuration.
    let tuned = provenant_routing::RoutingConfig {
        settlement_retries: 5,
        capacity_retry_seconds: 400,
        capacity_retry_max_seconds: 900,
        retry_delay_ceiling_seconds: 7_200,
        ..Default::default()
    };
    let capped = assess_settlement_retry(&tuned, |_, _| false, &task, &record, &capacity, 4, NOW)
        .retry
        .ok_or("tuned retry")?;
    assert_eq!(capped.max_attempts, 6);
    assert_eq!(wait(&capped), Some(900));
    let ceiling = provenant_routing::RoutingConfig {
        retry_delay_ceiling_seconds: 45,
        ..tuned
    };
    let ceiled = assess_settlement_retry(&ceiling, |_, _| false, &task, &record, &capacity, 1, NOW)
        .retry
        .ok_or("ceiled retry")?;
    assert_eq!(wait(&ceiled), Some(45));
    // Every configurable retry limit fits the run store's chain bound.
    assert!(u64::from(provenant_routing::MAX_SETTLEMENT_RETRIES) < run::POLICY_ATTEMPT_CEILING);
    Ok(())
}
#[test]
fn unsafe_store_path_reason_survives_cli_conversion() {
    let failure = super::Fail::from(provenant_run::RunError::UnsafePath {
        path: "runs/example/process.json".into(),
        check: provenant_run::PathCheck::Links,
    });
    assert_eq!(
        failure.message,
        "run-store path runs/example/process.json is not private or contained: link count"
    );
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
        let detail = failure.detail.expect("typed detail");
        assert_eq!(detail.code(), "io");
        assert_eq!(detail.to_string(), text);
    }
    let failure = super::Fail::from(provenant_run::RunError::Delivery(
        provenant_records::run::WriterDeliveryStage::Prepare,
    ));
    assert_eq!(
        failure.detail.expect("stage detail").to_string(),
        failure.message
    );
}

#[test]
fn host_delivery_limit_cli_error_matches_golden() -> Result<(), Box<dyn std::error::Error>> {
    let failure = super::Fail::from(provenant_run::RunError::DeliveryLimit {
        stage: provenant_records::run::WriterDeliveryStage::Integrate,
        tool: "git",
        command: "worktree list".into(),
        limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
    });
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
    let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/delivery-limit-error.txt");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden_path, &actual)?;
    }
    let expected = std::fs::read_to_string(&golden_path)?;
    assert_eq!(actual, expected);
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
fn core_run_status_uses_policy_successor_and_retains_receipts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &root.join("home"),
        assets: root,
        cwd: root,
    };
    for (tail, expected) in [("succeeded", Status::Ok), ("failed", Status::Failed)] {
        let result = super::reply(
            "dispatch",
            Ok((
                json!({
                    "lanes": [
                        {"attempt_id":"01010101010101010101010101010101", "state":"failed", "superseded_by":"02020202020202020202020202020202"},
                        {"attempt_id":"02020202020202020202020202020202", "state":tail}
                    ]
                }),
                String::new(),
            )),
        );
        let report = super::render_run(result, &edge);
        assert_eq!(report.result.status, expected);
        assert_eq!(
            report.result.data["lanes"].as_array().ok_or("lanes")?.len(),
            2
        );
        assert!(report.human.contains("01010101 failed"));
        assert!(report.human.contains(&format!("02020202 {tail}")));
    }
    Ok(())
}

#[test]
fn core_run_timeout_only_offers_waiting_for_unsettled_lanes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let edge = Edge {
        roots: &roots,
        home: &root.join("home"),
        assets: root,
        cwd: root,
    };
    let result = super::reply_waited(
        "dispatch",
        Ok((
            serde_json::json!({
                "operation_id": "timeout",
                "lanes": [
                    {"attempt_id":"01010101010101010101010101010101", "state":"running", "applied_route":{"model":"codex/test", "effort":"high"}, "network":"open"},
                    {"attempt_id":"02020202020202020202020202020202", "state":"succeeded"},
                    {"attempt_id":"03030303030303030303030303030303", "state":"input_required"}
                ]
            }),
            String::new(),
            true,
        )),
    );
    let report = super::render_run(result, &edge);
    assert!(
        report
            .human
            .contains("codex/test (effort: high, network: open)")
    );
    assert_eq!(report.result.status, Status::Waiting);
    assert!(
            report.human.contains(
                "run is still running; keep waiting with `provenant lanes --id 01010101010101010101010101010101 --wait 4m`"
            ),
            "{}",
            report.human
        );
    let next = |reply: &super::Reply| {
        reply
            .summary
            .as_ref()
            .and_then(|summary| summary.next.clone())
    };
    assert!(!report.human.contains("keep waiting:"), "{}", report.human);
    assert_eq!(
        next(&report).as_deref(),
        Some("provenant lanes --id 01010101010101010101010101010101 --wait 4m")
    );
    let input = super::reply(
        "dispatch",
        Ok((
            serde_json::json!({
                "operation_id": "answer",
                "lanes": [{"attempt_id":"03030303030303030303030303030303", "state":"input_required"}]
            }),
            String::new(),
        )),
    );
    let input = super::render_run(input, &edge);
    assert_eq!(input.result.status, Status::Ok);
    assert_eq!(
        next(&input).as_deref(),
        Some("provenant resume 03030303030303030303030303030303 --prompt-file ANSWER")
    );
    assert!(!input.human.contains("keep waiting:"));
    Ok(())
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
    let Err(unknown) = super::catalogued("codex/gpt-6-lunaa", None) else {
        panic!("uncatalogued model accepted");
    };
    assert_eq!(unknown.status, Status::Invalid);
    assert!(
        unknown
            .message
            .starts_with("model codex/gpt-6-lunaa is not in the catalogue"),
        "{}",
        unknown.message
    );
    let Err(effort) = super::catalogued("claude/claude-sonnet-5-5", Some("low")) else {
        panic!("unsupported effort accepted");
    };
    assert_eq!(effort.status, Status::Refused);
    assert_eq!(effort.kind.code(), "UNSUPPORTED");
    assert!(
        effort
            .message
            .ends_with("supported efforts: medium, high, xhigh"),
        "{}",
        effort.message
    );
    let Err(none) = super::catalogued("opencode/opencode/deepseek-v4.1-flash-free", Some("high"))
    else {
        panic!("effort accepted for a model that takes none");
    };
    assert!(
        none.message.ends_with("this model takes no effort"),
        "{}",
        none.message
    );
    assert!(super::catalogued("codex/gpt-6-luna", Some("xhigh")).is_ok());
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
fn dispatch_existing_worktree_checks_custody_and_replays() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_confinement::worktrees::{self, WorktreeOrigin, WorktreeOwner};
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use provenant_supervisor::custody::{self, Scope};
    use std::os::unix::fs::DirBuilderExt;
    const MARKER: &str = "PROVENANT_TEST_WORKTREE_ADMISSION";
    let executor = SystemExecutor::new(1)?;
    let parent = AttemptId::from_bytes([8; 16]);
    let Some(root) = std::env::var_os(MARKER).map(PathBuf::from) else {
        let fixture = linked_fixture()?;
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "\"$@\" & child=$!; wait \"$child\"".into(),
            "worktree-admission".into(),
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::tests::dispatch_existing_worktree_checks_custody_and_replays".into(),
            "--nocapture".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !key.to_string_lossy().starts_with("GIT_")
                    && key != MARKER
                    && key != provenant_core::RUN_STATE_DIRECTORY_ENV
                    && key != run::LANE_DEPTH_ENV
                    && key != run::PARENT_ATTEMPT_ENV
            })
            .collect();
        env.extend([
            (MARKER.into(), fixture.root.as_os_str().to_owned()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            (
                provenant_core::RUN_STATE_DIRECTORY_ENV.into(),
                fixture
                    .root
                    .join("state/children")
                    .join(parent.to_string())
                    .into_os_string(),
            ),
            (run::LANE_DEPTH_ENV.into(), "1".into()),
            (run::PARENT_ATTEMPT_ENV.into(), parent.to_string().into()),
        ]);
        // Nested writer proposals must stay beneath the primary checkout.
        let tree = fixture.project.join("writer");
        let move_argv = vec![
            provenant_os::git::git_executable()
                .ok_or("Git unavailable")?
                .into_os_string(),
            "worktree".into(),
            "move".into(),
            fixture.tree.as_os_str().to_owned(),
            tree.as_os_str().to_owned(),
        ];
        let mut options = ExecOptions::new(&move_argv, &fixture.project, Duration::from_secs(5));
        options.env = Some(&env);
        let out = executor.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        // Both the submitting process and its parent shell are inside the tree.
        let mut options = ExecOptions::new(&argv, &tree, Duration::from_secs(30));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65_536 };
        options.stderr = StreamPolicy::Capture { limit: 65_536 };
        let out = executor.execute(options)?;
        assert_eq!(
            out.termination,
            Termination::Exit(0),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        return Ok(());
    };
    let project = root.join("project");
    let tree = project.join("writer");
    let prompt = root.join("prompt.md");
    std::fs::write(&prompt, "reply OK")?;
    with_edge(
        &root,
        &tree,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(edge.roots.state.as_path())?;
            provenant_config::bind_project(
                edge.roots,
                provenant_config::ProjectLocation::new(project.join(".git"))?,
            )?;
            provenant_config::bind_host(edge.roots)?;
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let command = |operation| {
                parsed_run(&[
                    "--operation-id",
                    operation,
                    "--model",
                    "codex/gpt-6-luna",
                    "--mode",
                    "worktree-write",
                    "--worktree",
                    tree.to_str().unwrap(),
                    "--prompt-file",
                    prompt.to_str().unwrap(),
                ])
                .map(|mut command| {
                    command.wait = None;
                    command
                })
            };
            let switch = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
                provenant_os::fixture::run_git(&tree, args)?;
                Ok(())
            };
            switch(&["switch", "--detach"])?;
            let detached = dispatch(command("detached-tree")?, edge);
            assert_eq!(detached.result.status, Status::Refused);
            let message = &detached
                .result
                .error
                .as_ref()
                .ok_or("detached error")?
                .message;
            assert!(message.contains("detached HEAD at"), "{message}");
            assert!(
                message.contains("remove --worktree and pass --mode read-only"),
                "{message}"
            );
            assert!(
                session
                    .store
                    .operation(&"detached-tree".parse()?, &call)?
                    .is_none()
            );
            switch(&["switch", "writer"])?;
            let admitted = dispatch(command("free-tree")?, edge);
            assert_eq!(
                admitted.result.status,
                Status::Ok,
                "{:?}",
                admitted.result.error
            );
            let run_id = admitted.result.data["run_id"].clone();
            assert!(!run_id.is_null(), "{:?}", admitted.result.data);
            let holder = AttemptId::from_bytes([7; 16]);
            let held = worktrees::with_context(
                edge.roots,
                edge.home,
                &project,
                &clock,
                &cancel,
                |context| {
                    let verified = provenant_confinement::verify_worktree(context.repo, &tree)?;
                    let info = worktrees::list(context, &cancel)?
                        .into_iter()
                        .find(|info| info.path == tree)
                        .ok_or(worktrees::WorktreeError::NotFound)?;
                    let (directory, binding) = worktrees::writer_custody(
                        context.project_state,
                        &verified,
                        &provenant_confinement::repo::GitRefName::branch("writer").unwrap(),
                        info.head,
                        || {
                            Ok(provenant_confinement::run_utility(
                                context.executor,
                                provenant_confinement::GitRead::WorktreeList,
                                context.repo,
                                context.utility,
                                context.utility_budget,
                                &cancel,
                            )?
                            .stdout
                            .bytes)
                        },
                    )?;
                    worktrees::save_owner(
                        &verified,
                        &WorktreeOwner {
                            attempt_id: Some(holder),
                            operation_id: Some("holder".parse().unwrap()),
                            branch: "writer".into(),
                            base: info.head.to_string(),
                            host: provenant_config::bind_host(edge.roots).unwrap(),
                            created_at: WallTime::from_micros(1).unwrap(),
                            origin: WorktreeOrigin::Lane,
                            label: None,
                        },
                    )?;
                    Ok((directory, binding))
                },
            )?;
            let lease = custody::acquire_writer(
                held.0,
                Scope {
                    run: RunId::from_bytes([7; 16]),
                    attempt: holder,
                    generation: 1,
                },
                held.1,
                provenant_core::WaitBudget::cli(clock.mono_now(), 1)?,
                &cancel,
                &clock,
            )?;
            let busy = dispatch(command("busy-tree")?, edge);
            assert_eq!(busy.result.status, Status::Refused);
            let message = &busy.result.error.as_ref().ok_or("busy error")?.message;
            assert!(
                message.contains(&format!(
                    "worktree writer is busy, held by attempt {holder}"
                )),
                "{message}"
            );
            assert!(
                session
                    .store
                    .operation(&"busy-tree".parse()?, &call)?
                    .is_none()
            );
            let replay = dispatch(command("free-tree")?, edge);
            assert_eq!(
                replay.result.status,
                Status::Ok,
                "{:?}",
                replay.result.error
            );
            assert_eq!(replay.result.data["run_id"], run_id);
            drop(lease);
            Ok(())
        },
    )??;
    Ok(())
}

#[test]
fn dispatch_reuses_only_free_lane_or_unowned_worktree() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{
        self, WorkerObserver, WorkerState, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
    };
    use provenant_core::RootInputs;
    use provenant_os::fixture::run_git;
    use std::ffi::OsString;
    struct Free;
    impl WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Free))
                .collect()
        }
    }
    struct Unknown;
    impl WorkerObserver for Unknown {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect()
        }
    }
    if reexec_test_in_lane("run::tests::dispatch_reuses_only_free_lane_or_unowned_worktree")? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    let mut inputs = RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        provenant_os::fixture::run_git(&project, &args)?;
    }
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    // Opening dispatch's real session initialises the state root first.
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let _session = open(&edge, &call).map_err(|fail| fail.message)?;
    let host = provenant_config::bind_host(&roots)?;
    let prompt = root.join("protected-prompt.md");
    std::fs::write(&prompt, "reply OK")?;
    let existing = worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
        worktrees::create(
            &worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            },
            WorktreeCreate {
                branch: "protected-existing".into(),
                base: Some("HEAD".into()),
            },
            &cancel,
        )
    })?;
    let primary = String::from_utf8(provenant_os::fixture::run_git(
        &project,
        &["symbolic-ref", "--short", "HEAD"],
    )?)?
    .trim()
    .to_owned();
    for (index, (target, placement, delivery)) in ["main", "master", primary.as_str()]
        .into_iter()
        .flat_map(|target| {
            [
                (target, "owned", "integrate"),
                (target, "existing", "integrate"),
                (target, "manifest", "integrate"),
                (target, "owned", "legacy-push"),
                (target, "existing", "legacy-push"),
                (target, "manifest", "push"),
                (target, "owned", "push"),
            ]
        })
        .enumerate()
    {
        let operation = format!("protected-integration-{index}");
        let mut args: Vec<OsString> = ["dispatch", "--operation-id", &operation, "--base", "HEAD"]
            .map(OsString::from)
            .into();
        let mode = if delivery == "integrate" {
            run::DeliveryMode::Integrate(target.into())
        } else {
            run::DeliveryMode::Push
        };
        if placement != "manifest" {
            args.extend(
                ["--model", "codex/gpt-6-luna", "--mode", "worktree-write"].map(OsString::from),
            );
        }
        if placement == "owned" {
            args.extend(
                [
                    "--worktree-branch",
                    if delivery == "push" {
                        target
                    } else {
                        "refused-lane"
                    },
                ]
                .map(OsString::from),
            );
        } else if placement == "existing" {
            args.extend([
                OsString::from("--worktree"),
                existing.path.as_os_str().to_owned(),
            ]);
        }
        if placement == "manifest" {
            let manifest = root.join("protected-manifest.json");
            std::fs::write(
                &manifest,
                serde_json::to_vec(&json!({"schema_version":1,"tasks":[{
                    "prompt":"reply OK", "route":run::RequestedRoute::Model {extra: Default::default(),  provider:"codex".into(), model:"codex/gpt-6-luna".into(), effort:None },
                    "deliver":run::HostDeliveryContext::destination(mode, target)?
                }]}))?,
            )?;
            args.extend([OsString::from("--manifest"), manifest.into_os_string()]);
        } else {
            if delivery == "legacy-push" {
                args.extend(
                    ["--deliver-remote", "origin", "--deliver-branch", target].map(OsString::from),
                );
            } else {
                args.extend([
                    OsString::from("--deliver"),
                    if delivery == "integrate" {
                        format!("integrate:{target}")
                    } else {
                        "push".into()
                    }
                    .into(),
                ]);
            }
            args.extend([
                OsString::from("--prompt-file"),
                prompt.as_os_str().to_owned(),
            ]);
        }
        let crate::ParseOutcome::Invocation(invocation) = crate::parse(args) else {
            return Err("parse".into());
        };
        let crate::Command::Dispatch(command) = invocation.command else {
            return Err("dispatch".into());
        };
        let reply = dispatch_with_workers(*command, &edge, &Free);
        assert!(
            _session
                .store
                .operation(&operation.parse()?, &call)?
                .is_none(),
            "protected delivery was admitted"
        );
        assert_eq!(reply.result.status, Status::Refused);
        assert!(matches!(
            run::HostDeliveryContext::check_delivery_branch(&project, target),
            Err(run::RunError::ProtectedDeliveryBranch)
        ));
        let branch = if delivery == "push" && placement == "owned" {
            target
        } else {
            "refused-lane"
        };
        assert!(!project.join(".worktrees").join(branch).exists());
    }
    let pr = run::HostDeliveryContext::destination(run::DeliveryMode::Pr, "protected-existing")?
        .ok_or("PR destination")?;
    check_delivery_branch(&project, &pr.branch).map_err(|error| error.message)?;
    assert!(pr.open_pr);
    for origin in [
        None,
        Some(WorktreeOrigin::Lane),
        Some(WorktreeOrigin::Adopted),
    ] {
        let branch = match origin {
            None => "unowned",
            Some(WorktreeOrigin::Lane) => "lane",
            Some(WorktreeOrigin::Adopted) => "adopted",
        };
        let (path, base) =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                let context = &worktrees::WorktreeContext {
                    workers: &Free,
                    ..*context
                };
                let info = worktrees::create(
                    context,
                    WorktreeCreate {
                        branch: branch.into(),
                        base: Some("HEAD".into()),
                    },
                    &cancel,
                )?;
                let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                if let Some(origin) = origin {
                    worktrees::save_owner(
                        &tree,
                        &WorktreeOwner {
                            attempt_id: None,
                            operation_id: Some(
                                OperationId::parse("previous-lane")
                                    .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                            ),
                            branch: branch.into(),
                            base: info.head.to_string(),
                            host,
                            created_at: WallTime::from_micros(1)
                                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                            origin,
                            label: Some("preserved-label".into()),
                        },
                    )?;
                }
                Ok((info.path, info.head.to_string()))
            })?;
        let missing = root.join("missing-prompt");
        let args: Vec<OsString> = [
            "dispatch",
            "--operation-id",
            "replacement-lane",
            "--model",
            "codex/gpt-6-luna",
            "--mode",
            "worktree-write",
            "--worktree-branch",
            branch,
            "--base",
            "HEAD",
            "--prompt-file",
            missing.to_str().ok_or("path")?,
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let crate::ParseOutcome::Invocation(invocation) = crate::parse(args) else {
            return Err("parse".into());
        };
        let crate::Command::Dispatch(command) = invocation.command else {
            return Err("dispatch".into());
        };
        let reply = dispatch_with_workers(*command, &edge, &Free);
        assert_ne!(reply.result.status, Status::Ok);
        let load = || {
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                worktrees::load_owner(&provenant_confinement::verify_worktree(
                    context.repo,
                    &path,
                )?)
            })
        };
        // The unreadable prompt fails preflight, before any ownership change.
        let previous = OperationId::parse("previous-lane")?;
        assert_eq!(
            load()?.map(|owner| owner.operation_id),
            origin.map(|_| Some(previous.clone()))
        );
        let claimed = super::own_worktree(
            &super::OwnedWorktree {
                branch: branch.into(),
                host,
                created_at: WallTime::from_micros(2)?,
            },
            Some("HEAD"),
            &OperationId::parse("replacement-lane")?,
            &edge,
            &Free,
            |_| false,
            &mut None,
            &clock,
            &cancel,
        );
        let owner = load()?.ok_or("owner")?;
        assert_eq!(owner.base, base);
        if origin == Some(WorktreeOrigin::Adopted) {
            assert_eq!(owner.origin, WorktreeOrigin::Adopted);
            assert_eq!(owner.operation_id, Some(previous));
            assert_eq!(owner.label.as_deref(), Some("preserved-label"));
            assert!(matches!(claimed, Err(OwnWorktreeError::ForeignOwner)));
        } else {
            assert_eq!(claimed?.0.path, path);
            assert_eq!(owner.origin, WorktreeOrigin::Lane);
            assert_eq!(
                owner.operation_id,
                Some(OperationId::parse("replacement-lane")?)
            );
        }
    }

    let base_a = String::from_utf8(run_git(&project, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    run_git(
        &project,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "middle",
        ],
    )?;
    let base_b = String::from_utf8(run_git(&project, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let prior_attempt = AttemptId::from_bytes([222; 16]);
    let (path, head) =
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            let context = &worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let info = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "terminal-unknown".into(),
                    base: Some(base_b.clone()),
                },
                &cancel,
            )?;
            run_git(
                &info.path,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "tip",
                ],
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let head = String::from_utf8(
                run_git(&info.path, &["rev-parse", "HEAD"])
                    .map_err(|_| worktrees::WorktreeError::InvalidOutput)?,
            )
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?
            .trim()
            .parse()
            .map_err(|_| worktrees::WorktreeError::InvalidOutput)?;
            let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(prior_attempt),
                    operation_id: Some(
                        "terminal-prior"
                            .parse()
                            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                    ),
                    branch: "terminal-unknown".into(),
                    base: base_a.clone(),
                    host,
                    created_at: WallTime::from_micros(1)
                        .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            )?;
            Ok((info.path, head))
        })?;
    let reused = super::own_worktree(
        &super::OwnedWorktree {
            branch: "terminal-unknown".into(),
            host,
            created_at: WallTime::from_micros(2)?,
        },
        Some(&base_b),
        &OperationId::parse("terminal-replacement")?,
        &edge,
        &Unknown,
        |attempt| attempt == prior_attempt,
        &mut None,
        &clock,
        &cancel,
    )?;
    assert_eq!(reused.0.path, path);
    assert_eq!(reused.0.head, head);
    assert_eq!(reused.1.to_string(), base_b);
    Ok(())
}

#[test]
fn failed_preflight_creates_no_worktree() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{WorkerObserver, WorkerState};
    use provenant_core::RootInputs;
    use std::ffi::OsString;
    // A census that cannot clear any tree: cleanup after a late failure
    // would retain whatever creation left behind.
    struct Unknown;
    impl WorkerObserver for Unknown {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, WorkerState> {
            roots
                .iter()
                .cloned()
                .map(|root| (root, WorkerState::Unknown))
                .collect()
        }
    }
    if reexec_test_in_lane("run::tests::failed_preflight_creates_no_worktree")? {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    let mut inputs = RootInputs::new(home.clone(), project.clone());
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let run_git = |args: &[&str]| provenant_os::fixture::run_git(&project, args);
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ][..],
    ] {
        run_git(args)?;
    }
    let edge = Edge {
        roots: &roots,
        home: &home,
        assets: &project,
        cwd: &project,
    };
    let prompt = root.join("prompt");
    let secret = root.join("secret-prompt");
    std::fs::write(&prompt, "review changes")?;
    std::fs::write(&secret, "sk-abcdefghijklmnopqrstuvwxyz123456")?;
    let missing = root.join("missing-prompt");
    for (input, extra, invalid) in [
        (&missing, vec![], None),
        (&prompt, vec!["--knob", "not.a.knob=1"], None),
        (&prompt, vec![], Some("title")),
        (&prompt, vec![], Some("fallback")),
        (&secret, vec![], None),
    ] {
        let mut args: Vec<OsString> = [
            "dispatch",
            "--operation-id",
            "preflight-lane",
            "--model",
            "codex/gpt-6-luna",
            "--mode",
            "worktree-write",
            "--worktree-branch",
            "preflight-lane",
            "--base",
            "HEAD",
            "--prompt-file",
            input.to_str().ok_or("path")?,
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        args.extend(extra.into_iter().map(OsString::from));
        let crate::ParseOutcome::Invocation(invocation) = crate::parse(args) else {
            return Err("parse".into());
        };
        let crate::Command::Dispatch(mut command) = invocation.command else {
            return Err("dispatch".into());
        };
        match invalid {
            Some("title") => command.title = Some("invalid\ntitle".into()),
            Some("fallback") => {
                command.route.fallback = vec![crate::route::Model("invalid".into())]
            }
            _ => {}
        }
        let reply = dispatch_with_workers(*command, &edge, &Unknown);
        assert_ne!(reply.result.status, Status::Ok);
        assert!(
            !project.join(".worktrees").join("preflight-lane").exists(),
            "{:?}",
            reply.result
        );
        // `rev-parse --verify --quiet` exits non-zero when the branch does not exist.
        assert!(
            run_git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/preflight-lane",
            ])
            .is_err()
        );
    }
    // A relative base is resolved in the source checkout exactly once.
    // The new tree has no HEAD^, so admission must use creation's receipt.
    run_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--allow-empty",
        "-qm",
        "second",
    ])?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
    let session = open(&edge, &call).map_err(|fail| fail.message)?;
    let operation = OperationId::parse("relative-base-lane")?;
    let (info, pinned) = own_worktree(
        &OwnedWorktree {
            branch: "relative-base-lane".into(),
            host: provenant_config::bind_host(&roots)?,
            created_at: clock.wall_now()?,
        },
        Some("HEAD^"),
        &operation,
        &edge,
        &Unknown,
        |_| false,
        &mut None,
        &clock,
        &cancel,
    )
    .map_err(|fail| fail.to_string())?;
    assert_eq!(info.head, pinned);
    let dispatch = |base: &str| run::DispatchRequest {
        chair: None,
        operation_id: operation.clone(),
        concurrency: 1,
        lane_depth: 0,
        parent_attempt: None,
        tasks: vec![run::DispatchTask {
            context: None,
            build_heavy: false,
            title: None,
            task_id: TaskId::from_bytes([3; 16]),
            prompt: "review changes".into(),
            base: Some(base.into()),
            pinned_base: Some(pinned),
            head: None,
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "fixture".into(),
                model: "fixture/model".into(),
                effort: None,
            },
            account: None,
            fallback: vec![],
            profile: Default::default(),
            timeout_seconds: 10,
            delivery_id: None,
            deliver: None,
            worktree: Some(info.path.clone()),
            access: Default::default(),
        }],
    };
    let service = run::Service::new(&session.store);
    let record = service.dispatch(dispatch("HEAD^"), &project, &call)?;
    let canonical = session.store.request(record.run_id, &call)?;
    assert_eq!(
        canonical.tasks[0].base.as_deref(),
        Some(pinned.to_string().as_str())
    );
    assert_eq!(canonical.tasks[0].base_ref.as_deref(), Some("HEAD^"));
    assert_eq!(
        service.dispatch(dispatch("HEAD^"), &project, &call)?.run_id,
        record.run_id
    );
    let changed_base = service.dispatch(dispatch("HEAD"), &project, &call);
    assert!(
        matches!(changed_base, Err(run::RunError::OperationConflict)),
        "{changed_base:?}"
    );
    let mut with_head = dispatch("HEAD^");
    with_head.operation_id = OperationId::parse("relative-base-with-head")?;
    with_head.tasks[0].head = Some(pinned.to_string());
    service.dispatch(with_head, &project, &call)?;
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
fn manifest_accepts_task_git_base() {
    let manifest: Manifest = serde_json::from_value(json!({
            "schema_version": 1,
            "tasks": [
                {"prompt": "review", "title": "Review changes", "base": "main", "route": {"kind": "named", "name": "senior"}},
                {"prompt": "review", "route": {"kind": "model", "provider": "codex", "model": "gpt-6-luna"}}
            ]
        }))
        .unwrap();
    assert_eq!(manifest.tasks[0].base.as_deref(), Some("main"));
    assert_eq!(manifest.tasks[1].base, None);
    assert_eq!(manifest.tasks[0].title.as_deref(), Some("Review changes"));
    assert_eq!(manifest.tasks[1].title, None);
    assert!(
        matches!(&manifest.tasks[0].route, run::RequestedRoute::Named { name, .. } if name == "senior")
    );
    assert!(
        matches!(&manifest.tasks[1].route, run::RequestedRoute::Model { provider, model, .. } if provider == "codex" && model == "gpt-6-luna")
    );
}

#[test]
fn manifest_task_network_and_adapter_route_syntax() -> Result<(), Box<dyn std::error::Error>> {
    let manifest: Manifest = serde_json::from_value(json!({"schema_version": 1, "tasks": [
        {"prompt": "x", "route": {"kind": "adapter", "provider": "command", "name": "fmt"}, "network": "off"},
        {"prompt": "x", "route": {"kind": "adapter", "provider": "command", "name": "fmt"}}
    ]}))?;
    assert!(matches!(manifest.tasks[0].network, Some(Switch::Off)));
    assert!(manifest.tasks[1].network.is_none());
    assert!(
            serde_json::from_value::<Manifest>(json!({"schema_version": 1, "tasks": [
                {"prompt": "x", "route": {"kind": "adapter", "provider": "command", "name": "fmt"}, "network": "maybe"}
            ]}))
            .is_err()
        );
    assert!(matches!(
        requested(None, Some("command/fmt"), None).map_err(|_| "adapter route")?,
        run::RequestedRoute::Adapter { provider, name, .. } if provider == "command" && name == "fmt"
    ));
    assert!(requested(None, Some("command/fmt"), Some("high".into())).is_err());
    assert!(matches!(
        requested(None, Some("senior"), None).map_err(|_| "named route")?,
        run::RequestedRoute::Named { .. }
    ));
    Ok(())
}

#[test]
fn manifest_rejects_flat_route_fields() {
    for task in [
        json!({"prompt": "review", "model": "codex/gpt-6-luna", "effort": "high"}),
        json!({"prompt": "review", "route": "senior"}),
        json!({"prompt": "review", "route": {"kind": "named", "name": "senior"}, "effort": "high"}),
        json!({"prompt": "review"}),
    ] {
        assert!(
            serde_json::from_value::<Manifest>(json!({"schema_version": 1, "tasks": [task]}))
                .is_err()
        );
    }
}

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
            .chain(
                [
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                ]
                .into_iter()
                .chain(args.iter().copied())
                .map(OsString::from),
            )
            .collect();
        let env = vec![
            (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                OsString::from("/dev/null"),
            ),
        ];
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
            context: None,
            build_heavy: false,
            title: None,
            task_id: TaskId::from_bytes([3; 16]),
            prompt: "review".into(),
            base: Some(base.into()),
            pinned_base: None,
            head: None,
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "fixture".into(),
                model: "fixture/model".into(),
                effort: None,
            },
            account: None,
            fallback: vec![],
            provider_timeout: 10,
            delivery_id: None,
            deliver: None,
            worktree: None,
            access: Default::default(),
        }],
        concurrency: Some(1),
        knobs: vec![],
    };
    let (first, _) =
        submit(&session, &edge, submission("context-base"), &call).map_err(|f| f.message)?;
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
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
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
                fallback: vec![],
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
        ) -> Result<run::PreparedLaunch, String> {
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
    assert_eq!(error.code, "reconcile_incomplete");
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
    for (wire, expected) in [
        (json!({"state":"not_attempted"}), "NotAttempted"),
        (json!({"state":"no_new_commits"}), "NoNewCommits"),
        (
            json!({"state":"failed","stage":"push"}),
            "Failed { stage: Push }",
        ),
        (
            json!({"state":"delivered","pull_request":null}),
            "Delivered { pull_request: None }",
        ),
        (
            json!({"state":"delivered","pull_request":"https://example.invalid/pr/1"}),
            "Delivered { pull_request: Some(\"https://example.invalid/pr/1\") }",
        ),
        (
            json!({"state":"delivered","pull_request":"pr\u{001b}\n1"}),
            "Delivered { pull_request: Some(\"pr1\") }",
        ),
    ] {
        let mut wire = wire;
        wire["future_note"] = json!("future-secret");
        let state = serde_json::from_value(wire).unwrap();
        assert_eq!(delivery_state_text(&state), expected);
    }
}

#[test]
fn render_lanes_removes_peer_terminal_controls() -> Result<(), Box<dyn std::error::Error>> {
    let reason = "\x1b[31mreason\x1b[0m";
    let account = "\x1b]0;account\x07";
    let detail = "skip\n\r\t\u{0008}\u{202e}detail";
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
    assert_eq!(
        human,
        format!(
            "{}/{}/{} Reviewchanges failed fixture/[32mmodel account=]0;account ([31mreason[0m)\n  skipped fixture/[33mfallback Unavailable: skipdetail\n  delivery origin:mainline NotAttempted head=unknown\n",
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
    assert_eq!(data["lanes"][0]["reason"], json!(reason));
    assert_eq!(data["lanes"][0]["write_grants"][0]["source"], "write_root");
    assert_eq!(data["lanes"][0]["write_grants"][0]["scope"], "subtree");
    assert_eq!(
        data["lanes"][0]["write_grants"][0]["path"]["relative_to_root"]["path"],
        "work/shared"
    );
    assert_eq!(data["lanes"][0]["read_only_writable"], json!(["locks"]));
    assert_eq!(
        data["lanes"][0]["git_grants_digest"],
        json!(provenant_core::Digest::from_bytes([6; 32]))
    );
    assert_eq!(data["lanes"][0]["account"], json!(account));
    assert_eq!(
        data["lanes"][0]["skipped_routes"][0]["detail"],
        json!(detail)
    );
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
    assert!(human.contains("(provider capacity: Selected model is at capacity)"));
    assert!(human.contains(&format!(
        "continue: provenant resume {} --prompt-file PROMPT",
        lane.attempt_id
    )));
    lane.session = None;
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(!human.contains("provenant resume"));
    assert!(human.contains("redispatch with another model"));
    lane.session = Some("session".into());
    if let run::AttemptState::Terminal { reason, .. } = &mut lane.state {
        *reason = Some("provider model unavailable: no route".into());
    }
    let (_, human) =
        render_lanes_without_cooldowns(std::slice::from_ref(&lane), &[], None, Path::new("/state"));
    assert!(!human.contains("provenant resume"));
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
        data["lanes"][0]["protected_paths"][0]["path"],
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
    assert_eq!(data["lanes"][0]["settlement"]["reason"], "provider failed");
    assert_eq!(data["lanes"][1]["settlement"], Value::Null);
    assert_eq!(data["lanes"][1]["reason"], Value::Null);
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
        transcript
            .join("sess-1848.jsonl")
            .to_string_lossy()
            .as_ref()
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
        path.to_string_lossy().as_ref()
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
            AttemptId::from_bytes([1; 16])
        )
    );
    assert!(!human.contains("account="));
    assert!(data["lanes"][0].get("title").is_none());
    assert_eq!(data["lanes"][0]["account"], json!("unknown"));
    assert_eq!(data["lanes"][0]["state"], "rejected");
    assert_eq!(data["lanes"][0]["reason"], json!(reason));
    assert!(data["lanes"][0]["applied_route"].is_null());
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
fn render_lanes_shows_uncommitted_with_its_dirty_paths() -> Result<(), Box<dyn std::error::Error>> {
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
        human.contains(&format!(" uncommitted unresolved ({reason})\n")),
        "{human}"
    );
    assert_eq!(data["lanes"][0]["state"], "uncommitted");
    assert_eq!(data["lanes"][0]["reason"], json!(reason));
    Ok(())
}
#[test]
fn explicit_write_root_is_checked_at_preflight_and_routing()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("safe");
    std::fs::create_dir_all(&root)?;
    let root = std::fs::canonicalize(root)?;
    let parse = |flag: &str, route: &str| {
        let args = [
            "run",
            "--prompt-stdin",
            "--mode",
            "read-only",
            "--write-root",
            root.to_str().unwrap(),
            flag,
            route,
        ];
        match crate::parse(args.into_iter().map(std::ffi::OsString::from)) {
            crate::ParseOutcome::Invocation(inv) => match inv.command {
                crate::Command::Run(request) => request,
                _ => panic!("expected run"),
            },
            other => panic!("{other:?}"),
        }
    };
    let command = parse("--route", "command/fmt");
    let access = lane_access(
        &command.access,
        &temp.path().join("state"),
        Path::new("/fixture/host-home"),
    )
    .map_err(|e| e.message)?;
    assert_eq!(access.write_roots, vec![root.clone()]);
    let fail = check_flag_capabilities(&command, None).unwrap_err();
    assert_eq!(fail.kind.code(), "UNSUPPORTED");
    assert_eq!(fail.message, "command does not support write");
    let command = parse("--model", "codex/gpt-6-luna");
    check_flag_capabilities(&command, None).map_err(|e| e.message)?;
    let mut task = trace_run(
        1,
        AttemptId::from_bytes([3; 16]),
        WallTime::from_micros(100)?,
    )?
    .request
    .tasks
    .remove(0);
    task.route = requested(Some("codex/gpt-6-luna"), None, None).map_err(|e| e.message)?;
    task.access = access;
    assert!(task.writer.is_none());
    assert!(route_request(&task)?.intent.writes);
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
                mode: Some(Mode::ReadOnly),
                worktree: None,
                read_root: if read { vec![root.clone()] } else { vec![] },
                write_root: if read { vec![] } else { vec![root.clone()] },
                network: None,
                capability: vec![],
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
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: vec![],
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
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: vec![],
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
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&project)?;
    let access = |mode, network, capability: &[&str], write_root: Vec<PathBuf>| crate::Access {
        mode: Some(mode),
        worktree: None,
        read_root: Vec::new(),
        write_root,
        network,
        capability: capability.iter().map(|name| name.to_string()).collect(),
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
    assert!(matches!(
        lane_access_checked(
            &access(Mode::ReadOnly, None, &["rust"], Vec::new()),
            &state,
            Path::new("/fixture/host-home"),
        ),
        Err(AccessError::UnknownCapability(name)) if name == "rust"
    ));
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
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&inside)?;
    let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
        mode: Some(Mode::WorktreeWrite),
        worktree: Some(worktree.clone()),
        read_root,
        write_root,
        network: None,
        capability: Vec::new(),
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
        mode: Some(Mode::ReadOnly),
        worktree: None,
        read_root,
        write_root,
        network: None,
        capability: Vec::new(),
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
fn quota_reason_uses_provider_message_without_exposing_known_secret_shapes() {
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: RunId::from_bytes([1; 16]),
            attempt: AttemptId::from_bytes([2; 16]),
            generation: 1,
        },
        termination: Some(provenant_os::exec::Termination::Exit(1)),
        never_spawned: false,
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
    };
    for (message, expected) in [
        (
            "Free allowance exhausted. See https://opencode.ai/go\\quota",
            "Free allowance exhausted. See https://opencode.ai/go\\quota",
        ),
        (
            "Free allowance exhausted for ghp_abcdefghijklmnopqrstuvwxyz0123456789",
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
        assert_eq!(decoded.reason.as_deref(), Some(format!("{expected}; free tier exhausted; use a paid route or wait for its allowance reset").as_str()));
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
    assert!(reason.ends_with('☀'));
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
    assert!(
        reason
            .starts_with("provider model unavailable: Model unavailable: opencode/misspelt-free;"),
        "{reason}"
    );
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
        json!({"type":"turn.failed","error":{"message":"Selected model is at capacity. Please try a different model.","codex_error_info":"server_overloaded"}})
    );
    let decoded = Registry::default()
        .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
        .unwrap();
    assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
    assert_eq!(
        decoded.reason.as_deref(),
        Some("provider capacity: Selected model is at capacity. Please try a different model.")
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
        Some("provider capacity: at capacity")
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
    std::fs::create_dir_all(roots.state.as_path())?;
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
        Some("provider capacity: at capacity")
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
        Some("provider capacity: at capacity")
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
        codex_home: None,
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
                .starts_with("provider capacity: Overloaded")
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
    for (message, expected) in [
        (
            "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
            "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
        ),
        (
            "429 Too Many Requests for ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
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
        assert_eq!(decoded.reason.as_deref(), Some(expected.as_str()));
        assert!(decoded.reason.unwrap().len() <= 8192);
    }
}

#[test]
fn agy_denials_preserve_answers_and_prior_failures() {
    let route = run::RouteRecord {
        provider: Some("agy".into()),
        model: Some("agy/gemini-3.8-flash".into()),
        ..Default::default()
    };
    let fixture = include_str!("../../../../tests/fixtures/providers/agy/denied-result.jsonl");
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
        Some("provider denied: RunCommand")
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
        Some("provider denied: RunCommand")
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
    assert_eq!(
        decoded.session.as_deref(),
        Some("01234567-89ab-cdef-0123-456789abcdef")
    );
    assert!(decoded.observed_route.is_none());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("Codex rollout identity: provider continuation identity is unknown")
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
    let (data, line) = reviewer_receipt(&record, false, &[]).unwrap();
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
    let unobserved = reviewer_receipt(&record, false, &[]).unwrap().0;
    assert_eq!(unobserved["reviewer"]["provenance"], "unknown");
    assert_ne!(unobserved["reviewer"]["actor_id"], "unknown");
    record.observed_route = record.applied_route.clone();
    let observed_fact = run::fact(record.clone())?.reviewer.ok_or("reviewer fact")?;
    let observed = reviewer_receipt(&record, false, &[]).unwrap();
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
    let (writer_data, writer_line) = reviewer_receipt(&record, true, &[]).unwrap();
    assert_eq!(writer_data["role"], "writer");
    assert_eq!(writer_data["family"], "openai");
    assert!(writer_line.starts_with("Writer: codex/gpt-6.1-sol@high (openai;"));
    record.applied_route.as_mut().unwrap().effort = None;
    assert!(
        reviewer_receipt(&record, false, &[])
            .unwrap()
            .1
            .contains("@unknown")
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/uncatalogued".into());
    let unknown_family = reviewer_receipt(&record, false, &[]).unwrap().0;
    assert_eq!(unknown_family["family"], "unknown");
    assert_eq!(unknown_family["resolved_from"], "unknown");
    // A differing legacy route does not prove fallback without canonical task evidence.
    let fallback = record.applied_route.clone().ok_or("fallback route")?;
    assert_eq!(
        reviewer_receipt(&record, false, std::slice::from_ref(&fallback))
            .unwrap()
            .0["resolved_from"],
        "unknown"
    );
    record.skipped_routes.push(serde_json::from_value(
        json!({"route":"codex/gpt-6.1-sol","kind":"unavailable","detail":"unavailable"}),
    )?);
    assert_eq!(
        reviewer_receipt(&record, false, &[fallback]).unwrap().0["resolved_from"],
        "fallback"
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/gpt-6.1-sol".into());
    record.applied_route.as_mut().unwrap().pool_selection = Some(serde_json::from_value(
        json!({"pool":"junior","candidates":[],"roll":0,"total_weight":1,"pick":"codex/gpt-6.1-sol"}),
    )?);
    // An unavailable candidate is still a pool selection, not a fallback.
    assert_eq!(
        reviewer_receipt(&record, false, &[]).unwrap().0["resolved_from"],
        "pool junior"
    );
    record.applied_route.as_mut().unwrap().model = Some("codex/other".into());
    assert_eq!(
        reviewer_receipt(&record, false, &[]).unwrap().0["resolved_from"],
        "unknown"
    );
    record.applied_route = None;
    record.observed_route = None;
    let unknown = reviewer_receipt(&record, false, &[]).unwrap().0;
    for field in ["adapter", "model", "effort", "family", "resolved_from"] {
        assert_eq!(unknown[field], "unknown", "{field}");
    }
    record.applied_route = Some(Default::default());
    assert_eq!(
        reviewer_receipt(&record, false, &[]).unwrap().0["model"],
        "unknown"
    );
    record.state = run::AttemptState::Running {
        extra: Default::default(),
    };
    let refused = reviewer_receipt(&record, false, &[]).unwrap_err();
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
    assert!(decoded.observed_route.is_none());
    assert_eq!(
        decoded.reason.as_deref(),
        Some("OpenCode session identity: provider continuation identity is unknown")
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
        Some("provider stopped early: stop_reason max_tokens")
    );
    assert_ne!(decoded.outcome, run::Outcome::Failed);
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
}
