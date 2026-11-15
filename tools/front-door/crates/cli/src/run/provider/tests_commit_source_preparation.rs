#[derive(serde::Serialize)]
struct ConfigDocument<Domains> {
    schema_version: u64,
    domains: Domains,
}

#[derive(serde::Serialize)]
struct RoutingDomain<'a> {
    routing: &'a provenant_routing::RoutingConfig,
}

#[derive(serde::Serialize)]
struct RunDomain<'a> {
    run: &'a provenant_confinement::run_config::RunConfig,
}

fn write_config<Domains: serde::Serialize>(
    state: &std::path::Path,
    domains: Domains,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ConfigDocument {
        schema_version: provenant_config::CONFIG_SCHEMA_VERSION,
        domains,
    };
    let text = toml_edit::ser::to_string(&config)?;
    std::fs::write(state.join("config.toml"), text)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefusalScenario {
    CredentialRoot,
    InvalidRoots,
    Resolver,
}

impl RefusalScenario {
    const fn boundary(self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass;
        match self {
            Self::CredentialRoot | Self::Resolver => BoundaryClass::Credentials,
            Self::InvalidRoots => BoundaryClass::Authority,
        }
    }

    const fn plan_error(self) -> &'static str {
        match self {
            Self::CredentialRoot | Self::Resolver => "credential_root",
            Self::InvalidRoots => "invalid_roots",
        }
    }
}

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
    let mut routing = provenant_routing::RoutingConfig::default();
    routing.pools.insert(
        "senior".into(),
        vec![provenant_routing::RoutePoolMember {
            model: provenant_routing::ModelId::parse("cursor/gpt-5.3-codex-low")?,
            weight: 1,
            effort: None,
            efforts: Vec::new(),
            free: false,
        }],
    );
    write_config(&fixture.state, RoutingDomain { routing: &routing })?;
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
    let policy = permissions.policy.as_ref().ok_or("missing policy")?;
    let source_grant = provenant_confinement::permissions::PathGrant {
        path: launch.cwd.clone(),
        scope: PathScope::Subtree,
    };
    assert!(policy.mandatory().state_reads().contains(&source_grant));
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
    assert_eq!(private.scope, PathScope::Subtree);
    assert!(private.path.starts_with(&fixture.paths.provider_state));
    assert!(handoff_writable(permissions));
    let handoff = private.path.join("handoff.json").to_string_lossy();
    let prompt = launch.argv.last().ok_or("Cursor prompt argument")?;
    assert!(prompt.to_string_lossy().contains(handoff.as_ref()));
    let writes = provenant_confinement::write_grants(permissions, &confinement.profile);
    assert!(writes.iter().any(|grant| grant.path == private.path));
    source_tree::ensure_view_read_only(&launch.cwd, &writes)?;
    Ok(())
}

#[test]
fn host_write_policy_preserves_the_declared_handoff_grant() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = CommitSourcePreparation::new()?;
    let mut config = provenant_confinement::run_config::RunConfig::default();
    config.host_policy = Some(serde_json::from_value(serde_json::json!({
        "deny_write": [fixture.state.clone()]
    }))?);
    write_config(&fixture.state, RunDomain { run: &config })?;

    let launch = fixture.prepare()?;
    let confinement = launch.confinement.as_ref().ok_or("missing confinement")?;
    let permissions = confinement.plan.permissions();
    let policy = permissions.policy.as_ref().ok_or("missing policy")?;
    assert!(
        policy
            .optional()
            .deny_write
            .iter()
            .any(|grant| grant.path == fixture.state)
    );
    let private = permissions
        .private_state
        .as_ref()
        .ok_or("missing private state")?;
    assert!(handoff_writable(permissions));
    let handoff = private.path.join("handoff.json").to_string_lossy();
    let prompt = launch.argv.last().ok_or("Cursor prompt argument")?;
    assert!(prompt.to_string_lossy().contains(handoff.as_ref()));
    Ok(())
}

#[test]
fn commit_source_preparation_refusal_retains_preselection_permission_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::BoundaryClass;
    for scenario in [
        RefusalScenario::CredentialRoot,
        RefusalScenario::InvalidRoots,
        RefusalScenario::Resolver,
    ] {
        let boundary = scenario.boundary();
        let mut fixture = CommitSourcePreparation::new()?;
        if scenario == RefusalScenario::InvalidRoots {
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
        if scenario == RefusalScenario::Resolver {
            let mut routing = provenant_routing::RoutingConfig::default();
            routing.pools.insert(
                "preparation-fixture".into(),
                vec![provenant_routing::RoutePoolMember {
                    model: provenant_routing::ModelId::parse("cursor/gpt-5.3-codex-low")?,
                    weight: 1,
                    effort: None,
                    efforts: Vec::new(),
                    free: false,
                }],
            );
            write_config(&fixture.state, RoutingDomain { routing: &routing })?;
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
            Ok(_) => panic!("{scenario:?} must be refused"),
            Err(error) => error,
        };
        let refusal = error.into_refusal();
        if scenario == RefusalScenario::Resolver {
            assert_eq!(
                refusal.terminal_cause,
                provenant_records::run::TerminalCause::RouteResolutionRefused,
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
            Some(&serde_json::json!(scenario.plan_error())),
        );
        assert_eq!(
            skipped.cause,
            Some(provenant_records::RouteExclusionReason::ConfinementUnavailable),
        );
        let failure: provenant_records::diagnostic::PlanFailure =
            serde_json::from_value(skipped.extra["plan_error"].clone())?;
        assert_eq!(
            skipped.diagnostic().detail.as_deref(),
            Some(failure.detail())
        );
        assert_eq!(
            skipped.extra["confinement_guard"],
            serde_json::json!(if scenario == RefusalScenario::InvalidRoots {
                "invalid_plan_roots"
            } else {
                "credential_root_refused"
            }),
        );
    }
    Ok(())
}
