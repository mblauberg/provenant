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
        let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
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
        std::fs::write(&executable, b"#!/bin/sh\nif [ \"$1\" = status ]; then printf 'Logged in successfully\\n'; else printf 'fixture-cursor 1.0\\n'; fi\n")?;
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
            context: None,
            extra: Default::default(),
            build_heavy: false,
            title: None,
            base: Some(head.clone()),
            head: Some(head),
            source_head: None,
            base_ref: None,
            profile: Default::default(),
            prompt_digest: provenant_records::CanonicalV1::file_digest(b"review fixture"),
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "cursor".into(),
                model: "cursor/gpt-5.3-codex-low".into(),
                effort: None,
            },
            account: None,
            authority_digest: Digest::from_bytes([74; 32]),
            writer: None,
            deliver: None,
            provider_timeout_seconds: 20,
            delivery_id: None,
            access: run::LaneAccess {
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
        "schema_version=1\n[[domains.routing.pools.senior]]\nmodel='cursor/gpt-5.3-codex-low'\nweight=1\nefforts=[]\n",
    )?;
    fixture.task.route = run::RequestedRoute::Review {
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
    let private = permissions.private_state.as_ref().ok_or("missing private state")?;
    let handoff = private.path.join("handoff.json");
    let prompt = launch.argv.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>().join(" ");
    let stdin = String::from_utf8_lossy(&launch.stdin);
    assert!(prompt.contains(&handoff.display().to_string()) || stdin.contains(&handoff.display().to_string()));
    let policy = provenant_confinement::permissions::snapshot::render(permissions, &confinement.profile, &fixture.home)?;
    assert!(policy.contains(&format!("(allow file-write* (subpath \"{}\"))", private.path.display())));
    let denial = policy.lines().find(|line| line.starts_with("(deny file-write* (require-all") && line.contains(fixture.state.to_str().unwrap())).ok_or("state denial")?;
    assert!(denial.contains(&format!("(require-not (subpath \"{}\"))", private.path.display())));
    assert!(policy.contains(&format!("(deny file-write* (subpath \"{}\"))", launch.cwd.display())));
    let writes = provenant_confinement::write_grants(permissions, &confinement.profile);
    source_tree::ensure_view_read_only(&launch.cwd, &writes)?;
    Ok(())
}

#[test]
fn host_denied_handoff_remains_best_effort() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = CommitSourcePreparation::new()?;
    std::fs::write(fixture.state.join("config.toml"), format!(
        "schema_version=1\n[domains.run.host_policy]\ndeny_write=[{:?}]\n", fixture.state.to_str().ok_or("state path")?
    ))?;
    let launch = fixture.prepare()?;
    let prompt = launch.argv.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>().join(" ");
    let permissions = launch.confinement.as_ref().ok_or("missing confinement")?.plan.permissions();
    assert!(permissions.private_state.is_some());
    assert!(prompt.contains("Progress handoff JSON:") || String::from_utf8_lossy(&launch.stdin).contains("Progress handoff JSON:"));
    Ok(())
}

#[test]
fn commit_source_preparation_refusal_retains_preselection_permission_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::BoundaryClass;
    for (plan_error, boundary) in [
        ("credential_root", BoundaryClass::Credentials),
        ("invalid_roots", BoundaryClass::Authority),
        ("resolver", BoundaryClass::Credentials),
    ] {
        let mut fixture = CommitSourcePreparation::new()?;
        if plan_error == "invalid_roots" {
            fixture
                .task
                .access
                .read_roots
                .push(fixture.paths.scratch.clone());
        } else {
            fixture
                .task
                .access
                .write_roots
                .push(fixture.home.join(".ssh"));
        }
        if plan_error == "resolver" {
            std::fs::write(
                fixture.state.join("config.toml"),
                "schema_version=1\n[[domains.routing.pools.preparation-fixture]]\nmodel='cursor/gpt-5.3-codex-low'\nweight=1\nefforts=[]\n",
            )?;
            fixture.task.route = run::RequestedRoute::Review {
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
                refusal.terminal_cause,
                provenant_records::run::TerminalCause::RouteResolutionRefused,
            );
            assert_eq!(
                refusal.terminal_note.as_deref(),
                Some("route resolver classification: family_separation"),
            );
            // A denied adapter check cannot reclassify a separate router error.
            assert_eq!(refusal.boundary_class, BoundaryClass::None);
        } else {
            assert_eq!(refusal.boundary_class, boundary);
        }
        let skipped = refusal
            .skipped_routes
            .iter()
            .find(|route| route.route.starts_with("cursor"))
            .ok_or("preselection refusal discarded the adapter plan error")?;
        assert_eq!(
            skipped.extra.get("guard_boundary"),
            Some(&serde_json::json!(boundary)),
        );
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
        assert_eq!(
            skipped.extra.get("plan_error"),
            Some(&serde_json::json!(if plan_error == "resolver" {
                "credential_root"
            } else {
                plan_error
            })),
        );
        assert_eq!(
            skipped.cause,
            Some(provenant_records::RouteExclusionReason::ConfinementUnavailable),
        );
        let failure: provenant_records::diagnostic::PlanFailure =
            serde_json::from_value(skipped.extra["plan_error"].clone())?;
        assert_eq!(skipped.diagnostic().detail.as_deref(), Some(failure.detail()));
        assert_eq!(
            skipped.extra["confinement_guard"],
            serde_json::json!(if plan_error == "invalid_roots" {
                "invalid_plan_roots"
            } else {
                "credential_root_refused"
            }),
        );
    }
    Ok(())
}
