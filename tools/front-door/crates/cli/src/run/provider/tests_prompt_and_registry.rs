#[test]
fn launch_prompt_refuses_invalid_input_before_account_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let attempt = canonical(temp.path())?;
    let paths = prompt_paths(&attempt)?;
    let full = format!("Durable output instructions\n\n{}", "é".repeat(75_000));
    // Refuse invalid input before consulting accounts or executables.
    for entry in REGISTRY {
        let adapter = entry.adapter.id();
        let requested = run::TaskRequest {
            context: None,
            extra: Default::default(),
            limit_extensions: Default::default(),
            build_heavy: false,
            title: None,
            base: None,
            head: None,
            source_head: None,
            base_ref: None,
            profile: Default::default(),
            prompt_digest: provenant_records::CanonicalV1::file_digest(full.as_bytes()),
            route: run::RequestedRoute::Model { required_family: None,
                extra: Default::default(),
                provider: adapter.into(),
                model: format!("{adapter}/fixture-model"),
                effort: None,
            },
            account: None,
            authority_digest: provenant_core::Digest::from_bytes([4; 32]),
            writer: None,
            deliver: None,
            provider_timeout_seconds: 20,
            delivery_id: None,
            access: Default::default(),
        };
        let oversized = format!("Durable output instructions\n\n{}", "a".repeat(1024 * 1024));
        let error = prepare_with(
            &Registry::default(),
            &requested,
            &oversized,
            &paths,
            None,
            |_| unreachable!("invalid prompt must not probe executables"),
            None,
        )
        .err()
        .ok_or("oversized launch accepted")?;
        assert!(
            matches!(error, PrepareError::Prompt(LaunchPromptError::TooLarge)),
            "{adapter}: {error}"
        );
        if matches!(adapter, "cursor" | "agy") {
            let mut nested = prompt_paths(&attempt)?;
            nested.lane_depth = 2;
            let prompt = launch_prompt(
                &full,
                &nested,
                &attempt,
                adapter,
                65_536,
                provenant_records::diagnostic::SettingSource::ShippedDefault,
                &provenant_records::policy::Policy::default(),
            )?;
            assert_eq!(prompt.text(), full);
            assert!(!attempt.join("prompt.md").exists());
            if adapter == "cursor" {
                let home = attempt.join("fixture-home");
                let checkout = attempt.join("fixture-checkout");
                let state = attempt.join("fixture-state");
                for path in [&home, &checkout, &state] {
                    std::fs::create_dir_all(path)?;
                }
                std::fs::write(
                    state.join("config.toml"),
                    "schema_version = 1\n[domains.run]\nargv_prompt_limit_bytes = 262144\n",
                )?;
                let mut inputs = provenant_core::RootInputs::new(home, checkout);
                inputs.state_dir = Some(state.clone());
                let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
                let configured = Registry {
                    roots: Some(&roots),
                    ..Registry::default()
                };
                let error = prepare_with(
                    &configured,
                    &requested,
                    &full,
                    &nested,
                    None,
                    |_| unreachable!("missing home must precede executable probing"),
                    None,
                )
                .err()
                .ok_or("missing home accepted")?;
                assert!(
                    error.to_string().contains("home directory is unavailable"),
                    "{error}"
                );
                std::fs::write(
                    state.join("config.toml"),
                    "schema_version = 1\n[domains.run]\nargv_prompt_limit_bytes = 65536\n",
                )?;
                let error = prepare_with(
                    &configured,
                    &requested,
                    &full,
                    &nested,
                    None,
                    |_| unreachable!("nested threshold must precede probing"),
                    None,
                )
                .err()
                .ok_or("nested threshold accepted")?;
                assert!(
                    !matches!(error, PrepareError::Prompt(_)),
                    "a warning-level argv threshold must not refuse: {error}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn detect_failure_distinguishes_probe_directory_from_unknown_identity()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{ProviderError, accounts::Registration};

    let entry: Registration = serde_json::from_value(serde_json::json!({
        "adapter": "claude",
        "label": "fixture",
        "key": "fixture-key",
        "directory": "/home/test/.claude-fixture",
    }))?;
    let error = ProviderError::ProbeDirectory("fixture host state fault".into());
    let message = detect_failure(&entry, &error);
    assert_eq!(message, format!("claude account fixture: {error}"));
    assert!(!message.contains("re-register"));

    let error = ProviderError::UnknownIdentity;
    assert_eq!(
        detect_failure(&entry, &error),
        format!("claude account fixture: {error}; re-register the signed-in identity")
    );
    Ok(())
}

#[test]
fn candidate_leases_skip_busy_providers_and_share_one_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::Clock;
    use provenant_os::fs::LockMode;
    let temp = tempfile::tempdir()?;
    let _a = provenant_os::provider_fence::acquire(temp.path(), "codex", LockMode::Exclusive)?;
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0)?,
        provenant_core::MonoTime::from_micros(0),
    );
    let immediate = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        Some(0),
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(immediate.contains_key("cursor"));
    assert!(!immediate.contains_key("codex"));
    assert_eq!(clock.mono_now().as_micros(), 0);
    drop(immediate);
    let leases = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        None,
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(leases.contains_key("cursor"));
    assert!(!leases.contains_key("codex"));
    assert_eq!(clock.mono_now().as_micros(), 0);
    drop(leases);
    let _b = provenant_os::provider_fence::acquire(temp.path(), "cursor", LockMode::Exclusive)?;
    let immediate = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        Some(0),
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(immediate.is_empty());
    assert_eq!(clock.mono_now().as_micros(), 0);
    let leases = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        Some(1),
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(leases.is_empty());
    assert_eq!(clock.mono_now().as_micros(), 1_000_000);
    let leases = available_provider_leases(
        temp.path(),
        &["codex", "cursor"],
        None,
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    assert!(leases.is_empty());
    assert_eq!(clock.mono_now().as_micros(), 31_000_000);
    Ok(())
}

#[test]
fn candidate_lease_wait_honours_cancellation() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CancelToken, Clock, MonoTime, TestClock, TimeError, WallTime};
    use provenant_os::fs::LockMode;
    struct CancelOnWait<'a> {
        clock: TestClock,
        cancel: &'a CancelToken,
    }
    impl Clock for CancelOnWait<'_> {
        fn wall_now(&self) -> Result<WallTime, TimeError> {
            self.clock.wall_now()
        }
        fn mono_now(&self) -> MonoTime {
            self.clock.mono_now()
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError> {
            self.clock.wait_until(deadline)?;
            self.cancel.cancel();
            Ok(())
        }
    }
    let temp = tempfile::tempdir()?;
    let updating =
        provenant_os::provider_fence::acquire(temp.path(), "codex", LockMode::Exclusive)?;
    let cancel = CancelToken::new();
    let clock = CancelOnWait {
        clock: TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0)),
        cancel: &cancel,
    };
    let error = available_provider_leases(temp.path(), &["codex"], None, &clock, &cancel)
        .err()
        .ok_or("cancelled lease wait admitted")?;
    assert!(matches!(error, ProviderLeaseError::Cancelled));
    assert_eq!(clock.mono_now().as_micros(), 50_000);
    // Cancellation is also checked when the provider becomes available.
    drop(updating);
    assert!(available_provider_leases(temp.path(), &["codex"], None, &clock, &cancel,).is_err());
    Ok(())
}

#[test]
fn executable_discovery_after_wait_observes_the_completed_update()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{Clock, DurationMicros};
    use provenant_os::fs::LockMode;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let bin = temp.path().join("requester-bin");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&bin)?;
    let executable = bin.join("cursor-agent");
    let host = run::HostLaunchContext {
        graph_directory: None,
        configuration_root: temp.path().join("state"),
        cwd: project.clone(),
        executable_search_path: vec![bin],
        lane_path: None,
        git: "/usr/bin/git".into(),
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
        cargo_build_jobs: None,
        #[cfg(feature = "fixture")]
        fixture_state_write: false,
    };
    let search = run::FilteredLanePath::for_requester(Some(&host), &project, &project)?;
    assert!(super::preparation::preparation_executable(&search, "cursor").is_err());
    let updating =
        provenant_os::provider_fence::acquire(temp.path(), "cursor", LockMode::Exclusive)?;
    let clock = provenant_os::clock::SystemClock::new();
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let executable = &executable;
        let clock = &clock;
        let update = scope.spawn(move || -> Result<(), String> {
            clock
                .wait_until(
                    clock
                        .mono_now()
                        .checked_add(DurationMicros::from_micros(50_000))
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            std::fs::write(executable, "new provider").map_err(|e| e.to_string())?;
            std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            drop(updating);
            Ok(())
        });
        let leases = available_provider_leases(
            temp.path(),
            &["cursor"],
            Some(2),
            clock,
            &provenant_core::CancelToken::new(),
        )?;
        assert!(leases.contains_key("cursor"));
        assert_eq!(
            super::preparation::preparation_executable(&search, "cursor")?,
            executable.as_path()
        );
        assert_eq!(std::fs::read_to_string(executable)?, "new provider");
        update.join().expect("update thread")?;
        Ok(())
    })
}

/// Initialise a repository with one commit in `project` under a minimal explicit
/// environment, and return the canonical Git executable it used.
fn init_fixture_repository(
    executor: &provenant_os::exec::SystemExecutor,
    project: &std::path::Path,
    git_home: &std::path::Path,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, Termination};
    use std::time::Duration;
    let git = provenant_os::git::git_executable()
        .ok_or("git unavailable")?
        .canonicalize()?;
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["commit", "-q", "--allow-empty", "-m", "Initial"],
    ] {
        let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
            .chain(args.into_iter().map(Into::into))
            .collect();
        let mut env = vec![
            ("HOME".into(), git_home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        env.extend(provenant_os::fixture::fixture_git_settings());
        let mut options = ExecOptions::new(&argv, project, Duration::from_secs(5));
        options.env = Some(&env);
        let output = executor.execute(options)?;
        assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
    }
    Ok(git)
}

#[test]
fn an_account_mismatch_is_cleared_only_by_a_check_that_names_the_account() {
    for adapter in ["codex", "cursor", "opencode"] {
        assert!(super::preparation::login_check_can_clear(adapter, false));
        assert!(!super::preparation::login_check_can_clear(adapter, true));
    }
    for adapter in ["agy", "claude"] {
        assert!(super::preparation::login_check_can_clear(adapter, true));
    }
}

#[test]
fn registry_only_provider_resolves_prepares_and_reports_quota()
-> Result<(), Box<dyn std::error::Error>> {
    let spec = provenant_records::AdapterSpec::get("fixture-provider")
        .ok_or("fixture AdapterSpec is missing")?;
    assert!(spec.local_login);
    assert_eq!(spec.local_account, ("fixture", "work"));
    registry_provider_resolves_prepares_and_reports_quota("fixture-provider", "work", false)
}

#[test]
fn registry_spec_drives_local_provider_preparation_and_quota()
-> Result<(), Box<dyn std::error::Error>> {
    // Spec: docs/specs/routing.md#monthly-allowance-ledger
    let catalogue = provenant_routing::Catalogue::bundled()?;
    let config = provenant_routing::RoutingConfig::default();
    let model = provenant_routing::ModelId::parse("cursor/composer-2.5")?;
    let now = provenant_core::WallTime::from_unix_seconds(100)?;
    let assess = |account| {
        provenant_routing::monthly_allowance_assessment(
            &catalogue,
            &config,
            now,
            &model,
            Some(account),
            None,
        )
        .expect("Cursor declares a monthly allowance")
    };
    assert!(!assess("local").constrained);
    assert!(assess("registered").constrained);
    let scope = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: "cursor".into(),
        account_key: "local".into(),
        label: "local".into(),
    };
    let snapshot = provenant_quota::QuotaSnapshot {
        extra: Default::default(),
        schema_version: 1,
        account: scope,
        revision: 1,
        status: provenant_quota::QuotaStatus::Available,
        observed_unix_seconds: 90,
        source_ref: "fixture".into(),
        authority_ref: None,
        details: None,
        windows: vec![provenant_quota::QuotaWindow {
            extra: Default::default(),
            pool: "billing_cycle".into(),
            window_seconds: Some(30 * 86400),
            resets_unix_seconds: Some(30 * 86400),
            used_basis_points: 9000,
        }],
    };
    let mut allowance = provenant_quota::AllowanceLedger::default();
    allowance.observe(&snapshot);
    let observation = provenant_routing::QuotaObservation {
        model: model.clone(),
        snapshot,
        allowance,
        expires_unix_seconds: 200,
    };
    assert!(
        provenant_routing::monthly_allowance_assessment(
            &catalogue,
            &config,
            now,
            &model,
            Some("local"),
            Some(&observation),
        )
        .expect("monthly assessment")
        .constrained
    );
    registry_provider_resolves_prepares_and_reports_quota("cursor", "local", false)
}

#[test]
// Spec: docs/specs/routing.md#accounts-and-credentials
fn unknown_identity_policy_reaches_provider_preparation() -> Result<(), Box<dyn std::error::Error>> {
    // Keep monthly eligibility out of this identity-policy fixture.
    registry_provider_resolves_prepares_and_reports_quota("fixture-provider", "work", true)
}

// Spec: docs/specs/routing.md#monthly-allowance-ledger
#[test]
fn local_login_preparation_booking_survives_ordinary_quota_tick()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_in_clean_environment("local_login_preparation_booking_survives_ordinary_quota_tick")? {
        return Ok(());
    }

    use provenant_core::{AttemptId, CancelToken, Clock, Digest, OperationId};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

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
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[domains.routing.pacing.accounts.\"cursor/local\".monthly]\ninitial_launch_cost_basis_points=5100\n",
    )?;

    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility =
        provenant_confinement::UtilityRuntime::new(&git, &git_home, roots.state.as_path())?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &project,
        provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
        &cancel,
    )?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;

    let now = provenant_os::clock::SystemClock::new().unix_seconds();
    let account = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: "cursor".into(),
        account_key: "local".into(),
        label: "Local".into(),
    };
    let snapshot = provenant_quota::QuotaSnapshot {
        extra: Default::default(),
        schema_version: 1,
        account: account.clone(),
        revision: 1,
        status: provenant_quota::QuotaStatus::Available,
        observed_unix_seconds: now,
        source_ref: "fixture".into(),
        authority_ref: None,
        details: None,
        windows: vec![provenant_quota::QuotaWindow {
            extra: Default::default(),
            pool: "billing_cycle".into(),
            window_seconds: Some(30 * 86400),
            resets_unix_seconds: Some(now + 3 * 86400),
            used_basis_points: 0,
        }],
    };
    let mut record = provenant_quota::AccountRecord::empty(account);
    record.revision = 1;
    record.snapshot = Some(snapshot);
    let quota = state.join("quota");
    let account_dir = quota.join("local");
    std::fs::create_dir(&quota)?;
    std::fs::set_permissions(&quota, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir(&account_dir)?;
    std::fs::set_permissions(&account_dir, std::fs::Permissions::from_mode(0o700))?;
    let quota_record = account_dir.join("cursor.json");
    std::fs::write(&quota_record, serde_json::to_vec(&record)?)?;
    std::fs::set_permissions(&quota_record, std::fs::Permissions::from_mode(0o600))?;

    let executable = assets.join("cursor/bin/agent");
    std::fs::create_dir_all(executable.parent().ok_or("cursor executable parent")?)?;
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nif [ \"$1\" = status ]; then printf '%s' '{}'; else printf 'fixture-cursor 1.0\\n'; fi\n",
            include_str!("../../../../../tests/fixtures/providers/cursor/status.stdout")
        ),
    )?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;

    let prompt = "local pacing fixture";
    let task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        limit_extensions: Default::default(),
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            required_family: None,
            extra: Default::default(),
            provider: "cursor".into(),
            model: "cursor/composer-2.5".into(),
            effort: None,
        },
        account: Some("local".into()),
        authority_digest: Digest::from_bytes([4; 32]),
        writer: None,
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
    let prepare = |attempt_bytes: [u8; 16], operation: &str| {
        let attempt_id = AttemptId::from_bytes(attempt_bytes);
        let attempt = state.join(format!("attempts/{attempt_id}"));
        let paths = run::LaunchPaths {
            attempt_id,
            operation_id: OperationId::parse(operation).expect("fixture operation id"),
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
                .create(path)
                .expect("private launch directory");
        }
        super::preparation::prepare_with(
            &registry,
            &task,
            prompt,
            &paths,
            None,
            |name| {
                if name == "cursor" {
                    Ok(executable.clone())
                } else {
                    Err("fixture provider unavailable".into())
                }
            },
            None,
        )
    };

    let first = prepare([1; 16], "local-pacing-first")?;
    assert_eq!(first.route.account_key.as_deref(), Some("local"));

    let effects = provenant_providers::quota_live::LiveQuota::new(
        &roots,
        &home,
        &provenant_providers::accounts::Settings::default(),
    )?;
    crate::quota::execute(
        &crate::quota::Quota::Tick {
            adapter: crate::quota::Filter {
                adapter: Some("cursor".into()),
            },
            trigger: crate::quota::Trigger::Timer,
            power: crate::quota::Power::Ac,
        },
        &roots,
        provenant_providers::REGISTRY,
        &effects,
    )?;

    let retained: provenant_quota::AccountRecord =
        serde_json::from_slice(&std::fs::read(&quota_record)?)?;
    assert_eq!(retained.allowance.launches, 1);
    assert!(retained.snapshot.is_none(), "quota observations must be dropped");
    assert!(
        prepare([2; 16], "local-pacing-second").is_err(),
        "the retained local-login booking must constrain another prepared launch"
    );
    // Prove the second preparation was constrained by monthly debt, rather
    // than an unrelated setup or login failure after the tick.
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[domains.routing.pacing.accounts.\"cursor/local\".monthly]\nenabled=false\n",
    )?;
    let bypassed = prepare([3; 16], "local-pacing-bypass")?;
    assert_eq!(bypassed.route.account_key.as_deref(), Some("local"));
    let unchanged: provenant_quota::AccountRecord =
        serde_json::from_slice(&std::fs::read(&quota_record)?)?;
    assert_eq!(unchanged.allowance, retained.allowance);
    Ok(())
}

fn registry_provider_resolves_prepares_and_reports_quota(
    adapter_name: &'static str,
    account_key: &'static str,
    unknown_identity: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, CancelToken, Digest, OperationId};
    use provenant_os::exec::SystemExecutor;
    use provenant_providers::{
        Adapter, Continuation, ExitObservation, LaunchSpec, PrepareInput, ProviderError,
        ProviderEvent, ProviderOutcome, RouteProvenance, StreamState, VerifiedLineage,
    };
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    struct Fixture(&'static str);
    impl Adapter for Fixture {
        fn id(&self) -> &'static str {
            self.0
        }
        fn account(
            &self,
            _: &StreamState,
        ) -> Result<provenant_providers::AccountDetection, ProviderError> {
            Ok(provenant_providers::AccountDetection::Unknown)
        }
        fn prepare(&self, input: PrepareInput<'_>) -> Result<LaunchSpec, ProviderError> {
            if self.0 == "cursor" {
                assert!(
                    input.account.is_none(),
                    "local login must not pin an account"
                );
                return provenant_providers::cursor::Cursor.prepare(input);
            }
            assert!(input.account.is_none(), "fixture uses its local login");
            let mut command_route = input.route.clone();
            command_route.adapter = "command".into();
            command_route.model.clear();
            command_route.account_key = None;
            let argv = vec![input.executable.to_string_lossy().into_owned()];
            let command = provenant_providers::command::Invocation {
                argv: &argv,
                program: input.executable,
                runtimes: &[],
            };
            provenant_providers::command::Command.prepare(PrepareInput {
                route: &command_route,
                account: None,
                command: Some(command),
                ..input
            })
        }
        fn stream(
            &self,
            continuation: Option<&Continuation>,
            policy: &provenant_records::policy::Policy,
        ) -> Result<StreamState, ProviderError> {
            provenant_providers::command::Command.stream(continuation, policy)
        }
        fn observe(
            &self,
            state: &mut StreamState,
            bytes: &[u8],
        ) -> Result<Vec<ProviderEvent>, ProviderError> {
            provenant_providers::command::Command.observe(state, bytes)
        }
        fn finish(
            &self,
            state: StreamState,
            exit: ExitObservation,
        ) -> Result<ProviderOutcome, ProviderError> {
            provenant_providers::command::Command.finish(state, exit)
        }
        fn continuation(
            &self,
            outcome: &ProviderOutcome,
            route: &provenant_records::ResolvedRoute,
            digest: &str,
        ) -> Result<Continuation, ProviderError> {
            provenant_providers::command::Command.continuation(outcome, route, digest)
        }
        fn validate_provenance(
            &self,
            provenance: &RouteProvenance<'_>,
        ) -> Result<VerifiedLineage, ProviderError> {
            provenant_providers::command::Command.validate_provenance(provenance)
        }
    }
    impl provenant_providers::QuotaSource for Fixture {
        fn adapter_id(&self) -> &'static str {
            self.0
        }
        fn probe(
            &self,
            account: &provenant_quota::AccountScope,
            input: &provenant_quota::QuotaProbeInput,
        ) -> Result<provenant_quota::QuotaProbePlan, provenant_quota::QuotaError> {
            Ok(provenant_quota::QuotaProbePlan {
                kind: provenant_quota::QuotaEffectKind::Command,
                account: account.clone(),
                input: input.clone(),
            })
        }
        fn capture(
            &self,
            _: &provenant_providers::quota_live::LiveQuota,
            _: &provenant_quota::QuotaProbePlan,
            _: Option<&provenant_quota::AccountRecord>,
        ) -> Result<provenant_quota::ProbeCapture, provenant_quota::ProbeFailure> {
            Err(provenant_quota::ProbeFailure::Disabled)
        }
        fn decode(
            &self,
            plan: &provenant_quota::QuotaProbePlan,
            _: &provenant_quota::ProbeCapture,
        ) -> Result<provenant_quota::QuotaSnapshot, provenant_quota::QuotaError> {
            Ok(provenant_quota::QuotaSnapshot { extra: Default::default(),
                schema_version: 1,
                account: plan.account.clone(),
                revision: 0,
                status: provenant_quota::QuotaStatus::Available,
                observed_unix_seconds: plan.input.now_unix_seconds,
                source_ref: "fixture".into(),
                authority_ref: None,
                windows: vec![],
                details: None,
            })
        }
    }
    struct Effects(std::cell::Cell<u32>, &'static str, &'static str);
    impl provenant_quota::QuotaEffects for Effects {
        fn accounts(
            &self,
        ) -> Result<Vec<provenant_quota::AccountScope>, provenant_quota::QuotaError> {
            Ok(vec![provenant_quota::AccountScope { extra: Default::default(),
                adapter: self.1.into(),
                account_key: self.2.into(),
                label: self.2.into(),
            }])
        }
        fn capture(
            &self,
            _: &provenant_quota::QuotaProbePlan,
            _: Option<&provenant_quota::AccountRecord>,
        ) -> Result<provenant_quota::ProbeCapture, provenant_quota::ProbeFailure> {
            self.0.set(self.0.get() + 1);
            Ok(provenant_quota::ProbeCapture {
                stdout: vec![],
                stderr: vec![],
                exit_code: Some(0),
                overflowed: false,
            })
        }
    }
    static FIXTURE: Fixture = Fixture("fixture-provider");
    static LOCAL: Fixture = Fixture("cursor");
    let adapter = if adapter_name == LOCAL.0 {
        &LOCAL
    } else {
        &FIXTURE
    };
    let providers = [provenant_providers::Registration {
        adapter,
        quota_source: Some(adapter),
        quota: provenant_quota::QuotaPolicy {
            adapter: adapter_name,
            timer_seconds: 300,
            panel_seconds: 120,
            probe_seconds: 10,
            low_power: true,
            solo: false,
        },
        ..provenant_providers::command::REGISTRATION
    }];
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
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let effects = Effects(std::cell::Cell::new(0), adapter_name, account_key);
    let report = crate::quota::execute(
        &crate::quota::Quota::Tick {
            adapter: crate::quota::Filter {
                adapter: Some(adapter_name.into()),
            },
            trigger: crate::quota::Trigger::Manual,
            power: crate::quota::Power::Ac,
        },
        &roots,
        &providers,
        &effects,
    )?;
    assert_eq!(effects.0.get(), 1);
    assert!(
        report
            .accounts
            .iter()
            .any(|row| row.account.adapter == adapter_name
                && row.status == provenant_quota::QuotaStatus::Available)
    );
    let utility =
        provenant_confinement::UtilityRuntime::new(&git, &git_home, roots.state.as_path())?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &project,
        provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
        &cancel,
    )?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let executable = assets.join(adapter_name);
    std::fs::write(&executable, b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf x >> \"$HOME/probe-count\"; fi\nprintf 'fixture 1.0\\n'\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let attempt = state.join(format!("attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("registry-fixture")?,
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
    let prompt = "fixture prompt";
    let model = if adapter_name == "cursor" {
        "cursor/grok-4.7".to_owned()
    } else {
        format!("{adapter_name}/test")
    };
    let readiness_dir = state.join("readiness").join(account_key);
    std::fs::create_dir_all(&readiness_dir)?;
    let stale = provenant_quota::ReadinessFile { extra: Default::default(),
        schema_version: 1,
        login: None,
        records: vec![provenant_quota::RouteReadiness { extra: Default::default(),
            schema_version: 1,
            scope: provenant_quota::ReadinessScope { extra: Default::default(),
                host: provenant_quota::LOCAL_HOST.into(),
                adapter: adapter_name.into(),
                account_key: account_key.into(),
                route: model.clone(),
            },
            state: provenant_quota::ReadinessState::Ready,
            reason: None,
            observed_unix_seconds: 1,
            expires_unix_seconds: 2,
            reset_unix_seconds: None,
            evidence_ref: Some("fixture".into()),
        }],
    };
    std::fs::write(
        readiness_dir.join(format!("{adapter_name}.json")),
        serde_json::to_vec(&stale)?,
    )?;
    let mut task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        limit_extensions: Default::default(),
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model { required_family: None,
            extra: Default::default(),
            provider: adapter_name.into(),
            model,
            effort: None,
        },
        account: (!unknown_identity).then(|| account_key.into()),
        authority_digest: Digest::from_bytes([4; 32]),
        writer: None,
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
    let locate = |name: &str| {
        if name == adapter_name { Ok(executable.clone()) } else { Err("unavailable".into()) }
    };
    let alter = |records: &mut Vec<provenant_providers::AvailabilityRecord>| {
        for record in records { record.account_key = None; }
    };
    if unknown_identity {
        let mut strict = provenant_records::policy::Policy::default();
        strict.overlay(&[("QUOTA_IDENTITY_UNKNOWN".into(), provenant_records::policy::Strength::Refuse)].into(), provenant_records::policy::Source::Project);
        task.profile.launch_policy = Some(strict);
        let result = super::preparation::prepare_candidates_inner(
            &registry, &providers, &task, prompt, &paths, None, Some(&locate), None, None,
            Some(&alter),
        );
        assert!(matches!(
            result,
            Err(super::preparation::PrepareError::Candidates { skipped_routes, .. })
                if skipped_routes.iter().any(|route| route.cause == Some(provenant_records::RouteExclusionReason::UnknownAccount))
        ), "strict unknown identity must refuse preparation for that reason");
        task.profile.launch_policy = None;
    }
    let prepared = super::preparation::prepare_candidates_inner(
        &registry,
        &providers,
        &task,
        prompt,
        &paths,
        None,
        Some(&locate),
        None,
        None,
        if unknown_identity { Some(&alter) } else { None },
    )?;
    assert_eq!(prepared.route.provider.as_deref(), Some(adapter_name));
    assert_eq!(prepared.route.account_key.as_deref(), (!unknown_identity).then_some(account_key));
    assert!(!prepared.argv.is_empty());
    assert!(unknown_identity ||
        prepared
            .route
            .availability_warning
            .as_deref()
            .is_some_and(|v| v.contains("Stale"))
    );
    Ok(())
}

#[test]
fn requester_path_excludes_primary_and_linked_checkout_binaries()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CancelToken;
    use provenant_os::exec::SystemExecutor;
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project");
    let linked = temp.path().join("linked");
    let home = temp.path().join("git-home");
    let primary_bin = project.join("bin");
    let linked_bin = linked.join("bin");
    let external_bin = temp.path().join("external/bin");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&external_bin)?;
    let executor = SystemExecutor::new(2)?;
    let git = init_fixture_repository(&executor, &project, &home)?;
    let linked_text = linked.to_str().ok_or("linked worktree path")?;
    provenant_os::fixture::run_git(
        &project,
        &["worktree", "add", "-q", "-b", "linked", linked_text],
    )?;
    let nested = linked.join("src/nested");
    std::fs::create_dir_all(&primary_bin)?;
    std::fs::create_dir_all(&nested)?;
    std::fs::create_dir_all(&linked_bin)?;
    let primary_executable = primary_bin.join("primary-provider");
    let linked_executable = linked_bin.join("linked-provider");
    let safe_executable = external_bin.join("safe-provider");
    for executable in [&primary_executable, &linked_executable, &safe_executable] {
        std::fs::write(executable, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))?;
    }

    let path = std::env::join_paths([
        primary_bin.as_os_str(),
        linked_bin.as_os_str(),
        external_bin.as_os_str(),
    ])?;
    let mut host = run::HostLaunchContext::from_environment(&home, &nested, git.clone());
    host.lane_path = Some(path.as_os_str().as_bytes().to_vec());
    let filtered = crate::run::filtered_requester_lane_path(
        Some(&host),
        &nested,
        &nested,
        &provenant_confinement::UtilityRuntime::read_only_without_host_state(&git)?,
        &executor,
        &CancelToken::new(),
        None,
    )?;
    assert!(filtered.resolve("primary-provider").is_err());
    assert!(filtered.resolve("linked-provider").is_err());
    assert_eq!(filtered.resolve("safe-provider")?, safe_executable);
    Ok(())
}

// Spec: docs/specs/run-git.md#project-layout-ownership
#[test]
fn requester_path_uses_registered_primary_with_external_git_dir()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CancelToken;
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    use std::time::Duration;

    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("repo/project");
    let common = root.join("metadata/external.git");
    let linked = root.join("repo/linked");
    let home = root.join("git-home");
    let state = root.join("state");
    let primary_bin = project.join("bin");
    let linked_bin = linked.join("bin");
    let external_bin = root.join("external/bin");
    for path in [
        &project,
        common.parent().ok_or("Git metadata parent")?,
        &home,
        &external_bin,
    ] {
        std::fs::create_dir_all(path)?;
    }
    provenant_os::fixture::private_state_dir(&state)?;
    let git = provenant_os::git::git_executable()
        .ok_or("git unavailable")?
        .canonicalize()?;
    let executor = SystemExecutor::new(2)?;
    let init_argv = [
        git.as_os_str().to_owned(),
        "init".into(),
        "-q".into(),
        "-b".into(),
        "main".into(),
        "--separate-git-dir".into(),
        common.as_os_str().to_owned(),
        project.as_os_str().to_owned(),
    ];
    let mut environment = vec![
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    environment.extend(provenant_os::fixture::fixture_git_settings());
    let mut options = ExecOptions::new(&init_argv, &root, Duration::from_secs(5));
    options.env = Some(&environment);
    assert_eq!(executor.execute(options)?.termination, Termination::Exit(0));
    let commit_argv = [
        git.as_os_str().to_owned(),
        "-C".into(),
        project.as_os_str().to_owned(),
        "commit".into(),
        "-q".into(),
        "--allow-empty".into(),
        "-m".into(),
        "Initial".into(),
    ];
    let mut options = ExecOptions::new(&commit_argv, &root, Duration::from_secs(5));
    options.env = Some(&environment);
    assert_eq!(executor.execute(options)?.termination, Termination::Exit(0));
    let linked_text = linked.to_str().ok_or("linked worktree path")?;
    provenant_os::fixture::run_git(
        &project,
        &["worktree", "add", "-q", "-b", "linked", linked_text],
    )?;
    let nested = linked.join("src/nested");
    std::fs::create_dir_all(&primary_bin)?;
    std::fs::create_dir_all(&nested)?;
    std::fs::create_dir_all(&linked_bin)?;
    let primary_executable = primary_bin.join("primary-provider");
    let linked_executable = linked_bin.join("linked-provider");
    let safe_executable = external_bin.join("safe-provider");
    for executable in [&primary_executable, &linked_executable, &safe_executable] {
        std::fs::write(executable, "#!/bin/sh\nexit 0\n")?;
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700))?;
    }

    let mut roots = provenant_core::RootInputs::new(home.clone(), project.clone());
    roots.state_dir = Some(state.clone());
    let roots = roots.resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = provenant_confinement::UtilityRuntime::read_only(&git, &state)?;
    let discovered = utility.project_location(
        &executor,
        &project,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        &CancelToken::new(),
    )?;
    let binding = provenant_config::bind_project(&roots, discovered.location)?;
    let path = std::env::join_paths([
        primary_bin.as_os_str(),
        linked_bin.as_os_str(),
        external_bin.as_os_str(),
    ])?;
    let mut host = run::HostLaunchContext::from_environment(&home, &nested, git.clone());
    host.lane_path = Some(path.as_os_str().as_bytes().to_vec());
    let filtered = crate::run::filtered_requester_lane_path(
        Some(&host),
        &nested,
        &nested,
        &utility,
        &executor,
        &CancelToken::new(),
        Some(&binding),
    )?;
    assert!(filtered.resolve("primary-provider").is_err());
    assert!(filtered.resolve("linked-provider").is_err());
    assert_eq!(filtered.resolve("safe-provider")?, safe_executable);
    Ok(())
}

#[test]
fn opencode_stale_auth_prepares_with_an_unverified_warning()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, CancelToken, Clock, Digest, OperationId};
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };
    const CHILD: &str = "PROVENANT_TEST_OPENCODE_AUTH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let argv = [
            std::env::current_exe()?.into_os_string(),
            "--exact".into(),
            "run::provider::tests::opencode_stale_auth_prepares_with_an_unverified_warning".into(),
            "--nocapture".into(),
        ];
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| {
                !matches!(
                    key.to_str(),
                    Some(
                        "XDG_CONFIG_HOME"
                            | "XDG_DATA_HOME"
                            | "XDG_CACHE_HOME"
                            | "XDG_STATE_HOME"
                            | "OPENCODE_DB"
                            | "OPENCODE_CONFIG"
                            | "OPENCODE_CONFIG_DIR"
                            | "OPENCODE_CONFIG_CONTENT"
                    )
                ) && key != provenant_core::RUN_STATE_DIRECTORY_ENV
                    && key != CHILD
            })
            .collect();
        env.push((CHILD.into(), "1".into()));
        let cwd = std::env::current_dir()?;
        let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(60));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Capture { limit: 65536 };
        let out = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        assert!(!out.timed_out, "{out:?}");
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
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility =
        provenant_confinement::UtilityRuntime::new(&git, &git_home, roots.state.as_path())?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &project,
        provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
        &cancel,
    )?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let executable = assets.join("opencode/bin/opencode");
    std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
    std::fs::write(&executable, b"#!/bin/sh\nprintf 'opencode 2.2.0\\n'\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let now = provenant_os::clock::SystemClock::new().unix_seconds();
    let account = provenant_quota::AccountRecord::empty(provenant_quota::AccountScope { extra: Default::default(),
        adapter: "opencode".into(),
        account_key: "go".into(),
        label: "go".into(),
    });
    let mut readiness = provenant_quota::ReadinessFile::default();
    readiness.sync_probe_result(
        &account,
        provenant_quota::LOCAL_HOST,
        now - 301,
        300,
        Some(provenant_quota::ReadinessReason::SignedOut),
    )?;
    let readiness_dir = state.join("readiness/go");
    std::fs::create_dir_all(&readiness_dir)?;
    for dir in [state.join("readiness"), readiness_dir.clone()] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let readiness_path = readiness_dir.join("opencode.json");
    std::fs::write(&readiness_path, serde_json::to_vec(&readiness)?)?;
    std::fs::set_permissions(&readiness_path, std::fs::Permissions::from_mode(0o600))?;
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let attempt = state.join(format!("attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("opencode-auth-fixture")?,
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
    let prompt = "fake OpenCode login recovery";
    let task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        limit_extensions: Default::default(),
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model { required_family: None,
            extra: Default::default(),
            provider: "opencode".into(),
            model: "opencode/opencode-go/deepseek-v4.1-flash".into(),
            effort: None,
        },
        account: Some("go".into()),
        authority_digest: Digest::from_bytes([4; 32]),
        writer: None,
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
    let prepare = || prepare_with(
        &registry,
        &task,
        prompt,
        &paths,
        None,
        |name| {
            if name == "opencode" {
                Ok(executable.clone())
            } else {
                Err("fixture provider unavailable".into())
            }
        },
        None,
    );
    let refused = prepare().err().ok_or("expired sign-out was accepted")?;
    assert!(matches!(
        refused,
        super::preparation::PrepareError::Candidates { skipped_routes, .. }
            if skipped_routes.iter().any(|route| route.cause == Some(provenant_records::RouteExclusionReason::AuthRequired))
    ));
    std::fs::remove_file(readiness_path)?;
    let prepared = prepare()?;
    assert_eq!(prepared.route.provider.as_deref(), Some("opencode"));
    assert_eq!(prepared.route.account_key.as_deref(), Some("go"));
    assert!(prepared.route.launch_warnings.iter().any(|warning| {
        warning.contains("login status is unverified")
    }));
    Ok(())
}

/// Re-run the named test in a child with the host's OpenCode environment
/// removed, so the walk sees only the fixture accounts. Returns true in the
/// parent once the child has passed.
fn rerun_in_clean_environment(test: &str) -> Result<bool, Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    const CHILD: &str = "PROVENANT_TEST_EXHAUSTED_WALK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return Ok(false);
    }
    let argv = [
        std::env::current_exe()?.into_os_string(),
        "--exact".into(),
        format!("run::provider::tests::{test}").into(),
        "--nocapture".into(),
    ];
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| {
            !matches!(
                key.to_str(),
                Some(
                    "XDG_CONFIG_HOME"
                        | "XDG_DATA_HOME"
                        | "XDG_CACHE_HOME"
                        | "XDG_STATE_HOME"
                        | "OPENCODE_DB"
                        | "OPENCODE_CONFIG"
                        | "OPENCODE_CONFIG_DIR"
                        | "OPENCODE_CONFIG_CONTENT"
                )
            ) && key != provenant_core::RUN_STATE_DIRECTORY_ENV
        })
        .collect();
    env.push((CHILD.into(), "1".into()));
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, std::time::Duration::from_secs(60));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65536 };
    options.stderr = StreamPolicy::Capture { limit: 65536 };
    let out = SystemExecutor::new(1)?.execute(options)?;
    assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
    assert!(!out.timed_out, "{out:?}");
    Ok(true)
}

/// Prepare one unpinned (or pinned) OpenCode Go launch over three registered
/// accounts, `spent` carrying advisory local-ledger usage estimates.
fn prepare_with_partial_account_estimates(
    spent: &[&str],
    pin: Option<&str>,
) -> Result<Result<run::PreparedLaunch, PrepareError>, Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, CancelToken, Clock, Digest, OperationId};
    use provenant_os::exec::SystemExecutor;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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
    let executor = SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    // Local-ledger estimates remain advisory, including at the reported limit.
    let mut config = String::from("schema_version=1\n");
    for label in ["ready", "spent-a", "spent-b"] {
        let directory = home.join(format!("opencode-{label}"));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        config.push_str(&format!(
            "[domains.accounts.entries.{label}]\nadapter='opencode'\nlabel='{label}'\nkey='{label}'\ndirectory='{}'\n",
            directory.display()
        ));
    }
    std::fs::write(state.join("config.toml"), config)?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let now = provenant_os::clock::SystemClock::new().unix_seconds();
    // Keep readiness positive so this invariant isolates advisory local usage
    // estimates from the independent account-login admission boundary.
    let readiness_root = state.join("readiness");
    std::fs::DirBuilder::new().mode(0o700).create(&readiness_root)?;
    for key in ["ready", "spent-a", "spent-b"] {
        let directory = readiness_root.join(key);
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let readiness_path = directory.join("opencode.json");
        let readiness = provenant_quota::ReadinessFile { extra: Default::default(),
            schema_version: 1,
            login: None,
            records: vec![provenant_quota::RouteReadiness { extra: Default::default(),
                schema_version: 1,
                scope: provenant_quota::ReadinessScope { extra: Default::default(),
                    host: provenant_quota::LOCAL_HOST.into(),
                    adapter: "opencode".into(),
                    account_key: key.into(),
                    route: "opencode/opencode-go/deepseek-v4.1-flash".into(),
                },
                state: provenant_quota::ReadinessState::Ready,
                reason: None,
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
                reset_unix_seconds: None,
                evidence_ref: Some("fixture".into()),
            }],
        };
        std::fs::write(&readiness_path, serde_json::to_vec(&readiness)?)?;
        std::fs::set_permissions(
            &readiness_path,
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    let quota = state.join("quota");
    std::fs::DirBuilder::new().mode(0o700).create(&quota)?;
    for key in spent {
        let account = provenant_quota::AccountScope { extra: Default::default(),
            adapter: "opencode".into(),
            account_key: (*key).into(),
            label: (*key).into(),
        };
        let mut record = provenant_quota::AccountRecord::empty(account.clone());
        record.snapshot = Some(provenant_quota::QuotaSnapshot { extra: Default::default(),
            schema_version: 1,
            account,
            revision: 0,
            status: provenant_quota::QuotaStatus::Available,
            observed_unix_seconds: now,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: Some(provenant_quota::QuotaDetails {
                coverage: Some("partial".into()),
                ..Default::default()
            }),
            windows: vec![provenant_quota::QuotaWindow { extra: Default::default(),
                pool: "five_hour".into(),
                window_seconds: None,
                used_basis_points: 10000,
                resets_unix_seconds: None,
            }],
        });
        let directory = quota.join(key);
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let path = directory.join("opencode.json");
        std::fs::write(&path, serde_json::to_vec(&record)?)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let utility =
        provenant_confinement::UtilityRuntime::new(&git, &git_home, roots.state.as_path())?;
    let cancel = CancelToken::new();
    let repo = utility.open_repo(
        &executor,
        &project,
        provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
        &cancel,
    )?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let executable = assets.join("opencode/bin/opencode");
    std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
    std::fs::write(&executable, b"#!/bin/sh\nprintf 'opencode 2.2.0\\n'\n")?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let attempt = state.join(format!("attempts/{attempt_id}"));
    let paths = run::LaunchPaths {
        attempt_id,
        operation_id: OperationId::parse("exhausted-walk-fixture")?,
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
    let prompt = "exhausted account walk";
    let task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        limit_extensions: Default::default(),
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model { required_family: None,
            extra: Default::default(),
            provider: "opencode".into(),
            model: "opencode/opencode-go/deepseek-v4.1-flash".into(),
            effort: None,
        },
        account: pin.map(Into::into),
        authority_digest: Digest::from_bytes([4; 32]),
        writer: None,
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
    Ok(prepare_with(
        &registry,
        &task,
        prompt,
        &paths,
        None,
        |name| {
            if name == "opencode" {
                Ok(executable.clone())
            } else {
                Err("fixture provider unavailable".into())
            }
        },
        None,
    ))
}

#[test]
fn partial_account_estimates_allow_launch_without_swapping_a_pinned_account()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_in_clean_environment(
        "partial_account_estimates_allow_launch_without_swapping_a_pinned_account",
    )? {
        return Ok(());
    }
    for (at_limit, pin) in [
        (&["spent-a"][..], None),
        (&["ready", "spent-a", "spent-b"][..], None),
        (&["spent-a"][..], Some("spent-a")),
    ] {
        let prepared = prepare_with_partial_account_estimates(at_limit, pin)?
            .map_err(|error| format!("{error}"))?;
        if let Some(pin) = pin {
            assert_eq!(prepared.route.account_key.as_deref(), Some(pin));
        }
        assert!(prepared.skipped_routes.iter().all(|route| {
            route.cause != Some(provenant_records::RouteExclusionReason::Exhausted)
        }));
    }
    Ok(())
}
