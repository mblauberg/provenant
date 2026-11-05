//! Same-binary guardian driver for the writer continuation launch regression.
#![allow(dead_code, unused_imports, clippy::disallowed_methods)]
use crate::*;
use provenant_confinement::{
    CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest,
    permissions::{
        AccessMode, AuthenticationProfile, BackendContext, FixtureOperation, FixtureOutcome,
        PathGrant, PathScope, PlanPermissions, ProviderProfile, ProviderProfileSpec,
        WorktreeWriteAuthority, system_backend,
    },
};
use provenant_core::{CancelToken, Clock, GitOid, RootInputs, WorkScope};
use provenant_os::{
    clock::SystemClock,
    exec::{ExecOptions, Executor, SystemExecutor, Termination},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) fn main() -> TestResult {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "internal") {
        let holder = RunHolder {
            providers: &PinnedWriter,
            resources: &SystemResources,
        };
        provenant_supervisor::supervise(
            &args[2..],
            provenant_supervisor::GuardianHooks {
                holder: Some(&holder),
                nested_children: Some(&NoChildren),
                observer: Some(&provenant_supervisor::fixture::RecordedBootObserver),
                ..Default::default()
            },
        )?;
        return Ok(());
    }
    // Match the existing worktree driver's host isolation: ambient lane
    // context must not turn this host-owned retry into a nested lane launch.
    let removed = [
        provenant_core::RUN_STATE_DIRECTORY_ENV,
        crate::LANE_DEPTH_ENV,
        crate::PARENT_ATTEMPT_ENV,
    ];
    if removed.iter().any(|name| std::env::var_os(name).is_some()) {
        let argv: Vec<_> = std::iter::once(std::env::current_exe()?.into_os_string())
            .chain(args)
            .collect();
        let environment: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| !removed.iter().any(|name| key == name))
            .collect();
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(60));
        options.env = Some(&environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        use std::io::Write;
        std::io::stdout().lock().write_all(&output.stdout.bytes)?;
        std::io::stderr().lock().write_all(&output.stderr.bytes)?;
        if output.termination != Termination::Exit(0) {
            return Err("isolated continuation test failed".into());
        }
        return Ok(());
    }
    provenant_supervisor::fixture::run_libtest(
        &args,
        &["writer_continuation_launches_pinned_to_predecessor_head"],
        |_| launches().map_err(|error| std::io::Error::other(error.to_string())),
    )?;
    Ok(())
}
const HANDOFF: &str = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"pinned writer fixture\"}\n```";
struct NoChildren;
impl provenant_supervisor::NestedChildStore for NoChildren {
    fn has_no_nested_runs(&self, _: &[u8]) -> bool {
        true
    }
}
struct PinnedWriter;
impl Providers for PinnedWriter {
    fn prepare(
        &self,
        task: &TaskRequest,
        _: &str,
        paths: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String> {
        prepare(task, paths).map_err(|error| error.to_string())
    }
    fn decode(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        exit: &provenant_supervisor::custody::ExitObservation,
        _: &Path,
    ) -> std::result::Result<Decoded, String> {
        if exit.termination != Some(Termination::Exit(0)) {
            return Err("provider failed".into());
        }
        Ok(Decoded {
            outcome: Outcome::Succeeded,
            // A finished lane reports a handoff; without one it settles review_required.
            result: format!("{}\n{HANDOFF}", String::from_utf8_lossy(stdout).trim_end()),
            complete: true,
            observed_route: Some(route.clone()),
            usage: Default::default(),
            session: None,
            reason: None,
            partial_signals: Vec::new(),
        })
    }
}
fn prepare(task: &TaskRequest, paths: &LaunchPaths) -> TestResult<PreparedLaunch> {
    let request = task.writer.as_ref().ok_or("writer")?;
    let state = paths
        .provider_state
        .ancestors()
        .nth(5)
        .ok_or("state root")?;
    let home = state.parent().ok_or("fixture root")?.join("home");
    let roots = RootInputs {
        state_dir: Some(state.into()),
        ..RootInputs::new(home.clone(), request.project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(request.project.join(".git"))?,
    )?;
    let writer = prepare_writer(request, state, binding.project_id())?;
    let source = task
        .source_head
        .as_deref()
        .ok_or("missing lineage source")?
        .parse()?;
    let predecessor = task
        .head
        .as_deref()
        .ok_or("missing predecessor head")?
        .parse()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    writer.require_continuation_source(&git, source, predecessor)?;
    let grant = |path| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let authority = WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch, &home)?;
    let scratch = paths.temporary.join("candidate-0");
    {
        use std::os::unix::fs::DirBuilderExt;
        // Confinement refuses scratch that other users can read.
        std::fs::DirBuilder::new().mode(0o700).create(&scratch)?;
    }
    let plan = ConfinedPlan::new(PlanRequest {
        scope: WorkScope::Project(binding.project_id()),
        permissions: PlanPermissions {
            read_only: None,
            mode: AccessMode::WorktreeWrite(Box::new(authority)),
            scratch: grant(scratch),
            private_state: Some(grant(paths.provider_state.clone())),
            read_roots: Vec::new(),
            protected_roots: Vec::new(),
            secret_roots: Vec::new(),
            selected_authentication: None,
            include_continuation: false,
            write_roots: Vec::new(),
            network: provenant_confinement::permissions::NetworkAccess::Provider,
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })?;
    let executable = std::fs::canonicalize("/usr/bin/printf")?;
    use FixtureOperation as Op;
    use FixtureOutcome::{Allowed, Denied};
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            authentication: vec![AuthenticationProfile {
                mechanism: "login".into(),
                roots: vec![PathGrant {
                    path: executable.clone(),
                    scope: PathScope::Exact,
                }],
                in_place_writes: Vec::new(),
            }],
            process_executables: vec![PathGrant {
                path: executable.clone(),
                scope: PathScope::Exact,
            }],
            ..Default::default()
        },
        observations: [
            (Op::DeclaredRead, Allowed),
            (Op::UndeclaredRead, Denied),
            (Op::UndeclaredWrite, Denied),
            (Op::SecretRead, Denied),
            (Op::ProtectedWrite, Denied),
            (Op::OtherNetwork, Denied),
            (Op::RuntimeRead, Allowed),
            (Op::AuthenticationRead, Allowed),
            (Op::ProtectedInstructionRead, Allowed),
            (Op::DeclaredProcessExec, Allowed),
            (Op::UndeclaredProcessExec, Denied),
            (Op::ProcessFork, Denied),
            (Op::WorktreeWrite, Allowed),
            (Op::GitIndexWrite, Allowed),
            (Op::GitHeadWrite, Denied),
            (Op::GitObjectsWrite, Denied),
            (Op::SelectedRefWrite, Denied),
            (Op::GitConfigWrite, Denied),
            (Op::OtherRefWrite, Denied),
        ]
        .into(),
    };
    Ok(PreparedLaunch {
        // The launched process reports the base accepted by the real writer
        // preparation, rather than merely echoing the requested pin.
        argv: vec![
            executable.into_os_string(),
            "%s".into(),
            writer.base.to_string().into(),
        ],
        environment: Vec::new(),
        stdin: Vec::new(),
        secret_input: None,
        cwd: writer.worktree.path().into(),
        route: crate::tests::route(),
        skipped_routes: Vec::new(),
        observed_head: None,
        repository: None,
        confinement: Some(LaunchConfinement {
            plan,
            profile,
            writer: Some(writer),
            home,
            cargo_registry_seed: false,
            state_root: state.into(),
        }),
    })
}
fn launches() -> TestResult {
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut context = crate::tests::context(&base)?;
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(context.state_root.as_path())?;
    let project = base.join("checkout");
    let worktree = project.join(".worktrees/writer");
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let run_git = |args: &[&str], cwd: &Path| -> TestResult<String> {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.iter().map(Into::into))
            .collect();
        let env = [
            ("HOME".into(), base.join("home").into_os_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ];
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
        options.env = Some(&env);
        let output = executor.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
    };
    run_git(&["init", "-q"], &project)?;
    run_git(
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "source",
        ],
        &project,
    )?;
    let source = run_git(&["rev-parse", "HEAD"], &project)?;
    run_git(
        &[
            "worktree",
            "add",
            "-qb",
            "writer",
            worktree.to_str().ok_or("path")?,
        ],
        &project,
    )?;
    run_git(
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "predecessor",
        ],
        &worktree,
    )?;
    let head: GitOid = run_git(&["rev-parse", "HEAD"], &worktree)?.parse()?;
    let roots = RootInputs {
        state_dir: Some(context.state_root.as_path().into()),
        ..RootInputs::new(base.join("home"), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    context.project_id = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(project.join(".git"))?,
    )?
    .project_id();
    let clock = SystemClock::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&context, &call)?;
    let cleanup =
        provenant_supervisor::fixture::ProcessCleanupGuard::new(context.state_root.as_path());
    let mut input = crate::tests::input();
    input.created_at = clock.wall_now()?;
    input.request.tasks[0].writer = Some(WriterRequest {
        extra: Default::default(),
        project: project.clone(),
        worktree,
    });
    input.request.tasks[0].head = Some(source);
    store.create(&input, &call)?;
    crate::tests::seal_observation(&store, &input, &crate::tests::exit())?;
    let id = input.attempts[0].attempt_id;
    let old = store.record(id, &call)?;
    store.stage_policy_retry(
        &old,
        &PolicyRetry {
            schema_version: 2,
            cause: PolicyCause::Quota,
            action: PolicyAction::Retry,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Continue".into(),
            not_before_unix_seconds: None,
        },
        &call,
    )?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    crate::artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &crate::db::bytes(&crate::writer::WriterTerminal {
            schema_version: 1,
            head,
            protected_paths: Vec::new(),
        })?,
        RECORD_LIMIT,
    )?;
    let mut publication = crate::tests::publication();
    publication.outcome = Outcome::QuotaLimited;
    publication.finished_at = clock.wall_now()?;
    publication.started_at = None;
    publication.provider_ms = None;
    store.publish_terminal(id, 1, &publication, &crate::tests::exit(), &call)?;
    let queued = store.pending_attempts(input.run_id, &call)?;
    assert_eq!(queued.len(), 1);
    let successor = queued[0].attempt_id;
    match system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        #[cfg(target_os = "macos")]
        env: &[],
    }) {
        Ok(_) => {}
        Err(error) => {
            provenant_test_support::skip_sandbox_dependent_test(error)?;
            return Ok(());
        }
    }
    let started = store.start_with_writer_lease(
        input.run_id,
        &PinnedWriter,
        &|record, actor| {
            HolderContext {
                schema_version: 1,
                roots: RootSeed {
                    home: base.join("home"),
                    checkout: project.clone(),
                    state_root: context.state_root.as_path().into(),
                },
                project_id: context.project_id,
                actor_id: actor,
                run_id: record.run_id,
                attempt_id: record.attempt_id,
                host_delivery: None,
                launch: None,
                sealed: None,
            }
            .encode()
        },
        &|directory, scope, binding| {
            provenant_supervisor::fixture::acquire_writer_with_binding(
                directory,
                scope,
                binding,
                provenant_core::WaitBudget::nonblocking(clock.mono_now()),
                &cancel,
                &clock,
            )
        },
        None,
        None,
        &call,
    )?;
    assert!(
        matches!(started.as_slice(), [Started::Launched(id)] if *id == successor),
        "{started:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let record = store.record(successor, &call)?;
        if let AttemptState::Terminal { outcome, .. } = record.state {
            assert_eq!(outcome, Outcome::Succeeded, "{record:?}");
            assert_eq!(record.head, Some(head));
            let result = record.result.as_ref().ok_or("provider result")?;
            assert_eq!(
                store.artifact_bytes(record.run_id, result, RECORD_LIMIT)?,
                format!("{head}\n{HANDOFF}").as_bytes()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "successor did not settle: {record:?}"
        );
        std::thread::park_timeout(Duration::from_millis(20));
    }
    while !store.writer_fences()?.is_empty() {
        assert!(
            Instant::now() < deadline,
            "successor custody was not released"
        );
        std::thread::park_timeout(Duration::from_millis(20));
    }
    cleanup.disarm();
    Ok(())
}
