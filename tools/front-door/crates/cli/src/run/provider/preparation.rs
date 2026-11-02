use super::*;

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
            _ if name == "gh" => resolve_executable(name),
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
                ..
            } => Some((excluded_families.as_slice(), *require_distinct)),
            _ => None,
        },
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
                    || format!("{reason:?}"),
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
            "{}\n{}\n{text}",
            model_policy.guidance(),
            run::handoff_progress_guidance(&private_state.join("handoff.json"))
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
        let provider_route = adapter_route(&route);
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
        let provider = RouteRecord {
            provider: Some(route.adapter.clone()),
            model: Some(format!("{}/{}", route.adapter, route.model)),
            effort: route.effort,
            account_label: entry.map(|e| e.label.clone()),
            account_key: route.account_key,
            pool_selection: route.pool_selection.clone(),
            ..Default::default()
        };
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
