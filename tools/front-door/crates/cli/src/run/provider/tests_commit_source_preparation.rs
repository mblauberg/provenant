struct CommitSourcePreparation {
    _temporary: tempfile::TempDir,
    roots: provenant_core::Roots,
    project: PathBuf,
    home: PathBuf,
    state: PathBuf,
    assets: PathBuf,
    executable: PathBuf,
    project_id: ProjectId,
    task: run::TaskRequest,
    paths: run::LaunchPaths,
}

impl CommitSourcePreparation {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        use provenant_core::{AttemptId, Digest, OperationId};
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let temporary = tempfile::tempdir()?;
        let base = temporary.path().canonicalize()?;
        let home = base.join("home");
        let state = base.join("state");
        let project = base.join("repo");
        let assets = base.join("release");
        let git_home = base.join("git-home");
        for path in [&home, &state, &project, &assets, &git_home] {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let executor = provenant_os::exec::SystemExecutor::new(1)?;
        let git = init_fixture_repository(&executor, &project, &git_home)?;
        std::fs::write(project.join("subject.txt"), "pinned subject\n")?;
        provenant_os::fixture::run_git(&project, &["add", "subject.txt"])?;
        provenant_os::fixture::run_git(&project, &["commit", "-qm", "subject"])?;
        let head = run::resolve_git_base(&git, &project, "HEAD")?;
        // Preparation must use the pinned tree even when the live source differs.
        std::fs::write(project.join("subject.txt"), "uncommitted source\n")?;
        let roots = provenant_core::RootInputs {
            state_dir: Some(state.clone()),
            ..provenant_core::RootInputs::new(home.clone(), project.clone())
        }
        .resolve(&provenant_os::fs::SystemPathResolver)?;
        let utility =
            provenant_confinement::UtilityRuntime::new(&git, &git_home, roots.state.as_path())?;
        let repo = utility.open_repo(
            &executor,
            &project,
            provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?,
            &provenant_core::CancelToken::new(),
        )?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(repo.common_git_dir())?,
        )?;
        let executable = assets.join("cursor/bin/agent");
        std::fs::create_dir_all(executable.parent().ok_or("provider parent")?)?;
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nif [ \"$1\" = status ]; then printf '%s' '{}'; else printf 'fixture-cursor 1.0\\n'; fi\n",
                include_str!("../../../../../tests/fixtures/providers/cursor/status.stdout")
            ),
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let attempt_id = AttemptId::from_bytes([72; 16]);
        let attempt = state.join(format!("attempts/{attempt_id}"));
        let paths = run::LaunchPaths {
            attempt_id,
            operation_id: OperationId::parse("commit-source-preparation")?,
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
        let task = run::TaskRequest {
            limit_extensions: Default::default(),
            context: None,
            extra: Default::default(),
            build_heavy: false,
            title: None,
            base: Some(head.clone()),
            fixture: None,
            head: Some(head),
            source_head: None,
            base_ref: None,
            profile: provenant_records::run::HarnessProfile {
                knobs: [("lanes.access".into(), serde_json::json!("isolated"))].into(),
                knob_sources: [("lanes.access".into(), provenant_records::Layer::Launch)].into(),
                ..Default::default()
            },
            prompt_digest: provenant_records::CanonicalV1::file_digest(b"review fixture"),
            route: run::RequestedRoute::Model {
                required_family: None,
                extra: Default::default(),
                provider: "cursor".into(),
                model: "cursor/composer-2.5".into(),
                effort: None,
            },
            account: None,
            authority_digest: Digest::from_bytes([74; 32]),
            writer: None,
            deliver: None,
            provider_timeout_seconds: 20,
            delivery_id: None,
            access: run::LaneAccessRequest {
                source_root: Some(project.clone()),
                source_identity: Some(source_identity(&project)?),
                ..Default::default()
            },
        };
        Ok(Self {
            _temporary: temporary,
            roots,
            project,
            home,
            state,
            assets,
            executable,
            project_id: binding.project_id(),
            task,
            paths,
        })
    }

    fn registry(&self) -> Registry<'_> {
        Registry {
            roots: Some(&self.roots),
            home: Some(&self.home),
            assets: Some(&self.assets),
            cwd: Some(&self.project),
            state_root: Some(&self.state),
            project_id: Some(self.project_id),
            host: None,
            cancel: None,
            repo_facts: Default::default(),
        }
    }

    fn prepare(&self) -> Result<run::PreparedLaunch, PrepareError> {
        prepare_with(
            &self.registry(),
            &self.task,
            "review fixture",
            &self.paths,
            None,
            |name| {
                if name == "cursor" {
                    Ok(self.executable.clone())
                } else {
                    Err("fixture provider unavailable".into())
                }
            },
            None,
        )
    }
}

#[test]
fn commit_source_preparation_grants_an_immutable_materialised_view()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = CommitSourcePreparation::new()?;
    std::fs::write(
        fixture.state.join("config.toml"),
        "schema_version=1\n[[domains.routing.pools.senior]]\nmodel='cursor/composer-2.5'\nweight=1\nefforts=[]\n",
    )?;
    fixture.task.route = run::RequestedRoute::Review {
        required_family: None,
        extra: Default::default(),
        name: "senior".into(),
        effort: None,
        excluded_families: Vec::new(),
        distinct_families: true,
        preferred_families: Vec::new(),
        require_distinct: false,
    };
    let launch = fixture.prepare()?;
    assert!(launch.cwd.starts_with(&fixture.paths.temporary));
    assert_ne!(launch.cwd, fixture.project);
    assert_eq!(
        std::fs::read(launch.cwd.join("subject.txt"))?,
        b"pinned subject\n"
    );
    let confinement = launch.confinement.as_ref().ok_or("missing confinement")?;
    let permissions = confinement.plan.permissions();
    assert!(
        permissions
            .policy
            .as_ref()
            .ok_or("missing policy")?
            .mandatory()
            .state_reads()
            .iter()
            .any(|grant| grant.path == launch.cwd && grant.scope == PathScope::Subtree)
    );
    assert!(
        !permissions
            .read_roots
            .iter()
            .any(|grant| grant.path == fixture.project)
    );
    let private = permissions
        .private_state
        .as_ref()
        .ok_or("missing private state")?;
    let prompt = launch
        .argv
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let stdin = String::from_utf8_lossy(&launch.stdin);
    // Settlement now travels through the typed declaration channel: the brief
    // teaches `provenant declare --stdin` instead of naming a handoff file in
    // the host-owned private state.
    assert!(
        prompt.contains("provenant declare --stdin") || stdin.contains("provenant declare --stdin")
    );
    let policy = provenant_confinement::permissions::snapshot::render(
        permissions,
        &confinement.profile,
        &fixture.home,
    )?;
    assert!(policy.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        private.path.display()
    )));
    let denial = policy
        .lines()
        .find(|line| {
            line.starts_with("(deny file-write* (require-all")
                && line.contains(fixture.state.to_str().unwrap())
        })
        .ok_or("state denial")?;
    assert!(denial.contains(&format!(
        "(require-not (subpath \"{}\"))",
        private.path.display()
    )));
    assert!(policy.contains(&format!(
        "(deny file-write* (subpath \"{}\"))",
        launch.cwd.display()
    )));
    let writes = provenant_confinement::write_grants(permissions, &confinement.profile);
    source_tree::ensure_view_read_only(&launch.cwd, &writes)?;
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#inputs-and-resources
fn review_lane_working_directory_is_the_pinned_head_not_the_primary()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = CommitSourcePreparation::new()?;
    // Advance the primary after the pin was resolved. A lane that reads the
    // primary, or checks the requested head out there, sees this later tree.
    std::fs::write(fixture.project.join("subject.txt"), "primary moved on\n")?;
    provenant_os::fixture::run_git(&fixture.project, &["add", "subject.txt"])?;
    provenant_os::fixture::run_git(&fixture.project, &["commit", "-qm", "primary moved"])?;
    fixture.task.route = run::RequestedRoute::Review {
        required_family: None,
        extra: Default::default(),
        name: "senior".into(),
        effort: None,
        excluded_families: Vec::new(),
        distinct_families: true,
        preferred_families: Vec::new(),
        require_distinct: false,
    };
    let launch = fixture.prepare()?;
    let pinned = fixture.task.head.as_deref().ok_or("pinned review head")?;
    assert_ne!(
        launch.cwd, fixture.project,
        "a pinned review lane must not run in the primary checkout"
    );
    assert!(
        launch.cwd.starts_with(&fixture.paths.temporary),
        "a pinned review lane must run in its attempt-private view"
    );
    assert_eq!(
        std::fs::read(launch.cwd.join("subject.txt"))?,
        b"pinned subject\n",
        "the view must hold the requested head, not the primary's later tree"
    );
    let view_head = std::fs::read_to_string(launch.cwd.join(".git").join("HEAD"))?;
    assert_eq!(
        view_head.trim(),
        pinned,
        "the view must be detached at the requested head"
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#inputs-and-resources
fn pinned_read_only_lane_reads_the_recorded_head_under_host_access()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = CommitSourcePreparation::new()?;
    // `--source default_branch` records this newer head as the pin even when
    // the host checkout is behind it. Advance the tree the pin names, then
    // leave a different, uncommitted tree in the live checkout.
    std::fs::write(fixture.project.join("subject.txt"), "pinned version\n")?;
    provenant_os::fixture::run_git(&fixture.project, &["add", "subject.txt"])?;
    provenant_os::fixture::run_git(&fixture.project, &["commit", "-qm", "default branch"])?;
    let pinned = String::from_utf8(provenant_os::fixture::run_git(
        &fixture.project,
        &["rev-parse", "HEAD"],
    )?)?
    .trim()
    .to_owned();
    fixture.task.head = Some(pinned.clone());
    fixture.task.base = Some(pinned.clone());
    // Host access seals no source root; the recorded pin alone must select the
    // tree. A live child-directory grant must not override it either.
    fixture.task.access.source_root = None;
    fixture.task.access.source_identity = None;
    fixture.task.access.source_live = false;
    fixture
        .task
        .profile
        .knobs
        .insert("lanes.access".into(), serde_json::json!("host"));
    std::fs::write(
        fixture.project.join("subject.txt"),
        "live checkout version\n",
    )?;

    let launch = fixture.prepare()?;
    assert_ne!(
        launch.cwd, fixture.project,
        "a pinned reader must not run in the live checkout"
    );
    assert!(
        launch.cwd.starts_with(&fixture.paths.temporary),
        "a pinned reader must run in its attempt-private view"
    );
    assert_eq!(
        std::fs::read(launch.cwd.join("subject.txt"))?,
        b"pinned version\n",
        "the view must hold the recorded head, not the live checkout"
    );
    let view_head = std::fs::read_to_string(launch.cwd.join(".git").join("HEAD"))?;
    assert_eq!(
        view_head.trim(),
        pinned,
        "the view must be detached at the recorded head"
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#inputs-and-resources
fn review_view_ships_a_named_ref_at_the_host_pinned_base() -> Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = CommitSourcePreparation::new()?;
    // The fixture checkout has no remote default ref. Pin a base and a later
    // head so the two are distinguishable inside the detached view.
    let base = fixture.task.base.clone().ok_or("fixture review base")?;
    std::fs::write(fixture.project.join("reviewed.txt"), "reviewed\n")?;
    provenant_os::fixture::run_git(&fixture.project, &["add", "reviewed.txt"])?;
    provenant_os::fixture::run_git(&fixture.project, &["commit", "-qm", "reviewed"])?;
    let head = String::from_utf8(provenant_os::fixture::run_git(
        &fixture.project,
        &["rev-parse", "HEAD"],
    )?)?;
    fixture.task.base = Some(base.clone());
    fixture.task.head = Some(head.trim().to_owned());
    fixture.task.route = run::RequestedRoute::Review {
        required_family: None,
        extra: Default::default(),
        name: "senior".into(),
        effort: None,
        excluded_families: Vec::new(),
        distinct_families: true,
        preferred_families: Vec::new(),
        require_distinct: false,
    };
    let launch = fixture.prepare()?;
    let git_dir = launch.cwd.join(".git");
    assert!(
        !git_dir.join("refs/remotes/origin/HEAD").exists(),
        "the review view must not rely on a remote default ref"
    );
    let shipped = std::fs::read_to_string(git_dir.join("refs/provenant/review/base"))?;
    assert_eq!(
        shipped.trim(),
        base.as_str(),
        "the view must ship the pinned base ref"
    );
    Ok(())
}

#[test]
fn host_denied_handoff_remains_best_effort() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = CommitSourcePreparation::new()?;
    std::fs::write(
        fixture.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.run.host_policy]\ndeny_write=[{:?}]\n",
            fixture.state.to_str().ok_or("state path")?
        ),
    )?;
    let launch = fixture.prepare()?;
    let prompt = launch
        .argv
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let permissions = launch
        .confinement
        .as_ref()
        .ok_or("missing confinement")?
        .plan
        .permissions();
    assert!(permissions.private_state.is_some());
    // The host denial is best effort: even when writes to state are denied,
    // the brief still teaches the typed declaration the lane settles with.
    assert!(
        prompt.contains("provenant declare --stdin")
            || String::from_utf8_lossy(&launch.stdin).contains("provenant declare --stdin")
    );
    Ok(())
}

#[test]
fn commit_source_preparation_refusal_retains_preselection_permission_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::policy::Boundary;
    for (plan_error, boundary) in [
        ("credential_root", Boundary::Credentials),
        ("resolver", Boundary::Credentials),
    ] {
        let mut fixture = CommitSourcePreparation::new()?;
        // Preflight uses host access before a model-specific access policy is
        // selected. A protected state overlap is refused at that stage; a
        // credential overlap is refused only for the selected isolated route.
        let denied_root = if plan_error == "resolver" {
            fixture.state.clone()
        } else {
            fixture.home.join(".ssh")
        };
        fixture.task.access.write_roots.push(denied_root);
        if plan_error == "resolver" {
            std::fs::write(
                fixture.state.join("config.toml"),
                "schema_version=1\n[[domains.routing.pools.preparation-fixture]]\nmodel='cursor/composer-2.5'\nweight=1\nefforts=[]\n",
            )?;
            fixture.task.route = run::RequestedRoute::Review {
                required_family: None,
                extra: Default::default(),
                name: "preparation-fixture".into(),
                effort: None,
                // Cursor's unspecified family is excluded whenever a review
                // requires separation from an already-used family.
                excluded_families: vec!["openai".into()],
                distinct_families: true,
                preferred_families: Vec::new(),
                require_distinct: true,
            };
        }
        let error = match fixture.prepare() {
            Ok(_) => panic!("{plan_error} must be refused"),
            Err(error) => error,
        };
        let refusal = error.into_refusal();
        if plan_error == "resolver" {
            assert_eq!(
                refusal.cause,
                provenant_records::run::CauseKind::RouteResolutionRefused,
            );
            assert_eq!(
                refusal.terminal_note.as_deref(),
                Some("route resolver classification: family_separation"),
            );
            // A denied adapter check cannot reclassify a separate router
            // error; its preselection diagnostic remains attached.
            assert_eq!(refusal.boundary_class, Boundary::None);
        } else {
            assert_eq!(refusal.boundary_class, boundary);
        }
        if plan_error == "resolver" {
            let skipped = refusal
                .skipped_routes
                .iter()
                .find(|route| route.route.starts_with("cursor"))
                .ok_or("route refusal discarded the preselection permission diagnostic")?;
            assert_eq!(skipped.guard_boundary, Some(Boundary::Authority));
            assert_eq!(
                skipped.plan_error,
                Some(provenant_records::diagnostic::PlanFailure::ProtectedOverlap)
            );
            assert_eq!(
                skipped.confinement_guard,
                Some(provenant_records::diagnostic::ConfinementGuard::ProtectedPathOverlap)
            );
            continue;
        }
        let skipped = refusal
            .skipped_routes
            .iter()
            .find(|route| route.route.starts_with("cursor"))
            .ok_or("preselection refusal discarded the adapter plan error")?;
        assert_eq!(skipped.guard_boundary, Some(boundary),);
        assert!(!skipped.detail.is_empty());
        assert!(
            !skipped
                .detail
                .contains(fixture.home.to_str().ok_or("home encoding")?)
        );
        assert!(
            !skipped
                .detail
                .contains(fixture.paths.scratch.to_str().ok_or("scratch encoding")?)
        );
        let failure = provenant_records::diagnostic::PlanFailure::CredentialRoot;
        assert_eq!(skipped.plan_error, Some(failure));
        assert_eq!(
            skipped.cause,
            Some(provenant_records::RouteExclusionReason::ConfinementUnavailable),
        );
        assert_eq!(
            skipped.diagnostic().detail.as_deref(),
            Some(failure.detail())
        );
        assert_eq!(
            skipped.confinement_guard,
            Some(provenant_records::diagnostic::ConfinementGuard::CredentialRootRefused),
        );
    }
    Ok(())
}

// Spec: docs/specs/routing.md#unknown-never-means-ready
#[test]
fn consecutive_preparations_skip_the_account_after_its_first_login_failure()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::Clock;
    let mut fixture = CommitSourcePreparation::new()?;
    let account_home = fixture.home.join("codex-signed-out");
    std::fs::create_dir(&account_home)?;
    std::fs::write(
        fixture.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.accounts.entries.work]\nadapter='codex'\nlabel='work'\nkey='signed-out'\ndirectory='{}'\n[[domains.routing.pools.trial]]\nmodel='codex/gpt-6-luna'\nweight=1\nefforts=[]\n[[domains.routing.pools.trial]]\nmodel='cursor/composer-2.5'\nweight=0\nefforts=[]\n",
            account_home.display()
        ),
    )?;
    fixture.task.route = run::RequestedRoute::Named {
        extra: Default::default(),
        name: "trial".into(),
        effort: None,
        required_family: None,
    };
    for _ in 0..2 {
        let launch = prepare_with(
            &fixture.registry(),
            &fixture.task,
            "independent launch",
            &fixture.paths,
            None,
            |name| {
                if name == "codex" || name == "cursor" {
                    Ok(fixture.executable.clone())
                } else {
                    Err("fixture provider unavailable".into())
                }
            },
            None,
        )?;
        assert_eq!(launch.route.provider.as_deref(), Some("cursor"));
        let scope = provenant_quota::ReadinessScope {
            extra: Default::default(),
            host: provenant_quota::LOCAL_HOST.into(),
            adapter: "codex".into(),
            account_key: "signed-out".into(),
            route: provenant_quota::ReadinessRoute::from_text("codex/gpt-6-luna"),
        };
        let observed = crate::quota::read_readiness(
            &fixture.state,
            &scope,
            provenant_os::clock::SystemClock::new().unix_seconds(),
            &provenant_quota::DEFAULT_FRESHNESS,
        );
        assert_eq!(observed.state, provenant_quota::ReadinessState::Unavailable);
        assert_eq!(
            observed.reason,
            Some(provenant_quota::ReadinessReason::SignedOut)
        );
    }
    Ok(())
}

// Spec: docs/specs/routing.md#unknown-never-means-ready
#[test]
fn successful_login_probe_recovers_an_expired_preparation_failure()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::Clock;
    let fixture = CommitSourcePreparation::new()?;
    let now = provenant_os::clock::SystemClock::new().unix_seconds();
    let account = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: "cursor".into(),
        account_key: "local".into(),
        label: "default".into(),
    };
    crate::quota::persist_preparation_login_failure(
        &fixture.state,
        &account,
        provenant_quota::ReadinessReason::SignedOut,
        now - 1000,
    )?;
    let scope = provenant_quota::ReadinessScope {
        extra: Default::default(),
        host: provenant_quota::LOCAL_HOST.into(),
        adapter: account.adapter.clone(),
        account_key: account.account_key.clone(),
        route: provenant_quota::ReadinessRoute::from_text("cursor/composer-2.5"),
    };
    assert_eq!(
        crate::quota::read_readiness(
            &fixture.state,
            &scope,
            now,
            &provenant_quota::DEFAULT_FRESHNESS
        )
        .state,
        provenant_quota::ReadinessState::Unavailable
    );
    let launch = fixture.prepare()?;
    assert_eq!(launch.route.provider.as_deref(), Some("cursor"));
    assert_eq!(
        crate::quota::read_readiness(
            &fixture.state,
            &scope,
            provenant_os::clock::SystemClock::new().unix_seconds(),
            &provenant_quota::DEFAULT_FRESHNESS,
        )
        .reason,
        Some(provenant_quota::ReadinessReason::ProbedNoFigures)
    );
    Ok(())
}
