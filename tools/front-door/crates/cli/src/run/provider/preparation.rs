use super::*;

const DEPENDENCY_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const DEPENDENCY_STDERR_TAIL: usize = 4096;

pub(super) fn prepare_writer_dependencies(
    worktree: &Path,
    enabled: bool,
    locate: &dyn Fn(&str) -> Result<PathBuf, String>,
    cancel: &provenant_core::CancelToken,
) -> Result<serde_json::Value, String> {
    let command = "pnpm install --frozen-lockfile";
    let skipped = |reason| {
        serde_json::json!({
            "command": command,
            "outcome": "skipped",
            "reason": reason,
            "elapsed_micros": 0,
        })
    };
    if !enabled {
        return Ok(skipped("disabled"));
    }
    let lockfile = worktree.join("pnpm-lock.yaml");
    if !lockfile.is_file() {
        return Ok(skipped("no_pnpm_lockfile"));
    }
    // pnpm writes this marker last, so a bare or interrupted `node_modules`
    // is repaired by the install rather than trusted.
    if worktree.join("node_modules/.modules.yaml").is_file() {
        return Ok(skipped("already_installed"));
    }

    let executable = locate("pnpm").map_err(|error| format!("{command} failed: {error}"))?;
    let argv = [
        executable.as_os_str().to_owned(),
        "install".into(),
        "--frozen-lockfile".into(),
    ];
    let executor = provenant_os::exec::SystemExecutor::new(1)
        .map_err(|error| format!("{command} failed: {error}"))?;
    let mut options =
        provenant_os::exec::ExecOptions::new(&argv, worktree, DEPENDENCY_INSTALL_TIMEOUT);
    options.process_group = true;
    options.cleanup_process_group = true;
    options.cancel = Some(cancel);
    options.stdout = provenant_os::exec::StreamPolicy::Discard;
    options.stderr = provenant_os::exec::StreamPolicy::Tail {
        limit: DEPENDENCY_STDERR_TAIL,
    };
    use provenant_os::exec::Executor;
    let outcome = executor
        .execute(options)
        .map_err(|error| format!("{command} failed: {error}"))?;
    let elapsed_micros = outcome.elapsed_micros;
    let stderr = String::from_utf8_lossy(&outcome.stderr.bytes);
    let stderr_tail = stderr.trim();
    if outcome.termination != provenant_os::exec::Termination::Exit(0)
        || outcome.timed_out
        || outcome.cancelled
    {
        let detail = if stderr_tail.is_empty() {
            String::new()
        } else {
            format!("; stderr tail: {stderr_tail}")
        };
        let reason = if outcome.cancelled {
            "cancelled".to_owned()
        } else if outcome.timed_out {
            format!(
                "timed out after {} seconds",
                DEPENDENCY_INSTALL_TIMEOUT.as_secs()
            )
        } else {
            match outcome.termination {
                provenant_os::exec::Termination::Exit(code) => {
                    format!("exited with status {code}")
                }
                provenant_os::exec::Termination::Signal(signal) => {
                    format!("terminated by signal {signal}")
                }
            }
        };
        return Err(format!("{command} failed ({reason}){detail}"));
    }
    Ok(serde_json::json!({
        "command": command,
        "outcome": "installed",
        "elapsed_micros": elapsed_micros,
    }))
}

pub(crate) fn prepare(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    wait: Option<u64>,
) -> Result<run::PreparedLaunch, String> {
    prepare_with_wait(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        |name| match registry.host {
            Some(host) if matches!(&task.route, run::RequestedRoute::Adapter { provider, .. } if provider == "command") => {
                find_command_on_path(name, &host.executable_search_path)
            }
            None if matches!(&task.route, run::RequestedRoute::Adapter { provider, .. } if provider == "command") =>
            {
                let path = std::env::var_os("PATH").ok_or("PATH is unavailable")?;
                find_command_on_path(name, &std::env::split_paths(&path).collect::<Vec<_>>())
            }
            _ if name == "gh" || name == "pnpm" => resolve_executable(name),
            Some(host) => host_executable(host, name),
            None => resolve_executable(name),
        },
        None,
        wait,
    )
    .map_err(|error| error.to_string())
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ProviderLeaseError {
    #[error("provider lease wait cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

// Try every candidate before waiting. One shared deadline applies only while
// all candidates are updating; an unrelated busy provider never delays a lane.
pub(super) fn available_provider_leases(
    state: &Path,
    providers: &[&str],
    wait: Option<u64>,
    clock: &dyn provenant_core::Clock,
    cancel: &provenant_core::CancelToken,
) -> Result<std::collections::BTreeMap<String, provenant_os::fs::LockGuard>, ProviderLeaseError> {
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), wait.unwrap_or(30).min(30))
        .map_err(|error| ProviderLeaseError::Other(error.to_string()))?;
    loop {
        if cancel.is_cancelled() {
            return Err(ProviderLeaseError::Cancelled);
        }
        let mut leases = std::collections::BTreeMap::new();
        for provider in providers {
            match provenant_os::provider_fence::acquire(
                state,
                provider,
                provenant_os::fs::LockMode::Shared,
            ) {
                Ok(lease) => {
                    leases.insert((*provider).to_owned(), lease);
                }
                Err(FsError::LockTimeout { .. }) => {}
                Err(error) => return Err(ProviderLeaseError::Other(error.to_string())),
            }
        }
        if cancel.is_cancelled() {
            return Err(ProviderLeaseError::Cancelled);
        }
        if !leases.is_empty() || providers.is_empty() {
            return Ok(leases);
        }
        let now = clock.mono_now();
        let remaining = budget.remaining(now).as_micros();
        if remaining == 0 {
            return Ok(leases);
        }
        let next = now
            .checked_add(provenant_core::DurationMicros::from_micros(
                remaining.min(50_000),
            ))
            .map_err(|e| ProviderLeaseError::Other(e.to_string()))?;
        clock
            .wait_until(next)
            .map_err(|e| ProviderLeaseError::Other(e.to_string()))?;
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PrepareError {
    #[error(transparent)]
    Prompt(#[from] LaunchPromptError),
    #[error(transparent)]
    Run(#[from] run::RunError),
    #[error("{message}")]
    Candidates {
        message: String,
        plans: Vec<provenant_confinement::PlanError>,
    },
    #[error("{0}")]
    Other(String),
}
impl From<String> for PrepareError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}
impl From<&str> for PrepareError {
    fn from(message: &str) -> Self {
        Self::Other(message.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PlanCheckError {
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error("{0}")]
    Other(String),
}

#[cfg(any(test, feature = "fixture"))]
pub(super) fn prepare_with(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate: impl Fn(&str) -> Result<PathBuf, String>,
    fixture_token: Option<&provenant_providers::claude::OauthToken>,
) -> Result<run::PreparedLaunch, PrepareError> {
    prepare_with_wait(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        locate,
        fixture_token,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_with_wait(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate: impl Fn(&str) -> Result<PathBuf, String>,
    fixture_token: Option<&provenant_providers::claude::OauthToken>,
    wait: Option<u64>,
) -> Result<run::PreparedLaunch, PrepareError> {
    if let run::RequestedRoute::Adapter { provider, name, .. } = &task.route
        && provider == "host-native"
    {
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
        let native = config.domains.fabric.host_native.get(name).ok_or_else(|| {
            format!("host-native {name} is not configured in domains.fabric.host_native")
        })?;
        let provenant_fabric_runtime::config::HostNativeConfig::ClaudeAgent {
            model,
            effort,
            description,
            prompt,
        } = native;
        let mut delegated = task.clone();
        delegated.route = run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "claude".into(),
            model: model.clone(),
            effort: effort.clone(),
        };
        let mut launch = prepare_candidates(
            registry,
            &delegated,
            prompt_text,
            paths,
            None,
            &locate,
            fixture_token,
            false,
            wait,
        )?;
        if launch.route.model.as_deref() != Some(model) {
            return Err("host-native requires its configured model; use graph routes for non-native fallback".into());
        }
        let agents = serde_json::json!({name: {"description": description, "prompt": prompt}});
        launch.argv.extend([
            "--agents".into(),
            agents.to_string().into(),
            "--agent".into(),
            name.into(),
        ]);
        launch.route.source = Some(format!("domains.fabric.host_native.{name}"));
        launch
            .route
            .extra
            .insert("host_native".into(), name.clone().into());
        return Ok(launch);
    }
    prepare_candidates(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        &locate,
        fixture_token,
        false,
        wait,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_candidates(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate: &dyn Fn(&str) -> Result<PathBuf, String>,
    fixture_token: Option<&provenant_providers::claude::OauthToken>,
    redirected: bool,
    wait: Option<u64>,
) -> Result<run::PreparedLaunch, PrepareError> {
    if let run::RequestedRoute::Adapter { provider, name, .. } = &task.route {
        return prepare_command(registry, task, paths, resume, locate, provider, name)
            .map_err(PrepareError::from);
    }
    check_launch_prompt(prompt_text, paths, None)?;
    if task.fallback.is_empty()
        && let run::RequestedRoute::Model { model, .. } = &task.route
        && let Some((adapter, _)) = model.split_once('/')
    {
        check_launch_prompt(prompt_text, paths, Some(adapter))?;
    }
    // Only fixture builds may stand a supplied token in for host authority.
    #[cfg(not(feature = "fixture"))]
    let fixture_token: Option<&provenant_providers::claude::OauthToken> = {
        let _ = fixture_token;
        None
    };
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let home = canonical(registry.home.ok_or("home directory is unavailable")?)?;
    let state_root = registry.state_root.ok_or("state root is unavailable")?;
    let assets = registry.assets.ok_or("product assets are unavailable")?;
    let provenant_executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let provenant_executable = canonical(&provenant_executable)?;
    let git = canonical(&match registry.host {
        Some(host) => host.git.clone(),
        None => provenant_os::git::git_executable().ok_or("Git unavailable")?,
    })?;
    let project_id = registry
        .project_id
        .ok_or("selected project identity is unavailable")?;
    check_graph_inputs_root(
        state_root,
        project_id,
        task.context.as_ref(),
        task.access.inputs_root.as_deref(),
    )?;
    let writer = match &task.writer {
        Some(request) => Some(
            run::prepare_writer(request, roots.state.as_path(), project_id)
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    let cwd = match writer.as_ref() {
        Some(writer) => writer.worktree.path().to_owned(),
        None => canonical(registry.cwd.ok_or("working directory is unavailable")?)?,
    };
    if let (Some(writer), Some(head)) = (&writer, task.head.as_deref()) {
        writer
            .require_source(head.parse().map_err(|_| "invalid pinned writer source")?)
            .map_err(|error| error.to_string())?;
    }
    // Publishing is opt-in (`lanes.publish`): it hands the lane the GitHub
    // credential. Missing gh tools or configuration leave the
    // writer able to do local work without publishing credential access.
    let publishing_gh = publishing_requested(
        writer.is_some(),
        task.access.network(true) == NetworkAccess::Open,
        &task.profile.knobs,
    )
    .then(|| publishing_executable(&home, &locate))
    .flatten();
    let git_context = task
        .base
        .as_deref()
        .map(|base| match task.head.as_deref() {
            Some(head) => run::GitContext::capture_at(&git, &cwd, base, head),
            None => run::GitContext::capture(&git, &cwd, base),
        })
        .transpose()?;
    let user = provenant_os::sys::account(provenant_os::sys::Account::Current)
        .map_err(|error| error.to_string())?
        .ok_or("current user identity is unavailable")?
        .name;

    let executor = provenant_os::exec::SystemExecutor::new(1).map_err(|e| e.to_string())?;
    // Nested attempts live in a lane-writable child store. Unconfined provider
    // commands, including status, version and token refresh, get a fresh cwd
    // under the host configuration root retained in the launch context.
    let executor =
        provenant_providers::host::HostProbeExecutor::new(&executor, roots.state.as_path())
            .map_err(|e| e.to_string())?;
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?;
    // Continue the applied decision rather than redrawing the named pool
    // using the new operation seed.
    let mut request = match resume {
        Some(prior) => continuation_request(prior)?,
        None => route_request(task)?,
    };
    request.intent = route_request(task)?.intent;
    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
    let eligible = provenant_routing::probe_adapters(&request, &catalogue, &config.domains.routing)
        .map_err(|e| e.to_string())?;
    // Exclude updates during preparation when the bounded wait succeeds.
    let clock = provenant_os::clock::SystemClock::new();
    use provenant_core::Clock;
    let fallback_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&fallback_cancel);
    let provider_leases = available_provider_leases(
        roots.state.as_path(),
        &eligible.iter().map(|spec| spec.name).collect::<Vec<_>>(),
        wait,
        &clock,
        cancel,
    )
    .map_err(|error| error.to_string())?;
    let eligible = |name: &str| provider_leases.is_empty() || provider_leases.contains_key(name);
    let settings = config.domains.accounts.with_derived_accounts();
    let host_writes = config.domains.run;
    let mut entries: Vec<_> = settings.entries.into_values().collect();
    if eligible("codex") && !entries.iter().any(|e| e.adapter == "codex") {
        let directory = registry
            .host
            .and_then(|host| host.codex_home.clone())
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".codex"));
        if let Ok(directory) = canonical(&directory)
            && let Ok(executable) = locate("codex")
            && let Ok(key) = provenant_providers::accounts::detect(
                &executor,
                "codex",
                &executable,
                &home,
                &user,
                &directory,
                executor.root(),
            )
        {
            entries.push(provenant_providers::accounts::Registration {
                adapter: "codex".into(),
                label: "default".into(),
                key,
                org_fingerprint: None,
                directory,
                plan: None,
                priority: None,
                billing_anchor: None,
                plans: Vec::new(),
                directories: Vec::new(),
                limits: None,
                display_name: None,
                hidden: false,
                order: None,
                headline: None,
                settings: Default::default(),
            });
        }
    }
    // These adapters use the CLI's existing local login and configuration.
    for adapter in ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.local_login && eligible(spec.name))
        .map(|spec| spec.name)
    {
        if !entries.iter().any(|entry| entry.adapter == adapter) && locate(adapter).is_ok() {
            entries.push(provenant_providers::accounts::Registration {
                adapter: adapter.into(),
                label: if adapter == "opencode" {
                    "go"
                } else {
                    "default"
                }
                .into(),
                key: if adapter == "opencode" { "go" } else { "local" }.into(),
                org_fingerprint: None,
                directory: home.clone(),
                plan: None,
                priority: None,
                billing_anchor: None,
                plans: Vec::new(),
                directories: Vec::new(),
                limits: None,
                display_name: None,
                hidden: false,
                order: None,
                headline: None,
                settings: Default::default(),
            });
        }
    }
    // Include a distinct Zen identity for an unregistered local OpenCode login.
    let settings = provenant_providers::accounts::Settings {
        entries: entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| (i.to_string(), entry))
            .collect(),
        ..Default::default()
    }
    .with_derived_accounts();
    let entries: Vec<_> = settings.entries.into_values().collect();
    let timestamp = clock.unix_seconds();
    let mut availability = Vec::new();
    let mut reasons: Vec<SkippedRoute> = Vec::new();
    let probe = Probe {
        roots: registry.roots,
        executor: &executor,
        home: &home,
        user: &user,
        scratch: &paths.scratch,
    };
    for entry in entries.iter().filter(|entry| eligible(&entry.adapter)) {
        match locate(&entry.adapter).and_then(|executable| {
            observe_account(entry, &executable, &probe, catalogue.revision(), timestamp)
                .map_err(|error| error.to_string())
        }) {
            Ok(record) => availability.push(record),
            Err(error) => reasons.push(skip(
                &entry.adapter,
                SkipKind::Account,
                format!("account {}: {error}", entry.label),
            )),
        }
    }
    // Availability is diagnostic evidence; admission uses the in-memory records.
    // A failed publication must not prevent a correctly prepared lane launching.
    let _ = (|| -> Result<(), String> {
        let attempt_dir =
            Directory::open_anchor(paths.scratch.parent().ok_or("attempt path unavailable")?)
                .map_err(|e| e.to_string())?;
        attempt_dir
            .atomic_write(
                std::ffi::OsStr::new("availability.json"),
                "availability",
                &serde_json::to_vec(&availability).map_err(|e| e.to_string())?,
                PublishMode::Replace,
            )
            .and_then(|p| p.require_durable())
            .map_err(|e| e.to_string())
    })();
    let digest = task.authority_digest.to_string();
    let mut access = Vec::new();
    let mut plan_errors = std::collections::BTreeMap::new();
    let mut reported_plan_errors = std::collections::BTreeSet::new();
    for adapter in REGISTRY
        .iter()
        .map(|entry| entry.adapter.id())
        .filter(|adapter| eligible(adapter))
    {
        // Repository-bound read-only grants are checked per candidate below.
        match confinement_request(
            project_id,
            ConfinementRoots {
                cwd: &cwd,
                home: &home,
                scratch: &paths.scratch,
                private_state: &paths.provider_state,
                assets,
                prompt_file: None,
            },
            writer.as_ref(),
            &task.access,
            &host_writes,
            adapter,
            publishing_gh.is_some(),
        )
        .map_err(PlanCheckError::Other)
        .and_then(|request| ConfinedPlan::new(request).map_err(PlanCheckError::from))
        {
            Ok(_) => access.push(provenant_routing::AccessContext {
                adapter,
                disclosure_ref: &digest,
                confinement_ref: &digest,
            }),
            Err(error) => {
                plan_errors.insert(adapter, error);
            }
        }
    }
    let requested_adapter = match &task.route {
        run::RequestedRoute::Adapter { provider, .. } => Some(provider.as_str()),
        run::RequestedRoute::Model { model, .. } => {
            model.split_once('/').map(|(adapter, _)| adapter)
        }
        run::RequestedRoute::Named { .. } | run::RequestedRoute::Review { .. } => None,
    };
    // A continuation keeps the prior attempt's account, never a fresh pick.
    let prior_account = resume
        .and_then(|prior| prior.applied_route.as_ref())
        .and_then(|route| route.account_key.as_deref());
    let selected = task
        .account
        .as_deref()
        .filter(|_| resume.is_none())
        .map(|label| {
            let mut matching = entries.iter().filter(|e| {
                (e.label == label || e.key == label)
                    && requested_adapter.is_none_or(|adapter| e.adapter == adapter)
            });
            let entry = matching.next().ok_or_else(|| {
                format!("account {label} is not registered for the requested route")
            })?;
            if matching.any(|other| other.key != entry.key) {
                return Err(format!(
                    "account alias {label} is ambiguous; use its exact registered key"
                ));
            }
            Ok(entry.key.as_str())
        })
        .transpose()?
        .or(prior_account);
    let host = provenant_config::bind_host(roots)
        .map_err(|e| e.to_string())?
        .to_string();
    let rules = crate::route::task_exclusion_rules(&task.profile, roots)
        .map_err(|error| error.to_string())?;
    let resolved = crate::route::resolve_dispatch(
        &request,
        roots,
        selected,
        (paths.operation_id.as_str(), paths.task_index),
        &availability,
        &access,
        &host,
        match &task.route {
            run::RequestedRoute::Review {
                excluded_families,
                distinct_families: true,
                require_distinct,
                preferred_families,
                ..
            } => Some((
                excluded_families.as_slice(),
                *require_distinct,
                preferred_families.as_slice(),
            )),
            _ => None,
        },
        &rules,
    )
    .map_err(|error| error.to_string())?;
    if let Some(selection) = &resolved.pool_selection {
        for redraw in &selection.redraws {
            reasons.push(skip(
                &redraw.rejected_pick,
                SkipKind::Unavailable,
                &redraw.reason,
            ));
        }
    }
    for (candidate, route) in candidates(resolved).into_iter().enumerate() {
        let label = format!("{}/{}", route.adapter, route.model);
        let effective = config.domains.routing.model_policy.effective(
            &provenant_routing::ModelId::parse(&label).map_err(|error| error.to_string())?,
            route.account_key.as_deref(),
            clock.wall_now().map_err(|error| error.to_string())?,
        );
        let fallback_dropped = effective.dropped_fallback;
        let model_policy = effective.policy;
        if !model_policy.permits(request.intent) {
            reasons.push(skip(&label, SkipKind::Refused, "read-only model policy"));
            continue;
        }
        // A child store is writable by its parent lane. Pick the reliable
        // fallback before launch there, rather than trust a later retry record.
        if paths.lane_depth > 1
            && model_policy.result.check == Some(true)
            && !request.intent.writes
            && resume.is_none()
        {
            if redirected {
                reasons.push(skip(
                    &label,
                    SkipKind::Refused,
                    "restricted nested route needs a reliable read-only fallback",
                ));
                continue;
            }
            let fallback = nested_policy_fallback(task, &route, &model_policy)?;
            let mut prepared = prepare_candidates(
                registry,
                &fallback,
                prompt_text,
                paths,
                None,
                locate,
                fixture_token,
                true,
                wait,
            )?;
            prepared.skipped_routes.insert(
                0,
                skip(
                    &label,
                    SkipKind::Refused,
                    "nested route selected its result-policy fallback before launch",
                ),
            );
            return Ok(prepared);
        }
        let Some(registration) = REGISTRY
            .iter()
            .find(|registration| registration.adapter.id() == route.adapter)
        else {
            reasons.push(skip(&label, SkipKind::Unavailable, "unknown adapter"));
            continue;
        };
        let adapter = registration.adapter;
        if let provenant_records::RouteReadiness::Unavailable { reason, .. } = &route.readiness {
            // An adapter left out of `access` names its confinement error, not the router's denial.
            let detail = plan_errors
                .get(route.adapter.as_str())
                .filter(|_| {
                    matches!(
                        reason,
                        provenant_records::RouteExclusionReason::DisclosureDenied
                    )
                })
                .map_or_else(
                    || provenant_routing::exclusion_detail(reason),
                    |error| {
                        reported_plan_errors.insert(route.adapter.clone());
                        format!("confinement plan: {error}")
                    },
                );
            reasons.push(skip(&label, SkipKind::Unavailable, detail));
            continue;
        }
        if let Err(error) = crate::route::check_dispatch_candidate(roots, &route) {
            reasons.push(skip(&label, SkipKind::Unavailable, error));
            continue;
        }
        let entry = entries
            .iter()
            .find(|e| e.adapter == route.adapter && Some(&e.key) == route.account_key.as_ref());
        let account = match entry
            .filter(|entry| {
                AdapterSpec::get(&entry.adapter).is_some_and(|spec| {
                    spec.registered_account && !spec.is_unregistered_local(&entry.key)
                })
            })
            .map(|entry| entry.registered())
            .transpose()
        {
            Ok(account) => account,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Account, format!("account: {error}")));
                continue;
            }
        };
        // An unregistered local login runs without a pinned account.
        if AdapterSpec::get(adapter.id()).is_some_and(|spec| {
            spec.registered_account
                && !route
                    .account_key
                    .as_deref()
                    .is_some_and(|key| spec.is_unregistered_local(key))
        }) && account.is_none()
        {
            reasons.push(skip(
                &label,
                SkipKind::Account,
                "account is not registered; use accounts register",
            ));
            continue;
        }
        let executable = match locate(adapter.id()) {
            Ok(path) => path,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Unavailable, error));
                continue;
            }
        };
        // Every lane stages temporary files under tmp for terminal cleanup.
        // A failed setup never poisons the next explicit fallback candidate.
        let staging = &paths.temporary;
        let scratch = staging.join(format!("candidate-{candidate}"));
        let private_state = paths.provider_state.join(format!("candidate-{candidate}"));
        // Retain the handle opened while creating scratch: a parent lane can
        // rename tmp and redirect its pathname before context is published.
        let scratch_directory =
            match apply_setup_directory(&[staging], &scratch).and_then(|directory| {
                apply_setup(
                    &[SetupStep::CreateDir(private_state.clone())],
                    &[&paths.provider_state],
                    None,
                )?;
                Ok(directory)
            }) {
                Ok(directory) => directory,
                Err(error) => {
                    reasons.push(skip(&label, SkipKind::Setup, error));
                    continue;
                }
            };
        let continuation = match resume {
            Some(prior) => match continuation_for(
                prior,
                &route,
                &task.authority_digest.to_string(),
                state_root,
                &private_state,
            ) {
                Ok(continuation) => Some(continuation),
                Err(error) => {
                    reasons.push(skip(&label, SkipKind::Refused, error));
                    continue;
                }
            },
            None => None,
        };
        let (scratch, private_state) = (scratch.as_path(), private_state.as_path());
        Directory::open_anchor(private_state)
            .and_then(|directory| {
                directory.atomic_write(
                    std::ffi::OsStr::new("provider.txt"),
                    ".provider-",
                    adapter.id().as_bytes(),
                    provenant_os::fs::PublishMode::Replace,
                )
            })
            .map_err(|error| error.to_string())?;
        let text = match &git_context {
            Some(context) => {
                let location = context
                    .write_to(&scratch_directory, scratch)
                    .map_err(|error| error.to_string())?;
                format!("{location}\n{prompt_text}")
            }
            None => prompt_text.to_owned(),
        };
        let text = format!(
            "{}\n{text}",
            run::lane_preamble(
                &model_policy.guidance(),
                writer.is_some(),
                &private_state.join("handoff.json")
            )
        );
        let prompt = match launch_prompt(&text, paths, state_root, adapter.id()) {
            Ok(prompt) => prompt,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Refused, error.to_string()));
                continue;
            }
        };
        let child_state = state_root
            .join("children")
            .join(paths.attempt_id.to_string());
        let (plan, observed_head, repository) = match confinement_request(
            project_id,
            ConfinementRoots {
                cwd: &cwd,
                home: &home,
                scratch,
                private_state,
                assets,
                prompt_file: prompt.file(),
            },
            writer.as_ref(),
            &task.access,
            &host_writes,
            adapter.id(),
            publishing_gh.is_some(),
        )
        .and_then(|request| match writer.as_ref() {
            Some(writer) => ConfinedPlan::new(request)
                .map(|plan| {
                    (
                        plan,
                        None,
                        provenant_records::AbsolutePath::new(
                            writer.worktree.repo().common_git_dir(),
                        )
                        .ok(),
                    )
                })
                .map_err(|error| error.to_string()),
            None => read_only_plan(roots, project_id, &cwd, &git, request),
        }) {
            Ok(planned) => planned,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Refused, error));
                continue;
            }
        };
        let refreshed = if adapter.id() == "claude" && fixture_token.is_none() {
            let account = account.as_ref().ok_or("Claude account is not registered")?;
            let login = provenant_providers::claude::Login {
                executable: &executable,
                home: &home,
                user: &user,
                config_dir: account.directory(),
            };
            let authority = match credential_authority(roots.state.as_path(), paths, &login) {
                Ok(authority) => authority,
                Err(error) => {
                    reasons.push(skip(&label, SkipKind::AuthRequired, error));
                    continue;
                }
            };
            let now = || {
                clock
                    .wall_now()
                    .ok()
                    .and_then(|at| u64::try_from(at.unix_millis()).ok())
                    .unwrap_or(0)
            };
            match provenant_providers::claude::refresh_for_account_authorised(
                &executor,
                &login,
                account,
                &now,
                paths.deadline_unix_ms,
                &authority,
            ) {
                Ok(token) => Some(token),
                Err(error) => {
                    reasons.push(skip(
                        &label,
                        SkipKind::AuthRequired,
                        credential_failure_message(
                            &error,
                            entry.map_or("unknown", |e| e.label.as_str()),
                            account.directory(),
                        ),
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        let token = match adapter.id() {
            "claude" => fixture_token.or(refreshed.as_ref()),
            _ => None,
        };
        let mut provider_route = adapter_route(&route);
        let project_id_str = project_id.to_string();
        let project_key = format!("domains.lanes.projects.{project_id_str}.user_skills");
        let project_override = config
            .domains
            .lanes
            .projects
            .get(&project_id_str)
            .and_then(|project| project.user_skills);
        let user_skills_mode = model_policy
            .lane
            .user_skills
            .unwrap_or_else(|| config.domains.lanes.user_skills_for(Some(&project_id_str)));
        let user_skills_origin = if model_policy.lane.user_skills.is_some() {
            serde_json::json!({"route_policy": effective.sources})
        } else {
            let key = if project_override.is_some() {
                project_key.as_str()
            } else {
                "domains.lanes.user_skills"
            };
            serde_json::json!({"key": key, "origin": config.origins.get(key)})
        };
        provider_route.set_user_skills(user_skills_mode.is_discover());
        let nonce = entropy_128().map_err(|error| error.to_string())?;
        let grant_digest = task.authority_digest.to_string();
        let mut spec = match adapter.prepare(PrepareInput {
            command: None,
            route: &provider_route,
            confined: &plan,
            prompt: &prompt,
            continuation: continuation.as_ref(),
            grant_digest: &grant_digest,
            executable: &executable,
            home: &home,
            user: &user,
            host_var: &|key| std::env::var_os(key),
            account: account.as_ref(),
            token,
            cwd: &cwd,
            git_context: git_context.is_some(),
            nonce,
            provenant: LaneRuntime {
                executable: &provenant_executable,
                git: &git,
                state: roots.state.as_path(),
                child_state: &child_state,
                home: &home,
                depth: paths.lane_depth,
                attempt: paths.attempt_id,
                assets,
            },
        }) {
            Ok(spec) => spec,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Refused, error.to_string()));
                continue;
            }
        };
        if let Some(alias) = &paths.temporary_alias {
            alias_temporary_env(
                &mut spec.env,
                scratch,
                &alias.join(format!("candidate-{candidate}")),
            );
        }
        spec.profile.spec.allow_database_ipc = task.access.database_ipc;
        configure_process_tree(&mut spec, paths)?;
        let build_settings = config.domains.fabric.lanes.build_settings();
        configure_build_slots(
            &mut spec,
            &build_settings,
            &home,
            scratch,
            &provenant_executable,
            roots.state.as_path(),
        )?;
        if !host_indexes_protected(
            roots.state.as_path(),
            &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
        ) {
            return Err("write grants overlap a host index".into());
        }
        if !target_seeds_protected(
            roots.state.as_path(),
            &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
        ) {
            return Err("write grants overlap the host target seed store".into());
        }
        if let Some(head) = &task.head {
            spec.env.push(("PROVENANT_LANE_HEAD".into(), head.into()));
            if let Some(base) = &task.base {
                spec.env.push(("PROVENANT_LANE_BASE".into(), base.into()));
            }
        }
        set_cargo_profile(&mut spec.env, &build_settings);
        spec.env.extend(
            model_policy
                .launch_env(adapter.id())
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        set_cargo_jobs(
            &mut spec.env,
            registry
                .host
                .and_then(|host| host.cargo_build_jobs.as_deref()),
            build_settings.jobs(),
        );
        if adapter.id() == "claude" {
            spec.configure_claude_shell(scratch)
                .map_err(|error| error.to_string())?;
        }
        if let Some(gh) = publishing_gh.as_ref() {
            let policy = provenant_config::GithubPolicy::read(&roots.state, project_id)
                .map_err(|error| error.to_string())?;
            let publishing = spec
                .configure_publishing(scratch, &git, gh, &home, &policy)
                .map_err(|error| error.to_string());
            if let Err(error) = publishing {
                reasons.push(skip(&label, SkipKind::Setup, error));
                continue;
            }
        }
        if git_context.is_some() && adapter.id() == "claude" {
            // Scratch is already granted by the outer profile. Claude's file
            // tools also need it in their working directories to read context.
            spec.argv
                .splice(1..1, ["--add-dir".into(), scratch.as_os_str().to_owned()]);
        }
        let transcript_account = account
            .as_ref()
            .filter(|_| adapter.id() == "claude")
            .map(|account| account.directory());
        let mut setup_roots = vec![scratch, private_state, roots.state.as_path()];
        // Only directory creation uses these additional provider state roots;
        // generated files and links remain confined to attempt-owned roots.
        let opencode_data = spec
            .env
            .iter()
            .find(|(key, _)| key == "XDG_DATA_HOME")
            .map(|(_, value)| Path::new(value));
        if adapter.id() == "opencode" {
            if let Some(data) = opencode_data {
                let existing = data
                    .ancestors()
                    .find(|path| path.is_dir())
                    .ok_or("OpenCode data directory has no existing ancestor")?;
                setup_roots.push(existing);
            }
        } else if adapter.id() == "cursor" {
            setup_roots.push(&home);
        }
        if let Err(error) = apply_setup(&spec.setup, &setup_roots, transcript_account) {
            reasons.push(skip(
                &label,
                SkipKind::Setup,
                format!("setup failed: {error}"),
            ));
            continue;
        }
        for warning in config.domains.routing.pool_effort_warnings(&catalogue) {
            let _ = writeln!(std::io::stderr().lock(), "{warning}");
        }
        if let Some((fallback, reason)) = &fallback_dropped {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: {label}: model policy fallback {} ({reason}); dropped",
                fallback.as_str()
            );
        }
        if writer.is_some()
            && let Some(advisory) = config
                .domains
                .routing
                .opus_writer_advisory(&catalogue, &route)
        {
            // Advice must never refuse an otherwise valid dispatch, even if
            // the caller has closed its diagnostic stream.
            let _ = writeln!(std::io::stderr().lock(), "{advisory}");
        }
        // Install on the host only once the head, route and lane setup are
        // known to be usable, and before the sandbox is entered.
        let dependency_event = match &writer {
            Some(writer) => {
                let enabled = task
                    .profile
                    .knobs
                    .get("lanes.prepare_deps")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                Some(prepare_writer_dependencies(
                    writer.worktree.path(),
                    enabled,
                    locate,
                    cancel,
                )?)
            }
            None => None,
        };
        let mut provider = launch_route_record(
            &route,
            entry.map(|e| e.label.as_str()),
            &task.route,
            resume.is_some(),
            redirected || candidate > 0,
        );
        provider.extra.insert(
            "account_source".into(),
            account_source(resume.is_some(), task.account.is_some()).into(),
        );
        if let Some(event) = &dependency_event {
            provider
                .extra
                .insert("dependency_preparation".into(), event.clone());
        }
        provider.extra.insert(
            "user_skills".into(),
            user_skills_exposure(
                adapter.id(),
                user_skills_mode,
                &home,
                &|key| std::env::var_os(key),
                user_skills_origin,
                &spec.omitted_discovery,
            ),
        );
        let stdin = match spec.stdin {
            Stdin::Bytes(bytes) => bytes,
            Stdin::Null => Vec::new(),
        };
        return Ok(run::PreparedLaunch {
            skipped_routes: reasons,
            argv: spec.argv,
            environment: spec.env,
            stdin,
            secret_input: spec.secret_input,
            cwd: spec.cwd,
            route: provider,
            observed_head,
            repository,
            confinement: Some(run::LaunchConfinement {
                plan,
                profile: spec.profile,
                writer,
                home: home.clone(),
                cargo_registry_seed: config.domains.fabric.lanes.cargo_seed.enabled,
                state_root: roots.state.as_path().to_path_buf(),
            }),
        });
    }
    Err(PrepareError::Candidates {
        message: reasons
            .iter()
            .map(|skipped| format!("{}: {}", skipped.route, skipped.detail))
            .collect::<Vec<_>>()
            .join("; "),
        plans: plan_errors
            .into_iter()
            .filter(|(adapter, _)| reported_plan_errors.contains(*adapter))
            .filter_map(|(_, error)| match error {
                PlanCheckError::Plan(error) => Some(error),
                PlanCheckError::Other(_) => None,
            })
            .collect(),
    })
}

/// The one state-root path a lane may read is its own node's graph inputs,
/// rebuilt here from the task's project, graph and node rather than trusted
/// from the request. The request must name exactly that directory, and the
/// directory must resolve to it with no link on the way, so neither the shared
/// root, another node's directory nor a redirected path is granted.
pub(super) fn check_graph_inputs_root(
    state_root: &Path,
    project: impl std::fmt::Display,
    context: Option<&provenant_records::run::WorkContext>,
    inputs: Option<&Path>,
) -> Result<(), String> {
    let Some(inputs) = inputs else {
        return Ok(());
    };
    let refused = || "graph inputs root is not this task's own node directory".to_owned();
    let (graph_id, node_id) = context
        .and_then(|context| context.graph_id.as_deref().zip(context.node_id.as_deref()))
        .ok_or_else(refused)?;
    let expected =
        graph_node_inputs_directory(state_root, &project, graph_id, node_id).ok_or_else(refused)?;
    if inputs != expected {
        return Err(refused());
    }
    let relative = expected.strip_prefix(state_root).map_err(|_| refused())?;
    let direct = canonical(state_root)?.join(relative);
    if canonical(&expected).map_err(|_| refused())? != direct {
        return Err(refused());
    }
    Ok(())
}

fn account_source(continuation: bool, pinned: bool) -> &'static str {
    if continuation {
        "continuation"
    } else if pinned {
        "pin"
    } else {
        "automatic"
    }
}

/// Preserve the resolver's decision at the launch boundary.
fn launch_route_record(
    route: &ResolvedRoute,
    account_label: Option<&str>,
    requested: &run::RequestedRoute,
    continuation: bool,
    fallback: bool,
) -> RouteRecord {
    let source = if fallback {
        "fallback"
    } else if continuation {
        "continuation"
    } else if matches!(requested, run::RequestedRoute::Model { .. }) {
        "pin"
    } else if route.pool_selection.is_some() {
        "pool"
    } else {
        "named_route"
    };
    RouteRecord::from_resolved(route, account_label, source)
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use provenant_routing::{
        Catalogue, Fallback, NamedRoute, RouteChoice, RouteName, RouteRequest, RoutingConfig,
        RoutingContext,
    };

    #[test]
    fn alias_effort_reaches_launch_record() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(account_source(true, true), "continuation");
        assert_eq!(account_source(false, true), "pin");
        assert_eq!(account_source(false, false), "automatic");
        let catalogue = Catalogue::bundled()?;
        let mut config = RoutingConfig::default();
        config.routes.insert(
            "fixture".into(),
            NamedRoute {
                model: provenant_routing::ModelId::parse("codex/gpt-6.1-sol")?,
                effort: Some("high".into()),
            },
        );
        let context = RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability: &[],
            quota: &[],
            capacity_cooldowns: &[],
            accounts: &[],
            quota_policies: &[],
            readiness: None,
            access: &[],
            now: provenant_core::WallTime::from_micros(100_000_000)?,
            host: "fixture",
        };
        let request = RouteRequest {
            intent: Default::default(),
            choice: RouteChoice::NamedRoute(RouteName::parse("fixture")?),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        };
        let resolved = provenant_routing::resolve(&request, &context)?;
        let requested = run::RequestedRoute::Named {
            extra: Default::default(),
            name: "fixture".into(),
            effort: None,
        };
        let record = launch_route_record(&resolved, Some("work"), &requested, false, false);
        let wire = serde_json::to_value(&record)?;
        assert_eq!(wire["model"], "codex/gpt-6.1-sol");
        assert_eq!(wire["effort"], "high");
        assert_eq!(wire["account_label"], "work");
        assert_eq!(wire["source"], "named_route");
        let pinned = run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "codex".into(),
            model: "codex/gpt-6.1-sol".into(),
            effort: Some("high".into()),
        };
        assert_eq!(
            launch_route_record(&resolved, None, &pinned, false, false)
                .source
                .as_deref(),
            Some("pin")
        );
        assert_eq!(
            launch_route_record(&resolved, None, &requested, false, true)
                .source
                .as_deref(),
            Some("fallback")
        );
        assert!(wire.get("prompt").is_none());
        assert!(wire.get("credential").is_none());
        Ok(())
    }
}

pub(super) fn discovery_warnings(omitted: &[PathBuf], home: &Path) -> Vec<String> {
    omitted
        .iter()
        .map(|path| {
            let display = path
                .strip_prefix(home)
                .map(|relative| format!("~/{}", relative.display()))
                .unwrap_or_else(|_| path.display().to_string());
            format!("omitted discovered grant {display}: credential root guard")
        })
        .collect()
}

/// Report the same provider-owned locations and grants used during prepare.
fn user_skills_exposure(
    adapter: &str,
    mode: provenant_records::UserSkillsMode,
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    origin: serde_json::Value,
    omitted: &[PathBuf],
) -> serde_json::Value {
    let directory = mode
        .is_discover()
        .then(|| provenant_providers::user_skills_directory(adapter, home, host_var))
        .flatten();
    let reads = directory
        .as_ref()
        .map(|path| provenant_providers::user_skills_grants_with_omissions(path, Some(home)).0)
        .unwrap_or_default();
    let warnings = discovery_warnings(omitted, home);
    serde_json::json!({
        "mode": mode,
        "exposed": !reads.is_empty(),
        "directory": directory,
        "read_grants": reads,
        "warnings": warnings,
        "source": origin,
    })
}

#[cfg(test)]
mod user_skills_tests {
    use super::*;
    #[test]
    fn agy_credential_skills_are_omitted_with_warning() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        std::fs::create_dir_all(home.join(".gemini/skills"))?;
        let record = user_skills_exposure(
            "agy",
            provenant_records::UserSkillsMode::Discover,
            &home,
            &|_| None,
            serde_json::Value::Null,
            &[home.join(".gemini/skills")],
        );
        assert_eq!(record["exposed"], false);
        assert_eq!(record["read_grants"], serde_json::json!([]));
        let warning = record["warnings"][0].as_str().ok_or("missing warning")?;
        assert!(warning.contains("~/.gemini/skills"));
        assert!(warning.contains("credential root"));
        Ok(())
    }

    #[test]
    fn user_skills_record_reports_mode_origin_and_canonical_grants()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let skills = home.join(".agents/skills");
        let target = home.join("release/skill");
        std::fs::create_dir_all(&skills)?;
        std::fs::create_dir_all(&target)?;
        std::os::unix::fs::symlink(&target, skills.join("skill"))?;
        let origin = serde_json::json!({"key": "domains.lanes.user_skills", "origin": "compiled"});
        let record = user_skills_exposure(
            "codex",
            provenant_records::UserSkillsMode::Discover,
            &home,
            &|_| None,
            origin.clone(),
            &[],
        );
        assert_eq!(record["exposed"], true);
        assert_eq!(record["directory"], serde_json::json!(skills));
        assert_eq!(
            record["read_grants"]
                .as_array()
                .ok_or("missing grants")?
                .len(),
            2
        );
        assert_eq!(record["source"], origin);
        let off = user_skills_exposure(
            "codex",
            provenant_records::UserSkillsMode::Off,
            &home,
            &|_| None,
            serde_json::json!({"route_policy": ["rules[0]"]}),
            &[],
        );
        assert_eq!(off["mode"], "off");
        assert_eq!(off["exposed"], false);
        assert_eq!(off["directory"], serde_json::Value::Null);
        assert_eq!(off["read_grants"], serde_json::json!([]));
        Ok(())
    }
}
