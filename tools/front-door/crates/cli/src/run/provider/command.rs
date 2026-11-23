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
        fallback_outcome: Some(provenant_records::run::PreparationFailure::AdapterUnavailable),
        note: Some(format!(
            "executable {}",
            provenant_records::diagnostic::safe_text(program)
        )),
    }
}

fn command_executable(
    program: &str,
    cwd: &Path,
    lane_path: &run::FilteredLanePath,
    locate_override: Option<&ExecutableLocator<'_>>,
) -> Result<PathBuf, PrepareError> {
    if program.contains('/') {
        Ok(cwd.join(program))
    } else if let Some(locate) = locate_override {
        locate(program).map_err(|_| executable_unavailable(program))
    } else {
        lane_path
            .resolve(program)
            .map_err(|_| executable_unavailable(program))
    }
}

/// Resolve only a host-configured entry. The result enters the ordinary lane
/// launcher, supervisor, guardian and settlement path below this edge.
pub(super) fn prepare_command(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate_override: Option<&ExecutableLocator<'_>>,
    provider: &str,
    name: &str,
) -> Result<run::PreparedLaunch, PrepareError> {
    #[cfg(feature = "fixture")]
    hold_command_preparation_for_fixture();
    if !AdapterSpec::get(provider)
        .is_some_and(|spec| spec.named_entry == provenant_records::NamedEntry::Command)
    {
        let hint =
            provenant_routing::close_matches_hint(&provenant_routing::adapter_candidates(provider));
        return Err(unavailable(format!(
            "{provider} has no named model-less entries{hint}"
        )));
    }
    AdapterSpec::get(provider)
        .ok_or_else(|| {
            format!(
                "unknown adapter `{provider}`{}",
                provenant_routing::close_matches_hint(&provenant_routing::adapter_candidates(
                    provider
                ))
            )
        })?
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
    let project = registry
        .project_id
        .ok_or("selected project identity is unavailable")?;
    let project_binding = super::confinement::registered_project_binding(roots, project)?;
    let config = crate::config_reads::read_for(roots, "run").map_err(|error| error.to_string())?;
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
    let git = canonical(&match registry.host {
        Some(host) => host.git.clone(),
        None => provenant_os::git::git_executable().ok_or("Git unavailable")?,
    })?;
    let fallback_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&fallback_cancel);
    let git_executor = provenant_os::exec::SystemExecutor::new(1).map_err(|e| e.to_string())?;
    let isolated = task.access.source_root.is_some() && !task.access.source_live;
    let cwd = if isolated {
        let assets = registry.assets.ok_or("product assets are unavailable")?;
        super::source_tree::ensure_source_roots(&source, &task.access, assets)?;
        super::source_tree::snapshot_source(registry, task, paths, &source, &git)?
    } else {
        source.clone()
    };
    let requester_cwd = registry.cwd.unwrap_or(&source);
    let project_path = match task.writer.as_ref() {
        Some(writer) => writer.project.as_path(),
        None => requester_cwd,
    };
    let worktree_path = if task.writer.is_some() { &cwd } else { &source };
    let utility = provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path())
        .map_err(|error| error.to_string())?;
    let lane_path = crate::run::filtered_requester_lane_path(
        registry.host,
        project_path,
        worktree_path,
        &utility,
        &git_executor,
        cancel,
        project_binding.as_ref(),
    )?;
    let invocation = command_executable(program, &cwd, &lane_path, locate_override)?;
    let executable = canonical(&invocation).map_err(|_| executable_unavailable(program))?;
    let runtimes = command
        .runtimes
        .iter()
        .map(|path| canonical(path))
        .collect::<Result<Vec<_>, _>>()?;
    let home = canonical(registry.home.ok_or("home directory is unavailable")?)?;
    let assets = registry.assets.ok_or("product assets are unavailable")?;
    let launch_policy = run::launch_policy(roots.state.as_path(), &project.to_string(), task);
    let policy_model = provenant_routing::ModelId::parse(&format!("{provider}/{name}"));
    let now = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new());
    let catalogue = provenant_routing::Catalogue::bundled();
    let mut access_policy_warning = None;
    let model_policy = match (policy_model, now, catalogue) {
        (Ok(model), Ok(now), Ok(catalogue)) => {
            config
                .domains
                .routing
                .model_policy
                .effective(&catalogue, &model, None, now)
                .policy
        }
        _ => {
            access_policy_warning = Some(
                "command route policy unavailable; resolving lanes.access from knobs".to_owned(),
            );
            Default::default()
        }
    };
    let lane_access = super::preparation::resolve_lane_access(task, &model_policy);
    let (scratch_directory, scratch_name) =
        super::preparation::fresh_scratch_directory(&paths.temporary, provider, None)?;
    let scratch = scratch_directory
        .current_path()
        .map_err(|error| error.to_string())?;
    let credential_roots = credential_roots(
        registry,
        &home,
        &config.domains.accounts,
        &[&source, &cwd, &scratch],
        Some(&lane_access),
        &launch_policy,
    )?;
    if let Some(error) = super::confinement::credential_access_grant_refusal(
        &task.access,
        &credential_roots,
        &lane_access,
        &launch_policy,
    ) {
        return Err(error.into());
    }
    let github_roots = github_roots(registry);
    let (protected_worktrees, protection_warnings) = protected_worktrees(registry, &source, &git)?;
    let mut access = task.access.clone();
    // With no API endpoints, provider-only reach is network off.
    access.network.get_or_insert(
        AdapterSpec::get(provider)
            .ok_or_else(|| {
                format!(
                    "unknown adapter `{provider}`{}",
                    provenant_routing::close_matches_hint(&provenant_routing::adapter_candidates(
                        provider
                    ))
                )
            })?
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
        InventoryContext::Launch(task, &format!("{provider}/{name}"), &lane_access),
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
    let credential_roots = plan.permissions().credential_roots.clone();
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
        selection_context: Default::default(),
        diagnostics: Default::default(),
        user_skills: true,
        access: lane_access.access,
        extra: Default::default(),
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
            availability_warning: None,
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
    let sandbox = sandbox_mode(
        &config.domains.run,
        &format!("{provider}/{name}"),
        task,
        &plan,
    );
    let prompt = Prompt::new(String::new());
    let mut spec = adapter
        .prepare(PrepareInput {
            credential_roots: credential_roots.clone(),
            images: &[],
            policy: &launch_policy,
            route: &route,
            provider_model: None,
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
            host_var: &|key| {
                if key == provenant_core::consumer_env::SANDBOX {
                    Some(sandbox.env_value().into())
                } else if key == "PATH" {
                    Some(lane_path.path().to_os_string())
                } else {
                    lane_provider_var(registry, key, lane_access.access)
                }
            },
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
    let mut launch_warnings =
        super::preparation::image_input_warning(provider, &task.access.attachments)
            .into_iter()
            .collect::<Vec<_>>();
    launch_warnings.extend(protection_warnings);
    launch_warnings.extend(super::preparation::ignore_file_warnings(
        provider,
        task,
        roots.state.as_path(),
    ));
    launch_warnings.extend(spec.warnings.iter().cloned());
    launch_warnings.extend(access_policy_warning);
    launch_warnings.extend(super::preparation::lane_access_warning(task, &lane_access));
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
            credential_roots: &credential_roots,
            lane_access: &lane_access,
            common_git: common_git_path.as_deref(),
            state_root: roots.state.as_path(),
            worktree_gitdir: worktree_gitdir.as_deref(),
            writer: false,
            settings: config.domains.lanes.projects.get(&project_key),
            project: &project_key,
            policy: &launch_policy,
        },
    )?;
    scratch_cargo_output(&mut spec.env, &scratch);
    let process_tree_read = configure_process_tree(&mut spec, paths)?;
    let settings = config.domains.fabric.lanes.build_settings();
    spec.configure_cargo_sources(settings.cargo_source_writes, plan.permissions().network);
    if let Some(policy) = plan.policy_mut() {
        policy.mandatory_mut().add_state_read(process_tree_read)?;
    }
    spec.complete_tool_requirements(&plan, &cwd)?;
    super::confinement::warn_profile_credential_grants(
        &spec.profile.spec,
        &credential_roots,
        &lane_access,
        &launch_policy,
    );
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
        .map(|entry| super::preparation::omission_warning(entry, &home, spec.credential_roots()))
        .collect();
    let applied_access = spec
        .applied_access()
        .receipt(&home, spec.credential_roots());
    let tool_closure = serde_json::Value::Array(
        spec.prepared_tools()
            .iter()
            .map(|tool| tool.receipt())
            .collect(),
    );
    let limits = run::lane_limits(
        task,
        registry.state_root.unwrap_or(std::path::Path::new("")),
        &registry
            .project_id
            .map(|project| project.to_string())
            .unwrap_or_default(),
        provider,
        run::HostCaps::default(),
    );
    let mut route = RouteRecord {
        provider: Some(provider.into()),
        lane_access: Some(lane_access),
        source: Some(format!("domains.fabric.commands.{name}")),
        extra: Default::default(),
        cargo_home: serde_json::from_value(spec.cargo_home_receipt())
            .map_err(|_| unavailable("invalid Cargo home receipt".into()))?,
        applied_access: Some(
            serde_json::from_value(applied_access)
                .map_err(|_| unavailable("invalid applied access receipt".into()))?,
        ),
        tool_closure: Some(
            serde_json::from_value(tool_closure)
                .map_err(|_| unavailable("invalid tool closure receipt".into()))?,
        ),
        discovery_warnings: Some(warnings),
        launch_warnings,
        project_paths: Some(
            serde_json::from_value(project_paths)
                .map_err(|_| unavailable("invalid project path receipt".into()))?,
        ),
        ..Default::default()
    };
    apply_sandbox_mode(task, sandbox.clone(), &mut spec.env, &mut route);
    let cargo_home = spec
        .cargo_home()
        .cloned()
        .ok_or_else(|| unavailable("host Cargo binding was not finalised".into()))?;
    Ok(run::PreparedLaunch {
        limits,
        continuation_expectation: None,
        skipped_routes: Vec::new(),
        argv: spec.argv,
        environment: spec.env,
        stdin: Vec::new(),
        secret_input: None,
        cwd: spec.cwd,
        route,
        observed_head,
        repository,
        confinement: Some(run::LaunchConfinement {
            plan,
            profile: spec.profile,
            writer: None,
            home,
            cargo_home,
            state_root: roots.state.as_path().to_owned(),
            sandbox,
            backend: None,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_slash_path_stays_on_the_configured_path_route()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let worktree = temp.path().join("worktree");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&worktree)?;
        let lane_path =
            run::FilteredLanePath::from_path(std::ffi::OsStr::new(""), &project, &worktree)?;

        let explicit = project.join("tools/runner");
        assert_eq!(
            command_executable("tools/runner", &project, &lane_path, None)?,
            explicit
        );
        assert!(command_executable("runner", &project, &lane_path, None).is_err());
        Ok(())
    }
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

// A nested lane inherits the outer lane's PATH. Recognise generated shims by
// their script contents so a real binary in a similarly named directory works.

/// Keep lane build output small: no incremental sessions and line tables
/// instead of full debug info. A value the submitting environment or brief
/// already supplied is left alone; `domains.fabric.lanes` overrides the defaults.
/// Read-only source views never own build output. Scratch already has a
/// settlement lifetime and no-follow disposal, including crashed attempts.
pub(super) fn scratch_cargo_output(
    env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    scratch: &Path,
) {
    let target = scratch.join("target").into_os_string();
    for key in ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
        env.retain(|(name, _)| name != key);
        env.push((key.into(), target.clone()));
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
        skipped_accounts: Vec::new(),
        extra: Default::default(),
        route: route.to_owned(),
        kind,
        cause: None,
        detail,
        ..Default::default()
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
            ..
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
