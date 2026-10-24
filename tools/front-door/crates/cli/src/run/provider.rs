//! Provider preparation at the CLI composition edge.
use super::{Registry, route_request};
use provenant_confinement::permissions::{
    AccessMode, PathGrant, PathScope, PlanPermissions, WorktreeWriteAuthority,
};
use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
use provenant_core::{ProjectId, WorkScope};
use provenant_providers::{LaneRuntime, PrepareInput, Prompt, REGISTRY, SetupStep, Stdin};
use provenant_records::{ResolvedRoute, RouteAlternative};
use provenant_run as run;
use provenant_supervisor::fs::{Directory, FileKind, PublishMode, entropy_128};
use run::RouteRecord;
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
            Some(host) => host
                .executables
                .get(name)
                .cloned()
                .ok_or_else(|| format!("{name} executable is unavailable on the host")),
            None => resolve_executable(name),
        },
        None,
    )
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
    // Only fixture builds may stand a supplied token in for host authority.
    #[cfg(not(feature = "fixture"))]
    let fixture_token: Option<&provenant_providers::claude::OauthToken> = {
        let _ = fixture_token;
        None
    };
    if resume.is_some() {
        return Err(
            "continuation needs a provider session; providers do not expose sessions yet".into(),
        );
    }
    let roots = registry.roots.ok_or("routing context is unavailable")?;
    let home = canonical(registry.home.ok_or("home directory is unavailable")?)?;
    let state_root = registry.state_root.ok_or("state root is unavailable")?;
    let assets = registry.assets.ok_or("product assets are unavailable")?;
    let provenant_executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let provenant_executable = canonical(&provenant_executable)?;
    let git = canonical(&match registry.host {
        Some(host) => host.git.clone(),
        None => provenant_supervisor::git::git_executable().ok_or("Git unavailable")?,
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
    let prompt = Prompt::new(prompt_text.to_owned());
    prompt.check_argv().map_err(|error| error.to_string())?;
    let user = provenant_supervisor::sys::account(provenant_supervisor::sys::Account::Current)
        .map_err(|error| error.to_string())?
        .ok_or("current user identity is unavailable")?
        .name;

    let executor = provenant_supervisor::exec::SystemExecutor::new(1).map_err(|e| e.to_string())?;
    let config = provenant_config::read_with_domains(
        roots,
        &provenant_supervisor::fs::LocalFs,
        &crate::domains::SECTIONS,
    )
    .map_err(|e| e.to_string())?;
    let settings = config.domains.accounts;
    let mut entries: Vec<_> = settings.entries.into_values().collect();
    if !entries.iter().any(|e| e.adapter == "codex") {
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
                &paths.scratch,
            )
        {
            entries.push(provenant_providers::accounts::Registration {
                adapter: "codex".into(),
                label: "default".into(),
                key,
                directory,
                plan: None,
                priority: None,
                billing_anchor: None,
                limits: None,
            });
        }
    }
    // This profile exposes no stored OpenCode login: every free request uses
    // the same keyless allowance on this host, rather than an inferred user.
    if locate("opencode").is_ok() {
        entries.push(provenant_providers::accounts::Registration {
            adapter: "opencode".into(),
            label: "keyless".into(),
            key: "keyless".into(),
            directory: paths.provider_state.clone(),
            plan: None,
            priority: None,
            billing_anchor: None,
            limits: None,
        });
    }
    let timestamp = jiff::Timestamp::now().as_second();
    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
    let mut availability = Vec::new();
    let mut reasons = Vec::new();
    let probe = Probe {
        executor: &executor,
        home: &home,
        user: &user,
        scratch: &paths.scratch,
    };
    for entry in &entries {
        if !matches!(entry.adapter.as_str(), "claude" | "codex") {
            continue;
        }
        match locate(&entry.adapter).and_then(|executable| {
            observe_account(entry, &executable, &probe, catalogue.revision(), timestamp)
        }) {
            Ok(record) => availability.push(record),
            Err(error) => reasons.push(format!(
                "{} account {}: {error}",
                entry.adapter, entry.label
            )),
        }
    }
    let digest = task.authority_digest.to_string();
    let mut access = Vec::new();
    for adapter in ["codex", "claude", "opencode"] {
        // Repository-bound read-only grants are checked per candidate below.
        if confinement_request(
            project_id,
            ConfinementRoots {
                cwd: &cwd,
                home: &home,
                scratch: &paths.scratch,
                private_state: &paths.provider_state,
                assets,
            },
            writer.as_ref(),
            adapter,
        )
        .and_then(|request| ConfinedPlan::new(request).map_err(|error| error.to_string()))
        .is_ok()
        {
            access.push(provenant_routing::AccessContext {
                adapter,
                disclosure_ref: &digest,
                confinement_ref: &digest,
            });
        }
    }
    let requested_adapter = match &task.route {
        run::RequestedRoute::Model { model, .. } => {
            model.split_once('/').map(|(adapter, _)| adapter)
        }
        run::RequestedRoute::Named { .. } => None,
    };
    let selected = task
        .account
        .as_deref()
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
        .transpose()?;
    let host = provenant_config::bind_host(roots, &provenant_supervisor::fs::LocalFs)
        .map_err(|e| e.to_string())?
        .to_string();
    let resolved = crate::route::resolve_dispatch(
        &route_request(task)?,
        roots,
        selected,
        (paths.operation_id.as_str(), paths.task_index),
        &availability,
        &access,
        &host,
    )?;
    for (candidate, route) in candidates(resolved).into_iter().enumerate() {
        let label = format!("{}/{}", route.adapter, route.model);
        let Some(registration) = REGISTRY
            .iter()
            .find(|registration| registration.adapter.id() == route.adapter)
        else {
            reasons.push(format!("{label}: unknown adapter"));
            continue;
        };
        let adapter = registration.adapter;
        if !adapter.execution_enabled() {
            reasons.push(format!("{label}: provider execution is disabled"));
            continue;
        }
        if let provenant_records::RouteReadiness::Unavailable { reason } = &route.readiness {
            reasons.push(format!("{label}: {reason:?}"));
            continue;
        }
        if let Err(error) = crate::route::check_dispatch_candidate(roots, &route) {
            reasons.push(format!("{label}: {error}"));
            continue;
        }
        if !matches!(adapter.id(), "codex" | "claude" | "opencode") {
            reasons.push(format!("{label}: provider launch is not supported here"));
            continue;
        }
        let entry = entries
            .iter()
            .find(|e| e.adapter == route.adapter && Some(&e.key) == route.account_key.as_ref());
        let account = match entry
            .filter(|entry| matches!(entry.adapter.as_str(), "claude" | "codex"))
            .map(|entry| entry.registered())
            .transpose()
        {
            Ok(account) => account,
            Err(error) => {
                reasons.push(format!("{label}: account: {error}"));
                continue;
            }
        };
        if matches!(adapter.id(), "codex" | "claude") && account.is_none() {
            reasons.push(format!(
                "{label}: account is not registered; use accounts register"
            ));
            continue;
        }
        let executable = match locate(adapter.id()) {
            Ok(path) => path,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        // A read-only lane's scratch is its private temporary directory, which
        // terminal commit and withdrawal remove; writer scratch keeps settlement.
        // A failed setup never poisons the next explicit fallback candidate.
        let staging = if writer.is_some() {
            &paths.scratch
        } else {
            &paths.temporary
        };
        let scratch = staging.join(format!("candidate-{candidate}"));
        let private_state = paths.provider_state.join(format!("candidate-{candidate}"));
        if let Err(error) = apply_setup(&[SetupStep::CreateDir(scratch.clone())], &[staging], None)
            .and_then(|()| {
                apply_setup(
                    &[SetupStep::CreateDir(private_state.clone())],
                    &[&paths.provider_state],
                    None,
                )
            })
        {
            reasons.push(format!("{label}: {error}"));
            continue;
        }
        let (scratch, private_state) = (scratch.as_path(), private_state.as_path());
        let child_state = state_root
            .join("children")
            .join(paths.attempt_id.to_string());
        let (plan, observed_head) = match confinement_request(
            project_id,
            ConfinementRoots {
                cwd: &cwd,
                home: &home,
                scratch,
                private_state,
                assets,
            },
            writer.as_ref(),
            adapter.id(),
        )
        .and_then(|request| match writer.as_ref() {
            Some(_) => ConfinedPlan::new(request)
                .map(|plan| (plan, None))
                .map_err(|error| error.to_string()),
            None => read_only_plan(roots, state_root, project_id, &cwd, &git, request),
        }) {
            Ok(planned) => planned,
            Err(error) => {
                reasons.push(format!("{label}: {error}"));
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
                scratch,
            };
            let authority = match credential_authority(paths, &login) {
                Ok(authority) => authority,
                Err(error) => {
                    reasons.push(format!("{label}: {error}"));
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
                    reasons.push(format!("{label}: account {}: {error}; sign in again with CLAUDE_CONFIG_DIR={} claude auth login", entry.map_or("unknown", |e| e.label.as_str()), account.directory().display()));
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
        let spec = match adapter.prepare(PrepareInput {
            route: &provider_route,
            confined: &plan,
            prompt: &prompt,
            continuation: None,
            grant_digest: &grant_digest,
            executable: &executable,
            home: &home,
            user: &user,
            account: account.as_ref(),
            token,
            cwd: &cwd,
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
                reasons.push(format!("{label}: {error}"));
                continue;
            }
        };
        let transcript_account = account
            .as_ref()
            .filter(|_| adapter.id() == "claude")
            .map(|account| account.directory());
        let setup_roots = [scratch, private_state, roots.state.as_path()];
        if let Err(error) = apply_setup(&spec.setup, &setup_roots, transcript_account) {
            reasons.push(format!("{label}: setup failed: {error}"));
            continue;
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
            argv: spec.argv,
            environment: spec.env,
            stdin,
            secret_input: spec.secret_input,
            cwd: spec.cwd,
            route: provider,
            observed_head,
            confinement: Some(run::LaunchConfinement {
                plan,
                profile: spec.profile,
                writer,
                home: home.clone(),
            }),
        });
    }
    Err(reasons.join("; "))
}

/// Host context for fresh account identity and version probes.
struct Probe<'a> {
    executor: &'a provenant_supervisor::exec::SystemExecutor,
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

fn observe_account(
    entry: &provenant_providers::accounts::Registration,
    executable: &Path,
    probe: &Probe<'_>,
    catalogue_revision: &str,
    timestamp: i64,
) -> Result<provenant_providers::AvailabilityRecord, String> {
    let Probe {
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
    let detected = if entry.adapter == "opencode" {
        entry.key.clone()
    } else {
        provenant_providers::accounts::detect(
            executor,
            &entry.adapter,
            executable,
            home,
            user,
            &directory,
            scratch,
        )
        .map_err(|e| {
            format!(
                "{} account {}: {e}; re-register the signed-in identity",
                entry.adapter, entry.label
            )
        })?
    };
    if detected != entry.key {
        return Err(format!(
            "{} account {} changed identity; re-register it",
            entry.adapter, entry.label
        ));
    }
    let argv = [executable.as_os_str().to_owned(), "--version".into()];
    let mut options = provenant_supervisor::exec::ExecOptions::new(
        &argv,
        scratch,
        std::time::Duration::from_secs(4),
    );
    let env = [
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    options.env = Some(&env);
    options.stdout = provenant_supervisor::exec::StreamPolicy::Capture { limit: 128 };
    options.stderr = provenant_supervisor::exec::StreamPolicy::Discard;
    use provenant_supervisor::exec::Executor;
    let out = executor.execute(options).map_err(|e| e.to_string())?;
    if out.termination != provenant_supervisor::exec::Termination::Exit(0)
        || out.timed_out
        || out.stdout.truncated
    {
        return Err("provider version is unavailable".into());
    }
    let version = String::from_utf8(out.stdout.bytes)
        .map_err(|_| "invalid provider version")?
        .trim()
        .to_owned();
    if version.is_empty() {
        return Err("provider version is unavailable".into());
    }
    Ok(provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: entry.adapter.clone(),
        executable: executable.to_string_lossy().into_owned(),
        version,
        fixture_ref: format!("activated/{}/fresh", entry.adapter),
        catalogue_revision: catalogue_revision.into(),
        account_key: Some(entry.key.clone()),
        observed_models: Vec::new(),
        last_preflight_error: None,
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
    let parent = Directory::open_anchor(attempt).map_err(|e| e.to_string())?;
    let publication = parent
        .atomic_write(
            std::ffi::OsStr::new("credential-authority.json"),
            ".authority-",
            &record,
            provenant_supervisor::fs::PublishMode::Replace,
        )
        .map_err(|e| e.to_string())?;
    if !matches!(publication, provenant_supervisor::fs::Publication::Durable) {
        return Err("credential authority durability unconfirmed".into());
    }
    Ok(authority)
}

struct ConfinementRoots<'a> {
    cwd: &'a Path,
    home: &'a Path,
    scratch: &'a Path,
    private_state: &'a Path,
    assets: &'a Path,
}

fn confinement_request(
    project_id: ProjectId,
    roots: ConfinementRoots<'_>,
    writer: Option<&run::WriterLaunch>,
    adapter: &str,
) -> Result<PlanRequest, String> {
    let mode = match writer {
        Some(writer) => AccessMode::WorktreeWrite(Box::new(
            WorktreeWriteAuthority::for_worktree(&writer.worktree, &writer.branch, roots.home)
                .map_err(|error| error.to_string())?,
        )),
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
                .collect(),
            protected_roots: vec![PathGrant {
                path: roots.cwd.join("AGENTS.md"),
                scope: PathScope::Exact,
            }],
            secret_roots: Vec::new(),
            selected_authentication: (adapter == "codex").then(|| "login".into()),
            include_continuation: false,
            network: Vec::new(),
            capabilities: Vec::new(),
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
}

/// Verify the selected repository on the host and match the persisted dispatch
/// project before deriving any Git read authority from that binding.
fn read_only_plan(
    roots: &provenant_core::Roots,
    state_root: &Path,
    project: ProjectId,
    cwd: &Path,
    git: &Path,
    request: PlanRequest,
) -> Result<(ConfinedPlan, Option<provenant_core::GitOid>), String> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_read_utility};
    use provenant_supervisor::{exec::SystemExecutor, fs::LocalFs};
    // The host-pinned Git, the same one the lane receives as PROVENANT_GIT.
    // Its home lives under the run state root: inside a lane that is the
    // child store, since the shared state root is not writable there.
    let utility_home = state_root.join("tmp/git-home");
    crate::fabric::private_dir(state_root, &utility_home).map_err(|error| error.to_string())?;
    let utility = UtilityRuntime::new(std::sync::Arc::new(LocalFs), git, utility_home)
        .map_err(|error| error.to_string())?;
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
    let binding = provenant_config::lookup_project(roots, &location, &LocalFs)
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
                .map(|plan| (plan, head))
                .map_err(|error| error.to_string())
        }
        // Non-Git workspaces have no Git metadata or repository exceptions.
        None => ConfinedPlan::new(request)
            .map(|plan| (plan, None))
            .map_err(|error| error.to_string()),
    }
}

pub(crate) fn resolve_executable(name: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or("PATH is unavailable")?;
    for directory in std::env::split_paths(&path).filter(|directory| directory.is_absolute()) {
        let candidate = directory.join(name);
        if candidate.is_file() && provenant_supervisor::sys::is_executable(&candidate) {
            return canonical(&candidate);
        }
    }
    Err(format!("{name} executable was not found on PATH"))
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    provenant_supervisor::fs::canonicalize(path).map_err(|error| error.to_string())
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
                let (parent, name) = match transcript_root {
                    Some(directory) => setup_parent(&[directory], path, true)?,
                    None => setup_parent(roots, path, true)?,
                };
                setup_directory(&parent, name)?;
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
                Ok(_) | Err(provenant_supervisor::fs::FsError::TooLarge { .. }) => false,
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
    let request = confinement_request(
        project,
        ConfinementRoots {
            cwd,
            home,
            scratch: &paths.scratch,
            private_state: &paths.provider_state,
            assets,
        },
        Some(writer),
        "codex",
    )?;
    ConfinedPlan::new(request).map_err(|error| error.to_string())
}

/// Exercise production preparation against a fake external provider executable.
/// A supplied Claude token stands in for the host authority and refresh path.
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
        |_| canonical(executable),
        token,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let private_state = Path::new("/state/attempt");
        let request = confinement_request(
            ProjectId::parse("0123456789abcdef0123456789abcdef")?,
            ConfinementRoots {
                cwd,
                home,
                scratch,
                private_state,
                assets,
            },
            None,
            "codex",
        )?;
        assert!(request.permissions.read_roots.contains(&PathGrant {
            path: assets.to_owned(),
            scope: PathScope::Subtree,
        }));
        Ok(())
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
