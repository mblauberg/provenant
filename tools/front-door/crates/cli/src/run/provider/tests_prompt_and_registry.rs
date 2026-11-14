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
            build_heavy: false,
            title: None,
            base: None,
            head: None,
            source_head: None,
            base_ref: None,
            profile: Default::default(),
            prompt_digest: provenant_records::CanonicalV1::file_digest(full.as_bytes()),
            route: run::RequestedRoute::Model {
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
                assert!(error.to_string().contains("host configuration"), "{error}");
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
    let executable = temp.path().join("cursor-agent");
    let host = run::HostLaunchContext {
        graph_directory: None,
        configuration_root: temp.path().into(),
        cwd: temp.path().into(),
        executables: Default::default(),
        executable_search_path: vec![temp.path().into()],
        git: "/usr/bin/git".into(),
        default_account_directories: Default::default(),
        credential_roots: Default::default(),
        github_roots: Default::default(),
        provider_path_environment: Default::default(),
        cargo_build_jobs: None,
        #[cfg(feature = "fixture")]
        fixture_state_write: false,
    };
    assert!(host_executable(&host, "cursor").is_err());
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
            host_executable(&host, "cursor")?,
            executable.canonicalize()?
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
    registry_provider_resolves_prepares_and_reports_quota("fixture-provider", "work")
}

#[test]
fn registry_spec_drives_local_provider_preparation_and_quota()
-> Result<(), Box<dyn std::error::Error>> {
    registry_provider_resolves_prepares_and_reports_quota("cursor", "local")
}

fn registry_provider_resolves_prepares_and_reports_quota(
    adapter_name: &'static str,
    account_key: &'static str,
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
        ) -> Result<StreamState, ProviderError> {
            provenant_providers::command::Command.stream(continuation)
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
    impl provenant_quota::QuotaSource for Fixture {
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
        fn decode(
            &self,
            plan: &provenant_quota::QuotaProbePlan,
            _: &provenant_quota::ProbeCapture,
        ) -> Result<provenant_quota::QuotaSnapshot, provenant_quota::QuotaError> {
            Ok(provenant_quota::QuotaSnapshot {
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
            Ok(vec![provenant_quota::AccountScope {
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
    let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
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
    let stale = provenant_quota::ReadinessFile {
        schema_version: 1,
        login: None,
        records: vec![provenant_quota::RouteReadiness {
            schema_version: 1,
            scope: provenant_quota::ReadinessScope {
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
    let task = run::TaskRequest {
        context: None,
        extra: Default::default(),
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
            extra: Default::default(),
            provider: adapter_name.into(),
            model,
            effort: None,
        },
        account: Some(account_key.into()),
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
    let prepared = super::preparation::prepare_candidates(
        &registry,
        &providers,
        &task,
        prompt,
        &paths,
        None,
        &|name| {
            if name == adapter_name {
                Ok(executable.clone())
            } else {
                Err("unavailable".into())
            }
        },
        None,
        None,
    )?;
    assert_eq!(prepared.route.provider.as_deref(), Some(adapter_name));
    assert_eq!(prepared.route.account_key.as_deref(), Some(account_key));
    assert!(!prepared.argv.is_empty());
    assert!(
        prepared
            .route
            .extra
            .get("availability_warning")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v.contains("Stale"))
    );
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
        assert!(
            String::from_utf8_lossy(&out.stderr.bytes)
                .contains("OpenCode login status is unverified")
        );
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
    let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
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
    let account = provenant_quota::AccountRecord::empty(provenant_quota::AccountScope {
        adapter: "opencode".into(),
        account_key: "go".into(),
        label: "go".into(),
    });
    let mut readiness = provenant_quota::ReadinessFile::default();
    readiness.sync_probe_result(
        &account,
        provenant_quota::LOCAL_HOST,
        now - 1,
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
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
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
    let prepared = prepare_with(
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
    )?;
    assert_eq!(prepared.route.provider.as_deref(), Some("opencode"));
    assert_eq!(prepared.route.account_key.as_deref(), Some("go"));
    // Readiness remains recorded; only the current launch treats it as unknown.
    let scope = provenant_quota::ReadinessScope {
        host: provenant_quota::LOCAL_HOST.into(),
        adapter: "opencode".into(),
        account_key: "go".into(),
        route: "opencode/opencode-go/deepseek-v4.1-flash".into(),
    };
    assert_eq!(
        crate::quota::read_readiness(&state, &scope, now).reason,
        Some(provenant_quota::ReadinessReason::SignedOut)
    );
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

/// Prepare one unpinned (or pinned) OpenCode Go launch over two registered
/// accounts, `spent` being recorded with a full allowance window.
fn prepare_over_two_accounts(
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
    // `ready` registers first and sorts first. Pacing scores each spent account
    // 0, so an unpinned lane draws an unspent account before any spent one.
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
    let quota = state.join("quota");
    std::fs::DirBuilder::new().mode(0o700).create(&quota)?;
    for key in spent {
        let account = provenant_quota::AccountScope {
            adapter: "opencode".into(),
            account_key: (*key).into(),
            label: (*key).into(),
        };
        let mut record = provenant_quota::AccountRecord::empty(account.clone());
        record.snapshot = Some(provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: provenant_quota::QuotaStatus::Available,
            observed_unix_seconds: now,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![provenant_quota::QuotaWindow {
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
    let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
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
        build_heavy: false,
        title: None,
        base: None,
        head: None,
        source_head: None,
        base_ref: None,
        profile: Default::default(),
        prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
        route: run::RequestedRoute::Model {
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
fn an_exhausted_account_stands_by_while_an_unspent_one_remains()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_in_clean_environment("an_exhausted_account_stands_by_while_an_unspent_one_remains")? {
        return Ok(());
    }
    let prepared =
        prepare_over_two_accounts(&["spent-a"], None)?.map_err(|error| format!("{error}"))?;
    // Pacing scores the spent account 0, so the walk never draws it while an
    // unprobed account remains. Either unprobed account is a valid pick; the
    // router breaks the tie between them, so the test does not pin which one.
    assert!(
        matches!(
            prepared.route.account_key.as_deref(),
            Some("ready" | "spent-b")
        ),
        "{:?}",
        prepared.route.account_key
    );
    assert!(
        !prepared
            .skipped_routes
            .iter()
            .any(|skipped| skipped.detail.contains("account spent-a")),
        "{:?}",
        prepared.skipped_routes
    );
    Ok(())
}

#[test]
fn every_refused_account_is_named_when_the_lane_is_usage_limited()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_in_clean_environment("every_refused_account_is_named_when_the_lane_is_usage_limited")?
    {
        return Ok(());
    }
    let refused = prepare_over_two_accounts(&["ready", "spent-a", "spent-b"], None)?;
    let Err(PrepareError::Candidates {
        fallback_outcome,
        message,
        ..
    }) = refused
    else {
        return Err("every account is spent, so no launch is prepared".into());
    };
    assert_eq!(
        fallback_outcome,
        Some(provenant_records::run::PreparationFailure::UsageLimited)
    );
    // The message lists each refused account as `accounts skipped: <label>: …`.
    for label in ["ready", "spent-a", "spent-b"] {
        assert!(message.contains(&format!("{label}: ")), "{message}");
    }
    Ok(())
}

#[test]
fn a_pinned_account_is_not_swapped_when_its_allowance_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    if rerun_in_clean_environment("a_pinned_account_is_not_swapped_when_its_allowance_is_refused")?
    {
        return Ok(());
    }
    match prepare_over_two_accounts(&["spent-a"], Some("spent-a"))? {
        Ok(prepared) => Err(format!(
            "the pinned account is spent, yet the lane ran on {:?}",
            prepared.route.account_key
        )
        .into()),
        Err(PrepareError::Candidates { message, .. }) => {
            assert!(message.contains("allowance is exhausted"), "{message}");
            assert!(!message.contains("account ready"), "{message}");
            Ok(())
        }
        Err(other) => Err(format!("{other}").into()),
    }
}
