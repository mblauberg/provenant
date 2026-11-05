use super::*;

fn unavailable(message: String) -> PrepareError {
    PrepareError::Candidates {
        message,
        plans: Vec::new(),
        skipped_routes: Vec::new(),
        fallback_outcome: Some(provenant_records::run::PreparationFailure::AdapterUnavailable),
    }
}

/// Resolve only a host-configured entry. The result enters the ordinary lane
/// launcher, supervisor, guardian and settlement path below this edge.
pub(super) fn prepare_command(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate: &dyn Fn(&str) -> Result<PathBuf, String>,
    provider: &str,
    name: &str,
) -> Result<run::PreparedLaunch, PrepareError> {
    use provenant_providers::Adapter;
    if provider != "command" {
        return Err(unavailable(format!(
            "{provider} has no named model-less entries"
        )));
    }
    AdapterSpec::get(provider)
        .ok_or("unknown adapter")?
        .check_launch(&provenant_records::LaunchRequest {
            resume: resume.is_some(),
            write: run::Service::has_writes(task.writer.is_some(), &task.access.write_roots),
            network: task.access.network,
            ..Default::default()
        })
        .map_err(|error| error.to_string())?;
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|error| error.to_string())?;
    let command = config.domains.fabric.commands.get(name).ok_or_else(|| {
        unavailable(format!(
            "command {name} is not configured in domains.fabric.commands"
        ))
    })?;
    let program = command
        .argv
        .first()
        .filter(|program| !program.is_empty())
        .ok_or("configured command has no executable")?;
    let cwd = canonical(registry.cwd.ok_or("working directory is unavailable")?)?;
    let invocation = if program.contains('/') {
        cwd.join(program)
    } else {
        locate(program).map_err(unavailable)?
    };
    let executable = canonical(&invocation).map_err(unavailable)?;
    let runtimes = command
        .runtimes
        .iter()
        .map(|path| canonical(path))
        .collect::<Result<Vec<_>, _>>()?;
    let home = canonical(registry.home.ok_or("home directory is unavailable")?)?;
    let assets = registry.assets.ok_or("product assets are unavailable")?;
    let project = registry
        .project_id
        .ok_or("selected project identity is unavailable")?;
    let git = canonical(&match registry.host {
        Some(host) => host.git.clone(),
        None => provenant_os::git::git_executable().ok_or("Git unavailable")?,
    })?;
    let scratch = paths.temporary.join("command");
    apply_setup_directory(&[&paths.temporary], &scratch)?;
    let mut access = task.access.clone();
    // With no API endpoints, provider-only reach is network off.
    access.network.get_or_insert(
        AdapterSpec::get(provider)
            .ok_or("unknown adapter")?
            .launch
            .default_network,
    );
    let request = confinement_request(
        project,
        ConfinementRoots {
            cwd: &cwd,
            home: &home,
            scratch: &scratch,
            private_state: &paths.provider_state,
            assets,
            prompt_file: None,
        },
        None,
        &access,
        &config.domains.run,
        provider,
        false,
    )?;
    let (plan, observed_head, repository, worktree_gitdir) =
        read_only_plan(roots, project, &cwd, &git, request)?;
    let user = provenant_os::sys::account(provenant_os::sys::Account::Current)
        .map_err(|error| error.to_string())?
        .ok_or("current user identity is unavailable")?
        .name;
    let runtime = canonical(&std::env::current_exe().map_err(|error| error.to_string())?)?;
    let child_state = roots
        .state
        .as_path()
        .join("children")
        .join(paths.attempt_id.to_string());
    let route = ResolvedRoute {
        extra: [("name".into(), serde_json::Value::String(name.into()))]
            .into_iter()
            .collect(),
        adapter: provider.into(),
        model: String::new(),
        effort: None,
        catalogue_revision: String::new(),
        account_key: None,
        host: "local".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: format!("domains.fabric.commands.{name}"),
            expires_unix_seconds: i64::MAX,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: task.authority_digest.to_string(),
        },
        pool_selection: None,
    };
    let prompt = Prompt::new(String::new());
    let mut spec = provenant_providers::command::Command
        .prepare(PrepareInput {
            route: &route,
            command: Some(provenant_providers::command::Invocation {
                argv: &command.argv,
                runtimes: &runtimes,
                program: &invocation,
            }),
            confined: &plan,
            prompt: &prompt,
            continuation: None,
            grant_digest: &task.authority_digest.to_string(),
            executable: &executable,
            home: &home,
            user: &user,
            host_var: &|key| std::env::var_os(key),
            skill_stage_max_bytes: config.domains.lanes.skill_stage_max_bytes,
            account: None,
            token: None,
            cwd: &cwd,
            git_context: false,
            nonce: [0; 16],
            provenant: LaneRuntime {
                executable: &runtime,
                git: &git,
                state: roots.state.as_path(),
                child_state: &child_state,
                home: &home,
                depth: paths.lane_depth,
                attempt: paths.attempt_id,
                assets,
            },
        })
        .map_err(|error| error.to_string())?;
    if let Some(alias) = &paths.temporary_alias {
        alias_temporary_env(&mut spec.env, &scratch, &alias.join("command"));
    }
    apply_lane_capabilities(&mut spec.profile.spec, &task.access);
    let common_git_path = repository
        .as_ref()
        .map(|path| path.to_path_buf())
        .transpose()
        .map_err(|error| error.to_string())?;
    let project_key = project.to_string();
    let project_paths = super::project_paths::apply(
        &mut spec,
        &plan,
        super::project_paths::Context {
            cwd: &cwd,
            home: &home,
            common_git: common_git_path.as_deref(),
            state_root: roots.state.as_path(),
            worktree_gitdir: worktree_gitdir.as_deref(),
            writer: false,
            settings: config.domains.lanes.projects.get(&project_key),
            project: &project_key,
        },
    );
    configure_process_tree(&mut spec, paths)?;
    let settings = config.domains.fabric.lanes.build_settings();
    configure_build_slots(
        &mut spec,
        &settings,
        &home,
        &scratch,
        &runtime,
        roots.state.as_path(),
    )?;
    set_cargo_profile(&mut spec.env, &settings);
    set_cargo_jobs(
        &mut spec.env,
        registry
            .host
            .and_then(|host| host.cargo_build_jobs.as_deref()),
        settings.jobs(),
    );
    spec.complete_tool_requirements(&plan, &home, &cwd);
    apply_setup(&spec.setup, &[&scratch, &paths.provider_state], None)?;
    let warnings: Vec<String> = spec
        .omitted_discovery
        .iter()
        .map(|entry| super::preparation::omission_warning(entry, &home))
        .collect();
    let applied_access = spec.applied_access().receipt(&home);
    let tool_closure = serde_json::Value::Array(
        spec.prepared_tools()
            .iter()
            .map(|tool| tool.receipt())
            .collect(),
    );
    Ok(run::PreparedLaunch {
        skipped_routes: Vec::new(),
        argv: spec.argv,
        environment: spec.env,
        stdin: Vec::new(),
        secret_input: None,
        cwd: spec.cwd,
        route: RouteRecord {
            provider: Some(provider.into()),
            source: Some(format!("domains.fabric.commands.{name}")),
            extra: [
                ("name".into(), serde_json::Value::String(name.into())),
                ("applied_access".into(), applied_access),
                ("tool_closure".into(), tool_closure),
                ("discovery_warnings".into(), serde_json::json!(warnings)),
                ("project_paths".into(), project_paths),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        },
        observed_head,
        repository,
        confinement: Some(run::LaunchConfinement {
            plan,
            profile: spec.profile,
            writer: None,
            home,
            cargo_registry_seed: false,
            state_root: roots.state.as_path().to_owned(),
        }),
    })
}

pub(super) fn nested_policy_fallback(
    task: &run::TaskRequest,
    route: &ResolvedRoute,
    policy: &provenant_routing::ModelPolicy,
) -> Result<run::TaskRequest, String> {
    if run::Service::has_writes(task.writer.is_some(), &task.access.write_roots) {
        return Err("model policy fallback requires read-only authority".into());
    }
    let fallback = task
        .fallback
        .first()
        .cloned()
        .or_else(|| {
            policy.result.fallback.as_ref().map(|model| RouteRecord {
                provider: Some(model.adapter().into()),
                model: Some(model.as_str().into()),
                effort: policy.result.fallback_effort.clone(),
                ..Default::default()
            })
        })
        .ok_or("restricted nested route has no result-policy fallback")?;
    let model = provenant_routing::ModelId::parse(
        fallback
            .model
            .as_deref()
            .ok_or("policy fallback model missing")?,
    )
    .map_err(|error| error.to_string())?;
    let mut task = task.clone();
    task.route = run::RequestedRoute::Model {
        extra: Default::default(),
        provider: model.adapter().into(),
        model: model.as_str().into(),
        effort: fallback.effort,
    };
    if model.adapter() != route.adapter {
        task.account = None;
    }
    task.fallback.clear();
    Ok(task)
}

pub(super) fn configure_process_tree(
    spec: &mut provenant_providers::LaunchSpec,
    paths: &run::LaunchPaths,
) -> Result<(), String> {
    let attempt = paths.scratch.parent().ok_or("attempt directory missing")?;
    let directory = Directory::open_anchor(attempt).map_err(|error| error.to_string())?;
    spec.profile.spec.runtime_reads.push(PathGrant {
        path: provenant_supervisor::process_tree::snapshot_path(&directory)
            .map_err(|error| error.to_string())?,
        scope: PathScope::Exact,
    });
    Ok(())
}

pub(super) fn configure_build_slots(
    spec: &mut provenant_providers::LaunchSpec,
    settings: &provenant_config::LaneBuildSettings,
    home: &Path,
    scratch: &Path,
    runtime: &Path,
    state: &Path,
) -> Result<(), String> {
    // Builds, not lanes, share the host's build slots: a `cargo` shim takes
    // one around each compiling command. The host (this process, outside
    // the sandbox) creates the slot files; the lane may only read and lock
    // them. Memory is unreadable only on a host that cannot run lanes.
    let cargo = home.join(".cargo/bin/cargo");
    if spec.env.iter().any(|(name, _)| name == "CARGO_TARGET_DIR") && cargo.is_file() {
        let total = provenant_os::sys::resources::memory().map_or(0, |memory| memory.total_bytes);
        let slots = settings.build_limit(total);
        match provenant_os::build_slots::BuildSlots::provision(state, slots) {
            Ok(()) => spec
                .configure_build_slots(
                    scratch,
                    &provenant_providers::BuildSlotGate {
                        provenant: runtime,
                        state,
                        slots,
                        wait_seconds: settings.build_wait().as_secs(),
                        cargo: &cargo,
                        // Inside the lane's writable scratch (its $TMPDIR), where
                        // the host finds it; each waiting build adds its pid.
                        wait_marker: &scratch.join(provenant_os::build_slots::WAIT_MARKER),
                    },
                )
                .map_err(|error| error.to_string())?,
            // Build slots are fair sharing, not a boundary: without them
            // the lane still runs, ungated.
            #[allow(clippy::print_stderr)]
            Err(error) => eprintln!(
                "provenant: build slots unavailable for this lane, building without them: {error}"
            ),
        }
    }
    Ok(())
}

pub(super) fn set_cargo_jobs(
    env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    inherited: Option<&str>,
    jobs: u64,
) {
    if !env.iter().any(|(name, _)| name == "CARGO_BUILD_JOBS") {
        env.push((
            "CARGO_BUILD_JOBS".into(),
            inherited
                .map_or_else(|| jobs.to_string(), str::to_owned)
                .into(),
        ));
    }
}

/// Keep lane build output small: no incremental sessions and line tables
/// instead of full debug info. A value the submitting environment or brief
/// already supplied is left alone; `domains.fabric.lanes` overrides the defaults.
pub(super) fn set_cargo_profile(
    env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    settings: &provenant_config::LaneBuildSettings,
) {
    for (name, value) in [
        (
            "CARGO_INCREMENTAL",
            if settings.incremental() { "1" } else { "0" },
        ),
        ("CARGO_PROFILE_DEV_DEBUG", settings.debug_level()),
    ] {
        if !env.iter().any(|(existing, _)| existing == name) {
            env.push((name.into(), value.into()));
        }
    }
}

/// A candidate that did not launch, typed by why it was skipped.
pub(super) fn skip(route: &str, kind: SkipKind, detail: impl Into<String>) -> SkippedRoute {
    SkippedRoute {
        extra: Default::default(),
        route: route.to_owned(),
        kind,
        detail: detail.into(),
    }
}
