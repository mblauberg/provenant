#[test]
fn writer_plan_stages_scratch_in_private_temporary_directory()
-> Result<(), Box<dyn std::error::Error>> {
    writer_preparation_fixture(false)
}

// Medium: real provider fences, routing and preparation, with synthetic CLIs.
#[test]
fn provider_fence_preparation_reports_causes_and_selects_available_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    writer_preparation_fixture(true)
}

fn writer_preparation_fixture(fence_only: bool) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::{UtilityBudget, UtilityRuntime, worktrees};
    use provenant_core::{AttemptId, CancelToken, Clock, Digest, OperationId, WaitBudget};
    use provenant_os::clock::SystemClock;
    use provenant_os::exec::ExecOptions;
    use provenant_os::exec::Executor;
    use provenant_os::exec::SystemExecutor;
    use provenant_os::exec::Termination;
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };

    // Isolate the host token from other parallel tests while exercising the
    // same process environment that production preparation reads.
    const CHILD: &str = "PROVENANT_TEST_WRITER_TOKEN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Each auth layout runs the real preparation boundary in an isolated
        // process; neither depends on another test's host credential state.
        let (test, layouts): (&str, &[&str]) = if fence_only {
            (
                "provider_fence_preparation_reports_causes_and_selects_available_candidate",
                &["missing"],
            )
        } else {
            (
                "writer_plan_stages_scratch_in_private_temporary_directory",
                &["missing", "symlink", "symlink-xdg"],
            )
        };
        for layout in layouts {
            #[allow(clippy::disallowed_methods)]
            let status = std::process::Command::new(std::env::current_exe()?)
                .arg(test)
                .env(CHILD, layout)
                .env("GH_TOKEN", "fixture-host-token")
                .env_remove("GIT_CONFIG_GLOBAL")
                .env_remove("XDG_CONFIG_HOME")
                .status()?;
            assert!(status.success(), "GitHub config layout {layout}");
        }
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("repo");
    let assets = base.join("release");
    let git_home = base.join("git-home");
    for path in [&home, &state, &project, &assets, &git_home] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("symlink-xdg")) {
        let config = base.join("host-xdg-config");
        std::fs::create_dir(&config)?;
        std::os::unix::fs::symlink(config, home.join(".config"))?;
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
    // Cursor's local account needs only --version. Keep its executable
    // inside the fixture assets, independent of host packages and PATH.
    let executable = assets.join("cursor/bin/agent");
    std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1\" = status ]; then printf '%s' '{}'; else printf 'fixture-cursor 1.0\\n'; fi\n",
            include_str!("../../../../../tests/fixtures/providers/cursor/status.stdout")
        ),
    )?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let attempt = state.join(format!("attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("writer-temporary")?,
        task_index: 0,
        lane_depth: 1,
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        provider_state: attempt.join("provider-state"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
    };
    for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let prompt = "é".repeat(75_000);
    let review_oid = run::resolve_git_base(&git, &project, "HEAD")?;
    let task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        build_heavy: false,
        title: None,
        base: Some(review_oid.clone()),
        head: Some(review_oid.clone()),
        source_head: None,
        base_ref: None,
        profile: provenant_records::run::HarnessProfile {
            knobs: [("lanes.publish".into(), serde_json::json!(false))].into(),
            ..Default::default()
        },
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/gpt-5.3-codex-low".into(),
            effort: None,
        },
        account: None,
        authority_digest: Digest::from_bytes([4; 32]),
        writer: Some(run::WriterRequest {
            extra: Default::default(),
            project: project.clone(),
            worktree: project.join(".worktrees/writer"),
        }),
        deliver: None,
        provider_timeout_seconds: 20,
        delivery_id: None,
        access: Default::default(),
    };
    let registry = Registry {
        roots: Some(&roots),
        home: Some(&home),
        assets: Some(&assets),
        cwd: Some(&project),
        state_root: Some(&state),
        project_id: Some(binding.project_id()),
        host: None,
        cancel: Some(&cancel),
        repo_facts: Default::default(),
    };
    // An exhausted fence wait must never probe the updating CLI,
    // including when the lease map is empty.
    if fence_only {
        let _updating = provenant_os::provider_fence::acquire(
            &state,
            "codex",
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let mut blocked = task.clone();
        blocked.route = run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "codex".into(),
            model: "codex/gpt-6.1-sol".into(),
            effort: None,
        };
        let refusal = prepare_with_wait(
            &registry,
            &blocked,
            "provider fence fixture",
            &paths,
            None,
            |_| panic!("an updating provider must not be probed"),
            None,
            Some(0),
        )
        .err()
        .ok_or("updating provider was admitted")?;
        let PrepareError::Candidates {
            skipped_routes,
            fallback_outcome,
            ..
        } = &refusal
        else {
            return Err(format!("provider fence lost its candidate diagnosis: {refusal}").into());
        };
        assert_eq!(skipped_routes.len(), 1);
        assert_eq!(skipped_routes[0].route, "codex/gpt-6.1-sol");
        assert_eq!(
            skipped_routes[0].cause,
            Some(provenant_records::RouteExclusionReason::ProviderUpdating)
        );
        assert!(
            skipped_routes[0]
                .detail
                .contains("provider update in progress for codex")
        );
        assert_eq!(
            *fallback_outcome,
            Some(provenant_records::run::PreparationFailure::AdapterUnavailable)
        );

        // A pool whose first member is updating must still consider its other
        // eligible providers. The existing route policy controls the pool.
        let saved_config = std::fs::read(state.join("config.toml")).ok();
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[domains.routing]\nsettlement_retries=1\n[[domains.routing.pools.fence-fixture]]\nmodel='codex/gpt-6.1-sol'\nweight=1\nefforts=[]\n[[domains.routing.pools.fence-fixture]]\nmodel='cursor/gpt-5.3-codex-low'\nweight=1\nefforts=[]\n",
        )?;
        let mut pool = task.clone();
        pool.route = run::RequestedRoute::Named {
            extra: Default::default(),
            name: "fence-fixture".into(),
            effort: None,
        };
        let pool_paths = run::LaunchPaths {
            scratch: attempt.join("fence-scratch"),
            provider_state: attempt.join("fence-provider-state"),
            temporary: attempt.join("fence-tmp"),
            attempt_id: paths.attempt_id,
            operation_id: paths.operation_id.clone(),
            task_index: paths.task_index,
            lane_depth: paths.lane_depth,
            deadline_unix_ms: paths.deadline_unix_ms,
            temporary_alias: None,
        };
        for path in [
            &pool_paths.scratch,
            &pool_paths.provider_state,
            &pool_paths.temporary,
        ] {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let prepared = prepare_with_wait(
            &registry,
            &pool,
            "provider fence fixture",
            &pool_paths,
            None,
            |adapter| {
                assert_eq!(adapter, "cursor", "updating provider was probed");
                Ok(executable.clone())
            },
            None,
            Some(0),
        )?;
        assert_eq!(prepared.route.provider.as_deref(), Some("cursor"));
        assert_eq!(
            prepared.route.model.as_deref(),
            Some("cursor/gpt-5.3-codex-low")
        );
        let selection = prepared
            .route
            .pool_selection
            .as_ref()
            .ok_or("pool receipt")?;
        let updating = selection
            .candidates
            .iter()
            .find(|candidate| candidate.model == "codex/gpt-6.1-sol")
            .ok_or("updating pool member disappeared")?;
        assert!(matches!(
            updating.readiness,
            Some(provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::ProviderUpdating,
                ..
            })
        ));
        assert!(
            updating
                .exclusion
                .as_deref()
                .is_some_and(|detail| detail.contains("update"))
        );
        drop(prepared);
        match saved_config {
            Some(config) => std::fs::write(state.join("config.toml"), config)?,
            None => std::fs::remove_file(state.join("config.toml"))?,
        }

        // A protected-paths project: confidential directories denied to every
        // lane, and a confidential task. Excluding a pool's first candidate at
        // preparation, for any cause, tries the next permitted member, even
        // with no settlement retry budget, and records every exclusion.
        let confidential = project.join("confidential");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&confidential)?;
        let protected_policy = format!(
            "[domains.run.host_policy]\ndeny_read = ['{0}']\ndeny_write = ['{0}']\n",
            confidential.display()
        );
        std::fs::write(
            state.join("config.toml"),
            format!(
                "schema_version=1\n[domains.routing]\nsettlement_retries=0\n[[domains.routing.pools.protected-fixture]]\nmodel='cursor/gpt-5.3-codex-low'\nweight=1\nefforts=[]\n[[domains.routing.pools.protected-fixture]]\nmodel='cursor/grok-4.7'\nweight=1\nefforts=[]\n[[domains.routing.pools.protected-fixture]]\nmodel='codex/gpt-6.1-terra'\nweight=1\nefforts=['low']\n{protected_policy}"
            ),
        )?;
        let mut protected = task.clone();
        protected
            .profile
            .extra
            .insert("confidential".into(), serde_json::json!(true));
        protected.route = run::RequestedRoute::Named {
            extra: Default::default(),
            name: "protected-fixture".into(),
            effort: None,
        };
        let fresh_paths = |name: &str| -> Result<run::LaunchPaths, std::io::Error> {
            let paths = run::LaunchPaths {
                scratch: attempt.join(format!("{name}-scratch")),
                provider_state: attempt.join(format!("{name}-provider-state")),
                temporary: attempt.join(format!("{name}-tmp")),
                temporary_alias: None,
                attempt_id: paths.attempt_id,
                operation_id: paths.operation_id.clone(),
                task_index: paths.task_index,
                lane_depth: paths.lane_depth,
                deadline_unix_ms: paths.deadline_unix_ms,
            };
            for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
                std::fs::DirBuilder::new().mode(0o700).create(path)?;
            }
            Ok(paths)
        };
        // Only Cursor is installed; Codex's default account cannot be observed.
        let cursor_only = |adapter: &str| {
            if adapter == "cursor" {
                Ok(executable.clone())
            } else {
                Err(format!("{adapter} is not installed"))
            }
        };
        let walk_paths = fresh_paths("protected-walk")?;
        // The first candidate's staging directory cannot be created.
        std::fs::write(walk_paths.temporary.join("candidate-0"), b"occupied")?;
        let walked = prepare_with_wait(
            &registry,
            &protected,
            "protected pool fixture",
            &walk_paths,
            None,
            cursor_only,
            None,
            Some(0),
        )?;
        assert_eq!(
            walked.skipped_routes.len(),
            1,
            "{:?}",
            walked.skipped_routes
        );
        let first = &walked.skipped_routes[0];
        assert_eq!(first.kind, SkipKind::Setup);
        assert!(first.route.starts_with("cursor/"));
        let chosen = walked.route.model.as_deref().ok_or("walked route model")?;
        assert!(chosen.starts_with("cursor/"));
        assert_ne!(chosen, first.route, "the excluded candidate was retried");
        assert_eq!(
            walked.route.provider.as_deref(),
            Some("cursor"),
            "the pool must not stop after one excluded member"
        );
        let selection = walked.route.pool_selection.as_ref().ok_or("pool receipt")?;
        let terra = selection
            .candidates
            .iter()
            .find(|candidate| candidate.model == "codex/gpt-6.1-terra")
            .ok_or("terra left the pool receipt")?;
        // Codex has an access context; its missing account is not a
        // disclosure-policy refusal.
        assert!(
            !matches!(
                terra.readiness,
                Some(provenant_records::RouteReadiness::Unavailable {
                    reason: provenant_records::RouteExclusionReason::DisclosureDenied,
                    ..
                })
            ),
            "{terra:?}"
        );
        drop(walked);

        // When every member is excluded, each is listed once with a typed cause.
        let exhausted_paths = fresh_paths("protected-exhausted")?;
        for index in 0..2 {
            std::fs::write(
                exhausted_paths.temporary.join(format!("candidate-{index}")),
                b"occupied",
            )?;
        }
        let refusal = prepare_with_wait(
            &registry,
            &protected,
            "protected pool fixture",
            &exhausted_paths,
            None,
            cursor_only,
            None,
            Some(0),
        )
        .err()
        .ok_or("a pool with no preparable member launched")?;
        let PrepareError::Candidates { skipped_routes, .. } = &refusal else {
            return Err(format!("pool exclusions lost their diagnosis: {refusal}").into());
        };
        let mut routes: Vec<_> = skipped_routes
            .iter()
            .map(|skipped| skipped.route.as_str())
            .collect();
        routes.sort_unstable();
        assert_eq!(
            routes,
            [
                "codex/gpt-6.1-terra",
                "cursor/gpt-5.3-codex-low",
                "cursor/grok-4.7"
            ],
            "{skipped_routes:?}"
        );
        for skipped in skipped_routes {
            assert!(skipped.cause.is_some(), "{skipped:?}");
            if skipped.route.starts_with("cursor/") {
                assert_eq!(skipped.kind, SkipKind::Setup, "{skipped:?}");
                assert_eq!(
                    skipped.cause,
                    Some(provenant_records::RouteExclusionReason::SetupFailed)
                );
            } else {
                assert!(skipped.cause.is_some(), "{skipped:?}");
            }
            assert_ne!(
                skipped.cause,
                Some(provenant_records::RouteExclusionReason::DisclosureDenied),
                "{skipped:?}"
            );
        }

        // A writer on the shipped senior route resolves to a permitted member.
        std::fs::write(
            state.join("config.toml"),
            format!("schema_version=1\n{protected_policy}"),
        )?;
        let mut senior = protected.clone();
        senior.route = run::RequestedRoute::Named {
            extra: Default::default(),
            name: "senior".into(),
            effort: None,
        };
        let senior_paths = fresh_paths("protected-senior")?;
        let resolved = prepare_with_wait(
            &registry,
            &senior,
            "protected senior fixture",
            &senior_paths,
            None,
            cursor_only,
            None,
            Some(0),
        )?;
        assert_eq!(resolved.route.model.as_deref(), Some("cursor/grok-4.7"));
        assert!(resolved.confinement.is_some());
        drop(resolved);
        std::fs::remove_file(state.join("config.toml"))?;

        // A separate access omission retains the confinement guard that
        // refused the plan, rather than being labelled a disclosure denial.
        std::fs::write(
            state.join("config.toml"),
            format!(
                "schema_version = 1\n[domains.run]\nwriter_writes = ['{}']\n",
                paths.scratch.display(),
            ),
        )?;
        let error = prepare_with_wait(
            &registry,
            &task,
            "confinement refusal fixture",
            &paths,
            None,
            |_| Ok(executable.clone()),
            None,
            Some(0),
        )
        .err()
        .ok_or("unconfinable worktree accepted")?;
        let PrepareError::Candidates {
            skipped_routes,
            plans,
            ..
        } = &error
        else {
            return Err(
                format!("confinement failure lost its candidate diagnosis: {error}").into(),
            );
        };
        assert!(plans.iter().any(|plan| matches!(
            plan,
            provenant_confinement::PlanError::Permission(
                provenant_confinement::permissions::PermissionError::ProtectedOverlap
            )
        )));
        assert_eq!(skipped_routes.len(), 1);
        assert_eq!(
            skipped_routes[0].cause,
            Some(provenant_records::RouteExclusionReason::ConfinementUnavailable)
        );
        assert_eq!(skipped_routes[0].extra["guard_boundary"], "authority");
        assert_eq!(
            skipped_routes[0].extra["confinement_guard"],
            "protected_path_overlap"
        );
        assert_eq!(skipped_routes[0].extra["plan_error"], "protected_overlap");
        assert_eq!(
            skipped_routes[0].diagnostic().detail.as_deref(),
            Some(provenant_records::diagnostic::PlanFailure::ProtectedOverlap.detail()),
        );
        assert!(skipped_routes[0].detail.contains("protected path"));
        return Ok(());
    }
    // A recorded sign-out followed by a healthy account-scoped status probe
    // must permit preparation without waiting for the quota backoff.
    let now = clock.unix_seconds();
    let stale_scope = provenant_quota::AccountScope {
        adapter: "cursor".into(),
        account_key: "local".into(),
        label: "default".into(),
    };
    let stale_account = provenant_quota::AccountRecord::empty(stale_scope);
    let mut readiness = provenant_quota::ReadinessFile::default();
    readiness.sync_probe_result(
        &stale_account,
        provenant_quota::LOCAL_HOST,
        now - 1,
        300,
        Some(provenant_quota::ReadinessReason::SignedOut),
    )?;
    let readiness_dir = state.join("readiness/local");
    std::fs::create_dir_all(&readiness_dir)?;
    for dir in [state.join("readiness"), readiness_dir.clone()] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let readiness_path = readiness_dir.join("cursor.json");
    std::fs::write(&readiness_path, serde_json::to_vec(&readiness)?)?;
    std::fs::set_permissions(&readiness_path, std::fs::Permissions::from_mode(0o600))?;
    let auth_paths = run::LaunchPaths {
        scratch: attempt.join("auth-scratch"),
        provider_state: attempt.join("auth-state"),
        temporary: attempt.join("auth-tmp"),
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        deadline_unix_ms: paths.deadline_unix_ms,
        temporary_alias: None,
    };
    for path in [
        &auth_paths.scratch,
        &auth_paths.provider_state,
        &auth_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let recovered = prepare_with(
        &registry,
        &task,
        "login recovery fixture",
        &auth_paths,
        None,
        |name| {
            if name == "cursor" {
                Ok(executable.clone())
            } else {
                Err("fixture provider unavailable".into())
            }
        },
        None,
    )?;
    assert_eq!(recovered.route.provider.as_deref(), Some("cursor"));
    drop(recovered);
    std::fs::remove_file(readiness_path)?;
    std::fs::remove_file(attempt.join("availability.json"))?;

    // A clean writer that moved after graph source preparation must be
    // refused before provider probing or any launch artefact is written.
    let mut stale_source = task.clone();
    stale_source.head = Some("a".repeat(40));
    let error = prepare_with(
        &registry,
        &stale_source,
        &prompt,
        &paths,
        None,
        |_| Err("provider lookup must follow source validation".into()),
        None,
    )
    .err()
    .ok_or("stale writer source was accepted")?;
    assert!(
        error
            .to_string()
            .contains("differs from pinned source commit"),
        "{error}"
    );
    assert!(error.to_string().contains(&"a".repeat(40)), "{error}");
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = run::RunStore::open(
        &run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: binding.project_id(),
            actor_id: provenant_core::ActorId::from_bytes([5; 16]),
            read_prompts: true,
        },
        &call,
    )?;
    store.create(
        &run::PreparedRun {
            attempt_id,
            operation_id: paths.operation_id.clone(),
            request: run::CanonicalRequest {
                extra: Default::default(),
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                task: run::TaskRequest {
                    context: Some(provenant_records::run::WorkContext {
                        graph_id: Some(format!("lane-{attempt_id}")),
                        node_id: Some("node".into()),
                        attempt: Some(1),
                        ..Default::default()
                    }),
                    ..task.clone()
                },
                parent: None,
                lane_depth: Some(1),
                parent_attempt: None,
            },
            prompt: prompt.clone(),
            created_at: provenant_core::WallTime::from_micros(100)?,
        },
        &call,
    )?;
    // Exercise the actual adapter, sandbox compiler and fixture executable in both lane modes.
    #[cfg(target_os = "macos")]
    {
        use provenant_confinement::permissions::{
            AccessMode, AnchoredDirectory, Anchors, BackendContext, prepare_writer_directories,
            required_anchors, system_backend,
        };
        let skill_root = home.join(".gemini/skills");
        std::fs::create_dir_all(&skill_root)?;
        // Skill discovery and Rust discovery omit the same canonical root.
        // Repeated read/exec grants for that root collapse within Toolchain.
        std::os::unix::fs::symlink(&skill_root, skill_root.join("bin"))?;
        std::os::unix::fs::symlink(&skill_root, home.join(".cargo"))?;
        std::fs::create_dir_all(home.join(".rustup/toolchains"))?;
        let login = home.join("Library/Keychains/login.keychain-db");
        std::fs::create_dir_all(login.parent().unwrap())?;
        std::fs::write(&login, b"fixture")?;
        let fake_agy = assets.join("agy");
        std::fs::write(
            &fake_agy,
            br##"#!/bin/sh
if [ "${IDENTITY_PROBE:-}" = writer ]; then
  [ "$PROVENANT_PRIMARY_CHECKOUT" = "$EXPECTED_PRIMARY" ] || exit 10
  [ "$PROVENANT_WORKTREE_GITDIR" = "$EXPECTED_ADMIN" ] || exit 11
  [ "$(git rev-parse --git-common-dir)" = "$EXPECTED_PRIVATE" ] || exit 12
  ! /usr/bin/touch "$PROVENANT_WORKTREE_GITDIR/identity-probe" || exit 13
elif [ "${IDENTITY_PROBE:-}" = reader ]; then
  [ "$PROVENANT_PRIMARY_CHECKOUT" = "$EXPECTED_PRIMARY" ] || exit 14
  [ -z "${PROVENANT_WORKTREE_GITDIR+x}" ] || exit 15
elif [ "${IDENTITY_PROBE:-}" = linked ]; then
  [ "$PROVENANT_PRIMARY_CHECKOUT" = "$EXPECTED_PRIMARY" ] || exit 16
  [ "$PROVENANT_WORKTREE_GITDIR" = "$EXPECTED_ADMIN" ] || exit 17
fi
printf 'agy 1.0.0\n'
"##,
        )?;
        std::fs::set_permissions(&fake_agy, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(state.join("config.toml"), "schema_version = 1\n")?;
        let backend = system_backend(&BackendContext {
            executor: &executor,
            cancel: &cancel,
            env: &[],
        });
        if let Err(error) = &backend {
            provenant_test_support::skip_sandbox_dependent_test(format!(
                "agy fixture execution requires native sandbox: {error}"
            ))?;
        }
        for (n, write, linked) in [(7u8, true, false), (8u8, false, false), (9u8, false, true)] {
            let mut request = task.clone();
            request.route = run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "agy".into(),
                model: "agy/gemini-3.8-flash".into(),
                effort: None,
            };
            if !write {
                request.writer = None;
            }
            let attempt_id = AttemptId::from_bytes([n; 16]);
            let attempt = state.join(format!("attempts/{attempt_id}"));
            let paths = run::LaunchPaths {
                attempt_id,
                operation_id: OperationId::parse(&format!("agy-fixture-{n}"))?,
                task_index: 0,
                lane_depth: 1,
                deadline_unix_ms: u64::MAX,
                scratch: attempt.join("scratch"),
                provider_state: attempt.join("provider-state"),
                temporary: attempt.join("tmp"),
                temporary_alias: None,
            };
            for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
                std::fs::create_dir_all(path)?;
            }
            let linked_cwd = project.join(".worktrees/writer");
            let selected_registry = Registry {
                cwd: Some(if linked { &linked_cwd } else { &project }),
                repo_facts: registry.repo_facts.clone(),
                ..registry
            };
            let prepared = prepare_with(
                &selected_registry,
                &request,
                "fixture",
                &paths,
                None,
                |_| Ok(fake_agy.clone()),
                None,
            )?;
            let skills = &prepared.route.extra["user_skills"];
            assert_eq!(skills["exposed"], true);
            assert!(
                skills["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning["code"] == "user_skill_skipped"
                        && warning["reason"] == "cycle")
            );
            let warnings = prepared.route.extra["discovery_warnings"]
                .as_array()
                .unwrap();
            if write {
                assert!(warnings.iter().any(|warning| {
                    warning
                        .as_str()
                        .is_some_and(|text| text.contains("~/.gemini/skills"))
                }));
            } else {
                assert!(warnings.is_empty());
            }
            let confinement = prepared.confinement.as_ref().ok_or("missing confinement")?;
            assert!(
                !confinement
                    .profile
                    .spec
                    .runtime_reads
                    .iter()
                    .any(|grant| grant.path == skill_root)
            );
            let writer = confinement
                .writer
                .as_ref()
                .map(|writer| (&writer.worktree, &writer.branch));
            if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
                (writer, &confinement.plan.permissions().mode)
            {
                prepare_writer_directories(worktree, branch, authority, &home)?;
            }
            let directories =
                required_anchors(confinement.plan.permissions(), &confinement.profile, writer)
                    .into_iter()
                    .map(AnchoredDirectory::open_canonical)
                    .collect::<Result<Vec<_>, _>>()?;
            let anchors = Anchors {
                home: &home,
                directories: &directories,
                worktree: writer,
            };
            let compiled = provenant_confinement::permissions::snapshot::compile(
                confinement.plan.permissions(),
                &confinement.profile,
                &anchors,
            )?;
            if backend.is_err() {
                continue;
            }
            let mut environment = prepared.environment.clone();
            environment.push(("EXPECTED_PRIMARY".into(), project.as_os_str().to_owned()));
            if write {
                let worktree = &request.writer.as_ref().ok_or("writer")?.worktree;
                let admin = String::from_utf8(provenant_os::fixture::run_git(
                    worktree,
                    &["rev-parse", "--absolute-git-dir"],
                )?)?;
                let admin = std::fs::canonicalize(admin.trim())?;
                let private = paths.temporary.join("candidate-0/git-common");
                std::fs::create_dir_all(private.join("objects"))?;
                std::fs::create_dir_all(private.join("refs"))?;
                std::fs::write(private.join("HEAD"), b"ref: refs/heads/writer\n")?;
                std::fs::copy(project.join(".git/config"), private.join("config"))?;
                environment.push(("IDENTITY_PROBE".into(), "writer".into()));
                environment.push(("EXPECTED_ADMIN".into(), admin.into_os_string()));
                environment.push(("EXPECTED_PRIVATE".into(), private.into_os_string()));
            } else if linked {
                let admin = String::from_utf8(provenant_os::fixture::run_git(
                    &linked_cwd,
                    &["rev-parse", "--absolute-git-dir"],
                )?)?;
                environment.push(("IDENTITY_PROBE".into(), "linked".into()));
                environment.push((
                    "EXPECTED_ADMIN".into(),
                    std::fs::canonicalize(admin.trim())?.into_os_string(),
                ));
            } else {
                environment.push(("IDENTITY_PROBE".into(), "reader".into()));
            }
            let argv = compiled.command(
                &prepared.argv,
                &attempt_id,
                &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
            )?;
            let mut options = argv.options(&prepared.cwd, Duration::from_secs(10))?;
            options.env = Some(&environment);
            let result = executor.execute(options)?;
            assert_eq!(result.termination, Termination::Exit(0), "{result:?}");
            assert!(String::from_utf8_lossy(&result.stdout.bytes).contains("agy 1.0.0"));
        }
        std::fs::remove_file(home.join(".cargo"))?;
    }
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.accounts.entries.unrelated]\nadapter = 'claude'\nlabel = 'other'\nkey = 'fixture-other'\ndirectory = '{}'\n",
            home.join(".claude-other").display(),
        ),
    )?;
    let full = format!("Durable output instructions\n\n{prompt}");
    let failed_update = provenant_providers::update::check(
        &state,
        "cursor",
        &executable,
        &provenant_config::ProviderSettings {
            update: provenant_config::ProviderUpdate::Command {
                command: vec!["/usr/bin/false".into()],
            },
            ..Default::default()
        },
        &executor,
        100,
    )?
    .ok_or("no failed update record")?;
    assert_eq!(failed_update.outcome, "failed");
    let _updating = provenant_os::provider_fence::acquire(
        &state,
        "codex",
        provenant_os::fs::LockMode::Exclusive,
    )?;
    // A failed diagnostic write must not fail an otherwise valid launch.
    std::fs::create_dir(attempt.join("availability.json"))?;
    // Check configuration precedence through real preparation and adapter grants.
    let skills = home.join(".cursor/skills");
    let skill_target = base.join("installed-skill");
    std::fs::create_dir_all(&skills)?;
    std::fs::create_dir_all(&skill_target)?;
    std::os::unix::fs::symlink(&skill_target, skills.join("linked"))?;
    let project_id = binding.project_id().to_string();
    for (settings, expected, source) in [
        (String::new(), true, "host"),
        (
            "[domains.lanes]\nuser_skills = 'off'\n".into(),
            false,
            "host",
        ),
        (
            format!(
                "[domains.lanes]\nuser_skills = 'discover'\n[domains.lanes.projects.{project_id}]\nuser_skills = 'off'\n"
            ),
            false,
            "project",
        ),
        (
            format!(
                "[domains.lanes]\nuser_skills = 'off'\n[domains.lanes.projects.{project_id}]\nuser_skills = 'discover'\n"
            ),
            true,
            "project",
        ),
        (
            format!(
                "[domains.lanes.projects.{project_id}]\nuser_skills = 'discover'\n[[domains.routing.model_policy.rules]]\nadapter = 'cursor'\n[domains.routing.model_policy.rules.policy.lane]\nuser_skills = 'off'\n"
            ),
            false,
            "route",
        ),
        (
            format!(
                "[domains.lanes.projects.{project_id}]\nuser_skills = 'off'\n[[domains.routing.model_policy.rules]]\nadapter = 'cursor'\n[domains.routing.model_policy.rules.policy.lane]\nuser_skills = 'discover'\n"
            ),
            true,
            "route",
        ),
    ] {
        std::fs::write(
            state.join("config.toml"),
            format!("schema_version = 1\n{settings}"),
        )?;
        let prepared = prepare_with(
            &registry,
            &task,
            "skill fixture",
            &paths,
            None,
            |_| Ok(executable.clone()),
            None,
        )?;
        let confinement = prepared.confinement.as_ref().ok_or("confinement")?;
        let reads = &confinement.profile.spec.runtime_reads;
        let writes = provenant_confinement::write_grants(
            confinement.plan.permissions(),
            &confinement.profile,
        );
        for path in [&skills, &skill_target] {
            assert!(
                !reads.iter().any(|grant| grant.path == *path),
                "{settings}: {path:?}"
            );
            assert!(
                !writes
                    .iter()
                    .any(|grant| path.starts_with(&grant.path) || grant.path.starts_with(path)),
                "write grant for {path:?}: {settings}"
            );
        }
        let receipt = &prepared.route.extra["user_skills"];
        // The linked target is an empty, safe directory: discovery stages
        // an empty tree without granting its original location.
        assert_eq!(receipt["exposed"], expected, "{settings}");
        assert_eq!(receipt["staged_file_count"], 0);
        assert!(receipt["warnings"].as_array().unwrap().is_empty());
        if source == "route" {
            assert!(receipt["source"]["route_policy"].is_array());
        } else {
            let key = if source == "project" {
                format!("domains.lanes.projects.{project_id}.user_skills")
            } else {
                "domains.lanes.user_skills".into()
            };
            assert_eq!(receipt["source"]["key"], key);
        }
    }
    // The primary checkout is another protected worktree for this writer; the
    // host owner's lock directory inside it must not refuse the launch.
    let locks = project.join(".agent-run/locks");
    std::fs::create_dir_all(&locks)?;
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.run]\nargv_prompt_limit_bytes = 60000\nwriter_writes = ['{}']\n[domains.accounts.entries.unrelated]\nadapter = 'claude'\nlabel = 'other'\nkey = 'fixture-other'\ndirectory = '{}'\n",
            locks.display(),
            home.join(".claude-other").display()
        ),
    )?;
    let launch = prepare_with(
        &registry,
        &task,
        &full,
        &paths,
        None,
        |adapter| {
            assert_eq!(adapter, "cursor", "unrelated adapter was probed");
            Ok(executable.clone())
        },
        None,
    )?;
    {
        let confinement = launch.confinement.as_ref().ok_or("writer confinement")?;
        let permissions = confinement.plan.permissions();
        let lock_grant = PathGrant {
            path: locks.clone(),
            scope: PathScope::Subtree,
        };
        assert!(permissions.write_roots.contains(&lock_grant));
        let mandatory = permissions.policy.as_ref().ok_or("policy")?.mandatory();
        assert!(
            mandatory
                .protected()
                .iter()
                .any(|root| root.path == project)
        );
        assert!(mandatory.host_writes().contains(&lock_grant));
        host_writer_roots_inside_other_checkouts(
            binding.project_id(),
            &project,
            &home,
            &assets,
            &paths,
            confinement.writer.as_ref().ok_or("writer")?,
        )?;
    }
    assert_eq!(launch.route.extra["argv_prompt_limit"]["value"], 60_000);
    assert_eq!(
        launch.route.extra["argv_prompt_limit"]["key"],
        "domains.run.argv_prompt_limit_bytes"
    );
    assert_eq!(
        launch.route.extra["argv_prompt_limit"]["origin"]["kind"],
        "user"
    );
    assert!(
        launch
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_HEAD" && value == review_oid.as_str())
    );
    assert!(
        launch
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_BASE" && value == review_oid.as_str())
    );
    // The writer plan the CLI prepares must leave the build slot directory
    // out of reach once an adapter adds its grants: a lane cannot create,
    // unlink or replace a slot file. Cursor never runs `LaneRuntime::grant`
    // or sets up build slots, so Codex, which does both, takes the plan.
    let confinement = launch.confinement.as_ref().ok_or("confinement")?;
    let cargo = home.join(".cargo/bin/cargo");
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        std::fs::create_dir_all(home.join(path))?;
    }
    std::fs::write(home.join(".rustup/settings.toml"), "")?;
    std::fs::write(&cargo, b"#!/bin/sh\n")?;
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700))?;
    let codex_route = provenant_records::ResolvedRoute {
        extra: Default::default(),
        adapter: "codex".into(),
        model: "fixture-model".into(),
        effort: None,
        catalogue_revision: "fixture".into(),
        account_key: Some("local".into()),
        host: "fixture".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 100,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    };
    let codex_account = provenant_providers::identity::RegisteredAccount::new(
        "codex",
        "local",
        home.join(".codex"),
    )?;
    let codex_prompt = provenant_providers::Prompt::new("grants".into());
    let launch_policy = provenant_records::policy::Policy::default();
    let images = vec![PathBuf::from("/outside/shot.png")];
    let codex_runtime = assets.join("provenant");
    let mut codex = provenant_providers::Adapter::prepare(
        &provenant_providers::codex::Codex,
        provenant_providers::PrepareInput {
            images: &images,
            policy: &launch_policy,
            command: None,
            route: &codex_route,
            confined: &confinement.plan,
            prompt: &codex_prompt,
            continuation: None,
            grant_digest: "fixture",
            executable: &executable,
            home: &home,
            user: "fixture".as_ref(),
            host_var: &|_| None,
            skill_stage_max_bytes: 16 * 1024 * 1024,
            account: Some(&codex_account),
            token: None,
            cwd: &project.join(".worktrees/writer"),
            git_context: false,
            nonce: [9; 16],
            provenant: provenant_providers::LaneRuntime {
                executable: &codex_runtime,
                git: &git,
                state: &state,
                home: &home,
                depth: 1,
                attempt: attempt_id,
                assets: &assets,
            },
        },
    )?;
    assert!(
        codex
            .argv
            .windows(2)
            .any(|pair| pair == ["--image", "/outside/shot.png"])
    );
    assert!(
        codex.env.iter().any(|(key, _)| key == "CARGO_TARGET_DIR"),
        "the lane builds into its own target"
    );
    let coordination_reads = configure_build_slots(
        &mut codex,
        &provenant_config::LaneBuildSettings::default(),
        &home,
        &confinement.plan.permissions().scratch.path,
        &codex_runtime,
        &state,
        &std::collections::BTreeMap::new(),
    );
    assert!(
        coordination_reads
            .iter()
            .all(|root| root.scope == PathScope::Exact)
    );
    assert!(
        coordination_reads
            .iter()
            .any(|root| root.path.ends_with("slot-0.lock"))
    );
    // An unrepresentable shim input warns and leaves the prepared lane intact.
    let env_before = codex.env.clone();
    let profile_before = codex.profile.spec.clone();
    let setup_count = codex.setup.len();
    let refused_coordination_reads = configure_build_slots(
        &mut codex,
        &provenant_config::LaneBuildSettings::default(),
        &home,
        &confinement.plan.permissions().scratch.path,
        Path::new("relative-runtime"),
        &state,
        &std::collections::BTreeMap::new(),
    );
    assert!(refused_coordination_reads.is_empty());
    assert_eq!(codex.env, env_before);
    assert_eq!(codex.profile.spec, profile_before);
    assert_eq!(codex.setup.len(), setup_count);
    let slots = state.join(provenant_os::build_slots::DIRECTORY.join("/"));
    assert!(
        codex
            .profile
            .spec
            .runtime_reads
            .iter()
            .any(|grant| grant.path == slots && grant.scope == PathScope::Subtree),
        "build slots were set up for this lane"
    );
    let grants =
        provenant_confinement::write_grants(confinement.plan.permissions(), &codex.profile);
    assert!(!grants.is_empty(), "the profile lists its write grants");
    for grant in &grants {
        let reaches = match grant.scope {
            PathScope::Exact => grant.path == slots,
            PathScope::Subtree => slots.starts_with(&grant.path),
        };
        assert!(
            !reaches,
            "{:?} grant {} reaches {}",
            grant.source,
            grant.path.display(),
            slots.display()
        );
    }
    // Exercise the host project policy through preparation and the actual
    // generated shim, with a fake gh so no GitHub access is needed.
    std::fs::create_dir_all(home.join(".config"))?;
    if std::env::var_os(CHILD)
        .as_deref()
        .is_some_and(|layout| layout != "missing")
    {
        let gh_config = base.join("host-gh-config");
        std::fs::create_dir(&gh_config)?;
        // Token launches must not consult even a malformed host configuration.
        std::fs::write(gh_config.join("config.yml"), "fixture: [unterminated\n")?;
        std::os::unix::fs::symlink(gh_config, home.join(".config/gh"))?;
    } else {
        assert!(!home.join(".config/gh").exists());
    }
    let gh = assets.join("gh");
    std::fs::write(&gh, b"#!/bin/sh\nif [ \"$1\" = auth ] && [ \"$2\" = token ]; then exit 1; fi\nprintf 'forwarded\\n'\n")?;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700))?;
    let policy_dir = state
        .join("projects")
        .join(binding.project_id().to_string());
    std::fs::create_dir_all(&policy_dir)?;
    std::fs::write(policy_dir.join("github.json"), r#"{"deny_labels":["ci"]}"#)?;
    // Both a linked global file and an XDG config beneath a linked directory
    // must survive preparation and the launcher's canonical anchor checks.
    let dotfiles = base.join("dotfiles");
    std::fs::create_dir_all(dotfiles.join("xdg/git"))?;
    std::fs::write(dotfiles.join("global"), "[fixture]\n global = true\n")?;
    std::fs::write(dotfiles.join("xdg/git/config"), "[fixture]\n xdg = true\n")?;
    std::fs::write(dotfiles.join("xdg/git/ignore"), "fixture-ignore\n")?;
    std::fs::write(dotfiles.join("xdg/git/attributes"), "*.fixture text\n")?;
    std::os::unix::fs::symlink(dotfiles.join("global"), home.join(".gitconfig"))?;
    std::os::unix::fs::symlink(dotfiles.join("xdg/git"), home.join(".config/git"))?;
    let publishing_paths = run::LaunchPaths {
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        deadline_unix_ms: paths.deadline_unix_ms,
        scratch: attempt.join("publishing-scratch"),
        provider_state: attempt.join("publishing-provider-state"),
        temporary: attempt.join("publishing-tmp"),
        temporary_alias: None,
    };
    for path in [
        &publishing_paths.scratch,
        &publishing_paths.provider_state,
        &publishing_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let mut publishing_task = task.clone();
    publishing_task
        .profile
        .knobs
        .insert("lanes.publish".into(), serde_json::json!(true));
    let publishing = prepare_with(
        &registry,
        &publishing_task,
        "publishing policy fixture",
        &publishing_paths,
        None,
        |name| {
            Ok(if name == "gh" {
                gh.clone()
            } else {
                executable.clone()
            })
        },
        None,
    )?;
    assert!(
        publishing
            .environment
            .iter()
            .any(|(key, value)| key == "GH_TOKEN" && value == "fixture-host-token")
    );
    assert!(!launch.environment.iter().any(|(key, _)| key == "GH_TOKEN"));
    // One credential decision per lane, recorded with its source.
    let credential = &publishing.route.extra["github_credential"];
    assert_eq!(credential["identity"], "user");
    assert_eq!(credential["source"], "default");
    assert_eq!(credential["delivery"], "injected");
    assert_eq!(credential["mediation"], "gh wrapper (advisory)");
    assert!(!credential.to_string().contains("fixture-host-token"));
    let gh_config = publishing_paths.temporary.join("candidate-0/gh-config");
    assert!(gh_config.is_dir());
    assert!(
        publishing
            .environment
            .iter()
            .any(|(key, value)| key == "GH_CONFIG_DIR" && value == gh_config.as_os_str())
    );
    assert_eq!(std::fs::read_dir(&gh_config)?.count(), 0);
    let gh_shim = std::fs::read_to_string(publishing_paths.temporary.join("candidate-0/bin/gh"))?;
    assert!(gh_shim.contains(gh_config.to_str().ok_or("gh config path")?));
    assert!(gh_shim.contains(provenant_config::GH_INVOCATIONS_FILE));
    assert!(
        !gh_shim.contains(
            home.join(".config/gh")
                .to_str()
                .ok_or("host gh config path")?
        )
    );
    // If gh is installed, exercise its real configuration parser without a
    // network request. The private directory gives the ordinary default even
    // when the host config is linked or absent.
    if let Ok(real_gh) = find_executable("gh") {
        let argv = [
            real_gh.into_os_string(),
            "config".into(),
            "get".into(),
            "git_protocol".into(),
        ];
        let environment = [
            ("HOME".into(), home.as_os_str().to_owned()),
            ("GH_CONFIG_DIR".into(), gh_config.as_os_str().to_owned()),
            ("GH_TOKEN".into(), "fixture-host-token".into()),
        ];
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(5));
        options.env = Some(&environment);
        let actual = executor.execute(options)?;
        assert_eq!(actual.termination, Termination::Exit(0));
        assert_eq!(actual.stdout.bytes, b"https\n");
    }
    let profile = &publishing
        .confinement
        .as_ref()
        .ok_or("confinement")?
        .profile;
    let config = std::fs::read_to_string(publishing_paths.temporary.join("candidate-0/gitconfig"))?;
    for target in [dotfiles.join("global"), dotfiles.join("xdg/git/config")] {
        let target = target.canonicalize()?;
        assert!(config.contains(target.to_str().ok_or("config path")?));
        assert!(
            profile
                .spec
                .runtime_reads
                .iter()
                .any(|grant| grant.path == target)
        );
        provenant_confinement::permissions::AnchoredDirectory::open_canonical(
            target.parent().ok_or("config parent")?,
        )?;
    }
    // The original spelling really is unsuitable for the launcher's canonical
    // anchor boundary: a plan containing it would fail before provider launch.
    assert!(
        provenant_confinement::permissions::AnchoredDirectory::open_canonical(
            home.join(".config/git"),
        )
        .is_err()
    );
    // Follow the launcher's real grant-anchor and compilation boundary. Merely
    // inspecting the plan misses non-canonical optional XDG file grants.
    let confinement = publishing
        .confinement
        .as_ref()
        .ok_or("publishing confinement")?;
    let writer = confinement
        .writer
        .as_ref()
        .map(|writer| (&writer.worktree, &writer.branch));
    if let (Some((worktree, branch)), AccessMode::WorktreeWrite(authority)) =
        (writer, &confinement.plan.permissions().mode)
    {
        provenant_confinement::permissions::prepare_writer_directories(
            worktree, branch, authority, &home,
        )?;
    }
    // Run the real canonical-anchor check on every host-config read. The
    // unrelated built-in /bin grants follow macOS's filesystem layout, so the
    // complete native anchor/command pass below remains macOS-only.
    for grant in &confinement.profile.spec.runtime_reads {
        if grant.path.starts_with(home.join(".config")) || grant.path.starts_with(&dotfiles) {
            let anchor = match grant.scope {
                PathScope::Exact => grant.path.parent().ok_or("config anchor")?,
                PathScope::Subtree => grant.path.as_path(),
            };
            provenant_confinement::permissions::AnchoredDirectory::open_canonical(anchor)?;
        }
    }
    let rendered = provenant_confinement::permissions::snapshot::render(
        confinement.plan.permissions(),
        &confinement.profile,
        &home,
    )?;
    assert!(!rendered.is_empty());
    #[cfg(target_os = "macos")]
    {
        let directories = provenant_confinement::permissions::required_anchors(
            confinement.plan.permissions(),
            &confinement.profile,
            writer,
        )
        .into_iter()
        .map(provenant_confinement::permissions::AnchoredDirectory::open_canonical)
        .collect::<std::result::Result<Vec<_>, _>>()?;
        let anchors = provenant_confinement::permissions::Anchors {
            home: &home,
            directories: &directories,
            worktree: writer,
        };
        let compiled = provenant_confinement::permissions::snapshot::compile(
            confinement.plan.permissions(),
            &confinement.profile,
            &anchors,
        )?;
        let confined_command = compiled.command(
            &publishing.argv,
            &attempt_id,
            &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
        )?;
        assert!(!confined_command.is_empty());
    }
    for name in ["ignore", "attributes"] {
        let expected = dotfiles.join("xdg/git").join(name).canonicalize()?;
        assert!(
            profile
                .spec
                .runtime_reads
                .iter()
                .any(|grant| grant.path == expected)
        );
    }
    let worktree = &publishing_task
        .writer
        .as_ref()
        .ok_or("writer missing")?
        .worktree;
    let gitdir = String::from_utf8(provenant_os::fixture::run_git(
        worktree,
        &["rev-parse", "--absolute-git-dir"],
    )?)?;
    let expected_gitdir = std::fs::canonicalize(gitdir.trim())?;
    let variable = |name: &str| {
        publishing
            .environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| PathBuf::from(value))
    };
    assert_eq!(
        variable("PROVENANT_PRIMARY_CHECKOUT"),
        Some(project.canonicalize()?)
    );
    assert_eq!(variable("PROVENANT_WORKTREE_GITDIR"), Some(expected_gitdir));
    // The runtime populates this private Git directory after preparation.
    let private_common = publishing_paths.temporary.join("candidate-0/git-common");
    std::fs::create_dir_all(private_common.join("objects"))?;
    std::fs::create_dir_all(private_common.join("refs"))?;
    std::fs::write(private_common.join("HEAD"), b"ref: refs/heads/writer\n")?;
    std::fs::copy(project.join(".git/config"), private_common.join("config"))?;
    let private_objects = publishing_paths.temporary.join("candidate-0/git-objects");
    std::fs::create_dir_all(private_objects.join("info"))?;
    std::fs::write(
        private_objects.join("info/alternates"),
        format!("{}\n", project.join(".git/objects").display()),
    )?;
    let shared_config = std::fs::read(project.join(".git/config"))?;
    let shared_index = std::fs::read(project.join(".git/index"))?;
    let shared_refs = std::fs::read_dir(project.join(".git/refs/heads"))?.count();

    std::fs::write(
        worktree.join("private-blob-fixture"),
        b"publishing private object fixture\n",
    )?;
    let git_shim = publishing_paths.temporary.join("candidate-0/bin/git");
    // Exercise the generated shim with the Git credential protocol on stdin.
    #[allow(clippy::disallowed_methods)]
    let mut credential = std::process::Command::new(&git_shim)
        .args(["credential", "fill"])
        .current_dir(worktree)
        .env_clear()
        .envs(
            publishing
                .environment
                .iter()
                .map(|(key, value)| (key, value)),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Write as _;
    credential
        .stdin
        .take()
        .ok_or("credential stdin")?
        .write_all(b"protocol=https\nhost=github.com\n\n")?;
    let credential = credential.wait_with_output()?;
    assert!(credential.status.success());
    assert!(
        credential
            .stdout
            .windows(b"password=fixture-host-token".len())
            .any(|window| window == b"password=fixture-host-token")
    );
    let env = [
        ("HOME".into(), git_home.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let run_git = |args: &[&str]| {
        let argv: Vec<_> = std::iter::once(git_shim.as_os_str().to_owned())
            .chain(args.iter().map(Into::into))
            .collect();
        let mut options = ExecOptions::new(&argv, worktree, Duration::from_secs(5));
        options.env = Some(&env);
        executor.execute(options)
    };
    let oid = run_git(&["hash-object", "private-blob-fixture"])?;
    assert_eq!(oid.termination, Termination::Exit(0), "{oid:?}");
    let oid = std::str::from_utf8(&oid.stdout.bytes)?.trim();
    let added = run_git(&["add", "private-blob-fixture"])?;
    assert_eq!(added.termination, Termination::Exit(0), "{added:?}");
    let object = format!("{}/{}", &oid[..2], &oid[2..]);
    assert!(
        publishing_paths
            .temporary
            .join("candidate-0/git-objects")
            .join(&object)
            .is_file()
    );
    assert!(!project.join(".git/objects").join(&object).exists());
    assert_eq!(std::fs::read(project.join(".git/config"))?, shared_config);
    assert_eq!(std::fs::read(project.join(".git/index"))?, shared_index);
    assert_eq!(
        std::fs::read_dir(project.join(".git/refs/heads"))?.count(),
        shared_refs
    );
    let credential = run_git(&["config", "--get", "credential.helper"])?;
    assert_eq!(credential.termination, Termination::Exit(0));
    assert!(String::from_utf8_lossy(&credential.stdout.bytes).contains("auth git-credential"));

    let shim = publishing_paths.temporary.join("candidate-0/bin/gh");
    let run_shim = |label: &str| {
        use provenant_os::exec::{ExecOptions, Executor};
        let argv = [
            shim.as_os_str().to_owned(),
            "pr".into(),
            "create".into(),
            "-l".into(),
            label.into(),
        ];
        executor.execute(ExecOptions::new(
            &argv,
            &attempt,
            std::time::Duration::from_secs(5),
        ))
    };
    let denied = run_shim("ci")?;
    assert_eq!(denied.termination, provenant_os::exec::Termination::Exit(1));
    assert_eq!(denied.stderr.bytes, b"github.deny_labels\n");
    let allowed = run_shim("docs")?;
    assert_eq!(
        allowed.termination,
        provenant_os::exec::Termination::Exit(0)
    );
    assert_eq!(allowed.stdout.bytes, b"forwarded\n");
    // A named pool redraws after an adapter disappears during local preparation.
    // The first two lookups establish availability; the third refuses the first
    // selected member, and the next lookup admits the remaining member.
    let saved_config = std::fs::read(state.join("config.toml"))?;
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[domains.routing]\nsettlement_retries=1\n[[domains.routing.pools.redraw-fixture]]\nmodel='cursor/gpt-5.3-codex-low'\nweight=1\nefforts=[]\n[[domains.routing.pools.redraw-fixture]]\nmodel='cursor/grok-4.7'\nweight=1\nefforts=[]\n",
    )?;
    let mut redraw_task = task.clone();
    redraw_task.route = run::RequestedRoute::Named {
        extra: Default::default(),
        name: "redraw-fixture".into(),
        effort: None,
    };
    let redraw_prompt = "pool redraw fixture";
    redraw_task.prompt_digest =
        provenant_records::CanonicalV1::file_digest(redraw_prompt.as_bytes());
    let redraw_paths = run::LaunchPaths {
        scratch: attempt.join("redraw-scratch"),
        provider_state: attempt.join("redraw-provider-state"),
        temporary: attempt.join("redraw-tmp"),
        temporary_alias: None,
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        deadline_unix_ms: paths.deadline_unix_ms,
    };
    for path in [
        &redraw_paths.scratch,
        &redraw_paths.provider_state,
        &redraw_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let lookup_count = std::cell::Cell::new(0);
    let redraw_launch = prepare_with(
        &registry,
        &redraw_task,
        redraw_prompt,
        &redraw_paths,
        None,
        |adapter| {
            assert_eq!(adapter, "cursor");
            let count = lookup_count.get() + 1;
            lookup_count.set(count);
            if count == 3 {
                Err("fixture executable disappeared".into())
            } else {
                Ok(executable.clone())
            }
        },
        None,
    )?;
    assert_eq!(redraw_launch.skipped_routes.len(), 1);
    assert!(
        redraw_launch.skipped_routes[0]
            .detail
            .contains("selected route is unavailable")
    );
    assert!(
        !redraw_launch.skipped_routes[0]
            .detail
            .contains("fixture executable disappeared")
    );
    assert_ne!(
        redraw_launch.route.model.as_deref(),
        Some(redraw_launch.skipped_routes[0].route.as_str())
    );
    let config = std::fs::read_to_string(state.join("config.toml"))?;
    std::fs::write(
        state.join("config.toml"),
        config.replace("settlement_retries=1", "settlement_retries=0"),
    )?;
    let no_retry_paths = run::LaunchPaths {
        scratch: attempt.join("no-redraw-scratch"),
        provider_state: attempt.join("no-redraw-provider-state"),
        temporary: attempt.join("no-redraw-tmp"),
        temporary_alias: None,
        attempt_id: paths.attempt_id,
        operation_id: paths.operation_id.clone(),
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        deadline_unix_ms: paths.deadline_unix_ms,
    };
    for path in [
        &no_retry_paths.scratch,
        &no_retry_paths.provider_state,
        &no_retry_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    // Settlement retries bound launched attempts. A member that preparation
    // excluded never launched, so a zero budget still tries the next member.
    let calls = std::cell::Cell::new(0);
    let no_retry = prepare_with(
        &registry,
        &redraw_task,
        redraw_prompt,
        &no_retry_paths,
        None,
        |_| {
            let count = calls.get() + 1;
            calls.set(count);
            if count == 3 {
                Err("fixture executable disappeared".into())
            } else {
                Ok(executable.clone())
            }
        },
        None,
    )?;
    assert_eq!(calls.get(), 4);
    assert_eq!(no_retry.skipped_routes.len(), 1);
    assert_eq!(no_retry.skipped_routes[0].kind, SkipKind::Unavailable);
    assert!(
        !no_retry.skipped_routes[0]
            .detail
            .contains("fixture executable disappeared")
    );
    assert_ne!(
        no_retry.route.model.as_deref(),
        Some(no_retry.skipped_routes[0].route.as_str())
    );
    drop(no_retry);
    std::fs::write(state.join("config.toml"), saved_config)?;

    let mut review_task = task.clone();
    review_task.writer = None;
    let review_paths = run::LaunchPaths {
        scratch: attempt.join("review-scratch"),
        provider_state: attempt.join("review-provider-state"),
        temporary: attempt.join("review-tmp"),
        temporary_alias: None,
        ..redraw_paths
    };
    for path in [
        &review_paths.scratch,
        &review_paths.provider_state,
        &review_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let review = prepare_with(
        &registry,
        &review_task,
        "short review",
        &review_paths,
        None,
        |_| Ok(executable.clone()),
        None,
    )?;
    assert!(
        review
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_HEAD" && value == review_oid.as_str())
    );
    assert!(
        review
            .environment
            .iter()
            .any(|(key, value)| key == "PROVENANT_LANE_BASE" && value == review_oid.as_str())
    );
    let executed = executor.execute(ExecOptions::new(
        &launch.argv,
        &project,
        Duration::from_secs(2),
    ))?;
    assert_eq!(executed.termination, Termination::Exit(0));
    let update = provenant_os::provider_fence::acquire(
        roots.state.as_path(),
        "cursor",
        provenant_os::fs::LockMode::Exclusive,
    )?;
    drop(update);
    assert_eq!(launch.route.provider.as_deref(), Some("cursor"));
    assert_eq!(
        launch.route.model.as_deref(),
        Some("cursor/gpt-5.3-codex-low")
    );
    assert_eq!(launch.argv[0], executable.as_os_str());
    let scratch = paths.temporary.join("candidate-0");
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch.join("git-context.json"))?)?;
    assert_eq!(metadata["base"], metadata["head"]);
    assert_eq!(std::fs::read(scratch.join("git-diff.patch"))?, b"");
    let pointer = launch
        .argv
        .last()
        .ok_or("prompt missing")?
        .to_str()
        .ok_or("prompt UTF-8")?;
    assert!(pointer.len() < 1024);
    let spilled = attempt.join("prompt.md");
    assert!(pointer.contains(spilled.to_str().ok_or("path UTF-8")?));
    let spilled_text = std::fs::read_to_string(&spilled)?;
    let location = spilled_text
        .strip_suffix(&full)
        .ok_or("spilled prompt lost the task text")?;
    assert!(location.contains("Host-written Git context"));
    assert!(location.contains(&scratch.display().to_string()));
    assert_eq!(
        store.record(attempt_id, &call)?.prompt.digest,
        task.prompt_digest
    );
    assert_ne!(
        task.prompt_digest,
        provenant_records::CanonicalV1::file_digest(pointer.as_bytes())
    );
    assert_eq!(std::fs::read_to_string(attempt.join("prompt.txt"))?, prompt);
    // The original named pool may change or disappear after dispatch.
    // A continuation must use the exact applied provider, model and
    // account without consulting that pool or the original account alias.
    let mut prior = store.record(attempt_id, &call)?;
    prior.requested_route = run::RequestedRoute::Named {
        extra: Default::default(),
        name: "retired-pool".into(),
        effort: None,
    };
    prior.applied_route = Some(launch.route.clone());
    prior.session = Some("fixture-session".into());
    // Cursor continuation is retained in the settled attempt, never the host home.
    let retained_session = paths
        .provider_state
        .join("candidate-0/chats/fixture-session");
    std::fs::create_dir_all(&retained_session)?;
    std::fs::write(
        retained_session.join("transcript"),
        b"retained fixture session",
    )?;
    let mut continued_task = task.clone();
    continued_task.route = prior.requested_route.clone();
    continued_task.account = Some("retired-account-alias".into());
    let continued_paths = run::LaunchPaths {
        attempt_id: paths.attempt_id,
        operation_id: OperationId::parse("different-resume-seed")?,
        task_index: paths.task_index,
        lane_depth: paths.lane_depth,
        deadline_unix_ms: paths.deadline_unix_ms,
        scratch: attempt.join("continued-scratch"),
        provider_state: attempt.join("continued-provider-state"),
        temporary: attempt.join("continued-tmp"),
        temporary_alias: None,
    };
    for path in [
        &continued_paths.scratch,
        &continued_paths.provider_state,
        &continued_paths.temporary,
    ] {
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let continued = prepare_with(
        &registry,
        &continued_task,
        "Continue the prior task",
        &continued_paths,
        Some(&prior),
        |adapter| {
            assert_eq!(adapter, "cursor", "resume probed a different provider");
            Ok(executable.clone())
        },
        None,
    )?;
    assert_eq!(continued.route.provider, launch.route.provider);
    assert_eq!(continued.route.model, launch.route.model);
    assert_eq!(continued.route.account_key, launch.route.account_key);
    assert!(continued.route.pool_selection.is_none());
    assert!(continued.argv.iter().any(|arg| arg == "--resume"));
    assert_eq!(
        std::fs::read(
            continued_paths
                .provider_state
                .join("candidate-0/chats/fixture-session/transcript")
        )?,
        b"retained fixture session"
    );
    let confinement = launch.confinement.ok_or("writer confinement missing")?;
    assert!(confinement.writer.is_some());
    // Exercise the actual request builder with two distinct project overrides.
    let project_a = binding.project_id();
    let project_b = ProjectId::from_bytes([0xff; 16]);
    let config = provenant_confinement::run_config::RunConfig {
        projects: [
            (
                project_a.to_string(),
                provenant_confinement::run_config::ProjectRunConfig {
                    writer_writes: Some(vec![base.join("a-locks")]),
                    ..Default::default()
                },
            ),
            (
                project_b.to_string(),
                provenant_confinement::run_config::ProjectRunConfig {
                    writer_writes: Some(vec![base.join("b-locks")]),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let request = confinement_request(
        project_a,
        ConfinementRoots {
            host_roots: None,
            cwd: &project,
            home: &home,
            state: Path::new("/fixture-host-state"),
            credential_roots: &[],
            github_roots: &[],
            protected_worktrees: &[],
            scratch: &paths.scratch,
            private_state: &paths.provider_state,
            assets: &assets,
            prompt_file: None,
        },
        confinement.writer.as_ref(),
        &run::LaneAccess::default(),
        &config,
        "cursor",
        false,
        InventoryContext::Sandboxed,
    )?;
    assert!(request.permissions.protected_roots.is_empty());
    assert_eq!(
        request.permissions.write_roots,
        vec![PathGrant {
            path: base.join("a-locks"),
            scope: PathScope::Subtree,
        }]
    );
    // The default config enables the cargo seed, and prepare hands that to the
    // lane's launch confinement, where `seed_target` reads it.
    assert!(
        confinement.cargo_registry_seed,
        "the default FabricConfig must reach the writer launch with the seed enabled"
    );
    assert!(!confinement.plan.permissions().writer_publishing());
    assert!(!scratch.join("publishing.gitconfig").exists());
    assert!(
        confinement
            .plan
            .permissions()
            .read_roots
            .contains(&PathGrant {
                path: spilled,
                scope: PathScope::Exact,
            })
    );
    assert_eq!(
        confinement.plan.permissions().scratch.path,
        paths.temporary.join("candidate-0")
    );
    let mut bad_task = task.clone();
    bad_task.base = Some("absent-base".into());
    let result = prepare_with(
        &registry,
        &bad_task,
        "fixture prompt",
        &paths,
        None,
        |_| Err("provider lookup must not happen for an invalid base".into()),
        None,
    );
    assert!(
        matches!(result, Err(PrepareError::Run(run::RunError::InvalidBase(base))) if base == "absent-base")
    );
    // A writable tree the plan cannot cover names that error, not a disclosure denial.
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.run]\nwriter_writes = ['{}']\n",
            paths.scratch.display(),
        ),
    )?;
    let error = prepare_with(
        &registry,
        &task,
        &full,
        &paths,
        None,
        |_| Ok(executable.clone()),
        None,
    )
    .err()
    .ok_or("unconfinable worktree accepted")?;
    assert!(
        matches!(error, PrepareError::Candidates { ref plans, .. } if plans.iter().any(|plan| matches!(plan, provenant_confinement::PlanError::Permission(provenant_confinement::permissions::PermissionError::ProtectedOverlap)))),
        "{error}"
    );
    Ok(())
}

/// The primary checkout is another protected worktree for a writer. The host
/// owner's `writer_writes` may still name its ignored lock directory; dispatch
/// roots there, and host roots that open the checkout or its Git, stay refused.
fn host_writer_roots_inside_other_checkouts(
    project_id: ProjectId,
    primary: &Path,
    home: &Path,
    assets: &Path,
    paths: &run::LaunchPaths,
    writer: &run::WriterLaunch,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::PermissionError;
    let worktree = writer.worktree.path().to_owned();
    assert!(worktree.starts_with(primary), "fixture worktree is nested");
    let locks = primary.join(".agent-run/locks");
    std::fs::create_dir_all(&locks)?;
    let protected = [PathGrant {
        path: primary.to_owned(),
        scope: PathScope::Subtree,
    }];
    let config = |writes: Vec<PathBuf>| provenant_confinement::run_config::RunConfig {
        writer_writes: writes,
        ..Default::default()
    };
    let request = |writer: Option<&run::WriterLaunch>,
                   access: &run::LaneAccess,
                   config: &provenant_confinement::run_config::RunConfig| {
        confinement_request(
            project_id,
            ConfinementRoots {
                host_roots: None,
                cwd: if writer.is_some() { &worktree } else { primary },
                home,
                state: Path::new("/fixture-host-state"),
                credential_roots: &[],
                github_roots: &[],
                protected_worktrees: &protected,
                scratch: &paths.scratch,
                private_state: &paths.provider_state,
                assets,
                prompt_file: None,
            },
            writer,
            access,
            config,
            "cursor",
            false,
            InventoryContext::Sandboxed,
        )
    };
    let hosted = config(vec![locks.clone()]);
    let launched = request(Some(writer), &run::LaneAccess::default(), &hosted)?;
    let lock_grant = PathGrant {
        path: locks.clone(),
        scope: PathScope::Subtree,
    };
    assert_eq!(launched.permissions.write_roots, vec![lock_grant.clone()]);
    let mandatory = launched
        .permissions
        .policy
        .as_ref()
        .ok_or("policy")?
        .mandatory();
    assert!(
        mandatory
            .protected()
            .iter()
            .any(|root| root.path == primary)
    );
    assert!(mandatory.host_writes().contains(&lock_grant));
    provenant_confinement::ConfinedPlan::new(launched)?;
    // A read-only lane receives no writer roots and no exception.
    let read_only = request(None, &run::LaneAccess::default(), &hosted)?;
    assert!(read_only.permissions.write_roots.is_empty());
    let mandatory = read_only
        .permissions
        .policy
        .as_ref()
        .ok_or("policy")?
        .mandatory();
    assert!(mandatory.host_writes().is_empty());
    // A dispatch root inside the checkout is not host policy.
    let dispatch = run::LaneAccess {
        write_roots: vec![primary.join("scripts")],
        ..Default::default()
    };
    assert!(matches!(
        request(Some(writer), &dispatch, &config(Vec::new())),
        Err(PermissionError::ProtectedOverlap)
    ));
    // Host roots equal to or enclosing the checkout, or inside Git, stay refused.
    for path in [
        primary.to_owned(),
        primary.join(".worktrees"),
        primary.join(".git/hooks"),
    ] {
        assert!(
            matches!(
                request(
                    Some(writer),
                    &run::LaneAccess::default(),
                    &config(vec![path.clone()])
                ),
                Err(PermissionError::ProtectedOverlap)
            ),
            "{}",
            path.display()
        );
    }
    Ok(())
}
