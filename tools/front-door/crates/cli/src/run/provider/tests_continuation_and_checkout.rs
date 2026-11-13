
#[test]
fn writer_continuation_preserves_and_reports_uncommitted_edits()
-> Result<(), Box<dyn std::error::Error>> {
    writer_continuation_dirty_brief(true)
}

#[test]
fn writer_continuation_with_unchanged_head_reports_dirty_paths()
-> Result<(), Box<dyn std::error::Error>> {
    writer_continuation_dirty_brief(false)
}

fn writer_continuation_dirty_brief(committed: bool) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime, worktrees};
    use provenant_core::{CancelToken, Clock, Digest, TaskId, WaitBudget};
    use provenant_os::clock::SystemClock;
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("repo");
    let git_home = base.join("git-home");
    for path in [&home, &state, &project, &git_home] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = UtilityRuntime::new(&git, &git_home)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = utility.open_repo(&executor, &project, budget, &cancel)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let worktree_state = state
        .join("worktrees")
        .join(binding.project_id().to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&worktree_state)?;
    let project_state = provenant_os::fs::Directory::open_anchor(&worktree_state)?;
    let clock = SystemClock::new();
    worktrees::create(
        &worktrees::WorktreeContext {
            home: &home,
            repo: &repo,
            binding: &binding,
            project_state: &project_state,
            executor: &executor,
            utility: &utility,
            facts: &worktrees::RepoFacts::default(),
            workers: &worktrees::SystemWorkerObserver,
            clock: &clock,
            utility_budget: budget,
            mutation_budget: budget,
            lock_budget: WaitBudget::cli(clock.mono_now(), 5)?,
        },
        worktrees::WorktreeCreate {
            branch: "writer".into(),
            base: Some("HEAD".into()),
        },
        &cancel,
    )?;
    let worktree = project.join(".worktrees/writer");
    let source_head = run::resolve_git_base(&git, &project, "HEAD")?;

    let run_git = |args: &[&str], cwd: &std::path::Path| {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.iter().map(Into::into))
            .collect();
        let mut env = vec![
            ("HOME".into(), git_home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        env.extend(provenant_os::fixture::fixture_git_settings());
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
        options.env = Some(&env);
        executor.execute(options)
    };
    if committed {
        std::fs::write(worktree.join("committed.txt"), "predecessor work")?;
        run_git(&["add", "committed.txt"], &worktree)?;
        let commit_res = run_git(&["commit", "-m", "predecessor commit"], &worktree)?;
        assert_eq!(commit_res.termination, Termination::Exit(0));
    }
    let predecessor_head = run::resolve_git_base(&git, &worktree, "HEAD")?;
    assert_eq!(source_head != predecessor_head, committed);

    std::fs::write(
        worktree.join("uncommitted_leftover.txt"),
        "uncommitted dirty edit",
    )?;

    let prompt = "Fix remaining issues in continuation";
    let task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        build_heavy: false,
        title: None,
        base: Some(source_head.clone()),
        head: Some(predecessor_head.clone()),
        source_head: Some(source_head.clone()),
        base_ref: None,
        task_id: TaskId::from_bytes([22; 16]),
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/gpt-5.3-codex-low".into(),
            effort: None,
        },
        account: None,
        authority_digest: Digest::from_bytes([23; 32]),
        writer: Some(run::WriterRequest {
            extra: Default::default(),
            project: project.clone(),
            worktree: worktree.clone(),
        }),
        deliver: None,
        provider_timeout_seconds: 20,
        delivery_id: None,
        access: Default::default(),
    };

    let writer = run::prepare_writer_with_facts(
        task.writer.as_ref().ok_or("writer request")?,
        &state,
        binding.project_id(),
        &Default::default(),
    )?;
    let context = writer_source_context(&writer, &task, &git)?.ok_or("retained paths missing")?;
    assert!(context.contains("uncommitted_leftover.txt"), "{context}");
    assert_eq!(
        std::fs::read_to_string(worktree.join("uncommitted_leftover.txt"))?,
        "uncommitted dirty edit"
    );

    // A source mismatch still refuses and names both commits and retained edits.
    let mut bad_task = task.clone();
    bad_task.source_head = Some("0".repeat(40));
    let error = writer_source_context(&writer, &bad_task, &git)
        .err()
        .ok_or("mismatched continuation should refuse")?;
    let error_text = error.to_string();
    assert!(error_text.contains(&"0".repeat(40)), "{error_text}");
    assert!(error_text.contains(&predecessor_head), "{error_text}");
    assert!(
        error_text.contains("uncommitted predecessor edits: uncommitted_leftover.txt"),
        "refusal must list uncommitted predecessor edits: {error_text}"
    );
    let mut moved_task = task.clone();
    moved_task.head = Some("1".repeat(40));
    assert!(writer_source_context(&writer, &moved_task, &git).is_err());

    // Inject a Git that works for ancestry but refuses status. Exercise both
    // the brief path and the mismatch path: neither may hide missing evidence.
    let status_git = base.join("status-git");
    std::fs::write(
        &status_git,
        format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = status ]; then echo forced-status-failure >&2; exit 73; fi\ndone\nexec '{}' \"$@\"\n",
            git.display()
        ),
    )?;
    std::fs::set_permissions(&status_git, std::fs::Permissions::from_mode(0o700))?;
    for candidate in [&task, &bad_task] {
        let error = writer_source_context(&writer, candidate, &status_git)
            .err()
            .ok_or("status failure must refuse preparation")?;
        assert!(
            error
                .to_string()
                .contains("cannot inspect uncommitted predecessor edits"),
            "status failure must be explicit: {error}"
        );
    }

    Ok(())
}

#[test]
fn read_only_checkout_warning_measures_the_selected_checkout_against_origin_default()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};

    fn git(
        executor: &SystemExecutor,
        executable: &Path,
        repo: &Path,
        env: &[(std::ffi::OsString, std::ffi::OsString)],
        args: &[&str],
    ) -> Result<String, Box<dyn std::error::Error>> {
        let argv: Vec<_> = std::iter::once(executable.as_os_str().to_owned())
            .chain(args.iter().map(std::ffi::OsString::from))
            .collect();
        let mut options = ExecOptions::new(&argv, repo, std::time::Duration::from_secs(10));
        options.env = Some(env);
        let output = executor.execute(options)?;
        if output.termination != Termination::Exit(0) {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes)
            )
            .into());
        }
        Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
    }

    // The remote's HEAD names master. origin/main is a different line, two
    // commits past the shared root, so a wrong default would change the count.
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let state = root.join("state");
    std::fs::create_dir_all(&state)?;
    let executable = provenant_os::git::git_executable().ok_or("git unavailable")?;
    let executor = SystemExecutor::new(1)?;
    let env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
        .chain(provenant_os::fixture::fixture_git_settings())
        .collect();
    let run = |repo: &Path, args: &[&str]| git(&executor, &executable, repo, &env, args);

    run(root, &["init", "-q", "--bare", "remote.git"])?;
    run(
        &root.join("remote.git"),
        &["symbolic-ref", "HEAD", "refs/heads/master"],
    )?;
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed)?;
    run(&seed, &["init", "-q", "-b", "master"])?;
    for args in [
        ["config", "user.name", "Test"],
        ["config", "user.email", "test@example.invalid"],
    ] {
        run(&seed, &args)?;
    }
    let commit =
        |repo: &Path, text: &str, message: &str| -> Result<String, Box<dyn std::error::Error>> {
            std::fs::write(repo.join("file"), text)?;
            run(repo, &["add", "file"])?;
            run(repo, &["commit", "-q", "-m", message])?;
            run(repo, &["rev-parse", "HEAD"])
        };
    let stale = commit(&seed, "one\n", "one")?;
    run(&seed, &["checkout", "-q", "-b", "main"])?;
    commit(&seed, "side one\n", "side one")?;
    commit(&seed, "side two\n", "side two")?;
    run(&seed, &["checkout", "-q", "master"])?;
    let default = commit(&seed, "two\n", "two")?;
    run(&seed, &["push", "-q", "../remote.git", "master", "main"])?;

    run(root, &["clone", "-q", "remote.git", "checkout"])?;
    let checkout = root.join("checkout");
    run(&checkout, &["config", "user.name", "Test"])?;
    run(&checkout, &["config", "user.email", "test@example.invalid"])?;
    assert_eq!(run(&checkout, &["rev-parse", "HEAD"])?, default);

    // Behind the remote default: the warning names the count, the checkout and the commit read.
    run(&checkout, &["reset", "-q", "--hard", &stale])?;
    let warning = confinement::read_only_checkout_warning(&state, &checkout)
        .ok_or("behind checkout did not warn")?;
    assert!(warning.contains("1 commit behind"), "{warning}");
    assert!(warning.contains(&stale), "{warning}");
    assert!(
        warning.contains(&checkout.display().to_string()),
        "{warning}"
    );

    // Level with, or ahead of, the default: no warning.
    run(&checkout, &["reset", "-q", "--hard", &default])?;
    assert!(confinement::read_only_checkout_warning(&state, &checkout).is_none());
    run(&checkout, &["commit", "-q", "--allow-empty", "-m", "three"])?;
    assert!(confinement::read_only_checkout_warning(&state, &checkout).is_none());

    // Without origin/HEAD, the remote's advertised default is used, not origin/main.
    run(&checkout, &["reset", "-q", "--hard", &stale])?;
    run(
        &checkout,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    )?;
    let warning = confinement::read_only_checkout_warning(&state, &checkout)
        .ok_or("advertised default did not warn")?;
    assert!(warning.contains("1 commit behind"), "{warning}");

    // An unreachable remote is advisory: the launch continues without a warning.
    run(
        &checkout,
        &["remote", "set-url", "origin", "../missing.git"],
    )?;
    assert!(confinement::read_only_checkout_warning(&state, &checkout).is_none());
    Ok(())
}

#[test]
fn read_only_plan_runs_host_git_with_a_private_home_outside_the_child_store()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{NetworkAccess, PlanPermissions};
    use std::os::unix::fs::PermissionsExt;
    struct Identity;
    impl provenant_core::PathResolver for Identity {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            let mut tail = PathBuf::new();
            let mut head = path;
            loop {
                if let Ok(resolved) = std::fs::canonicalize(head) {
                    return Ok(resolved.join(tail));
                }
                let (Some(parent), Some(name)) = (head.parent(), head.file_name()) else {
                    return Err(std::io::ErrorKind::NotFound.into());
                };
                tail = Path::new(name).join(tail);
                head = parent;
            }
        }
    }

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let host_root = base.join("host-state");
    // The lane controls its child store, including the old git-home path.
    let child_store = host_root.join("children/parent-attempt");
    let planted = child_store.join("tmp/git-home");
    std::fs::create_dir_all(&planted)?;
    std::fs::set_permissions(&host_root, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o777))?;
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(base.join("home"))?;
    let real = provenant_os::git::git_executable().ok_or("git unavailable")?;
    // The smallest repository layout Git accepts, without spawning Git.
    for dir in [".git/objects", ".git/refs/heads"] {
        std::fs::create_dir_all(repo.join(dir))?;
    }
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n")?;
    // A git that records the HOME and TMPDIR it was given, then runs the real one.
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin)?;
    let record = base.join("seen");
    let git = bin.join("git");
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\n[ \"$HOME\" = \"$TMPDIR\" ] || exit 97\nls -ld \"$HOME\" >> {r}\nexec {g} \"$@\"\n",
            r = record.display(),
            g = real.display()
        ),
    )?;
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))?;

    let roots = provenant_core::RootInputs {
        state_dir: Some(host_root.clone()),
        ..provenant_core::RootInputs::new(base.join("home"), repo.clone())
    }
    .resolve(&Identity)?;
    let grant = |name: &str| PathGrant {
        path: base.join(name),
        scope: PathScope::Subtree,
    };
    let request = PlanRequest {
        scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
        permissions: PlanPermissions {
            policy: None,
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("provider-state")),
            read_roots: vec![grant("repo")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    };
    // The unbound project is refused, but only after host git has run.
    let result = read_only_plan(
        &roots,
        ProjectId::from_bytes([1; 16]),
        &repo,
        &repo,
        &git,
        request,
        false,
        false,
    );
    assert!(
        matches!(result, Err(ReadOnlyPlanError::MissingBinding)),
        "{:?}",
        result.as_ref().err()
    );

    let seen = std::fs::read_to_string(&record)?;
    assert!(!seen.is_empty(), "host git never ran");
    for line in seen.lines() {
        assert!(line.starts_with("drwx------"), "{line}");
        assert!(
            line.contains(&format!("{}/probe-", host_root.display())),
            "{line}"
        );
        assert!(!line.contains("children"), "{line}");
    }
    // The home is gone once the plan is done, and the child store is unchanged.
    let leftovers: Vec<_> = std::fs::read_dir(&host_root)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("probe-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(std::fs::read_dir(&planted)?.count(), 0);
    Ok(())
}

#[test]
fn credential_authority_never_follows_links_below_the_host_root()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, RunId};

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let host_root = base.join("host-state");
    let relative = Path::new("children/parent/runs/run/attempts/attempt");
    let attempt = host_root.join(relative);
    let config = base.join("home/.claude-pro");
    std::fs::create_dir_all(attempt.join("provider-state"))?;
    std::fs::create_dir_all(&config)?;
    std::fs::write(config.join(".credentials.json"), b"{}")?;
    let mut paths = run::LaunchPaths {
        attempt_id: AttemptId::from_bytes([1; 16]),
        operation_id: OperationId::parse("fixture")?,
        task_index: 0,
        lane_depth: 2,
        run_id: RunId::from_bytes([2; 16]),
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    };
    let login = provenant_providers::claude::Login {
        executable: Path::new("/fixture/claude"),
        home: &base.join("home"),
        user: std::ffi::OsStr::new("fixture"),
        config_dir: &config,
    };
    credential_authority(&host_root, &paths, &login)?;
    let record = attempt.join("credential-authority.json");
    let original = std::fs::read(&record)?;
    // Test intermediate child-store ancestors as well as the final attempt.
    for ancestor in [
        Path::new("children"),
        Path::new("children/parent"),
        relative,
    ] {
        let source = host_root.join(ancestor);
        let redirected = base.join("redirected");
        std::fs::rename(&source, &redirected)?;
        std::os::unix::fs::symlink(&redirected, &source)?;
        assert!(credential_authority(&host_root, &paths, &login).is_err());
        assert_eq!(std::fs::read(&record)?, original);
        std::fs::remove_file(&source)?;
        std::fs::rename(&redirected, &source)?;
    }
    paths.provider_state = base.join("outside/provider-state");
    assert!(credential_authority(&host_root, &paths, &login).is_err());
    paths.provider_state = host_root.join("children/../outside/provider-state");
    assert!(credential_authority(&host_root, &paths, &login).is_err());
    Ok(())
}

#[test]
fn missing_legacy_sessions_fail_and_retained_codex_remains_resumable() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_core::{AttemptId, Digest, RunId, TaskId};
    use provenant_records::{
        FileDigest, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    for adapter in ["cursor", "opencode", "claude", "codex"] {
        let route = ResolvedRoute {
            extra: Default::default(),
            adapter: adapter.into(),
            model: "fixture-model".into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some("local".into()),
            host: "fixture".into(),
            alternatives: RouteAlternatives::default(),
            exclusions: Vec::new(),
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                extra: Default::default(),
                availability_ref: "fixture".into(),
                expires_unix_seconds: 100,
            },
            confidentiality: RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        };
        let mut prior: run::AttemptRecord = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]), "run_id": RunId::from_bytes([2; 16]),
            "project_id": ProjectId::from_bytes([3; 16]), "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "fixture", "attempt_number": 1, "revision": 1, "generation": 0,
            "requested_route": {"kind": "model", "provider": adapter, "model": "fixture-model"},
            "applied_route": {"provider": adapter, "model": format!("{adapter}/fixture-model"), "account_key": "local"},
            "authority_digest": Digest::from_bytes([0; 32]), "session": "fixture-session",
            "queued_at": "2026-10-25T00:00:00.000000Z", "state": {"state": "queued"}, "usage": {},
            "prompt": {"path": {"encoding": "utf8", "path": "prompt.txt"}, "digest": FileDigest::from_bytes([0; 32]), "bytes": 0, "complete": true, "redacted": false}
        }))?;
        let state = Path::new("/fixture/missing-state");
        let result = continuation_for(&prior, &route, "fixture", state, state, state);
        assert!(result.is_err(), "retained state is required for {adapter}");
        if adapter == "codex" {
            let session = "01234567-89ab-cdef-0123-456789abcdef";
            prior.session = Some(session.into());
            let rollout =
                format!("sessions/2026/10/27/rollout-2026-10-27T00-00-00-{session}.jsonl");
            let transcript = format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_meta", "payload":{"id":session}}),
                serde_json::json!({"type":"turn_context", "payload":{"model":"fixture-model"}}),
            );
            use provenant_core::{ActorId, CancelToken, MonoTime, RootInputs, TestClock, WallTime};
            use std::os::unix::fs::DirBuilderExt;
            let temp = tempfile::tempdir()?;
            let base = temp.path().canonicalize()?;
            let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
            inputs.state_dir = Some(base.join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let context = run::ExecutionContext {
                state_root: roots.state,
                project_id: prior.project_id,
                actor_id: ActorId::from_bytes([5; 16]),
                read_prompts: true,
            };
            let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
            let cancel = CancelToken::new();
            let call = run::Request {
                clock: &clock,
                cancel: &cancel,
            };
            let store = run::RunStore::open(&context, &call)?;
            let request = run::PreparedRun {
                run_id: prior.run_id,
                operation_id: prior.operation_id.clone(),
                created_at: WallTime::from_micros(100)?,
                attempts: vec![run::NewAttempt {
                    attempt_id: prior.attempt_id,
                    task_id: prior.task_id,
                    prompt: String::new(),
                }],
                request: serde_json::from_value(serde_json::json!({
                    "schema_version": 1, "concurrency": 1, "parent": null,
                    "tasks": [{ "task_id": prior.task_id, "prompt_digest": provenant_records::CanonicalV1::file_digest(b""),
                        "route": prior.requested_route, "authority_digest": prior.authority_digest,
                        "provider_timeout_seconds": 20, "delivery_id": null }]
                }))?,
            };
            store.create(&request, &call)?;
            let candidate = context.state_root.as_path().join(format!(
                "runs/{}/attempts/{}/provider-state/candidate-0",
                prior.run_id, prior.attempt_id
            ));
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(candidate.join("sessions/2026/10/27"))?;
            std::fs::write(candidate.join("provider.txt"), "codex")?;
            std::fs::write(candidate.join(&rollout), &transcript)?;
            // Codex's thread index can point to the source attempt even
            // after its rollout has been copied into another CODEX_HOME.
            let stale_path = candidate.join(&rollout);
            for index in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm"] {
                std::fs::write(
                    candidate.join(index),
                    stale_path.as_os_str().as_encoded_bytes(),
                )?;
            }
            for cache in ["cache", "plugins", "skills"] {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(candidate.join(cache))?;
                std::fs::write(candidate.join(cache).join("payload"), "cache")?;
            }
            std::fs::write(candidate.join("models_cache.json"), "cache")?;
            store.withdraw(
                prior.attempt_id,
                run::AttemptState::Queued {
                    extra: Default::default(),
                },
                run::Outcome::Cancelled,
                "fixture",
                WallTime::from_micros(200)?,
                &call,
            )?;
            for cache in ["cache", "plugins", "skills", "models_cache.json"] {
                assert!(!candidate.join(cache).exists());
            }
            use std::os::unix::fs::MetadataExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(candidate.join("cache"))?;
            std::fs::write(candidate.join("cache/payload"), "pending deletion")?;
            let metadata = std::fs::metadata(candidate.join("cache"))?;
            let tombstone = format!(
                "{}{:016x}{:016x}-cache",
                run::TOMBSTONE,
                metadata.dev(),
                metadata.ino()
            );
            std::fs::rename(candidate.join("cache"), candidate.join(&tombstone))?;
            let nested_tombstone = format!("sessions/{}interrupted", run::TOMBSTONE);
            std::fs::write(candidate.join(&nested_tombstone), "pending deletion")?;

            let copied = base.join("resumed-state");
            let continuation = continuation_for(
                &prior,
                &route,
                "fixture",
                context.state_root.as_path(),
                context.state_root.as_path(),
                &copied,
            )?;
            assert_eq!(continuation.session(), session);
            assert!(!copied.join(&tombstone).exists());
            assert!(!copied.join(nested_tombstone).exists());
            assert!(candidate.join(&tombstone).exists());
            assert_eq!(std::fs::read_to_string(copied.join(&rollout))?, transcript);
            assert_eq!(
                provenant_providers::codex_rollout::observe(&copied, session)?.model,
                "fixture-model"
            );
            for index in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm"] {
                assert!(
                    !copied.join(index).exists(),
                    "resumed Codex must discover local rollouts without the source attempt's index: {index}"
                );
                assert_eq!(
                    std::fs::read(candidate.join(index))?,
                    stale_path.as_os_str().as_encoded_bytes()
                );
            }

            // Exercise the hand-off to the real adapter without launching
            // a nested Codex process under the harness's confinement.
            let home = base.join("home");
            let cwd = base.join("checkout");
            let scratch = base.join("scratch");
            let assets = base.join("assets");
            let plan = ConfinedPlan::new(confinement_request(
                prior.project_id,
                ConfinementRoots {
                    host_roots: None,
                    cwd: &cwd,
                    home: &home,
                    state: Path::new("/fixture-host-state"),
                    credential_roots: &[],
                    github_roots: &[],
                    protected_worktrees: &[],
                    scratch: &scratch,
                    private_state: &copied,
                    assets: &assets,
                    prompt_file: None,
                },
                None,
                &Default::default(),
                &Default::default(),
                "codex",
                true,
                InventoryContext::Sandboxed,
            )?)?;
            use provenant_providers::{Adapter, ProviderEvent, codex::Codex};
            let prompt = Prompt::new("continue".into());
            let account = provenant_providers::identity::RegisteredAccount::new(
                "codex",
                "local",
                home.join(".codex"),
            )?;
            let launch = Codex.prepare(PrepareInput {
                images: &[],
                command: None,
                route: &route,
                confined: &plan,
                prompt: &prompt,
                continuation: Some(&continuation),
                grant_digest: "fixture",
                executable: Path::new("/opt/homebrew/Cellar/codex/fixture/bin/codex"),
                home: &home,
                user: "fixture".as_ref(),
                host_var: &|_| None,
                skill_stage_max_bytes: 16 * 1024 * 1024,
                account: Some(&account),
                token: None,
                cwd: &cwd,
                git_context: false,
                nonce: [9; 16],
                provenant: LaneRuntime {
                    executable: Path::new("/fixture/provenant/bin/provenant"),
                    git: Path::new("/usr/bin/git"),
                    state: context.state_root.as_path(),
                    home: &home,
                    depth: 1,
                    attempt: AttemptId::from_bytes([6; 16]),
                    assets: &assets,
                    fabric_mcp: true,
                },
            })?;
            assert_eq!(launch.argv[2], "resume");
            assert_eq!(launch.argv[3], session);
            assert!(
                launch
                    .env
                    .iter()
                    .any(|(key, value)| { key == "CODEX_HOME" && value == copied.as_os_str() })
            );
            assert!(
                plan.permissions()
                    .private_state
                    .as_ref()
                    .is_some_and(|grant| {
                        grant.path == copied && grant.scope == PathScope::Subtree
                    })
            );
            assert!(
                plan.permissions()
                    .read_roots
                    .iter()
                    .all(|grant| { !candidate.starts_with(&grant.path) })
            );
            assert!(
                launch
                    .profile
                    .spec
                    .runtime_reads
                    .iter()
                    .all(|grant| { !candidate.starts_with(&grant.path) })
            );
            let recorded = include_str!(
                "../../../../../tests/fixtures/providers/codex/live-worktree_write-resume.jsonl"
            )
            .replace("FIXTURE_SESSION_IDENTIFIER", session);
            let mut stream = Codex.stream(Some(&continuation))?;
            let events = Codex.observe(&mut stream, recorded.as_bytes())?;
            assert!(events.iter().any(|event| {
                matches!(event, ProviderEvent::Session(started) if started == session)
            }));
            assert_eq!(
                Codex
                    .finish(
                        stream,
                        provenant_providers::ExitObservation {
                            code: Some(0),
                            timed_out: false,
                        }
                    )?
                    .status(),
                provenant_providers::OutcomeStatus::Success
            );
            // A nested execution has its own run files, but only the host
            // config controls copy/search budgets. A child cannot override it.
            let host = context.state_root.as_path();
            let nested = host.join("children/fixture-parent");
            std::fs::create_dir_all(&nested)?;
            std::fs::rename(host.join("runs"), nested.join("runs"))?;
            std::fs::write(
                host.join("config.toml"),
                "schema_version=1\n[domains.run]\ncontinuation_copy_entries=1\n",
            )?;
            std::fs::write(
                nested.join("config.toml"),
                "schema_version=1\n[domains.run]\ncontinuation_copy_entries=100000\n",
            )?;
            let nested_copy = base.join("nested-copy");
            let error = continuation_for(&prior, &route, "fixture", &nested, host, &nested_copy)
                .unwrap_err();
            assert!(matches!(
                error.resource_fact(),
                Some(
                    provenant_records::diagnostic::ResourceFact::ContinuationCopy {
                        limit: 1,
                        origin: provenant_records::diagnostic::SettingSource::Configuration,
                        setting: provenant_records::diagnostic::ContinuationCopyLimit::Entries,
                        ..
                    }
                )
            ));
            std::fs::write(
                host.join("config.toml"),
                "schema_version=1\n[domains.run]\ncontinuation_copy_entries=100000\n",
            )?;
            std::fs::write(
                nested.join("config.toml"),
                "schema_version=1\n[domains.run]\ncontinuation_copy_entries=1\n",
            )?;
            continuation_for(&prior, &route, "fixture", &nested, host, &nested_copy)?;
            assert_eq!(
                std::fs::read_to_string(nested_copy.join(&rollout))?,
                transcript
            );
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("retained provider continuation is missing or expired"));
            assert!(error.contains("attempt-private state"));
            assert!(error.contains("start a new lane"));
            if adapter == "opencode" {
                let temp = tempfile::tempdir()?;
                let root = temp.path().canonicalize()?;
                let state = root.join(format!("runs/{}/attempts/{}/provider-state", prior.run_id, prior.attempt_id));
                let data = state.join("candidate-0/data/opencode");
                std::fs::create_dir_all(&data)?;
                let db = rusqlite::Connection::open(data.join("opencode.db"))?;
                db.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY); CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT)")?;
                db.execute("INSERT INTO session VALUES (?1)", ["fixture-session"])?;
                db.execute("INSERT INTO session_message VALUES (?1, 'assistant', ?2)", ("fixture-session", r#"{"model":{"providerID":"fixture","id":"model"}}"#))?;
                drop(db);
                let copied = root.join("copy");
                let resumed = continuation_for(&prior, &route, "fixture", &root, &root, &copied)?;
                assert_eq!(resumed.session(), "fixture-session");
                assert_eq!(provenant_providers::opencode::observe_model(&copied.join("data/opencode"), "fixture-session")?, "opencode/fixture/model");
                let db = rusqlite::Connection::open(data.join("opencode.db"))?;
                db.execute("INSERT INTO session_message VALUES (?1, 'assistant', ?2)", ("fixture-session", r#"{"model":{"providerID":"other","id":"model"}}"#))?;
                for case in ["ambiguous", "absent"] {
                    if case == "absent" { db.execute("DELETE FROM session_message", [])?; }
                    assert!(provenant_providers::opencode::observe_model(&data, "fixture-session").is_err());
                    let target = root.join(format!("copy-{case}"));
                    assert_eq!(continuation_for(&prior, &route, "fixture", &root, &root, &target)?.session(), "fixture-session");
                }
                let other = state.join("candidate-1/data/opencode");
                std::fs::create_dir_all(&other)?;
                std::fs::copy(data.join("opencode.db"), other.join("opencode.db"))?;
                assert!(super::observation::find_opencode_session(&state, "fixture-session").is_err());
                std::fs::remove_dir_all(state.join("candidate-1"))?;
                db.execute("DELETE FROM session", [])?;
                assert!(super::observation::find_opencode_session(&state, "fixture-session").is_err());
            }
            prior.session = None;
            assert!(continuation_for(&prior, &route, "fixture", state, state, state).is_err());
        }
    }
    Ok(())
}

#[test]
fn retained_state_copy_keeps_only_codex_session_trees() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(source.join("sessions"))?;
    for name in ["cache", "plugins", "skills"] {
        std::fs::create_dir(source.join(name))?;
        std::fs::write(source.join(name).join("installed"), "partial cache")?;
        std::fs::create_dir(source.join("sessions").join(name))?;
        std::fs::write(source.join("sessions").join(name).join("keep"), "state")?;
    }
    std::fs::write(source.join("models_cache.json"), "cache")?;
    std::fs::write(source.join("state_5.sqlite"), "source rollout paths")?;
    std::fs::create_dir(source.join("archived_sessions"))?;
    std::fs::write(
        source.join("archived_sessions/rollout.jsonl"),
        "archived transcript",
    )?;
    std::fs::write(source.join("sessions/models_cache.json"), "state")?;
    std::fs::write(source.join("sessions/rollout.jsonl"), "transcript")?;
    for (name, sessions_only) in [("codex", true), ("claude", false)] {
        let copied = base.join(name);
        copy_state(&source, &copied, sessions_only)?;
        for name in [
            "cache",
            "plugins",
            "skills",
            "models_cache.json",
            "state_5.sqlite",
        ] {
            assert_eq!(copied.join(name).exists(), !sessions_only);
            assert!(source.join(name).exists());
        }
        for name in ["cache", "plugins", "skills", "models_cache.json"] {
            assert!(copied.join("sessions").join(name).exists());
        }
        assert_eq!(
            std::fs::read_to_string(copied.join("archived_sessions/rollout.jsonl"))?,
            "archived transcript"
        );
        assert_eq!(
            std::fs::read_to_string(copied.join("sessions/rollout.jsonl"))?,
            "transcript"
        );
    }
    Ok(())
}

#[test]
fn retained_state_never_follows_a_symlink() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let state = base.join("provider-state");
    std::fs::create_dir_all(base.join("outside/fixture-session"))?;
    std::fs::create_dir_all(&state)?;
    std::os::unix::fs::symlink(base.join("outside"), state.join("candidate-0"))?;
    assert_eq!(find_session_state(&state, "fixture-session"), Ok(None));
    std::fs::create_dir_all(state.join("candidate-1/fixture-session"))?;
    assert_eq!(
        find_session_state(&state, "fixture-session"),
        Ok(Some(state.join("candidate-1")))
    );
    std::os::unix::fs::symlink(&state, base.join("alias"))?;
    assert_eq!(
        find_session_state(&base.join("alias"), "fixture-session"),
        Ok(None)
    );
    assert!(copy_state(&state.join("candidate-0"), &base.join("copy"), false).is_err());
    std::fs::write(state.join("candidate-1/fixture-session/own"), "lane")?;
    copy_state(&state.join("candidate-1"), &base.join("copy"), false)?;
    assert!(base.join("copy/fixture-session/own").is_file());
    std::fs::write(base.join("outside/host-secret"), "host")?;
    std::fs::hard_link(
        base.join("outside/host-secret"),
        state.join("candidate-1/fixture-session/linked"),
    )?;
    let copied = base.join("linked-copy");
    assert_eq!(
        copy_state(&state.join("candidate-1"), &copied, false),
        Err("retained provider state holds a hard-linked file".into())
    );
    assert!(!copied.join("fixture-session/linked").exists());
    Ok(())
}

// Test-only: std has no mkfifo and this crate forbids unsafe code.
#[allow(clippy::disallowed_methods)]
fn mkfifo(path: &Path) -> std::io::Result<()> {
    let status = std::process::Command::new("mkfifo").arg(path).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("mkfifo failed"))
    }
}

/// Copy `source` while `swap` changes the entry `victim` after its check,
/// on a thread so a blocking open fails the test instead of hanging it.
fn copy_with_swap(
    source: &Path,
    copied: &Path,
    swap: impl FnOnce(&Path) -> std::io::Result<()> + Send + 'static,
) -> Result<(), String> {
    let (source, copied) = (source.to_owned(), copied.to_owned());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut swap = Some(swap);
        let entry = source.join("victim");
        BEFORE_OPEN.set(Some(Box::new(move |name| {
            if name == "victim"
                && let Some(swap) = swap.take()
            {
                swap(&entry).expect("swap the checked entry");
            }
        })));
        let _ = sender.send(copy_state(&source, &copied, false));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the copy finishes without blocking")
}

#[test]
fn retained_state_copy_skips_entries_swapped_after_their_check()
-> Result<(), Box<dyn std::error::Error>> {
    type Swap = fn(&Path, &Path) -> std::io::Result<()>;
    let swaps: [(&str, bool, Swap); 6] = [
        ("file to outside link", false, |entry, outside| {
            std::fs::remove_file(entry)?;
            std::os::unix::fs::symlink(outside.join("host-secret"), entry)
        }),
        ("directory to outside link", true, |entry, outside| {
            std::fs::remove_dir_all(entry)?;
            std::os::unix::fs::symlink(outside, entry)
        }),
        ("file to FIFO", false, |entry, _| {
            std::fs::remove_file(entry)?;
            mkfifo(entry)
        }),
        ("file to socket", false, |entry, _| {
            std::fs::remove_file(entry)?;
            std::os::unix::net::UnixListener::bind(entry).map(drop)
        }),
        ("directory to file", true, |entry, _| {
            std::fs::remove_dir_all(entry)?;
            std::fs::write(entry, "replaced")
        }),
        ("file removed", false, |entry, _| {
            std::fs::remove_file(entry)
        }),
    ];
    for (label, directory, swap) in swaps {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let (source, outside) = (base.join("candidate-0"), base.join("outside"));
        std::fs::create_dir_all(&outside)?;
        std::fs::write(outside.join("host-secret"), "host")?;
        std::fs::create_dir_all(&source)?;
        std::fs::write(source.join("keep"), "lane")?;
        if directory {
            std::fs::create_dir(source.join("victim"))?;
            std::fs::write(source.join("victim/inner"), "lane")?;
        } else {
            std::fs::write(source.join("victim"), "lane")?;
        }
        let copied = base.join("copy");
        let swap_outside = outside.clone();
        copy_with_swap(&source, &copied, move |entry| swap(entry, &swap_outside))
            .map_err(|error| format!("{label}: {error}"))?;
        assert_eq!(std::fs::read_to_string(copied.join("keep"))?, "lane");
        let entries = std::fs::read_dir(&copied)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(entries, ["keep"], "{label}: the swapped entry is skipped");
    }
    Ok(())
}

#[test]
fn retained_state_copy_refuses_a_file_swapped_for_a_hard_link()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(&source)?;
    std::fs::write(base.join("host-secret"), "host")?;
    std::fs::write(source.join("victim"), "lane")?;
    let copied = base.join("copy");
    let secret = base.join("host-secret");
    let result = copy_with_swap(&source, &copied, move |entry| {
        std::fs::remove_file(entry)?;
        std::fs::hard_link(&secret, entry)
    });
    assert_eq!(
        result,
        Err("retained provider state holds a hard-linked file".into())
    );
    assert!(std::fs::symlink_metadata(copied.join("victim")).is_err());
    Ok(())
}

#[test]
fn retained_state_copy_skips_nested_symlinks() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(base.join("outside/secrets"))?;
    std::fs::write(base.join("outside/host-secret"), "host")?;
    std::fs::write(base.join("outside/secrets/token"), "host")?;
    std::fs::create_dir_all(source.join("sessions/2026"))?;
    std::fs::write(source.join("config.toml"), "lane")?;
    std::fs::write(source.join("sessions/2026/rollout.jsonl"), "turn")?;
    std::fs::write(source.join("tool"), "#!/bin/sh\n")?;
    std::fs::set_permissions(source.join("tool"), std::fs::Permissions::from_mode(0o755))?;
    std::os::unix::fs::symlink(base.join("outside/host-secret"), source.join("file-link"))?;
    std::os::unix::fs::symlink(base.join("outside/secrets"), source.join("dir-link"))?;
    std::os::unix::fs::symlink(
        base.join("outside/host-secret"),
        source.join("sessions/2026/nested-link"),
    )?;
    let copied = base.join("copy");
    copy_state(&source, &copied, false)?;
    assert_eq!(std::fs::read_to_string(copied.join("config.toml"))?, "lane");
    assert_eq!(
        std::fs::read_to_string(copied.join("sessions/2026/rollout.jsonl"))?,
        "turn"
    );
    let tool = std::fs::metadata(copied.join("tool"))?.permissions().mode();
    assert_eq!(tool & 0o100, 0o100, "owner execute survives the copy");
    for skipped in ["file-link", "dir-link", "sessions/2026/nested-link"] {
        assert!(
            std::fs::symlink_metadata(copied.join(skipped)).is_err(),
            "{skipped} must not be copied"
        );
    }
    let codex_copy = base.join("codex-copy");
    copy_state(&source, &codex_copy, true)?;
    assert_eq!(
        std::fs::read_to_string(codex_copy.join("sessions/2026/rollout.jsonl"))?,
        "turn"
    );
    assert!(std::fs::symlink_metadata(codex_copy.join("sessions/2026/nested-link")).is_err());
    Ok(())
}

#[test]
fn retained_state_copy_refuses_state_past_its_byte_budget() -> Result<(), Box<dyn std::error::Error>>
{
    const LIMIT: u64 = 1 << 20;
    let limits = StateLimits {
        bytes: LIMIT,
        ..RETAINED_STATE
    };
    let assert_byte_refusal = |result: Result<(), String>| {
        let error = result.expect_err("copy byte ceiling must refuse");
        assert!(error.contains("domains.run.continuation_copy_limit_bytes"));
        assert!(error.contains(&LIMIT.to_string()));
        assert!(error.contains("shipped default"));
    };
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    // A sparse file costs the lane no blocks but claims far past the budget.
    let sparse = base.join("sparse");
    std::fs::create_dir(&sparse)?;
    std::fs::File::create(sparse.join("rollout.jsonl"))?.set_len(64 * LIMIT)?;
    let copied = base.join("sparse-copy");
    assert_byte_refusal(copy_state_within(&sparse, &copied, limits, false));
    assert!(
        !copied.join("rollout.jsonl").exists(),
        "the partial copy is removed"
    );
    // The budget spans the tree, not each file.
    let split = base.join("split");
    std::fs::create_dir_all(split.join("sessions"))?;
    let half = vec![b'x'; usize::try_from(LIMIT / 2 + 1)?];
    std::fs::write(split.join("history.jsonl"), &half)?;
    std::fs::write(split.join("sessions/rollout.jsonl"), &half)?;
    assert_byte_refusal(copy_state_within(
        &split,
        &base.join("split-copy"),
        limits,
        false,
    ));
    // State of exactly the budget still copies whole.
    let exact = base.join("exact");
    std::fs::create_dir(&exact)?;
    std::fs::write(exact.join("a"), &half)?;
    std::fs::write(exact.join("b"), vec![b'y'; usize::try_from(LIMIT / 2 - 1)?])?;
    copy_state_within(&exact, &base.join("exact-copy"), limits, false)?;
    assert_eq!(std::fs::read(base.join("exact-copy/a"))?, half);
    assert_eq!(
        std::fs::metadata(base.join("exact-copy/b"))?.len(),
        LIMIT / 2 - 1
    );
    Ok(())
}

#[test]
fn retained_state_copy_refuses_trees_past_its_entry_and_depth_budget()
-> Result<(), Box<dyn std::error::Error>> {
    let limits = StateLimits {
        depth: 2,
        entries: 4,
        ..RETAINED_STATE
    };
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let within = base.join("within");
    std::fs::create_dir_all(within.join("a/b"))?;
    std::fs::write(within.join("a/b/file"), "lane")?;
    std::fs::write(within.join("top"), "lane")?;
    copy_state_within(&within, &base.join("within-copy"), limits, false)?;
    assert_eq!(
        std::fs::read_to_string(base.join("within-copy/a/b/file"))?,
        "lane"
    );

    let deep = base.join("deep");
    std::fs::create_dir_all(deep.join("a/b/c"))?;
    let error = copy_state_within(&deep, &base.join("deep-copy"), limits, false).unwrap_err();
    assert!(error.contains("domains.run.continuation_copy_depth is 2"));

    let wide = base.join("wide");
    std::fs::create_dir(&wide)?;
    for name in ["1", "2", "3", "4", "5"] {
        std::fs::write(wide.join(name), "lane")?;
    }
    let error = copy_state_within(&wide, &base.join("wide-copy"), limits, false).unwrap_err();
    assert!(error.contains("domains.run.continuation_copy_entries is 4"));
    Ok(())
}
