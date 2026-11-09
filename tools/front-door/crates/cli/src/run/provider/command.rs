use super::*;

fn unavailable(message: String) -> PrepareError {
    PrepareError::Candidates {
        message,
        plans: Vec::new(),
        candidate_boundary: None,
        candidate_resource: None,
        skipped_routes: Vec::new(),
        fallback_outcome: Some(provenant_records::run::PreparationFailure::AdapterUnavailable),
    }
}

/// The configured executable could not be resolved. The note names the
/// operator's own `argv[0]`, not the OS error, which may echo other paths.
fn executable_unavailable(program: &str) -> PrepareError {
    PrepareError::Refusal {
        message: provenant_records::run::TerminalCause::CommandUnavailable
            .detail()
            .into(),
        cause: provenant_records::run::TerminalCause::CommandUnavailable,
        note: Some(format!(
            "executable {}",
            provenant_records::diagnostic::safe_text(program)
        )),
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
    #[cfg(feature = "fixture")]
    hold_command_preparation_for_fixture();
    if !AdapterSpec::get(provider)
        .is_some_and(|spec| spec.named_entry == provenant_records::NamedEntry::Command)
    {
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
            skill_location: task.profile.require_skill_location,
            ..Default::default()
        })?;
    let adapter = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == provider)
        .map(|entry| entry.adapter)
        .ok_or_else(|| unavailable(format!("{provider} is not registered")))?;
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
    let source = source_cwd(registry, task)?;
    let isolated = task.access.source_root.is_some() && !task.access.source_live;
    let cwd = if isolated {
        let assets = registry.assets.ok_or("product assets are unavailable")?;
        super::source_tree::ensure_source_roots(&source, &task.access, assets)?;
        let git = canonical(&match registry.host {
            Some(host) => host.git.clone(),
            None => provenant_os::git::git_executable().ok_or("Git unavailable")?,
        })?;
        super::source_tree::snapshot_source(registry, task, paths, &source, &git)?
    } else {
        source.clone()
    };
    let invocation = if program.contains('/') {
        cwd.join(program)
    } else {
        locate(program).map_err(|_| executable_unavailable(program))?
    };
    let executable = canonical(&invocation).map_err(|_| executable_unavailable(program))?;
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
    let (scratch_directory, scratch_name) =
        super::preparation::fresh_scratch_directory(&paths.temporary, provider, None)?;
    let scratch = scratch_directory
        .current_path()
        .map_err(|error| error.to_string())?;
    let credential_roots = credential_roots(registry, &home, &config.domains.accounts)?;
    let github_roots = github_roots(registry);
    let protected_worktrees = protected_worktrees(registry, &source, &git)?;
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
            host_roots: Some(roots),
            cwd: &cwd,
            home: &home,
            state: roots.state.as_path(),
            credential_roots: &credential_roots,
            github_roots: &github_roots,
            protected_worktrees: &protected_worktrees,
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
    let (mut plan, mut observed_head, repository, mut worktree_gitdir) = read_only_plan(
        roots,
        project,
        &source,
        &cwd,
        &git,
        request,
        isolated,
        task.access.source_live,
    )?;
    if isolated {
        observed_head = Some(source_head(task)?);
        worktree_gitdir = Some(cwd.join(".git"));
    } else {
        require_source_head(task, observed_head)?;
    }
    let user = provenant_os::sys::account(provenant_os::sys::Account::Current)
        .map_err(|error| error.to_string())?
        .ok_or("current user identity is unavailable")?
        .name;
    let runtime = canonical(&std::env::current_exe().map_err(|error| error.to_string())?)?;
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
    let mut spec = adapter
        .prepare(PrepareInput {
            images: &[],
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
            host_var: &|key| host_provider_var(registry, key),
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
                home: &home,
                depth: paths.lane_depth,
                attempt: paths.attempt_id,
                assets,
            },
        })
        .map_err(|error| error.to_string())?;
    if let Some(alias) = &paths.temporary_alias {
        alias_temporary_env(&mut spec.env, &scratch, &alias.join(&scratch_name));
    }
    if let Some(warning) =
        super::preparation::image_input_warning(provider, &task.access.attachments)
    {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "{warning}");
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
    let process_tree_read = configure_process_tree(&mut spec, paths)?;
    let settings = config.domains.fabric.lanes.build_settings();
    let coordination_reads = configure_build_slots(
        &mut spec,
        &settings,
        &home,
        &scratch,
        &runtime,
        roots.state.as_path(),
        &task.profile.knobs,
    );
    if let Some(policy) = plan.policy_mut() {
        for root in std::iter::once(process_tree_read).chain(coordination_reads) {
            policy.mandatory_mut().add_state_read(root)?;
        }
    }
    set_cargo_profile(&mut spec.env, &settings);
    set_cargo_jobs(
        &mut spec.env,
        registry
            .host
            .and_then(|host| host.cargo_build_jobs.as_deref()),
        settings.jobs(),
    );
    spec.complete_tool_requirements(&plan, &home, &cwd);
    if isolated {
        super::source_tree::ensure_profile_isolated(&source, &spec.profile.spec)?;
        super::source_tree::ensure_view_read_only(
            &cwd,
            &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
        )?;
    }
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

#[cfg(feature = "fixture")]
#[allow(clippy::disallowed_methods)]
fn hold_command_preparation_for_fixture() {
    let Some(path) = std::env::var_os("PROVENANT_FIXTURE_HOLD_COMMAND_PREPARATION") else {
        return;
    };
    let path = std::path::Path::new(&path);
    if std::fs::write(path.join("entered"), b"entered").is_err() {
        return;
    }
    let _ = provenant_test_support::wait_for(
        "command preparation release",
        std::time::Duration::from_secs(30),
        || !path.join("hold").exists(),
        |released| *released,
    );
    let _ = std::fs::write(path.join("released"), b"released");
}

pub(super) fn configure_process_tree(
    spec: &mut provenant_providers::LaunchSpec,
    paths: &run::LaunchPaths,
) -> Result<PathGrant, String> {
    let attempt = paths.scratch.parent().ok_or("attempt directory missing")?;
    let directory = Directory::open_anchor(attempt).map_err(|error| error.to_string())?;
    let grant = PathGrant {
        path: provenant_supervisor::process_tree::snapshot_path(&directory)
            .map_err(|error| error.to_string())?,
        scope: PathScope::Exact,
    };
    spec.profile.spec.runtime_reads.push(grant.clone());
    Ok(grant)
}

/// Cargo commands a lane may run locally, from the sealed `lanes.local_cargo`
/// knob (host configuration, the run's `--knob`, or the graph node's
/// `attempt.local_cargo`). The registry default and any unreadable value are
/// `all`.
pub(super) fn local_cargo(
    knobs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> &'static str {
    match knobs
        .get("lanes.local_cargo")
        .and_then(serde_json::Value::as_str)
    {
        Some("check") => "check",
        Some("none") => "none",
        _ => "all",
    }
}

#[allow(clippy::print_stderr)]
pub(super) fn configure_build_slots(
    spec: &mut provenant_providers::LaunchSpec,
    settings: &provenant_config::LaneBuildSettings,
    home: &Path,
    scratch: &Path,
    runtime: &Path,
    state: &Path,
    knobs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Vec<PathGrant> {
    // Builds, not lanes, share the host's build slots: a `cargo` shim takes
    // one around each compiling command. The host (this process, outside
    // the sandbox) creates the slot files; the lane may only read and lock
    // them. Memory is unreadable only on a host that cannot run lanes.
    let cargo = home.join(".cargo/bin/cargo");
    if spec.env.iter().any(|(name, _)| name == "CARGO_TARGET_DIR") && cargo.is_file() {
        let total = provenant_os::sys::resources::memory().map_or(0, |memory| memory.total_bytes);
        let slots = settings.build_limit(total);
        match provenant_os::build_slots::BuildSlots::provision(state, slots) {
            Ok(()) => {
                if let Err(error) = spec.configure_build_slots(
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
                        local_cargo: local_cargo(knobs),
                    },
                ) {
                    eprintln!(
                        "provenant: build slot shim unavailable for this lane, building without it: {error}"
                    );
                } else {
                    let directory = state.join(provenant_os::build_slots::DIRECTORY.join("/"));
                    // BuildSlots opens the directory and each existing slot
                    // read-only, then takes an advisory lock. The lane cannot
                    // create, replace or unlink these host coordination files.
                    return std::iter::once(PathGrant {
                        path: directory.clone(),
                        scope: PathScope::Exact,
                    })
                    .chain((0..slots).map(|index| PathGrant {
                        path: directory.join(format!("slot-{index}.lock")),
                        scope: PathScope::Exact,
                    }))
                    .collect();
                }
            }
            // Build slots are fair sharing, not a boundary: without them
            // the lane still runs, ungated.
            Err(error) => eprintln!(
                "provenant: build slots unavailable for this lane, building without them: {error}"
            ),
        }
    }
    Vec::new()
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
pub(super) fn skip(route: &str, kind: SkipKind, _detail: impl Into<String>) -> SkippedRoute {
    let diagnostic = kind.diagnostic();
    let detail = format!(
        "{}; {}",
        diagnostic.detail.unwrap_or_default(),
        diagnostic.next.unwrap_or_default()
    );
    SkippedRoute {
        extra: Default::default(),
        route: route.to_owned(),
        kind,
        cause: None,
        detail,
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    #[test]
    fn command_preparation_unavailable_retains_full_remedy() {
        let error = executable_unavailable("fixture-tool");
        let PrepareError::Refusal {
            message,
            cause,
            note,
        } = error
        else {
            unreachable!();
        };
        assert_eq!(
            cause,
            provenant_records::run::TerminalCause::CommandUnavailable
        );
        assert_eq!(message, cause.detail());
        assert!(message.contains("domains.fabric.commands"));
        assert_eq!(note.as_deref(), Some("executable fixture-tool"));
    }
}
