//! Provider preparation at the CLI composition edge.
use super::{Registry, route_request};
use provenant_confinement::permissions::{
    AccessMode, NetworkAccess, PathGrant, PathScope, PlanPermissions, WorktreeWriteAuthority,
};
use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
use provenant_core::{ProjectId, WorkScope};
use provenant_os::fs::{Directory, FileKind, FsError, PayloadTree, PublishMode, entropy_128};
use provenant_providers::{LaneRuntime, PrepareInput, Prompt, REGISTRY, SetupStep, Stdin};
use provenant_records::{ADAPTER_SPECS, AdapterSpec};
use provenant_records::{ResolvedRoute, RouteAlternative};
use provenant_run as run;
use run::{RouteRecord, SkipKind, SkippedRoute};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) fn prepare(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
) -> Result<run::PreparedLaunch, String> {
    prepare_with(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        |name| match registry.host {
            _ if name == "gh" => resolve_executable(name),
            Some(host) => host_executable(host, name),
            None => resolve_executable(name),
        },
        None,
    )
}

// Try every candidate before waiting. One shared deadline applies only while
// all candidates are updating; an unrelated busy provider never delays a lane.
fn available_provider_leases(
    state: &Path,
    providers: &[&str],
    budget: provenant_core::WaitBudget,
    clock: &dyn provenant_core::Clock,
    cancel: &provenant_core::CancelToken,
) -> Result<std::collections::BTreeMap<String, provenant_os::fs::LockGuard>, String> {
    loop {
        if cancel.is_cancelled() {
            return Err("provider lease wait cancelled".into());
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
                Err(error) => return Err(error.to_string()),
            }
        }
        if cancel.is_cancelled() {
            return Err("provider lease wait cancelled".into());
        }
        if !leases.is_empty() || providers.is_empty() {
            return Ok(leases);
        }
        let now = clock.mono_now();
        let remaining = budget.remaining(now).as_micros();
        if remaining == 0 {
            return Err("all candidate providers are updating; retry the lane".into());
        }
        let next = now
            .checked_add(provenant_core::DurationMicros::from_micros(
                remaining.min(50_000),
            ))
            .map_err(|e| e.to_string())?;
        clock.wait_until(next).map_err(|e| e.to_string())?;
    }
}

fn prepare_with(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate: impl Fn(&str) -> Result<PathBuf, String>,
    fixture_token: Option<&provenant_providers::claude::OauthToken>,
) -> Result<run::PreparedLaunch, String> {
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
    // Publishing is optional. Missing gh tools or configuration leave the
    // writer able to do local work without publishing credential access.
    let publishing_gh = (writer.is_some()
        && task.access.network(true) == NetworkAccess::Open
        && task
            .profile
            .knobs
            .get("lanes.publish")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true))
    .then(|| publishing_executable(&home, &locate))
    .flatten();
    let git_context = task
        .base
        .as_deref()
        .map(|base| match task.head.as_deref() {
            Some(head) => run::GitContext::capture_at(&git, &cwd, base, head),
            None => run::GitContext::capture(&git, &cwd, base),
        })
        .transpose()
        .map_err(|error| error.to_string())?;
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
    let request = match resume {
        Some(prior) => continuation_request(prior)?,
        None => route_request(task)?,
    };
    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
    let eligible = provenant_routing::probe_adapters(&request, &catalogue, &config.domains.routing)
        .map_err(|e| e.to_string())?;
    // Resolve and probe only while updates are excluded. Keep the selected
    // lease through handoff, including for fallback and prepared continuations.
    let clock = provenant_os::clock::SystemClock::new();
    use provenant_core::Clock;
    let fallback_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&fallback_cancel);
    let mut provider_leases = available_provider_leases(
        roots.state.as_path(),
        &eligible.iter().map(|spec| spec.name).collect::<Vec<_>>(),
        provenant_core::WaitBudget::cli(clock.mono_now(), 30).map_err(|e| e.to_string())?,
        &clock,
        cancel,
    )?;
    let eligible = |name: &str| provider_leases.contains_key(name);
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
                limits: None,
                display_name: None,
                hidden: false,
                order: None,
                headline: None,
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
                limits: None,
                display_name: None,
                hidden: false,
                order: None,
                headline: None,
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
    }
    .with_derived_accounts();
    let entries: Vec<_> = settings.entries.into_values().collect();
    let timestamp = jiff::Timestamp::now().as_second();
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
        .and_then(|request| ConfinedPlan::new(request).map_err(|error| error.to_string()))
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
                ..
            } => Some(excluded_families.as_slice()),
            _ => None,
        },
    )?;
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
                    |error| format!("confinement plan: {error}"),
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
                AdapterSpec::get(&entry.adapter).is_some_and(|spec| spec.registered_account)
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
        if AdapterSpec::get(adapter.id()).is_some_and(|spec| spec.registered_account)
            && account.is_none()
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
        let prompt = match launch_prompt(&text, paths, state_root, adapter.id()) {
            Ok(prompt) => prompt,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Refused, error));
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
            let now = || u64::try_from(jiff::Timestamp::now().as_millisecond()).unwrap_or(0);
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
        let jobs = provenant_config::LaneBuildSettings {
            max_concurrent_builds: config.domains.fabric.lanes.max_concurrent_builds,
            cargo_build_jobs: config.domains.fabric.lanes.cargo_build_jobs,
        }
        .jobs();
        set_cargo_jobs(
            &mut spec.env,
            registry
                .host
                .and_then(|host| host.cargo_build_jobs.as_deref()),
            jobs,
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
            provider_lease: provider_leases.remove(adapter.id()),
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
    Err(reasons
        .iter()
        .map(|skipped| format!("{}: {}", skipped.route, skipped.detail))
        .collect::<Vec<_>>()
        .join("; "))
}

fn set_cargo_jobs(
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

#[cfg(test)]
mod cargo_jobs_tests {
    use super::set_cargo_jobs;
    #[test]
    fn cargo_jobs_respect_supplied_environment() {
        let mut env = vec![("CARGO_BUILD_JOBS".into(), "7".into())];
        set_cargo_jobs(&mut env, Some("9"), 3);
        assert_eq!(env[0].1, "7");
        let mut env = Vec::new();
        set_cargo_jobs(&mut env, Some("9"), 3);
        assert_eq!(env[0].1, "9");
        let mut env = Vec::new();
        set_cargo_jobs(&mut env, None, 3);
        assert_eq!(env[0].1, "3");
    }
}

/// A candidate that did not launch, typed by why it was skipped.
fn skip(route: &str, kind: SkipKind, detail: impl Into<String>) -> SkippedRoute {
    SkippedRoute {
        extra: Default::default(),
        route: route.to_owned(),
        kind,
        detail: detail.into(),
    }
}

/// Recheck current readiness for the exact route the provider session used.
/// Resolving an exact model bypasses named-pool selection and quota redraws.
fn continuation_request(
    prior: &run::AttemptRecord,
) -> Result<provenant_routing::RouteRequest, String> {
    use provenant_routing::{Fallback, ModelId, RouteChoice};

    let applied = prior.applied_route.as_ref().ok_or(
        "the prior attempt recorded no applied route; run provenant run with its result as context",
    )?;
    let model = applied.model.as_deref().ok_or(
        "the prior attempt recorded no applied model; run provenant run with its result as context",
    )?;
    let model = ModelId::parse(model)
        .map_err(|error| format!("{error}; run provenant run with the prior result as context"))?;
    if applied.provider.as_deref() != Some(model.adapter()) {
        return Err(
            "the prior attempt's applied provider and model differ; run provenant run with its result as context"
                .into(),
        );
    }
    Ok(provenant_routing::RouteRequest {
        choice: RouteChoice::Model(model),
        effort: applied.effort.clone(),
        fallback: Fallback::None,
        confidential: false,
        allow_free: false,
    })
}

/// The continuation of `prior` on this candidate route. It must be the very
/// provider, model and account the prior attempt ran with. The prior attempt's
/// retained private state is copied into the new attempt's own, so the new
/// attempt never writes into settled history.
fn continuation_for(
    prior: &run::AttemptRecord,
    route: &ResolvedRoute,
    grant_digest: &str,
    state_root: &Path,
    private_state: &Path,
) -> Result<provenant_providers::Continuation, String> {
    let session = prior
        .session
        .as_deref()
        .ok_or("the prior attempt recorded no provider session")?;
    let applied = prior
        .applied_route
        .as_ref()
        .ok_or("the prior attempt recorded no applied route")?;
    if applied.provider.as_deref() != Some(route.adapter.as_str())
        || applied.model.as_deref() != Some(format!("{}/{}", route.adapter, route.model).as_str())
        || applied.account_key != route.account_key
    {
        return Err("route differs from the prior attempt's provider, model or account".into());
    }
    let continuation =
        provenant_providers::Continuation::retained(&route.adapter, session, route, grant_digest)
            .map_err(|error| error.to_string())?;
    let prior_state = state_root
        .join("runs")
        .join(prior.run_id.to_string())
        .join("attempts")
        .join(prior.attempt_id.to_string())
        .join("provider-state");
    match find_session_state(&prior_state, session) {
        // Codex's SQLite index holds absolute rollout paths. Rebuild it from
        // the copied sessions instead of resolving back into settled history.
        Ok(Some(retained)) => copy_state(&retained, private_state, route.adapter == "codex")?,
        // These adapters retain sessions in their existing user data stores.
        Ok(None) if AdapterSpec::get(&route.adapter).is_some_and(|spec| spec.user_sessions) => {}
        Ok(None) => return Err("the prior attempt's retained provider state is missing or has expired; dispatch a new lane with its retained result or transcripts as context".into()),
        // An unfinished search proves nothing missing, whatever the adapter.
        Err(SearchExhausted) => {
            return Err(format!(
                "the prior attempt's retained provider state exceeds {} entries",
                RETAINED_STATE.entries
            ));
        }
    }
    Ok(continuation)
}

/// Bounds on retained provider state, shared by its search and its copy. A
/// leftover process from the prior attempt may still grow the tree. The depth
/// and entry limits match the launch census of writable trees, which would
/// refuse a larger copy anyway, and leave room past the depth of Codex plugin
/// caches. The byte budget is many times a long session's state while bounding
/// what a sparse or still-growing file can make the host write.
#[derive(Clone, Copy)]
struct StateLimits {
    depth: u32,
    entries: u32,
    bytes: u64,
}
const RETAINED_STATE: StateLimits = StateLimits {
    depth: 32,
    entries: 100_000,
    bytes: 4 << 30,
};

/// The names in `directory`, each spending one of `entries`, or `None` once
/// they run out. Listing first closes the stream before any descent.
fn listed(
    directory: &Directory,
    entries: &mut u32,
) -> Result<Option<Vec<std::ffi::OsString>>, FsError> {
    let mut names = Vec::new();
    for name in directory.entries()? {
        let Some(left) = entries.checked_sub(1) else {
            return Ok(None);
        };
        *entries = left;
        names.push(name?);
    }
    Ok(Some(names))
}

/// A real directory on `device`, opened without following a link. Only the
/// stat before the open checks the device: a mount point stats as the mounted
/// root, so it is not opened unless it is swapped in after that stat.
fn open_local_directory(
    parent: &Directory,
    name: &std::ffi::OsStr,
    device: u64,
) -> Option<Directory> {
    parent
        .metadata_at(name, false)
        .is_ok_and(|metadata| metadata.kind == FileKind::Directory && metadata.device == device)
        .then(|| parent.open_directory(name).ok())
        .flatten()
}

/// The `candidate-N` directory of `provider_state` holding a file or directory
/// named for `session`, searched within `RETAINED_STATE`. The prior lane
/// wrote this tree, so the path must already be canonical and every directory
/// is opened from its parent's handle, never through a link or a mount.
pub(super) fn continuation_available(
    prior: &run::AttemptRecord,
    state_root: &Path,
) -> Result<(), String> {
    let provider = prior
        .applied_route
        .as_ref()
        .and_then(|route| route.provider.as_deref())
        .ok_or("the attempt never launched a provider")?;
    let session = prior
        .session
        .as_deref()
        .ok_or("the attempt recorded no provider session")?;
    if AdapterSpec::get(provider).is_some_and(|spec| spec.user_sessions) {
        return Ok(());
    }
    let state = state_root
        .join("runs")
        .join(prior.run_id.to_string())
        .join("attempts")
        .join(prior.attempt_id.to_string())
        .join("provider-state");
    match find_session_state(&state, session) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("retained provider continuation is missing or expired".into()),
        Err(_) => {
            Err("provider continuation search exceeded its bound; fresh fallback required".into())
        }
    }
}

fn find_session_state(
    provider_state: &Path,
    session: &str,
) -> Result<Option<PathBuf>, SearchExhausted> {
    find_session_state_within(provider_state, session, RETAINED_STATE)
}

/// The session search spent its entry budget before it finished.
#[derive(Debug, PartialEq)]
struct SearchExhausted;

fn find_session_state_within(
    provider_state: &Path,
    session: &str,
    limits: StateLimits,
) -> Result<Option<PathBuf>, SearchExhausted> {
    fn holds(
        dir: &Directory,
        session: &str,
        depth: u32,
        device: u64,
        budget: &mut u32,
    ) -> Result<bool, SearchExhausted> {
        let names = match listed(dir, budget) {
            Ok(Some(names)) => names,
            Ok(None) => return Err(SearchExhausted),
            Err(_) => return Ok(false),
        };
        for name in names {
            if name.to_string_lossy().contains(session) {
                return Ok(true);
            }
            if depth > 0
                && let Some(child) = open_local_directory(dir, &name, device)
                && holds(&child, session, depth - 1, device, budget)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    let opened = provenant_os::fs::canonicalize(provider_state)
        .ok()
        .filter(|canonical| canonical == provider_state)
        .and_then(|_| Directory::open_anchor(provider_state).ok())
        .and_then(|root| Some((root.metadata().ok()?.device, root)));
    let Some((device, root)) = opened else {
        return Ok(None);
    };
    let mut budget = limits.entries;
    let names = match listed(&root, &mut budget) {
        Ok(Some(names)) => names,
        Ok(None) => return Err(SearchExhausted),
        Err(_) => return Ok(None),
    };
    for name in names {
        if name.to_string_lossy().starts_with("candidate-")
            && let Some(candidate) = open_local_directory(&root, &name, device)
            && holds(&candidate, session, limits.depth, device, &mut budget)?
        {
            return Ok(Some(provider_state.join(name)));
        }
    }
    Ok(None)
}

/// Copy regular files and directories within `RETAINED_STATE`. GC tombstones,
/// symlinks and other entries are skipped. For Codex, copy only session trees:
/// setup recreates caches and Codex rebuilds its index with local rollout paths.
/// A leftover process may still change the tree, so every entry is opened from
/// its parent's handle without following a link, and the opened descriptor is
/// checked and copied.
fn copy_state(from: &Path, to: &Path, sessions_only: bool) -> Result<(), String> {
    copy_state_within(from, to, RETAINED_STATE, sessions_only)
}

fn copy_state_within(
    from: &Path,
    to: &Path,
    limits: StateLimits,
    sessions_only: bool,
) -> Result<(), String> {
    let failed =
        |detail: &dyn std::fmt::Display| format!("copying retained provider state: {detail}");
    // The anchor opens without following its final component; this stat only
    // names the common refusal.
    if !std::fs::symlink_metadata(from)
        .map_err(|error| failed(&error))?
        .is_dir()
    {
        return Err("retained provider state is not a directory".into());
    }
    let source = Directory::open_anchor(from).map_err(|error| failed(&error))?;
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(failed(&"the destination has no parent directory"));
    };
    std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
    let parent = Directory::open_anchor(parent).map_err(|error| failed(&error))?;
    // Free space caps the budget, so state the filesystem cannot hold is
    // refused before it fills the disk.
    let bytes = limits
        .bytes
        .min(parent.available_bytes().map_err(|error| failed(&error))?);
    let mut budget = Budget {
        entries: limits.entries,
        bytes,
        device: source.metadata().map_err(|error| failed(&error))?.device,
    };
    // The copy fills a fresh private sibling and is renamed into place, onto
    // the empty destination, only once whole. Any refusal removes it.
    let staging = provenant_os::fs::temporary_name().map_err(|error| failed(&error))?;
    parent
        .mkdir(&staging, 0o700)
        .map_err(|error| failed(&error))?;
    let copied = parent
        .open_directory(&staging)
        .map_err(CopyRefusal::from)
        .and_then(|target| copy_tree(&source, &target, limits.depth, &mut budget, sessions_only))
        .and_then(|()| Ok(parent.replace(&staging, &parent, name)?));
    let copied = copied.map_err(|refusal| match refusal {
        CopyRefusal::HardLink => "retained provider state holds a hard-linked file".into(),
        CopyRefusal::TooLarge => {
            format!("retained provider state exceeds its {bytes} byte copy budget")
        }
        CopyRefusal::TooMany => {
            format!("retained provider state exceeds {} entries", limits.entries)
        }
        CopyRefusal::TooDeep => format!(
            "retained provider state nests deeper than {} directories",
            limits.depth
        ),
        CopyRefusal::CrossDevice => "retained provider state crosses a filesystem boundary".into(),
        CopyRefusal::Fs(error) => failed(&error),
    });
    if let Err(refusal) = &copied
        && let Err(error) = remove_staged(&parent, &staging)
    {
        return Err(format!("{refusal}; removing its staged copy: {error}"));
    }
    copied
}

/// Remove a refused copy's staging tree. The host made it fresh and private,
/// so it may dispose of lock-named entries the copy wrote there.
fn remove_staged(parent: &Directory, name: &std::ffi::OsStr) -> Result<(), FsError> {
    fn remove_contents(tree: &PayloadTree) -> Result<(), FsError> {
        for entry in tree.entries()?.collect::<Result<Vec<_>, _>>()? {
            if tree.metadata_at(&entry)?.kind == FileKind::Directory {
                remove_contents(&tree.child(&entry)?)?;
                tree.rmdir(&entry)?;
            } else {
                tree.unlink(&entry)?;
            }
        }
        Ok(())
    }
    let staged = parent.metadata_at(name, false)?;
    remove_contents(&PayloadTree::claim(parent, name, &staged)?)?;
    parent.rmdir(name)
}

/// What the rest of a copy may still spend, and the source's filesystem.
struct Budget {
    entries: u32,
    bytes: u64,
    device: u64,
}

enum CopyRefusal {
    HardLink,
    TooLarge,
    TooMany,
    TooDeep,
    CrossDevice,
    Fs(FsError),
}

impl From<FsError> for CopyRefusal {
    fn from(error: FsError) -> Self {
        Self::Fs(error)
    }
}

/// The entry changed type or vanished after it was listed. A leftover process
/// may churn the tree; skipping such an entry copies nothing it names.
fn changed_entry(error: &FsError) -> bool {
    error.is_symlink()
        || error.is_not_found()
        || matches!(error, FsError::NotRegular { .. })
        // A directory now a file, or a file now a socket (Linux, then Darwin).
        || matches!(
            error.raw_os_error(),
            Some(libc::ENOTDIR | libc::ENXIO | libc::EOPNOTSUPP)
        )
}

fn credential_failure_message(
    error: &provenant_providers::ProviderError,
    label: &str,
    directory: &Path,
) -> String {
    let mut message = format!("account {label}: {error}");
    if matches!(
        error,
        provenant_providers::ProviderError::CredentialNotFound
            | provenant_providers::ProviderError::CredentialExpired
    ) {
        message.push_str(&format!(
            "; sign in again with CLAUDE_CONFIG_DIR={} claude auth login",
            directory.display()
        ));
    }
    message
}

// Lets a test change an entry between its stat and its open.
#[cfg(test)]
type OpenHook = Box<dyn FnMut(&std::ffi::OsStr)>;
#[cfg(test)]
thread_local! {
    static BEFORE_OPEN: std::cell::RefCell<Option<OpenHook>> =
        const { std::cell::RefCell::new(None) };
}

fn before_open(_name: &std::ffi::OsStr) {
    #[cfg(test)]
    BEFORE_OPEN.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(_name);
        }
    });
}

fn copy_tree(
    from: &Directory,
    to: &Directory,
    depth: u32,
    budget: &mut Budget,
    sessions_only: bool,
) -> Result<(), CopyRefusal> {
    for name in listed(from, &mut budget.entries)?.ok_or(CopyRefusal::TooMany)? {
        if name
            .as_encoded_bytes()
            .starts_with(run::TOMBSTONE.as_bytes())
            || (sessions_only && !matches!(name.to_str(), Some("sessions" | "archived_sessions")))
        {
            continue;
        }
        let metadata = match from.metadata_at(&name, false) {
            Ok(metadata) => metadata,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error.into()),
        };
        // A mount point stats as the mounted root. Like a verified directory
        // open, the walk refuses to leave its anchor's filesystem.
        if metadata.device != budget.device {
            return Err(CopyRefusal::CrossDevice);
        }
        before_open(&name);
        match metadata.kind {
            FileKind::Directory => {
                let depth = depth.checked_sub(1).ok_or(CopyRefusal::TooDeep)?;
                let child = match from.open_directory(&name) {
                    Ok(child) => child,
                    Err(error) if changed_entry(&error) => continue,
                    Err(error) => return Err(error.into()),
                };
                if child.metadata()?.device != budget.device {
                    return Err(CopyRefusal::CrossDevice);
                }
                match to.mkdir(&name, 0o700) {
                    Ok(()) => {}
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
                copy_tree(&child, &to.open_directory(&name)?, depth, budget, false)?;
            }
            FileKind::Regular => {
                let mut file = match from.open_read(&name) {
                    Ok(file) => file,
                    Err(error) if changed_entry(&error) => continue,
                    Err(error) => return Err(error.into()),
                };
                let opened = file.metadata()?;
                if opened.device != budget.device {
                    return Err(CopyRefusal::CrossDevice);
                }
                // A hard link can name a host file the lane could not otherwise
                // read; the host copy would then publish its contents.
                if opened.links > 1 {
                    return Err(CopyRefusal::HardLink);
                }
                // The destination is the copy's fresh staging directory, so an
                // existing entry there is refused, not replaced.
                let mut copy = to.create_new(&name)?;
                // Size is never trusted: read one byte past the budget, so a
                // sparse or still-growing file is refused, not copied unbounded.
                let copied = std::io::copy(
                    &mut std::io::Read::take(&mut file, budget.bytes.saturating_add(1)),
                    &mut copy,
                )
                .map_err(|source| FsError::Io {
                    operation: "copy file",
                    path: name.clone().into(),
                    source,
                })?;
                if copied > budget.bytes {
                    return Err(CopyRefusal::TooLarge);
                }
                budget.bytes -= copied;
                if opened.mode & 0o100 != 0 {
                    copy.make_owner_executable()?;
                }
            }
            FileKind::Symlink | FileKind::Other => {}
        }
    }
    Ok(())
}

/// Host context for fresh account identity and version probes.
struct Probe<'a> {
    roots: Option<&'a provenant_core::Roots>,
    executor: &'a provenant_providers::host::HostProbeExecutor<'a>,
    home: &'a Path,
    user: &'a std::ffi::OsStr,
    scratch: &'a Path,
}

/// Each explicit fallback candidate has its own private state, and only the
/// launched one holds Codex's rollout. Exactly one candidate may match.
pub(super) fn observe_codex_rollout(
    provider_state: &Path,
    thread: &str,
) -> Result<provenant_providers::codex_rollout::Turn, provenant_providers::ProviderError> {
    use provenant_providers::ProviderError;
    let entries = std::fs::read_dir(provider_state).map_err(|_| ProviderError::UnknownIdentity)?;
    let mut observed = Err(ProviderError::UnknownIdentity);
    for entry in entries.take(64) {
        let name = entry
            .map_err(|_| ProviderError::UnknownIdentity)?
            .file_name();
        let candidate = name
            .to_str()
            .and_then(|name| name.strip_prefix("candidate-"));
        if !candidate.is_some_and(|index| {
            !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            continue;
        }
        match provenant_providers::codex_rollout::observe(&provider_state.join(&name), thread) {
            Ok(_) if observed.is_ok() => return Err(ProviderError::IdentityMismatch),
            Ok(turn) => observed = Ok(turn),
            Err(ProviderError::UnknownIdentity) => {}
            Err(error) => return Err(error),
        }
    }
    observed
}

fn detect_failure(
    entry: &provenant_providers::accounts::Registration,
    error: &provenant_providers::ProviderError,
) -> String {
    let mut message = format!("{} account {}: {error}", entry.adapter, entry.label);
    if !matches!(error, provenant_providers::ProviderError::ProbeDirectory(_)) {
        message.push_str("; re-register the signed-in identity");
    }
    message
}

fn observe_account(
    entry: &provenant_providers::accounts::Registration,
    executable: &Path,
    probe: &Probe<'_>,
    catalogue_revision: &str,
    timestamp: i64,
) -> Result<provenant_providers::AvailabilityRecord, String> {
    let Probe {
        roots,
        executor,
        home,
        user,
        scratch,
    } = *probe;
    let directory = canonical(&entry.directory)?;
    if directory != entry.directory
        || (entry.adapter == "claude" && directory != home.join(format!(".claude-{}", entry.label)))
    {
        return Err(format!(
            "{} account {} has no fixed canonical identity directory",
            entry.adapter, entry.label
        ));
    }
    let (detected, org) =
        if !AdapterSpec::get(&entry.adapter).is_some_and(|spec| spec.registered_account) {
            (entry.key.clone(), None)
        } else {
            provenant_providers::accounts::detect_with_org(
                executor,
                &entry.adapter,
                executable,
                home,
                user,
                &directory,
                executor.root(),
            )
            .map_err(|error| detect_failure(entry, &error))?
        };
    if detected != entry.key {
        return Err(format!(
            "{} account {} changed identity; re-register it",
            entry.adapter, entry.label
        ));
    }
    match entry.check_org(org.as_deref()) {
        Err(_) => {
            return Err(format!(
                "{} account {} changed identity; re-register it",
                entry.adapter, entry.label
            ));
        }
        Ok(Some(org)) => {
            if let Some(roots) = roots {
                let _ = crate::accounts::record_org(roots, &entry.adapter, &entry.label, &org);
            }
        }
        Ok(None) => {}
    }
    let argv = [executable.as_os_str().to_owned(), "--version".into()];
    let mut options =
        provenant_os::exec::ExecOptions::new(&argv, scratch, std::time::Duration::from_secs(4));
    let env = [
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    options.env = Some(&env);
    options.stdout = provenant_os::exec::StreamPolicy::Capture { limit: 128 };
    options.stderr = provenant_os::exec::StreamPolicy::Discard;
    use provenant_os::exec::Executor;
    let out = executor.execute(options).map_err(|e| e.to_string())?;
    let unavailable = || {
        format!(
            "provider version is unavailable (termination={:?}, timed_out={}, stdout_truncated={})",
            out.termination, out.timed_out, out.stdout.truncated
        )
    };
    if out.termination != provenant_os::exec::Termination::Exit(0)
        || out.timed_out
        || out.stdout.truncated
    {
        return Err(unavailable());
    }
    let version = std::str::from_utf8(&out.stdout.bytes)
        .map_err(|_| "invalid provider version")?
        .trim()
        .to_owned();
    if version.is_empty() {
        return Err(format!("{}: empty version output", unavailable()));
    }
    Ok(provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: entry.adapter.clone(),
        executable: executable.to_string_lossy().into_owned(),
        version,
        last_update: roots.and_then(|roots| {
            provenant_providers::update::latest(roots.state.as_path(), &entry.adapter)
        }),
        fixture_ref: format!("installed/{}/version", entry.adapter),
        catalogue_revision: catalogue_revision.into(),
        account_key: Some(entry.key.clone()),
        observed_models: Vec::new(),
        observed_unix_seconds: timestamp,
        expires_unix_seconds: timestamp.saturating_add(300),
    })
}

fn candidates(resolved: ResolvedRoute) -> Vec<ResolvedRoute> {
    let mut routes = vec![resolved.clone()];
    routes.extend(
        resolved
            .alternatives
            .as_slice()
            .iter()
            .map(|alternative| with_alternative(&resolved, alternative)),
    );
    routes
}

fn with_alternative(base: &ResolvedRoute, alternative: &RouteAlternative) -> ResolvedRoute {
    ResolvedRoute {
        extra: Default::default(),
        adapter: alternative.adapter.clone(),
        model: alternative.model.clone(),
        effort: alternative.effort.clone(),
        catalogue_revision: base.catalogue_revision.clone(),
        account_key: alternative.account_key.clone(),
        host: alternative.host.clone(),
        alternatives: Default::default(),
        exclusions: base.exclusions.clone(),
        eligibility: alternative.eligibility.clone(),
        readiness: alternative.readiness.clone(),
        confidentiality: alternative.confidentiality.clone(),
        pool_selection: base.pool_selection.clone(),
    }
}

fn adapter_route(route: &ResolvedRoute) -> ResolvedRoute {
    let mut provider_route = route.clone();
    provider_route.alternatives = Default::default();
    provider_route
}

/// Record this dispatch's credential authority durably beside the attempt
/// before any refresh may act on it.
fn credential_authority(
    host_root: &Path,
    paths: &run::LaunchPaths,
    login: &provenant_providers::claude::Login<'_>,
) -> Result<provenant_providers::claude::CredentialAuthority, String> {
    let authority = provenant_providers::claude::CredentialAuthority::for_dispatch(
        paths.run_id.to_string(),
        login,
        paths.deadline_unix_ms,
    )
    .map_err(|e| e.to_string())?;
    let record = serde_json::to_vec(&authority).map_err(|e| e.to_string())?;
    let attempt = paths
        .provider_state
        .parent()
        .ok_or("attempt directory missing")?;
    let relative = attempt
        .strip_prefix(host_root)
        .map_err(|_| "attempt directory is outside host state")?;
    let mut parent = Directory::open_anchor(host_root).map_err(|e| e.to_string())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("attempt directory is not normalised".into());
        };
        parent = parent
            .open_verified_directory(name)
            .map_err(|e| e.to_string())?;
    }
    let publication = parent
        .atomic_write(
            std::ffi::OsStr::new("credential-authority.json"),
            ".authority-",
            &record,
            provenant_os::fs::PublishMode::Replace,
        )
        .map_err(|e| e.to_string())?;
    publication
        .require_durable()
        .map_err(|_| "credential authority durability unconfirmed")?;
    Ok(authority)
}

fn check_launch_prompt(
    text: &str,
    paths: &run::LaunchPaths,
    adapter: Option<&str>,
) -> Result<(), String> {
    if text.len() as u64 > super::PROMPT_LIMIT {
        return Err(
            "invalid input: launch prompt including the preamble exceeds 1 MiB; shorten the prompt"
                .into(),
        );
    }
    if paths.lane_depth > 1
        && adapter
            .and_then(AdapterSpec::get)
            .is_some_and(|spec| spec.argv_prompt)
        && Prompt::new(text.to_owned()).check_argv().is_err()
    {
        return Err("invalid input: nested argv launch prompt including the preamble exceeds 64 KiB; route the child to a stdin adapter (claude, codex or opencode), or shorten the prompt".into());
    }
    Ok(())
}

fn launch_prompt(
    text: &str,
    paths: &run::LaunchPaths,
    state_root: &Path,
    adapter: &str,
) -> Result<Prompt, String> {
    check_launch_prompt(text, paths, Some(adapter))?;
    let prompt = Prompt::new(text.to_owned());
    if !AdapterSpec::get(adapter).is_some_and(|spec| spec.argv_prompt)
        || prompt.check_argv().is_ok()
    {
        return Ok(prompt);
    }
    let attempt = paths
        .provider_state
        .parent()
        .ok_or("attempt directory missing")?;
    let relative = attempt
        .strip_prefix(state_root)
        .map_err(|_| "attempt directory is outside run state")?;
    let mut directory = Directory::open_anchor(state_root).map_err(|error| error.to_string())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("attempt directory is not normalised".into());
        };
        directory = directory
            .open_verified_directory(name)
            .map_err(|error| error.to_string())?;
    }
    let matches = |name: &str| -> Result<bool, String> {
        match directory.read_bounded(name.as_ref(), text.len() as u64) {
            Ok(bytes) => Ok(bytes == text.as_bytes()),
            Err(error) if error.is_not_found() => Ok(false),
            Err(FsError::TooLarge { .. }) => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    };
    let name = "prompt.md";
    match directory.atomic_write(
        name.as_ref(),
        ".prompt-",
        text.as_bytes(),
        PublishMode::NoReplace,
    ) {
        Ok(publication) => publication
            .require_durable()
            .map_err(|error| error.to_string())?,
        Err(error) if error.is_exists() && matches(name)? => {}
        Err(error) => return Err(error.to_string()),
    }
    let prompt = Prompt::from_file(attempt.join(name));
    prompt.check_argv().map_err(|error| error.to_string())?;
    Ok(prompt)
}

struct ConfinementRoots<'a> {
    cwd: &'a Path,
    home: &'a Path,
    scratch: &'a Path,
    private_state: &'a Path,
    assets: &'a Path,
    prompt_file: Option<&'a Path>,
}

fn host_indexes_protected(
    state: &Path,
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> bool {
    let resolve = |path: &Path| {
        provenant_core::PathResolver::resolve_existing_ancestor(
            &provenant_os::fs::SystemPathResolver,
            path,
        )
        .ok()
    };
    ["fabric/host.sqlite3", run::WORK_INDEX_FILE]
        .iter()
        .flat_map(|file| {
            ["", "-wal", "-shm", "-journal", ".lock"].map(|suffix| format!("{file}{suffix}"))
        })
        .all(|file| {
            let Some(path) = resolve(&state.join(file)) else {
                return false;
            };
            grants.iter().all(|grant| {
                resolve(&grant.path).is_some_and(|root| match grant.scope {
                    PathScope::Exact => root != path && !(root.is_dir() && path.starts_with(root)),
                    PathScope::Subtree => !path.starts_with(root),
                })
            })
        })
}

fn target_seeds_protected(
    state: &Path,
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> bool {
    let resolve = |path: &Path| {
        provenant_core::PathResolver::resolve_existing_ancestor(
            &provenant_os::fs::SystemPathResolver,
            path,
        )
        .ok()
    };
    let Some(store) = resolve(&run::target_seed::store_path(state)) else {
        return false;
    };
    grants.iter().all(|grant| {
        resolve(&grant.path).is_some_and(|root| {
            !root.starts_with(&store)
                && !(store.starts_with(&root)
                    && (grant.scope == PathScope::Subtree || root.is_dir()))
        })
    })
}

#[test]
fn configured_grants_cannot_expose_host_indexes() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::WriteGrant;
    for path in [
        "/state/work-index.sqlite3",
        "/state/work-index.sqlite3-wal",
        "/state/work-index.sqlite3-shm",
        "/state/work-index.sqlite3-journal",
        "/state/work-index.sqlite3.lock",
        "/state/fabric",
        "/state/fabric/host.sqlite3",
        "/state/fabric/host.sqlite3-wal",
        "/state/fabric/host.sqlite3-shm",
        "/state/fabric/host.sqlite3-journal",
        "/state/fabric/host.sqlite3.lock",
    ] {
        let grant = WriteGrant {
            source: run::WriteSource::WriteRoot,
            path: path.into(),
            scope: PathScope::Subtree,
        };
        assert!(!host_indexes_protected(Path::new("/state"), &[grant]));
    }
    assert!(host_indexes_protected(Path::new("/state"), &[]));
    let temp = tempfile::tempdir()?;
    let fabric = temp.path().join("fabric");
    std::fs::create_dir(&fabric)?;
    let grant = WriteGrant {
        source: run::WriteSource::StateWrite,
        path: fabric,
        scope: PathScope::Exact,
    };
    assert!(!host_indexes_protected(temp.path(), &[grant]));
    for path in [
        "/state",
        "/state/cargo-target-seeds",
        "/state/cargo-target-seeds/generations/a/target",
    ] {
        let grant = WriteGrant {
            source: run::WriteSource::WriteRoot,
            path: path.into(),
            scope: PathScope::Subtree,
        };
        assert!(!target_seeds_protected(Path::new("/state"), &[grant]));
    }
    assert!(target_seeds_protected(Path::new("/state"), &[]));
    Ok(())
}

/// Read access to the primary checkout's project scratch (`.provenant/scratch`, or
/// `.agent-run`), where briefs and graph documents live. Git excludes it, so a
/// writer's linked worktree does not hold it. Final-component links are refused;
/// parent links may only resolve within the canonical primary checkout.
fn project_scratch_reads(project: &Path) -> Vec<PathGrant> {
    let Ok(project) = std::fs::canonicalize(project) else {
        return Vec::new();
    };
    [".provenant/scratch", ".agent-run"]
        .map(|name| project.join(name))
        .into_iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .filter(|path| path.starts_with(&project))
        .map(|path| PathGrant {
            path,
            scope: PathScope::Subtree,
        })
        .collect()
}

/// Dispatch write roots, plus host-configured shared run state for writers only.
fn write_roots(
    access: &run::LaneAccess,
    host_writes: &provenant_confinement::run_config::RunConfig,
    writer: bool,
) -> Vec<PathGrant> {
    access
        .write_roots
        .iter()
        .map(PathBuf::as_path)
        .chain(
            writer
                .then(|| host_writes.writer_write_roots())
                .into_iter()
                .flatten(),
        )
        .map(|path| PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        })
        .collect()
}

fn confinement_request(
    project_id: ProjectId,
    roots: ConfinementRoots<'_>,
    writer: Option<&run::WriterLaunch>,
    access: &run::LaneAccess,
    host_writes: &provenant_confinement::run_config::RunConfig,
    adapter: &str,
    publishing: bool,
) -> Result<PlanRequest, String> {
    let mode = match writer {
        Some(writer) => {
            let mut authority =
                WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch, roots.home)
                    .map_err(|error| error.to_string())?;
            authority.set_publishing(publishing);
            AccessMode::WorktreeWrite(Box::new(authority))
        }
        None => AccessMode::ReadOnly,
    };
    let grant = |path: &Path| PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    };
    Ok(PlanRequest {
        scope: WorkScope::Project(project_id),
        permissions: PlanPermissions {
            read_only: None,
            mode,
            scratch: grant(roots.scratch),
            private_state: Some(grant(roots.private_state)),
            read_roots: std::iter::once(grant(roots.assets))
                .chain(writer.is_none().then(|| grant(roots.cwd)))
                .chain(
                    writer
                        .and_then(|writer| writer.worktree.repo().common_git_dir().parent())
                        .into_iter()
                        .flat_map(project_scratch_reads),
                )
                .chain(access.read_roots.iter().map(|path| grant(path)))
                .chain(roots.prompt_file.map(|path| PathGrant {
                    path: path.to_owned(),
                    scope: PathScope::Exact,
                }))
                .collect(),
            protected_roots: vec![PathGrant {
                path: roots.cwd.join("AGENTS.md"),
                scope: PathScope::Exact,
            }],
            secret_roots: Vec::new(),
            selected_authentication: AdapterSpec::get(adapter)
                .and_then(|spec| spec.authentication)
                .map(Into::into),
            include_continuation: false,
            write_roots: write_roots(access, host_writes, writer.is_some()),
            network: access.network(writer.is_some()),
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
}

/// Home and TMPDIR for the host's Git utility: a fresh private directory under
/// the host root, never the run state root. Inside a lane that is the
/// lane-writable child store, where the lane could pre-create or populate it.
fn host_git_home(host_root: &Path) -> Result<provenant_providers::host::HostProbeDir, String> {
    provenant_providers::host::HostProbeDir::create(host_root).map_err(|error| error.to_string())
}

/// Verify the selected repository on the host and match the persisted dispatch
/// project before deriving any Git read authority from that binding.
type PreparedReadPlan = (
    ConfinedPlan,
    Option<provenant_core::GitOid>,
    Option<provenant_records::AbsolutePath>,
);

fn read_only_plan(
    roots: &provenant_core::Roots,
    project: ProjectId,
    cwd: &Path,
    git: &Path,
    request: PlanRequest,
) -> Result<PreparedReadPlan, String> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility};
    use provenant_os::exec::SystemExecutor;

    // The host-pinned Git, the same one the lane receives as PROVENANT_GIT.
    let utility_home = host_git_home(roots.state.as_path())?;
    let utility =
        UtilityRuntime::new(git, utility_home.path()).map_err(|error| error.to_string())?;
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let budget = UtilityBudget::bounded(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    let cancel = provenant_core::CancelToken::new();
    // Find the root without opening a protected-history repo: read-only
    // repositories may use native object alternates. A malformed Git marker
    // must refuse, never fall through to a non-Git workspace.
    let mut repository = None;
    for ancestor in cwd.ancestors() {
        let directory = Directory::open_anchor(ancestor).map_err(|error| error.to_string())?;
        match directory.metadata_at(std::ffi::OsStr::new(".git"), false) {
            Ok(_) => {
                repository = Some(
                    utility
                        .open_read_repo(&executor, ancestor, budget, &cancel)
                        .map_err(|error| error.to_string())?,
                );
                break;
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let location = provenant_config::ProjectLocation::new(
        repository
            .as_ref()
            .map_or(cwd, |repo| repo.common_git_dir()),
    )
    .map_err(|error| error.to_string())?;
    let binding = provenant_config::lookup_project(roots, &location)
        .map_err(|error| error.to_string())?
        .ok_or("dispatch project binding missing")?;
    if binding.project_id() != project {
        return Err("repository does not match dispatch project binding".into());
    }
    match repository {
        Some(repo) => {
            // The host's own read of the checkout HEAD is the only reviewed
            // head a read-only lane can claim; an unreadable HEAD stays unproven.
            let head = run_read_utility(
                &executor,
                GitRead::RevParseHead,
                &repo,
                &utility,
                budget,
                &cancel,
            )
            .ok()
            .and_then(|out| String::from_utf8(out.stdout.bytes).ok())
            .and_then(|text| provenant_core::GitOid::parse(text.trim()).ok());
            run::read_only_plan(&roots.state, &repo, &binding, request)
                .map(|plan| {
                    (
                        plan,
                        head,
                        provenant_records::AbsolutePath::new(repo.common_git_dir()).ok(),
                    )
                })
                .map_err(|error| error.to_string())
        }
        // Non-Git workspaces have no Git metadata or repository exceptions.
        None => ConfinedPlan::new(request)
            .map(|plan| (plan, None, None))
            .map_err(|error| error.to_string()),
    }
}

fn publishing_executable(
    home: &Path,
    locate: &impl Fn(&str) -> Result<PathBuf, String>,
) -> Option<PathBuf> {
    if !home.join(".config/gh").is_dir() {
        return None;
    }
    // Resolve links before adding exact executable grants or writing wrappers.
    let executable = canonical(&locate("gh").ok()?).ok()?;
    (executable.is_file() && provenant_os::sys::is_executable(&executable)).then_some(executable)
}

pub(crate) fn resolve_executable(name: &str) -> Result<PathBuf, String> {
    canonical(&find_executable(name)?)
}

fn host_executable(host: &run::HostLaunchContext, name: &str) -> Result<PathBuf, String> {
    // The caller holds this provider's lease. Retry only an omitted entry,
    // using host-sealed directories rather than the guardian's environment.
    let path = match host.executables.get(name) {
        Some(path) => path.clone(),
        None => find_executable_on_path(name, &host.executable_search_path)?,
    };
    canonical(&path)
}

pub(crate) fn find_executable(name: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or("PATH is unavailable")?;
    find_executable_on_path(name, &std::env::split_paths(&path).collect::<Vec<_>>())
}

fn find_executable_on_path(name: &str, search_path: &[PathBuf]) -> Result<PathBuf, String> {
    let name = AdapterSpec::get(name).map_or(name, |spec| spec.binary);
    for directory in search_path
        .iter()
        .filter(|directory| directory.is_absolute())
    {
        let candidate = directory.join(name);
        if candidate.is_file() && provenant_os::sys::is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("{name} executable was not found on PATH"))
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    provenant_os::fs::canonicalize(path).map_err(|error| error.to_string())
}

/// Generated files stay in scratch and links in scratch or private state.
/// Directories may use any run-owned root, including trusted shared state, and
/// the selected Claude account's `projects/` tree. Traversal never follows links.
fn apply_setup(
    steps: &[SetupStep],
    roots: &[&Path],
    claude_account: Option<&Path>,
) -> Result<(), String> {
    for step in steps {
        match step {
            SetupStep::CreateDir(path) => {
                let transcript_root =
                    claude_account.filter(|directory| path.starts_with(directory.join("projects")));
                match transcript_root {
                    Some(directory) => apply_setup_directory(&[directory], path)?,
                    None => apply_setup_directory(roots, path)?,
                };
            }
            SetupStep::WriteFile { path, bytes } => write_setup_file(roots, path, bytes, false)?,
            SetupStep::WriteExecutable { path, bytes } => {
                write_setup_file(roots, path, bytes, true)?;
            }
            SetupStep::Symlink { link, target } => {
                let (parent, name) = setup_parent(&roots[..roots.len().min(2)], link, false)?;
                let matches = match parent.metadata_at(name, false) {
                    Ok(metadata) if metadata.kind == FileKind::Symlink => {
                        let matches =
                            parent.read_link(name).map_err(|error| error.to_string())? == *target;
                        if matches {
                            parent.sync().map_err(|error| error.to_string())?;
                        }
                        matches
                    }
                    Ok(_) => false,
                    Err(error) if error.is_not_found() => false,
                    Err(error) => return Err(error.to_string()),
                };
                if !matches {
                    parent
                        .replace_link(name, target)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
    Ok(())
}

fn write_setup_file(
    roots: &[&Path],
    path: &Path,
    bytes: &[u8],
    executable: bool,
) -> Result<(), String> {
    let (parent, name) = setup_parent(&roots[..1], path, false)?;
    let matches = match parent.metadata_at(name, false) {
        Ok(metadata) if metadata.kind == FileKind::Regular => {
            let mut file = parent.open_read(name).map_err(|error| error.to_string())?;
            match file.read_bounded(bytes.len() as u64) {
                Ok(existing) if existing == *bytes => {
                    file.sync().map_err(|error| error.to_string())?;
                    parent.sync().map_err(|error| error.to_string())?;
                    true
                }
                Ok(_) | Err(FsError::TooLarge { .. }) => false,
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(_) => false,
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.to_string()),
    };
    if !matches {
        parent
            .atomic_write(name, ".provider-setup.", bytes, PublishMode::Replace)
            .map_err(|error| error.to_string())?;
    }
    if executable {
        parent
            .open_read(name)
            .and_then(|file| file.make_owner_executable())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn apply_setup_directory(roots: &[&Path], path: &Path) -> Result<Directory, String> {
    let (parent, name) = setup_parent(roots, path, true)?;
    setup_directory(&parent, name)
}

fn setup_parent<'a>(
    roots: &[&Path],
    path: &'a Path,
    create: bool,
) -> Result<(Directory, &'a std::ffi::OsStr), String> {
    let (root, relative) = roots
        .iter()
        .find_map(|root| Some((root, path.strip_prefix(root).ok()?)))
        .ok_or("provider setup path is outside run-owned state")?;
    let mut names = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(name) => names.push(name),
            _ => return Err("provider setup path is not normalised".into()),
        }
    }
    let name = names.pop().ok_or("provider setup path names its root")?;
    let mut directory = Directory::open_anchor(root).map_err(|error| error.to_string())?;
    for component in names {
        directory = if create {
            setup_directory(&directory, component)?
        } else {
            directory
                .open_verified_directory(component)
                .map_err(|error| error.to_string())?
        };
    }
    Ok((directory, name))
}

// Existing shared ancestors need only read access in a confined MCP.
fn setup_directory(parent: &Directory, name: &std::ffi::OsStr) -> Result<Directory, String> {
    match parent.open_verified_directory(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.is_not_found() => {
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync().map_err(|error| error.to_string())?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.to_string()),
            }
            parent
                .open_verified_directory(name)
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Fixture access to the production plan builder; installed builds omit it.
#[cfg(feature = "fixture")]
pub fn writer_fixture_plan(
    project: ProjectId,
    cwd: &Path,
    home: &Path,
    paths: &run::LaunchPaths,
    writer: &run::WriterLaunch,
    assets: &Path,
) -> Result<ConfinedPlan, String> {
    let scratch = paths.temporary.join("candidate-0");
    apply_setup(
        &[SetupStep::CreateDir(scratch.clone())],
        &[&paths.temporary],
        None,
    )?;
    let request = confinement_request(
        project,
        ConfinementRoots {
            cwd,
            home,
            scratch: &scratch,
            private_state: &paths.provider_state,
            assets,
            prompt_file: None,
        },
        Some(writer),
        &run::LaneAccess::default(),
        &Default::default(),
        "codex",
        true,
    )?;
    ConfinedPlan::new(request).map_err(|error| error.to_string())
}

/// Exercise production preparation against a fake external provider executable.
/// A supplied Claude token stands in for the host authority and refresh path.
/// Optional publishing uses only a fixture gh beside the supplied executable.
#[cfg(feature = "fixture")]
pub fn prepare_fixture(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt: &str,
    paths: &run::LaunchPaths,
    executable: &Path,
    token: Option<&provenant_providers::claude::OauthToken>,
) -> Result<run::PreparedLaunch, String> {
    prepare_with(
        registry,
        task,
        prompt,
        paths,
        None,
        |name| {
            if name == "gh" {
                canonical(&executable.with_file_name("gh"))
            } else {
                canonical(executable)
            }
        },
        token,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_is_optional_without_gh_configuration() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let executable = std::env::current_exe()?.canonicalize()?;
        assert_eq!(
            publishing_executable(&home, &|_| Ok(executable.clone())),
            None
        );
        assert!(!home.join(".config/gh").exists());
        Ok(())
    }

    #[test]
    fn publishing_canonicalises_symlinked_gh_and_ignores_missing_tools()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        std::fs::create_dir_all(home.join(".config/gh"))?;
        let executable = std::env::current_exe()?.canonicalize()?;
        let gh = home.join("gh");
        std::os::unix::fs::symlink(&executable, &gh)?;
        assert_eq!(
            publishing_executable(&home, &|_| Ok(gh.clone())),
            Some(executable)
        );
        assert_eq!(
            publishing_executable(&home, &|_| Err("not installed".into())),
            None
        );
        std::fs::remove_file(&gh)?;
        assert_eq!(publishing_executable(&home, &|_| Ok(gh.clone())), None);
        Ok(())
    }

    #[test]
    fn version_probe_reports_outcomes_without_disclosing_output()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::{
            CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
        };

        struct VersionExecutor(ExecOutcome);
        impl Executor for VersionExecutor {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert_eq!(options.argv[1], "--version");
                assert_eq!(options.timeout, std::time::Duration::from_secs(4));
                assert!(matches!(
                    options.stdout,
                    provenant_os::exec::StreamPolicy::Capture { limit: 128 }
                ));
                assert!(matches!(
                    options.stderr,
                    provenant_os::exec::StreamPolicy::Discard
                ));
                Ok(self.0.clone())
            }
        }
        let temp = tempfile::tempdir()?;
        let root = canonical(temp.path())?;
        for path in ["home", "project", "state/provider-updates"] {
            std::fs::create_dir_all(root.join(path))?;
        }
        let roots = provenant_core::RootInputs {
            state_dir: Some(root.join("state")),
            ..provenant_core::RootInputs::new(root.join("home"), root.join("project"))
        }
        .resolve(&provenant_os::fs::SystemPathResolver)?;
        let last_update = provenant_providers::update::UpdateRecord {
            checked_unix_seconds: 99,
            old_version: Some("opencode v2.0.31".into()),
            new_version: Some("opencode v2.0.32".into()),
            outcome: "updated".into(),
        };
        std::fs::write(
            root.join("state/provider-updates/opencode.json"),
            serde_json::to_vec(&last_update)?,
        )?;
        let entry = serde_json::from_value(serde_json::json!({
            "adapter": "opencode", "label": "go", "key": "fixture", "directory": root,
        }))?;
        for (termination, timed_out, truncated, bytes, detail) in [
            (
                Termination::Exit(42),
                false,
                false,
                b"private output".as_slice(),
                Some("termination=Exit(42)"),
            ),
            (
                Termination::Signal(9),
                false,
                false,
                b"private output".as_slice(),
                Some("termination=Signal(9)"),
            ),
            (
                Termination::Exit(0),
                true,
                false,
                b"private output".as_slice(),
                Some("timed_out=true"),
            ),
            (
                Termination::Exit(0),
                false,
                true,
                b"private output".as_slice(),
                Some("stdout_truncated=true"),
            ),
            (
                Termination::Exit(0),
                false,
                false,
                b"".as_slice(),
                Some("empty version output"),
            ),
            (
                Termination::Exit(0),
                false,
                false,
                b"opencode v2.0.32\n".as_slice(),
                None,
            ),
        ] {
            let executor = VersionExecutor(ExecOutcome {
                termination,
                timed_out,
                stdout: CapturedStream {
                    bytes: bytes.to_vec(),
                    total_bytes: bytes.len() as u64,
                    truncated,
                    ..CapturedStream::default()
                },
                stderr: CapturedStream::default(),
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 80_000,
            });
            let executor = provenant_providers::host::HostProbeExecutor::new(&executor, &root)?;
            let probe = Probe {
                roots: Some(&roots),
                executor: &executor,
                home: &root,
                user: "fixture".as_ref(),
                scratch: &root,
            };
            let result = observe_account(
                &entry,
                Path::new("/fixture/opencode"),
                &probe,
                "fixture",
                100,
            );
            match detail {
                Some(detail) => {
                    let error = result.err().ok_or("failed version probe accepted")?;
                    assert!(error.contains("provider version is unavailable"), "{error}");
                    assert!(error.contains(detail), "{error}");
                    assert!(!error.contains("private output"));
                }
                None => {
                    let record = result?;
                    assert_eq!(record.version, "opencode v2.0.32");
                    assert_eq!(record.last_update, Some(last_update.clone()));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn credential_messages_suggest_signin_only_for_missing_or_expired_tokens() {
        use provenant_providers::ProviderError;
        let directory = Path::new("/fake/account");
        for error in [
            ProviderError::CredentialNotFound,
            ProviderError::CredentialExpired,
        ] {
            let message = credential_failure_message(&error, "work", directory);
            assert!(
                message.contains(
                    "sign in again with CLAUDE_CONFIG_DIR=/fake/account claude auth login"
                )
            );
        }
        for error in [
            ProviderError::CredentialReadFailed("security exited with code 1".into()),
            ProviderError::CredentialReadFailed("stored credential JSON is invalid".into()),
            ProviderError::CredentialUnavailable,
        ] {
            let message = credential_failure_message(&error, "work", directory);
            assert!(message.contains(&error.to_string()));
            assert!(!message.contains("sign in again"));
        }
    }

    fn prompt_paths(attempt: &Path) -> Result<run::LaunchPaths, Box<dyn std::error::Error>> {
        Ok(run::LaunchPaths {
            attempt_id: provenant_core::AttemptId::from_bytes([1; 16]),
            operation_id: provenant_core::OperationId::parse("spill-prompt")?,
            task_index: 0,
            lane_depth: 1,
            run_id: provenant_core::RunId::from_bytes([2; 16]),
            deadline_unix_ms: u64::MAX,
            scratch: attempt.join("scratch"),
            provider_state: attempt.join("provider-state"),
            temporary: attempt.join("tmp"),
        })
    }

    #[test]
    fn launch_prompt_preserves_small_text_and_refuses_more_than_one_mib()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let attempt = canonical(temp.path())?;
        let paths = prompt_paths(&attempt)?;
        for entry in REGISTRY {
            let adapter = entry.adapter.id();
            for text in ["small prompt".to_owned(), "a".repeat(65_536)] {
                let prompt = launch_prompt(&text, &paths, &attempt, adapter)?;
                assert_eq!(prompt.text(), text);
                assert!(prompt.file().is_none());
            }
            let text = format!("Durable output instructions\n\n{}", "a".repeat(1024 * 1024));
            let error = launch_prompt(&text, &paths, &attempt, adapter)
                .err()
                .ok_or("oversized launch accepted")?;
            assert!(error.contains("1 MiB"), "{adapter}: {error}");
        }
        assert!(!attempt.join("prompt.md").exists());
        Ok(())
    }

    #[test]
    fn launch_prompt_keeps_oversized_stdin_prompts_in_memory()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let attempt = canonical(temp.path())?;
        let mut paths = prompt_paths(&attempt)?;
        let text = format!("Durable output instructions\n\n{}", "é".repeat(75_000));
        for depth in [1, 2] {
            paths.lane_depth = depth;
            for adapter in ["claude", "codex", "opencode"] {
                for text in [&text, &"a".repeat(1024 * 1024)] {
                    let prompt = launch_prompt(text, &paths, &attempt, adapter)?;
                    assert_eq!(prompt.text(), text);
                    assert!(prompt.file().is_none());
                }
            }
        }
        assert!(!attempt.join("prompt.md").exists());
        Ok(())
    }

    #[test]
    fn launch_prompt_spills_only_top_level_argv_and_refuses_conflicting_file()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        for adapter in ["cursor", "agy"] {
            let temp = tempfile::tempdir()?;
            let attempt = canonical(temp.path())?;
            let mut paths = prompt_paths(&attempt)?;
            let text = format!("Durable output instructions\n\n{}", "é".repeat(75_000));
            paths.lane_depth = 2;
            let error = launch_prompt(&text, &paths, &attempt, adapter)
                .err()
                .ok_or("nested argv spill accepted")?;
            for expected in ["invalid input", "64 KiB", "stdin adapter", "shorten"] {
                assert!(error.contains(expected), "{error}");
            }
            assert!(!attempt.join("prompt.md").exists());

            paths.lane_depth = 1;
            for _ in 0..2 {
                let prompt = launch_prompt(&text, &paths, &attempt, adapter)?;
                assert_eq!(prompt.file(), Some(attempt.join("prompt.md").as_path()));
                assert!(prompt.text().len() < 1024);
                assert_eq!(std::fs::read_to_string(attempt.join("prompt.md"))?, text);
                assert_eq!(
                    std::fs::metadata(attempt.join("prompt.md"))?
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            std::fs::write(attempt.join("prompt.md"), "conflicting prompt")?;
            assert!(launch_prompt(&text, &paths, &attempt, adapter).is_err());
            assert_eq!(
                std::fs::read_to_string(attempt.join("prompt.md"))?,
                "conflicting prompt"
            );
        }
        Ok(())
    }

    #[test]
    fn launch_prompt_refuses_symlinked_attempt_ancestors() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let state = canonical(temp.path())?;
        std::fs::create_dir_all(state.join("actual/attempt"))?;
        std::os::unix::fs::symlink(state.join("actual"), state.join("runs"))?;
        let paths = prompt_paths(&state.join("runs/attempt"))?;
        assert!(launch_prompt(&"a".repeat(65_537), &paths, &state, "cursor").is_err());
        assert!(!state.join("actual/attempt/prompt.md").exists());
        Ok(())
    }

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
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                base_ref: None,
                task_id: provenant_core::TaskId::from_bytes([3; 16]),
                profile: Default::default(),
                prompt_digest: provenant_records::CanonicalV1::file_digest(full.as_bytes()),
                route: run::RequestedRoute::Model {
                    extra: Default::default(),
                    provider: adapter.into(),
                    model: format!("{adapter}/fixture-model"),
                    effort: None,
                },
                account: None,
                fallback: vec![],
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
            assert!(error.contains("1 MiB"), "{adapter}: {error}");
            if matches!(adapter, "cursor" | "agy") {
                let mut nested = prompt_paths(&attempt)?;
                nested.lane_depth = 2;
                let error = prepare_with(
                    &Registry::default(),
                    &requested,
                    &full,
                    &nested,
                    None,
                    |_| unreachable!("nested argv spill must not probe executables"),
                    None,
                )
                .err()
                .ok_or("nested argv launch accepted")?;
                assert!(error.contains("invalid input"), "{error}");
                assert!(error.contains("64 KiB"), "{error}");
                assert!(!attempt.join("prompt.md").exists());
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
        use provenant_core::{Clock, WaitBudget};
        use provenant_os::fs::LockMode;
        let temp = tempfile::tempdir()?;
        let _a = provenant_os::provider_fence::acquire(temp.path(), "codex", LockMode::Exclusive)?;
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0)?,
            provenant_core::MonoTime::from_micros(0),
        );
        let leases = available_provider_leases(
            temp.path(),
            &["codex", "cursor"],
            WaitBudget::cli(clock.mono_now(), 30)?,
            &clock,
            &provenant_core::CancelToken::new(),
        )?;
        assert!(leases.contains_key("cursor"));
        assert!(!leases.contains_key("codex"));
        assert_eq!(clock.mono_now().as_micros(), 0);
        drop(leases);
        let _b = provenant_os::provider_fence::acquire(temp.path(), "cursor", LockMode::Exclusive)?;
        let error = available_provider_leases(
            temp.path(),
            &["codex", "cursor"],
            WaitBudget::cli(clock.mono_now(), 30)?,
            &clock,
            &provenant_core::CancelToken::new(),
        )
        .err()
        .ok_or("busy providers admitted")?;
        assert!(error.contains("all candidate providers"));
        assert_eq!(clock.mono_now().as_micros(), 30_000_000);
        Ok(())
    }

    #[test]
    fn candidate_lease_wait_honours_cancellation() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{
            CancelToken, Clock, MonoTime, TestClock, TimeError, WaitBudget, WallTime,
        };
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
        let error = available_provider_leases(
            temp.path(),
            &["codex"],
            WaitBudget::cli(clock.mono_now(), 30)?,
            &clock,
            &cancel,
        )
        .err()
        .ok_or("cancelled lease wait admitted")?;
        assert!(error.contains("cancelled"));
        assert_eq!(clock.mono_now().as_micros(), 50_000);
        // Cancellation is also checked when the provider becomes available.
        drop(updating);
        assert!(
            available_provider_leases(
                temp.path(),
                &["codex"],
                WaitBudget::cli(clock.mono_now(), 30)?,
                &clock,
                &cancel,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn executable_discovery_after_wait_observes_the_completed_update()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{Clock, DurationMicros, WaitBudget};
        use provenant_os::fs::LockMode;
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("cursor-agent");
        let host = run::HostLaunchContext {
            configuration_root: temp.path().into(),
            cwd: temp.path().into(),
            executables: Default::default(),
            executable_search_path: vec![temp.path().into()],
            git: "/usr/bin/git".into(),
            codex_home: None,
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
                WaitBudget::cli(clock.mono_now(), 2)?,
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

    #[test]
    fn writer_plan_stages_scratch_in_private_temporary_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::{UtilityBudget, UtilityRuntime, worktrees};
        use provenant_core::{
            AttemptId, CancelToken, Clock, Digest, OperationId, RunId, TaskId, WaitBudget,
        };
        use provenant_os::clock::SystemClock;
        use provenant_os::exec::ExecOptions;
        use provenant_os::exec::Executor;
        use provenant_os::exec::SystemExecutor;
        use provenant_os::exec::Termination;
        use std::{
            os::unix::fs::{DirBuilderExt, PermissionsExt},
            time::Duration,
        };

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
        let git = provenant_os::git::git_executable()
            .ok_or("git unavailable")?
            .canonicalize()?;
        let executor = SystemExecutor::new(1)?;
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        ] {
            let argv: Vec<_> = std::iter::once(git.as_os_str().to_owned())
                .chain(args.into_iter().map(Into::into))
                .collect();
            let env = [
                ("HOME".into(), git_home.as_os_str().to_owned()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ];
            let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(5));
            options.env = Some(&env);
            let output = executor.execute(options)?;
            assert_eq!(output.termination, Termination::Exit(0), "{output:?}");
        }
        let roots = provenant_core::RootInputs {
            state_dir: Some(state.clone()),
            ..provenant_core::RootInputs::new(home.clone(), project.clone())
        }
        .resolve(&provenant_os::fs::SystemPathResolver)?;
        let utility = UtilityRuntime::new(&git, &git_home)?;
        let cancel = CancelToken::new();
        let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = utility.open_repo(&executor, &project, budget, &cancel)?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(repo.common_git_dir())?,
        )?;
        let worktree_state = state
            .join("worktrees")
            .join(binding.project_id().to_string());
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&worktree_state)?;
        let project_state = provenant_os::fs::Directory::open_anchor(&worktree_state)?;
        let clock = SystemClock::new();
        worktrees::create(
            &worktrees::WorktreeContext {
                home: &home,
                repo: &repo,
                binding: &binding,
                project_state: &project_state,
                executor: &executor,
                utility: &utility,
                workers: &worktrees::SystemWorkerObserver,
                clock: &clock,
                utility_budget: budget,
                mutation_budget: budget,
                lock_budget: WaitBudget::cli(clock.mono_now(), 5)?,
            },
            worktrees::WorktreeCreate {
                branch: "writer".into(),
                base: Some("HEAD".into()),
            },
            &cancel,
        )?;
        // Cursor's local account needs only --version. Keep its executable
        // inside the fixture assets, independent of host packages and PATH.
        let executable = assets.join("cursor/bin/agent");
        std::fs::create_dir_all(executable.parent().ok_or("provider bin")?)?;
        std::fs::write(&executable, b"#!/bin/sh\nprintf 'fixture-cursor 1.0\\n'\n")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let run_id = RunId::from_bytes([2; 16]);
        let attempt_id = AttemptId::from_bytes([1; 16]);
        let attempt = state.join(format!("runs/{run_id}/attempts/{attempt_id}"));
        let paths = run::LaunchPaths {
            attempt_id,
            operation_id: OperationId::parse("writer-temporary")?,
            task_index: 0,
            lane_depth: 1,
            run_id,
            deadline_unix_ms: u64::MAX,
            scratch: attempt.join("scratch"),
            provider_state: attempt.join("provider-state"),
            temporary: attempt.join("tmp"),
        };
        for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
        }
        let prompt = "é".repeat(75_000);
        let review_oid = run::resolve_git_base(&git, &project, "HEAD")?;
        let task = run::TaskRequest {
            extra: Default::default(),
            build_heavy: false,
            title: None,
            base: Some(review_oid.clone()),
            head: Some(review_oid.clone()),
            base_ref: None,
            task_id: TaskId::from_bytes([3; 16]),
            profile: provenant_records::run::HarnessProfile {
                knobs: [("lanes.publish".into(), serde_json::json!(false))].into(),
                ..Default::default()
            },
            prompt_digest: provenant_records::CanonicalV1::file_digest(prompt.as_bytes()),
            route: run::RequestedRoute::Model {
                extra: Default::default(),
                provider: "cursor".into(),
                model: "cursor/gpt-5.3-codex-low".into(),
                effort: None,
            },
            account: None,
            fallback: vec![run::RouteRecord {
                provider: Some("codex".into()),
                model: Some("codex/gpt-5.3-codex".into()),
                ..Default::default()
            }],
            authority_digest: Digest::from_bytes([4; 32]),
            writer: Some(run::WriterRequest {
                extra: Default::default(),
                project: project.clone(),
                worktree: project.join(".worktrees/writer"),
            }),
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
        };
        let call = run::Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 5)?,
            cancel: &cancel,
        };
        let store = run::RunStore::open(
            &run::ExecutionContext {
                state_root: roots.state.clone(),
                project_id: binding.project_id(),
                actor_id: provenant_core::ActorId::from_bytes([5; 16]),
                read_prompts: true,
            },
            &call,
        )?;
        store.create(
            &run::PreparedRun {
                run_id,
                operation_id: paths.operation_id.clone(),
                request: run::CanonicalRequest {
                    extra: Default::default(),
                    child_attempt_id_version: None,
                    chair: None,
                    fresh_continuation: None,
                    schema_version: 1,
                    tasks: vec![task.clone()],
                    concurrency: 1,
                    parent: None,
                    lane_depth: Some(1),
                    parent_attempt: None,
                },
                attempts: vec![run::NewAttempt {
                    task_id: task.task_id,
                    attempt_id,
                    prompt: prompt.clone(),
                }],
                created_at: provenant_core::WallTime::from_micros(100)?,
            },
            &call,
        )?;
        std::fs::write(
            state.join("config.toml"),
            format!(
                "schema_version = 1\n[domains.accounts.entries.unrelated]\nadapter = 'claude'\nlabel = 'other'\nkey = 'fixture-other'\ndirectory = '{}'\n",
                home.join(".claude-other").display(),
            ),
        )?;
        let full = format!("Durable output instructions\n\n{prompt}");
        let failed_update = provenant_providers::update::check(
            &state,
            "cursor",
            &executable,
            &provenant_config::ProviderSettings {
                update: provenant_config::ProviderUpdate::Command {
                    command: vec!["/usr/bin/false".into()],
                },
                ..Default::default()
            },
            &executor,
            100,
        )?
        .ok_or("no failed update record")?;
        assert_eq!(failed_update.outcome, "failed");
        let _updating = provenant_os::provider_fence::acquire(
            &state,
            "codex",
            provenant_os::fs::LockMode::Exclusive,
        )?;
        // A failed diagnostic write must not fail an otherwise valid launch.
        std::fs::create_dir(attempt.join("availability.json"))?;
        let launch = prepare_with(
            &registry,
            &task,
            &full,
            &paths,
            None,
            |adapter| {
                assert_eq!(adapter, "cursor", "unrelated adapter was probed");
                Ok(executable.clone())
            },
            None,
        )?;
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_LANE_HEAD" && value == review_oid.as_str())
        );
        assert!(
            launch
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_LANE_BASE" && value == review_oid.as_str())
        );
        // Exercise the host project policy through preparation and the actual
        // generated shim, with a fake gh so no GitHub access is needed.
        std::fs::create_dir_all(home.join(".config/gh"))?;
        let gh = assets.join("gh");
        std::fs::write(&gh, b"#!/bin/sh\nprintf 'forwarded\\n'\n")?;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700))?;
        let policy_dir = state
            .join("projects")
            .join(binding.project_id().to_string());
        std::fs::create_dir_all(&policy_dir)?;
        std::fs::write(policy_dir.join("github.json"), r#"{"deny_labels":["ci"]}"#)?;
        let publishing_paths = run::LaunchPaths {
            attempt_id: paths.attempt_id,
            operation_id: paths.operation_id.clone(),
            task_index: paths.task_index,
            lane_depth: paths.lane_depth,
            run_id: paths.run_id,
            deadline_unix_ms: paths.deadline_unix_ms,
            scratch: attempt.join("publishing-scratch"),
            provider_state: attempt.join("publishing-provider-state"),
            temporary: attempt.join("publishing-tmp"),
        };
        for path in [
            &publishing_paths.scratch,
            &publishing_paths.provider_state,
            &publishing_paths.temporary,
        ] {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let mut publishing_task = task.clone();
        publishing_task.profile.knobs.remove("lanes.publish");
        let _publishing = prepare_with(
            &registry,
            &publishing_task,
            "publishing policy fixture",
            &publishing_paths,
            None,
            |name| {
                Ok(if name == "gh" {
                    gh.clone()
                } else {
                    executable.clone()
                })
            },
            None,
        )?;
        let worktree = &publishing_task
            .writer
            .as_ref()
            .ok_or("writer missing")?
            .worktree;
        // The runtime populates this private Git directory after preparation.
        let private_common = publishing_paths.temporary.join("candidate-0/git-common");
        std::fs::create_dir_all(private_common.join("objects"))?;
        std::fs::create_dir_all(private_common.join("refs"))?;
        std::fs::write(private_common.join("HEAD"), b"ref: refs/heads/writer\n")?;
        std::fs::copy(project.join(".git/config"), private_common.join("config"))?;
        let private_objects = publishing_paths.temporary.join("candidate-0/git-objects");
        std::fs::create_dir_all(private_objects.join("info"))?;
        std::fs::write(
            private_objects.join("info/alternates"),
            format!("{}\n", project.join(".git/objects").display()),
        )?;
        let shared_config = std::fs::read(project.join(".git/config"))?;
        let shared_index = std::fs::read(project.join(".git/index"))?;
        let shared_refs = std::fs::read_dir(project.join(".git/refs/heads"))?.count();

        std::fs::write(
            worktree.join("private-blob-fixture"),
            b"publishing private object fixture\n",
        )?;
        let git_shim = publishing_paths.temporary.join("candidate-0/bin/git");
        let env = [
            ("HOME".into(), git_home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        let run_git = |args: &[&str]| {
            let argv: Vec<_> = std::iter::once(git_shim.as_os_str().to_owned())
                .chain(args.iter().map(Into::into))
                .collect();
            let mut options = ExecOptions::new(&argv, worktree, Duration::from_secs(5));
            options.env = Some(&env);
            executor.execute(options)
        };
        let oid = run_git(&["hash-object", "private-blob-fixture"])?;
        assert_eq!(oid.termination, Termination::Exit(0), "{oid:?}");
        let oid = std::str::from_utf8(&oid.stdout.bytes)?.trim();
        let added = run_git(&["add", "private-blob-fixture"])?;
        assert_eq!(added.termination, Termination::Exit(0), "{added:?}");
        let object = format!("{}/{}", &oid[..2], &oid[2..]);
        assert!(
            publishing_paths
                .temporary
                .join("candidate-0/git-objects")
                .join(&object)
                .is_file()
        );
        assert!(!project.join(".git/objects").join(&object).exists());
        assert_eq!(std::fs::read(project.join(".git/config"))?, shared_config);
        assert_eq!(std::fs::read(project.join(".git/index"))?, shared_index);
        assert_eq!(
            std::fs::read_dir(project.join(".git/refs/heads"))?.count(),
            shared_refs
        );
        let credential = run_git(&["config", "--get", "credential.helper"])?;
        assert_eq!(credential.termination, Termination::Exit(0));
        assert!(String::from_utf8_lossy(&credential.stdout.bytes).contains("auth git-credential"));

        let shim = publishing_paths.temporary.join("candidate-0/bin/gh");
        let run_shim = |label: &str| {
            use provenant_os::exec::{ExecOptions, Executor};
            let argv = [
                shim.as_os_str().to_owned(),
                "pr".into(),
                "create".into(),
                "-l".into(),
                label.into(),
            ];
            executor.execute(ExecOptions::new(
                &argv,
                &attempt,
                std::time::Duration::from_secs(5),
            ))
        };
        let denied = run_shim("ci")?;
        assert_eq!(denied.termination, provenant_os::exec::Termination::Exit(1));
        assert_eq!(denied.stderr.bytes, b"github.deny_labels\n");
        let allowed = run_shim("docs")?;
        assert_eq!(
            allowed.termination,
            provenant_os::exec::Termination::Exit(0)
        );
        assert_eq!(allowed.stdout.bytes, b"forwarded\n");
        // The preferred provider is updating, so preparation must select the
        // explicit fallback rather than just omit an unrelated busy adapter.
        let mut fallback_task = task.clone();
        let fallback_prompt = "Busy preferred provider fallback";
        fallback_task.prompt_digest =
            provenant_records::CanonicalV1::file_digest(fallback_prompt.as_bytes());
        fallback_task.route = run::RequestedRoute::Model {
            extra: Default::default(),
            provider: "codex".into(),
            model: "codex/gpt-5.3-codex".into(),
            effort: None,
        };
        fallback_task.fallback = vec![run::RouteRecord {
            provider: Some("cursor".into()),
            model: Some("cursor/gpt-5.3-codex-low".into()),
            ..Default::default()
        }];
        let fallback_paths = run::LaunchPaths {
            attempt_id: paths.attempt_id,
            operation_id: paths.operation_id.clone(),
            task_index: paths.task_index,
            lane_depth: paths.lane_depth,
            run_id: paths.run_id,
            deadline_unix_ms: paths.deadline_unix_ms,
            scratch: attempt.join("fallback-scratch"),
            provider_state: attempt.join("fallback-provider-state"),
            temporary: attempt.join("fallback-tmp"),
        };
        for path in [
            &fallback_paths.scratch,
            &fallback_paths.provider_state,
            &fallback_paths.temporary,
        ] {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let fallback_launch = prepare_with(
            &registry,
            &fallback_task,
            fallback_prompt,
            &fallback_paths,
            None,
            |adapter| {
                assert_eq!(adapter, "cursor", "busy preferred provider was probed");
                Ok(executable.clone())
            },
            None,
        )?;
        assert_eq!(fallback_launch.route.provider.as_deref(), Some("cursor"));
        assert_eq!(
            fallback_launch.route.model.as_deref(),
            Some("cursor/gpt-5.3-codex-low")
        );
        let mut review_task = task.clone();
        review_task.writer = None;
        let review_paths = run::LaunchPaths {
            scratch: attempt.join("review-scratch"),
            provider_state: attempt.join("review-provider-state"),
            temporary: attempt.join("review-tmp"),
            ..fallback_paths
        };
        for path in [
            &review_paths.scratch,
            &review_paths.provider_state,
            &review_paths.temporary,
        ] {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let review = prepare_with(
            &registry,
            &review_task,
            "short review",
            &review_paths,
            None,
            |_| Ok(executable.clone()),
            None,
        )?;
        assert!(
            review
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_LANE_HEAD" && value == review_oid.as_str())
        );
        assert!(
            review
                .environment
                .iter()
                .any(|(key, value)| key == "PROVENANT_LANE_BASE" && value == review_oid.as_str())
        );
        let executed = executor.execute(ExecOptions::new(
            &launch.argv,
            &project,
            Duration::from_secs(2),
        ))?;
        assert_eq!(executed.termination, Termination::Exit(0));
        assert!(launch.provider_lease.is_some());
        assert_eq!(launch.route.provider.as_deref(), Some("cursor"));
        assert_eq!(
            launch.route.model.as_deref(),
            Some("cursor/gpt-5.3-codex-low")
        );
        assert_eq!(launch.argv[0], executable.as_os_str());
        let scratch = paths.temporary.join("candidate-0");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(scratch.join("git-context.json"))?)?;
        assert_eq!(metadata["base"], metadata["head"]);
        assert_eq!(std::fs::read(scratch.join("git-diff.patch"))?, b"");
        let pointer = launch
            .argv
            .last()
            .ok_or("prompt missing")?
            .to_str()
            .ok_or("prompt UTF-8")?;
        assert!(pointer.len() < 1024);
        let spilled = attempt.join("prompt.md");
        assert!(pointer.contains(spilled.to_str().ok_or("path UTF-8")?));
        let spilled_text = std::fs::read_to_string(&spilled)?;
        let location = spilled_text
            .strip_suffix(&full)
            .ok_or("spilled prompt lost the task text")?;
        assert!(location.contains("Host-written Git context"));
        assert!(location.contains(&scratch.display().to_string()));
        assert_eq!(
            store.record(attempt_id, &call)?.prompt.digest,
            task.prompt_digest
        );
        assert_ne!(
            task.prompt_digest,
            provenant_records::CanonicalV1::file_digest(pointer.as_bytes())
        );
        assert_eq!(std::fs::read_to_string(attempt.join("prompt.txt"))?, prompt);
        // The original named pool may change or disappear after dispatch.
        // A continuation must use the exact applied provider, model and
        // account without consulting that pool or the original account alias.
        let mut prior = store.record(attempt_id, &call)?;
        prior.requested_route = run::RequestedRoute::Named {
            extra: Default::default(),
            name: "retired-pool".into(),
            effort: None,
        };
        prior.applied_route = Some(launch.route.clone());
        prior.session = Some("fixture-session".into());
        let mut continued_task = task.clone();
        continued_task.route = prior.requested_route.clone();
        continued_task.account = Some("retired-account-alias".into());
        let continued_paths = run::LaunchPaths {
            attempt_id: paths.attempt_id,
            operation_id: OperationId::parse("different-resume-seed")?,
            task_index: paths.task_index,
            lane_depth: paths.lane_depth,
            run_id: paths.run_id,
            deadline_unix_ms: paths.deadline_unix_ms,
            scratch: attempt.join("continued-scratch"),
            provider_state: attempt.join("continued-provider-state"),
            temporary: attempt.join("continued-tmp"),
        };
        for path in [
            &continued_paths.scratch,
            &continued_paths.provider_state,
            &continued_paths.temporary,
        ] {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        let continued = prepare_with(
            &registry,
            &continued_task,
            "Continue the prior task",
            &continued_paths,
            Some(&prior),
            |adapter| {
                assert_eq!(adapter, "cursor", "resume probed a different provider");
                Ok(executable.clone())
            },
            None,
        )?;
        assert_eq!(continued.route.provider, launch.route.provider);
        assert_eq!(continued.route.model, launch.route.model);
        assert_eq!(continued.route.account_key, launch.route.account_key);
        assert!(continued.route.pool_selection.is_none());
        assert!(continued.argv.iter().any(|arg| arg == "--resume"));
        let confinement = launch.confinement.ok_or("writer confinement missing")?;
        assert!(confinement.writer.is_some());
        assert!(!confinement.plan.permissions().writer_publishing());
        assert!(!scratch.join("publishing.gitconfig").exists());
        assert!(
            confinement
                .plan
                .permissions()
                .read_roots
                .contains(&PathGrant {
                    path: spilled,
                    scope: PathScope::Exact,
                })
        );
        assert_eq!(
            confinement.plan.permissions().scratch.path,
            paths.temporary.join("candidate-0")
        );
        let mut bad_task = task.clone();
        bad_task.base = Some("absent-base".into());
        let result = prepare_with(
            &registry,
            &bad_task,
            "fixture prompt",
            &paths,
            None,
            |_| Err("provider lookup must not happen for an invalid base".into()),
            None,
        );
        assert!(matches!(result, Err(error) if error.contains("invalid Git base \"absent-base\"")));
        // A writable tree the plan cannot cover names that error, not a disclosure denial.
        std::fs::write(
            state.join("config.toml"),
            format!(
                "schema_version = 1\n[domains.run]\nwriter_writes = ['{}']\n",
                paths.scratch.display(),
            ),
        )?;
        let error = prepare_with(
            &registry,
            &task,
            &full,
            &paths,
            None,
            |_| Ok(executable.clone()),
            None,
        )
        .err()
        .ok_or("unconfinable worktree accepted")?;
        assert!(!error.contains("DisclosureDenied"), "{error}");
        assert!(error.contains("plan roots are invalid"), "{error}");
        Ok(())
    }

    #[test]
    fn read_only_plan_runs_host_git_with_a_private_home_outside_the_child_store()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::permissions::{NetworkAccess, PlanPermissions};
        use std::os::unix::fs::PermissionsExt;
        struct Identity;
        impl provenant_core::PathResolver for Identity {
            fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
                let mut tail = PathBuf::new();
                let mut head = path;
                loop {
                    if let Ok(resolved) = std::fs::canonicalize(head) {
                        return Ok(resolved.join(tail));
                    }
                    let (Some(parent), Some(name)) = (head.parent(), head.file_name()) else {
                        return Err(std::io::ErrorKind::NotFound.into());
                    };
                    tail = Path::new(name).join(tail);
                    head = parent;
                }
            }
        }

        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let host_root = base.join("host-state");
        // The lane controls its child store, including the old git-home path.
        let child_store = host_root.join("children/parent-attempt");
        let planted = child_store.join("tmp/git-home");
        std::fs::create_dir_all(&planted)?;
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o777))?;
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo)?;
        std::fs::create_dir_all(base.join("home"))?;
        let real = provenant_os::git::git_executable().ok_or("git unavailable")?;
        // The smallest repository layout Git accepts, without spawning Git.
        for dir in [".git/objects", ".git/refs/heads"] {
            std::fs::create_dir_all(repo.join(dir))?;
        }
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        // A git that records the HOME and TMPDIR it was given, then runs the real one.
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin)?;
        let record = base.join("seen");
        let git = bin.join("git");
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\n[ \"$HOME\" = \"$TMPDIR\" ] || exit 97\nls -ld \"$HOME\" >> {r}\nexec {g} \"$@\"\n",
                r = record.display(),
                g = real.display()
            ),
        )?;
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))?;

        let roots = provenant_core::RootInputs {
            state_dir: Some(host_root.clone()),
            ..provenant_core::RootInputs::new(base.join("home"), repo.clone())
        }
        .resolve(&Identity)?;
        let grant = |name: &str| PathGrant {
            path: base.join(name),
            scope: PathScope::Subtree,
        };
        let request = PlanRequest {
            scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
            permissions: PlanPermissions {
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: grant("scratch"),
                private_state: Some(grant("provider-state")),
                read_roots: vec![grant("repo")],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        };
        // The unbound project is refused, but only after host git has run.
        let result = read_only_plan(&roots, ProjectId::from_bytes([1; 16]), &repo, &git, request);
        assert!(result.is_err());

        let seen = std::fs::read_to_string(&record)?;
        assert!(!seen.is_empty(), "host git never ran");
        for line in seen.lines() {
            assert!(line.starts_with("drwx------"), "{line}");
            assert!(
                line.contains(&format!("{}/probe-", host_root.display())),
                "{line}"
            );
            assert!(!line.contains("children"), "{line}");
        }
        // The home is gone once the plan is done, and the child store is unchanged.
        let leftovers: Vec<_> = std::fs::read_dir(&host_root)?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("probe-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        assert_eq!(std::fs::read_dir(&planted)?.count(), 0);
        Ok(())
    }

    #[test]
    fn credential_authority_never_follows_links_below_the_host_root()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{AttemptId, OperationId, RunId};

        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let host_root = base.join("host-state");
        let relative = Path::new("children/parent/runs/run/attempts/attempt");
        let attempt = host_root.join(relative);
        let config = base.join("home/.claude-pro");
        std::fs::create_dir_all(attempt.join("provider-state"))?;
        std::fs::create_dir_all(&config)?;
        std::fs::write(config.join(".credentials.json"), b"{}")?;
        let mut paths = run::LaunchPaths {
            attempt_id: AttemptId::from_bytes([1; 16]),
            operation_id: OperationId::parse("fixture")?,
            task_index: 0,
            lane_depth: 2,
            run_id: RunId::from_bytes([2; 16]),
            deadline_unix_ms: u64::MAX,
            scratch: attempt.join("scratch"),
            provider_state: attempt.join("provider-state"),
            temporary: attempt.join("tmp"),
        };
        let login = provenant_providers::claude::Login {
            executable: Path::new("/fixture/claude"),
            home: &base.join("home"),
            user: std::ffi::OsStr::new("fixture"),
            config_dir: &config,
        };
        credential_authority(&host_root, &paths, &login)?;
        let record = attempt.join("credential-authority.json");
        let original = std::fs::read(&record)?;
        // Test intermediate child-store ancestors as well as the final attempt.
        for ancestor in [
            Path::new("children"),
            Path::new("children/parent"),
            relative,
        ] {
            let source = host_root.join(ancestor);
            let redirected = base.join("redirected");
            std::fs::rename(&source, &redirected)?;
            std::os::unix::fs::symlink(&redirected, &source)?;
            assert!(credential_authority(&host_root, &paths, &login).is_err());
            assert_eq!(std::fs::read(&record)?, original);
            std::fs::remove_file(&source)?;
            std::fs::rename(&redirected, &source)?;
        }
        paths.provider_state = base.join("outside/provider-state");
        assert!(credential_authority(&host_root, &paths, &login).is_err());
        paths.provider_state = host_root.join("children/../outside/provider-state");
        assert!(credential_authority(&host_root, &paths, &login).is_err());
        Ok(())
    }

    #[test]
    fn user_sessions_and_pruned_codex_state_remain_resumable()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{AttemptId, Digest, RunId, TaskId};
        use provenant_records::{
            FileDigest, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteReadiness,
        };
        for adapter in ["cursor", "opencode", "codex"] {
            let route = ResolvedRoute {
                extra: Default::default(),
                adapter: adapter.into(),
                model: "fixture-model".into(),
                effort: None,
                catalogue_revision: "fixture".into(),
                account_key: Some("local".into()),
                host: "fixture".into(),
                alternatives: RouteAlternatives::default(),
                exclusions: Vec::new(),
                eligibility: RouteEligibility::Eligible,
                readiness: RouteReadiness::Ready {
                    extra: Default::default(),
                    availability_ref: "fixture".into(),
                    expires_unix_seconds: 100,
                },
                confidentiality: RouteConfidentiality {
                    extra: Default::default(),
                    evidence_ref: "fixture".into(),
                },
                pool_selection: None,
            };
            let mut prior: run::AttemptRecord = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "attempt_id": AttemptId::from_bytes([1; 16]), "run_id": RunId::from_bytes([2; 16]),
                "project_id": ProjectId::from_bytes([3; 16]), "task_id": TaskId::from_bytes([4; 16]),
                "operation_id": "fixture", "attempt_number": 1, "revision": 1, "generation": 0,
                "requested_route": {"kind": "model", "provider": adapter, "model": "fixture-model"},
                "applied_route": {"provider": adapter, "model": format!("{adapter}/fixture-model"), "account_key": "local"},
                "authority_digest": Digest::from_bytes([0; 32]), "session": "fixture-session",
                "queued_at": "2026-10-25T00:00:00.000000Z", "state": {"state": "queued"}, "usage": {},
                "prompt": {"path": {"encoding": "utf8", "path": "prompt.txt"}, "digest": FileDigest::from_bytes([0; 32]), "bytes": 0, "complete": true, "redacted": false}
            }))?;
            let state = Path::new("/fixture/missing-state");
            let result = continuation_for(&prior, &route, "fixture", state, state);
            if adapter == "codex" {
                assert!(result.is_err(), "Codex still requires its retained state");
                let session = "01234567-89ab-cdef-0123-456789abcdef";
                prior.session = Some(session.into());
                let rollout =
                    format!("sessions/2026/10/27/rollout-2026-10-27T00-00-00-{session}.jsonl");
                let transcript = format!(
                    "{}\n{}\n",
                    serde_json::json!({"type":"session_meta", "payload":{"id":session}}),
                    serde_json::json!({"type":"turn_context", "payload":{"model":"fixture-model"}}),
                );
                use provenant_core::{
                    ActorId, CancelToken, MonoTime, RootInputs, TestClock, WaitBudget, WallTime,
                };
                use std::os::unix::fs::DirBuilderExt;
                let temp = tempfile::tempdir()?;
                let base = temp.path().canonicalize()?;
                let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
                inputs.state_dir = Some(base.join("state"));
                let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
                let context = run::ExecutionContext {
                    state_root: roots.state,
                    project_id: prior.project_id,
                    actor_id: ActorId::from_bytes([5; 16]),
                    read_prompts: true,
                };
                let clock = TestClock::new(WallTime::from_micros(100)?, MonoTime::from_micros(0));
                let cancel = CancelToken::new();
                let call = run::Request {
                    clock: &clock,
                    cancel: &cancel,
                    budget: WaitBudget::cli(MonoTime::from_micros(0), 5)?,
                };
                let store = run::RunStore::open(&context, &call)?;
                let request = run::PreparedRun {
                    run_id: prior.run_id,
                    operation_id: prior.operation_id.clone(),
                    created_at: WallTime::from_micros(100)?,
                    attempts: vec![run::NewAttempt {
                        attempt_id: prior.attempt_id,
                        task_id: prior.task_id,
                        prompt: String::new(),
                    }],
                    request: serde_json::from_value(serde_json::json!({
                        "schema_version": 1, "concurrency": 1, "parent": null,
                        "tasks": [{ "task_id": prior.task_id, "prompt_digest": provenant_records::CanonicalV1::file_digest(b""),
                            "route": prior.requested_route, "fallback": [], "authority_digest": prior.authority_digest,
                            "provider_timeout_seconds": 20, "delivery_id": null }]
                    }))?,
                };
                store.create(&request, &call)?;
                let candidate = context.state_root.as_path().join(format!(
                    "runs/{}/attempts/{}/provider-state/candidate-0",
                    prior.run_id, prior.attempt_id
                ));
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(candidate.join("sessions/2026/10/27"))?;
                std::fs::write(candidate.join("provider.txt"), "codex")?;
                std::fs::write(candidate.join(&rollout), &transcript)?;
                // Codex's thread index can point to the source attempt even
                // after its rollout has been copied into another CODEX_HOME.
                let stale_path = candidate.join(&rollout);
                for index in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm"] {
                    std::fs::write(
                        candidate.join(index),
                        stale_path.as_os_str().as_encoded_bytes(),
                    )?;
                }
                for cache in ["cache", "plugins", "skills"] {
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(candidate.join(cache))?;
                    std::fs::write(candidate.join(cache).join("payload"), "cache")?;
                }
                std::fs::write(candidate.join("models_cache.json"), "cache")?;
                store.withdraw(
                    prior.attempt_id,
                    run::AttemptState::Queued {
                        extra: Default::default(),
                    },
                    run::Outcome::Cancelled,
                    "fixture",
                    WallTime::from_micros(200)?,
                    &call,
                )?;
                for cache in ["cache", "plugins", "skills", "models_cache.json"] {
                    assert!(!candidate.join(cache).exists());
                }
                use std::os::unix::fs::MetadataExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(candidate.join("cache"))?;
                std::fs::write(candidate.join("cache/payload"), "pending deletion")?;
                let metadata = std::fs::metadata(candidate.join("cache"))?;
                let tombstone = format!(
                    "{}{:016x}{:016x}-cache",
                    run::TOMBSTONE,
                    metadata.dev(),
                    metadata.ino()
                );
                std::fs::rename(candidate.join("cache"), candidate.join(&tombstone))?;
                let nested_tombstone = format!("sessions/{}interrupted", run::TOMBSTONE);
                std::fs::write(candidate.join(&nested_tombstone), "pending deletion")?;

                let copied = base.join("resumed-state");
                let continuation = continuation_for(
                    &prior,
                    &route,
                    "fixture",
                    context.state_root.as_path(),
                    &copied,
                )?;
                assert_eq!(continuation.session(), session);
                assert!(!copied.join(&tombstone).exists());
                assert!(!copied.join(nested_tombstone).exists());
                assert!(candidate.join(&tombstone).exists());
                assert_eq!(std::fs::read_to_string(copied.join(&rollout))?, transcript);
                assert_eq!(
                    provenant_providers::codex_rollout::observe(&copied, session)?.model,
                    "fixture-model"
                );
                for index in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm"] {
                    assert!(
                        !copied.join(index).exists(),
                        "resumed Codex must discover local rollouts without the source attempt's index: {index}"
                    );
                    assert_eq!(
                        std::fs::read(candidate.join(index))?,
                        stale_path.as_os_str().as_encoded_bytes()
                    );
                }

                // Exercise the hand-off to the real adapter without launching
                // a nested Codex process under the harness's confinement.
                let home = base.join("home");
                let cwd = base.join("checkout");
                let scratch = base.join("scratch");
                let assets = base.join("assets");
                let plan = ConfinedPlan::new(confinement_request(
                    prior.project_id,
                    ConfinementRoots {
                        cwd: &cwd,
                        home: &home,
                        scratch: &scratch,
                        private_state: &copied,
                        assets: &assets,
                        prompt_file: None,
                    },
                    None,
                    &Default::default(),
                    &Default::default(),
                    "codex",
                    true,
                )?)?;
                use provenant_providers::{Adapter, ProviderEvent, codex::Codex};
                let prompt = Prompt::new("continue".into());
                let account = provenant_providers::identity::RegisteredAccount::new(
                    "codex",
                    "local",
                    home.join(".codex"),
                )?;
                let launch = Codex.prepare(PrepareInput {
                    route: &route,
                    confined: &plan,
                    prompt: &prompt,
                    continuation: Some(&continuation),
                    grant_digest: "fixture",
                    executable: Path::new("/opt/homebrew/Cellar/codex/fixture/bin/codex"),
                    home: &home,
                    user: "fixture".as_ref(),
                    host_var: &|_| None,
                    account: Some(&account),
                    token: None,
                    cwd: &cwd,
                    git_context: false,
                    nonce: [9; 16],
                    provenant: LaneRuntime {
                        executable: Path::new("/fixture/provenant/bin/provenant"),
                        git: Path::new("/usr/bin/git"),
                        state: context.state_root.as_path(),
                        child_state: &base.join("children"),
                        home: &home,
                        depth: 1,
                        attempt: AttemptId::from_bytes([6; 16]),
                        assets: &assets,
                    },
                })?;
                assert_eq!(launch.argv[2], "resume");
                assert_eq!(launch.argv[3], session);
                assert!(
                    launch
                        .env
                        .iter()
                        .any(|(key, value)| { key == "CODEX_HOME" && value == copied.as_os_str() })
                );
                assert!(
                    plan.permissions()
                        .private_state
                        .as_ref()
                        .is_some_and(|grant| {
                            grant.path == copied && grant.scope == PathScope::Subtree
                        })
                );
                assert!(
                    plan.permissions()
                        .read_roots
                        .iter()
                        .all(|grant| { !candidate.starts_with(&grant.path) })
                );
                assert!(
                    launch
                        .profile
                        .spec
                        .runtime_reads
                        .iter()
                        .all(|grant| { !candidate.starts_with(&grant.path) })
                );
                let recorded = include_str!(
                    "../../../../tests/fixtures/providers/codex/live-worktree_write-resume.jsonl"
                )
                .replace("FIXTURE_SESSION_IDENTIFIER", session);
                let mut stream = Codex.stream(Some(&continuation))?;
                let events = Codex.observe(&mut stream, recorded.as_bytes())?;
                assert!(events.iter().any(|event| {
                    matches!(event, ProviderEvent::Session(started) if started == session)
                }));
                assert_eq!(
                    Codex
                        .finish(
                            stream,
                            provenant_providers::ExitObservation {
                                code: Some(0),
                                timed_out: false,
                            }
                        )?
                        .status(),
                    provenant_providers::OutcomeStatus::Success
                );
            } else {
                assert_eq!(result?.session(), "fixture-session");
                prior.session = None;
                assert!(continuation_for(&prior, &route, "fixture", state, state).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn retained_state_copy_keeps_only_codex_session_trees() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let source = base.join("candidate-0");
        std::fs::create_dir_all(source.join("sessions"))?;
        for name in ["cache", "plugins", "skills"] {
            std::fs::create_dir(source.join(name))?;
            std::fs::write(source.join(name).join("installed"), "partial cache")?;
            std::fs::create_dir(source.join("sessions").join(name))?;
            std::fs::write(source.join("sessions").join(name).join("keep"), "state")?;
        }
        std::fs::write(source.join("models_cache.json"), "cache")?;
        std::fs::write(source.join("state_5.sqlite"), "source rollout paths")?;
        std::fs::create_dir(source.join("archived_sessions"))?;
        std::fs::write(
            source.join("archived_sessions/rollout.jsonl"),
            "archived transcript",
        )?;
        std::fs::write(source.join("sessions/models_cache.json"), "state")?;
        std::fs::write(source.join("sessions/rollout.jsonl"), "transcript")?;
        for provider in [Some("codex"), Some("claude"), None] {
            if let Some(provider) = provider {
                std::fs::write(source.join("provider.txt"), provider)?;
            } else {
                std::fs::remove_file(source.join("provider.txt"))?;
            }
            let copied = base.join(provider.unwrap_or("legacy"));
            copy_state(&source, &copied, provider == Some("codex"))?;
            for name in [
                "cache",
                "plugins",
                "skills",
                "models_cache.json",
                "state_5.sqlite",
            ] {
                assert_eq!(copied.join(name).exists(), provider != Some("codex"));
                assert!(source.join(name).exists());
            }
            for name in ["cache", "plugins", "skills", "models_cache.json"] {
                assert!(copied.join("sessions").join(name).exists());
            }
            assert_eq!(
                std::fs::read_to_string(copied.join("archived_sessions/rollout.jsonl"))?,
                "archived transcript"
            );
            assert_eq!(
                std::fs::read_to_string(copied.join("sessions/rollout.jsonl"))?,
                "transcript"
            );
        }
        Ok(())
    }

    #[test]
    fn retained_state_never_follows_a_symlink() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let state = base.join("provider-state");
        std::fs::create_dir_all(base.join("outside/fixture-session"))?;
        std::fs::create_dir_all(&state)?;
        std::os::unix::fs::symlink(base.join("outside"), state.join("candidate-0"))?;
        assert_eq!(find_session_state(&state, "fixture-session"), Ok(None));
        std::fs::create_dir_all(state.join("candidate-1/fixture-session"))?;
        assert_eq!(
            find_session_state(&state, "fixture-session"),
            Ok(Some(state.join("candidate-1")))
        );
        std::os::unix::fs::symlink(&state, base.join("alias"))?;
        assert_eq!(
            find_session_state(&base.join("alias"), "fixture-session"),
            Ok(None)
        );
        assert!(copy_state(&state.join("candidate-0"), &base.join("copy"), false).is_err());
        std::fs::write(state.join("candidate-1/fixture-session/own"), "lane")?;
        copy_state(&state.join("candidate-1"), &base.join("copy"), false)?;
        assert!(base.join("copy/fixture-session/own").is_file());
        std::fs::write(base.join("outside/host-secret"), "host")?;
        std::fs::hard_link(
            base.join("outside/host-secret"),
            state.join("candidate-1/fixture-session/linked"),
        )?;
        let copied = base.join("linked-copy");
        assert_eq!(
            copy_state(&state.join("candidate-1"), &copied, false),
            Err("retained provider state holds a hard-linked file".into())
        );
        assert!(!copied.join("fixture-session/linked").exists());
        Ok(())
    }

    // Test-only: std has no mkfifo and this crate forbids unsafe code.
    #[allow(clippy::disallowed_methods)]
    fn mkfifo(path: &Path) -> std::io::Result<()> {
        let status = std::process::Command::new("mkfifo").arg(path).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("mkfifo failed"))
        }
    }

    /// Copy `source` while `swap` changes the entry `victim` after its check,
    /// on a thread so a blocking open fails the test instead of hanging it.
    fn copy_with_swap(
        source: &Path,
        copied: &Path,
        swap: impl FnOnce(&Path) -> std::io::Result<()> + Send + 'static,
    ) -> Result<(), String> {
        let (source, copied) = (source.to_owned(), copied.to_owned());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut swap = Some(swap);
            let entry = source.join("victim");
            BEFORE_OPEN.set(Some(Box::new(move |name| {
                if name == "victim"
                    && let Some(swap) = swap.take()
                {
                    swap(&entry).expect("swap the checked entry");
                }
            })));
            let _ = sender.send(copy_state(&source, &copied, false));
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the copy finishes without blocking")
    }

    #[test]
    fn retained_state_copy_skips_entries_swapped_after_their_check()
    -> Result<(), Box<dyn std::error::Error>> {
        type Swap = fn(&Path, &Path) -> std::io::Result<()>;
        let swaps: [(&str, bool, Swap); 6] = [
            ("file to outside link", false, |entry, outside| {
                std::fs::remove_file(entry)?;
                std::os::unix::fs::symlink(outside.join("host-secret"), entry)
            }),
            ("directory to outside link", true, |entry, outside| {
                std::fs::remove_dir_all(entry)?;
                std::os::unix::fs::symlink(outside, entry)
            }),
            ("file to FIFO", false, |entry, _| {
                std::fs::remove_file(entry)?;
                mkfifo(entry)
            }),
            ("file to socket", false, |entry, _| {
                std::fs::remove_file(entry)?;
                std::os::unix::net::UnixListener::bind(entry).map(drop)
            }),
            ("directory to file", true, |entry, _| {
                std::fs::remove_dir_all(entry)?;
                std::fs::write(entry, "replaced")
            }),
            ("file removed", false, |entry, _| {
                std::fs::remove_file(entry)
            }),
        ];
        for (label, directory, swap) in swaps {
            let temp = tempfile::tempdir()?;
            let base = std::fs::canonicalize(temp.path())?;
            let (source, outside) = (base.join("candidate-0"), base.join("outside"));
            std::fs::create_dir_all(&outside)?;
            std::fs::write(outside.join("host-secret"), "host")?;
            std::fs::create_dir_all(&source)?;
            std::fs::write(source.join("keep"), "lane")?;
            if directory {
                std::fs::create_dir(source.join("victim"))?;
                std::fs::write(source.join("victim/inner"), "lane")?;
            } else {
                std::fs::write(source.join("victim"), "lane")?;
            }
            let copied = base.join("copy");
            let swap_outside = outside.clone();
            copy_with_swap(&source, &copied, move |entry| swap(entry, &swap_outside))
                .map_err(|error| format!("{label}: {error}"))?;
            assert_eq!(std::fs::read_to_string(copied.join("keep"))?, "lane");
            let entries = std::fs::read_dir(&copied)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(entries, ["keep"], "{label}: the swapped entry is skipped");
        }
        Ok(())
    }

    #[test]
    fn retained_state_copy_refuses_a_file_swapped_for_a_hard_link()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let source = base.join("candidate-0");
        std::fs::create_dir_all(&source)?;
        std::fs::write(base.join("host-secret"), "host")?;
        std::fs::write(source.join("victim"), "lane")?;
        let copied = base.join("copy");
        let secret = base.join("host-secret");
        let result = copy_with_swap(&source, &copied, move |entry| {
            std::fs::remove_file(entry)?;
            std::fs::hard_link(&secret, entry)
        });
        assert_eq!(
            result,
            Err("retained provider state holds a hard-linked file".into())
        );
        assert!(std::fs::symlink_metadata(copied.join("victim")).is_err());
        Ok(())
    }

    #[test]
    fn retained_state_copy_skips_nested_symlinks() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let source = base.join("candidate-0");
        std::fs::create_dir_all(base.join("outside/secrets"))?;
        std::fs::write(base.join("outside/host-secret"), "host")?;
        std::fs::write(base.join("outside/secrets/token"), "host")?;
        std::fs::create_dir_all(source.join("sessions/2026"))?;
        std::fs::write(source.join("config.toml"), "lane")?;
        std::fs::write(source.join("sessions/2026/rollout.jsonl"), "turn")?;
        std::fs::write(source.join("tool"), "#!/bin/sh\n")?;
        std::fs::set_permissions(source.join("tool"), std::fs::Permissions::from_mode(0o755))?;
        std::os::unix::fs::symlink(base.join("outside/host-secret"), source.join("file-link"))?;
        std::os::unix::fs::symlink(base.join("outside/secrets"), source.join("dir-link"))?;
        std::os::unix::fs::symlink(
            base.join("outside/host-secret"),
            source.join("sessions/2026/nested-link"),
        )?;
        let copied = base.join("copy");
        copy_state(&source, &copied, false)?;
        assert_eq!(std::fs::read_to_string(copied.join("config.toml"))?, "lane");
        assert_eq!(
            std::fs::read_to_string(copied.join("sessions/2026/rollout.jsonl"))?,
            "turn"
        );
        let tool = std::fs::metadata(copied.join("tool"))?.permissions().mode();
        assert_eq!(tool & 0o100, 0o100, "owner execute survives the copy");
        for skipped in ["file-link", "dir-link", "sessions/2026/nested-link"] {
            assert!(
                std::fs::symlink_metadata(copied.join(skipped)).is_err(),
                "{skipped} must not be copied"
            );
        }
        let codex_copy = base.join("codex-copy");
        copy_state(&source, &codex_copy, true)?;
        assert_eq!(
            std::fs::read_to_string(codex_copy.join("sessions/2026/rollout.jsonl"))?,
            "turn"
        );
        assert!(std::fs::symlink_metadata(codex_copy.join("sessions/2026/nested-link")).is_err());
        Ok(())
    }

    #[test]
    fn retained_state_copy_refuses_state_past_its_byte_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        const LIMIT: u64 = 1 << 20;
        let limits = StateLimits {
            bytes: LIMIT,
            ..RETAINED_STATE
        };
        let refusal = Err(format!(
            "retained provider state exceeds its {LIMIT} byte copy budget"
        ));
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        // A sparse file costs the lane no blocks but claims far past the budget.
        let sparse = base.join("sparse");
        std::fs::create_dir(&sparse)?;
        std::fs::File::create(sparse.join("rollout.jsonl"))?.set_len(64 * LIMIT)?;
        let copied = base.join("sparse-copy");
        assert_eq!(copy_state_within(&sparse, &copied, limits, false), refusal);
        assert!(
            !copied.join("rollout.jsonl").exists(),
            "the partial copy is removed"
        );
        // The budget spans the tree, not each file.
        let split = base.join("split");
        std::fs::create_dir_all(split.join("sessions"))?;
        let half = vec![b'x'; usize::try_from(LIMIT / 2 + 1)?];
        std::fs::write(split.join("history.jsonl"), &half)?;
        std::fs::write(split.join("sessions/rollout.jsonl"), &half)?;
        assert_eq!(
            copy_state_within(&split, &base.join("split-copy"), limits, false),
            refusal
        );
        // State of exactly the budget still copies whole.
        let exact = base.join("exact");
        std::fs::create_dir(&exact)?;
        std::fs::write(exact.join("a"), &half)?;
        std::fs::write(exact.join("b"), vec![b'y'; usize::try_from(LIMIT / 2 - 1)?])?;
        copy_state_within(&exact, &base.join("exact-copy"), limits, false)?;
        assert_eq!(std::fs::read(base.join("exact-copy/a"))?, half);
        assert_eq!(
            std::fs::metadata(base.join("exact-copy/b"))?.len(),
            LIMIT / 2 - 1
        );
        Ok(())
    }

    #[test]
    fn retained_state_copy_refuses_trees_past_its_entry_and_depth_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        let limits = StateLimits {
            depth: 2,
            entries: 4,
            ..RETAINED_STATE
        };
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let within = base.join("within");
        std::fs::create_dir_all(within.join("a/b"))?;
        std::fs::write(within.join("a/b/file"), "lane")?;
        std::fs::write(within.join("top"), "lane")?;
        copy_state_within(&within, &base.join("within-copy"), limits, false)?;
        assert_eq!(
            std::fs::read_to_string(base.join("within-copy/a/b/file"))?,
            "lane"
        );

        let deep = base.join("deep");
        std::fs::create_dir_all(deep.join("a/b/c"))?;
        assert_eq!(
            copy_state_within(&deep, &base.join("deep-copy"), limits, false),
            Err("retained provider state nests deeper than 2 directories".into())
        );

        let wide = base.join("wide");
        std::fs::create_dir(&wide)?;
        for name in ["1", "2", "3", "4", "5"] {
            std::fs::write(wide.join(name), "lane")?;
        }
        assert_eq!(
            copy_state_within(&wide, &base.join("wide-copy"), limits, false),
            Err("retained provider state exceeds 4 entries".into())
        );
        Ok(())
    }

    #[test]
    fn retained_state_copy_leaves_nothing_when_refused() -> Result<(), Box<dyn std::error::Error>> {
        const LIMIT: u64 = 1 << 20;
        let limits = StateLimits {
            bytes: LIMIT,
            ..RETAINED_STATE
        };
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let source = base.join("candidate-0");
        std::fs::create_dir_all(source.join("sessions.lock"))?;
        std::fs::write(source.join("sessions.lock/rollout.jsonl"), "turn")?;
        std::fs::write(source.join("keep"), "lane")?;
        // A lock-named partial, which ordinary removal refuses.
        std::fs::File::create(source.join("Cargo.lock"))?.set_len(64 * LIMIT)?;
        // Like a launch, the destination is an empty candidate directory.
        let state = base.join("provider-state");
        let copied = state.join("candidate-0");
        std::fs::create_dir_all(&copied)?;
        assert_eq!(
            copy_state_within(&source, &copied, limits, false),
            Err(format!(
                "retained provider state exceeds its {LIMIT} byte copy budget"
            ))
        );
        let names = |path: &Path| -> std::io::Result<Vec<_>> {
            std::fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect()
        };
        assert_eq!(names(&state)?, ["candidate-0"], "no staging survives");
        assert!(names(&copied)?.is_empty(), "no partial output survives");
        std::fs::remove_file(source.join("Cargo.lock"))?;
        copy_state_within(&source, &copied, limits, false)?;
        assert_eq!(names(&state)?, ["candidate-0"]);
        assert_eq!(
            std::fs::read_to_string(copied.join("sessions.lock/rollout.jsonl"))?,
            "turn"
        );
        Ok(())
    }

    #[test]
    fn session_search_reports_its_spent_entry_budget() -> Result<(), Box<dyn std::error::Error>> {
        let limits = StateLimits {
            entries: 4,
            ..RETAINED_STATE
        };
        let temp = tempfile::tempdir()?;
        let state = std::fs::canonicalize(temp.path())?;
        std::fs::create_dir_all(state.join("candidate-0/sessions"))?;
        for name in ["1", "2", "3"] {
            std::fs::write(state.join("candidate-0/sessions").join(name), "lane")?;
        }
        // An unfinished search is not a missing session.
        assert_eq!(
            find_session_state_within(&state, "fixture-session", limits),
            Err(SearchExhausted)
        );
        std::fs::remove_file(state.join("candidate-0/sessions/3"))?;
        assert_eq!(
            find_session_state_within(&state, "fixture-session", limits),
            Ok(None)
        );
        Ok(())
    }

    // No unprivileged test can mount a filesystem inside a temporary tree, so
    // this runs the walk as if its anchor sat on another device.
    #[test]
    fn retained_state_copy_refuses_a_filesystem_boundary() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let source = base.join("candidate-0");
        std::fs::create_dir_all(source.join("sessions"))?;
        std::fs::create_dir(base.join("copy"))?;
        let (from, to) = (
            Directory::open_anchor(&source)?,
            Directory::open_anchor(&base.join("copy"))?,
        );
        let mut budget = Budget {
            entries: RETAINED_STATE.entries,
            bytes: RETAINED_STATE.bytes,
            device: from.metadata()?.device ^ 1,
        };
        let opened = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&opened);
        BEFORE_OPEN.set(Some(Box::new(move |_| seen.set(true))));
        let result = copy_tree(&from, &to, RETAINED_STATE.depth, &mut budget, true);
        BEFORE_OPEN.set(None);
        assert!(matches!(result, Err(CopyRefusal::CrossDevice)));
        assert!(!opened.get(), "a mount point is refused before its open");
        assert!(!base.join("copy/sessions").exists());
        Ok(())
    }

    #[test]
    fn codex_rollout_is_found_in_the_launched_candidate() -> Result<(), Box<dyn std::error::Error>>
    {
        let base =
            std::env::temp_dir().join(format!("provenant-candidates-{}", std::process::id()));
        let thread = "01234567-89ab-cdef-0123-456789abcdef";
        let day = base.join("candidate-1/sessions/2026/10/24");
        std::fs::create_dir_all(base.join("candidate-0"))?;
        std::fs::create_dir_all(&day)?;
        std::fs::write(
            day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":thread}}),
                serde_json::json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
            ),
        )?;
        let observed = observe_codex_rollout(&base, thread);
        std::fs::remove_dir_all(&base)?;
        assert_eq!(observed?.model, "gpt-6-luna");
        Ok(())
    }

    #[test]
    fn lane_plan_reads_the_active_release_root() -> Result<(), Box<dyn std::error::Error>> {
        let cwd = Path::new("/project");
        let assets = Path::new("/release/sha256:active");
        let home = Path::new("/home/user");
        let scratch = Path::new("/tmp/lane-scratch");
        let private_state = Path::new("/state/attempt/provider-state");
        let prompt = Path::new("/state/attempt/prompt.md");
        for entry in REGISTRY {
            let request = confinement_request(
                ProjectId::parse("0123456789abcdef0123456789abcdef")?,
                ConfinementRoots {
                    cwd,
                    home,
                    scratch,
                    private_state,
                    assets,
                    prompt_file: Some(prompt),
                },
                None,
                &run::LaneAccess::default(),
                &Default::default(),
                entry.adapter.id(),
                true,
            )?;
            assert!(request.permissions.read_roots.contains(&PathGrant {
                path: assets.to_owned(),
                scope: PathScope::Subtree,
            }));
            let plan = ConfinedPlan::new(request)?;
            assert!(plan.permissions().read_roots.contains(&PathGrant {
                path: prompt.to_owned(),
                scope: PathScope::Exact,
            }));
            assert!(
                !plan
                    .permissions()
                    .read_roots
                    .iter()
                    .any(|grant| { grant.path == prompt.parent().expect("attempt") })
            );
        }
        Ok(())
    }

    #[test]
    fn writers_read_the_primary_checkouts_untracked_scratch()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = tempfile::tempdir()?;
        let canonical = std::fs::canonicalize(project.path())?;
        let root = canonical.as_path();
        assert!(project_scratch_reads(root).is_empty());
        std::fs::create_dir_all(root.join(".provenant/scratch"))?;
        std::fs::create_dir(root.join(".agent-run"))?;
        std::fs::create_dir(root.join(".provenant/other"))?;
        let paths = |grants: Vec<PathGrant>| -> Vec<PathBuf> {
            assert!(grants.iter().all(|grant| grant.scope == PathScope::Subtree));
            grants.into_iter().map(|grant| grant.path).collect()
        };
        assert_eq!(
            paths(project_scratch_reads(root)),
            [root.join(".provenant/scratch"), root.join(".agent-run")]
        );
        // A link could point the grant anywhere.
        std::fs::remove_dir(root.join(".agent-run"))?;
        std::os::unix::fs::symlink("/", root.join(".agent-run"))?;
        assert_eq!(
            paths(project_scratch_reads(root)),
            [root.join(".provenant/scratch")]
        );
        Ok(())
    }

    #[test]
    fn project_scratch_reads_rejects_a_symlinked_parent_outside_the_checkout()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::fs::create_dir(outside.path().join("scratch"))?;
        std::os::unix::fs::symlink(outside.path(), project.path().join(".provenant"))?;
        assert!(project_scratch_reads(project.path()).is_empty());
        // An in-checkout parent link is safe, and grants its canonical target.
        std::fs::remove_file(project.path().join(".provenant"))?;
        std::fs::create_dir_all(project.path().join("owned/scratch"))?;
        std::os::unix::fs::symlink("owned", project.path().join(".provenant"))?;
        let canonical = std::fs::canonicalize(project.path())?;
        assert_eq!(
            project_scratch_reads(project.path()),
            vec![PathGrant {
                path: canonical.join("owned/scratch"),
                scope: PathScope::Subtree,
            }]
        );
        Ok(())
    }

    #[test]
    fn host_write_paths_reach_writers_only() {
        let host = provenant_confinement::run_config::RunConfig {
            writer_writes: vec!["/p/.agent-run/locks".into()],
        };
        let access = run::LaneAccess {
            write_roots: vec!["/p/extra".into()],
            ..Default::default()
        };
        let paths = |writer| -> Vec<PathBuf> {
            write_roots(&access, &host, writer)
                .into_iter()
                .map(|grant| grant.path)
                .collect()
        };
        assert_eq!(
            paths(true),
            [PathBuf::from("/p/extra"), "/p/.agent-run/locks".into()]
        );
        assert_eq!(paths(false), [PathBuf::from("/p/extra")]);
    }

    #[test]
    fn setup_stays_below_run_owned_roots_without_following_links()
    -> Result<(), Box<dyn std::error::Error>> {
        let base = std::env::temp_dir().join(format!("provenant-setup-{}", std::process::id()));
        std::fs::create_dir(&base)?;
        let base = canonical(&base)?;
        let (scratch, state, outside) = (base.join("tmp"), base.join("state"), base.join("out"));
        for path in [&scratch, &state, &outside] {
            std::fs::create_dir(path)?;
        }
        let roots = [scratch.as_path(), state.as_path()];
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            apply_setup(
                &[
                    SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                    SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                    SetupStep::Symlink {
                        link: scratch.join("cargo-home/registry"),
                        target: outside.clone(),
                    },
                ],
                &roots,
                None,
            )?;
            assert!(scratch.join("cargo-home/nested").is_dir());
            apply_setup(
                &[SetupStep::CreateDir(outside.join("projects/session"))],
                &roots,
                Some(&outside),
            )?;
            assert!(outside.join("projects/session").is_dir());
            assert!(
                apply_setup(
                    &[SetupStep::CreateDir(outside.join("credentials/new"))],
                    &roots,
                    Some(&outside)
                )
                .is_err()
            );
            assert!(
                apply_setup(
                    &[SetupStep::Symlink {
                        link: outside.join("projects/link"),
                        target: state.clone()
                    }],
                    &roots,
                    Some(&outside)
                )
                .is_err()
            );
            std::os::unix::fs::symlink(&outside, scratch.join("escape"))?;
            for step in [
                SetupStep::CreateDir(outside.join("x")),
                SetupStep::CreateDir(scratch.join("../out/x")),
                SetupStep::CreateDir(scratch.clone()),
                SetupStep::CreateDir(scratch.join("escape/x")),
                SetupStep::Symlink {
                    link: scratch.join("cargo-home/registry/x"),
                    target: outside.clone(),
                },
            ] {
                assert!(apply_setup(&[step], &roots, None).is_err());
            }
            assert!(!outside.join("x").exists());
            Ok(())
        })();
        std::fs::remove_dir_all(&base)?;
        result
    }

    #[test]
    fn provider_setup_recovers_after_interrupted_preparation_idempotently()
    -> Result<(), Box<dyn std::error::Error>> {
        let base =
            std::env::temp_dir().join(format!("provenant-setup-recovery-{}", std::process::id()));
        std::fs::create_dir(&base)?;
        let base = canonical(&base)?;
        let (scratch, state) = (base.join("tmp"), base.join("state"));
        for path in [&scratch, &state] {
            std::fs::create_dir(path)?;
        }
        let roots = [scratch.as_path(), state.as_path()];
        let file = scratch.join("settings.json");
        let setup = |contents: &[u8]| {
            apply_setup(
                &[SetupStep::WriteFile {
                    path: file.clone(),
                    bytes: contents.to_vec(),
                }],
                &roots,
                None,
            )
        };
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            setup(b"first")?;
            // A crash after setup but before the launch claim leaves these
            // entries in place for the next preparation of the same attempt.
            setup(b"first")?;
            assert_eq!(std::fs::read(&file)?, b"first");

            setup(b"second")?;
            assert_eq!(std::fs::read(&file)?, b"second");
            Ok(())
        })();
        std::fs::remove_dir_all(&base)?;
        result
    }
}
