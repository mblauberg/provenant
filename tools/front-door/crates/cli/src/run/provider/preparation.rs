use super::*;

const DEPENDENCY_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const DEPENDENCY_STDERR_TAIL: usize = 4096;

pub(super) fn image_input_warning(adapter: &str, attachments: &[PathBuf]) -> Option<String> {
    (!attachments.is_empty()
        && !AdapterSpec::get(adapter).is_some_and(|spec| spec.launch.image_input))
    .then(|| {
        format!(
            "warning: adapter {adapter} does not support image_input; attachments remain readable"
        )
    })
}

pub(super) fn violates_account_pin(
    selected_key: Option<&str>,
    selected_adapter: Option<&str>,
    candidate_key: Option<&str>,
    candidate_adapter: &str,
) -> bool {
    selected_key.is_some_and(|key| {
        candidate_key != Some(key)
            || selected_adapter.is_some_and(|adapter| adapter != candidate_adapter)
    })
}

/// A provider's typed login refusal can be retried before handoff. Policy,
/// identity and path errors remain unclassified and stop route switching.
pub(super) fn provider_preparation_fallback(
    error: &provenant_providers::ProviderError,
) -> provenant_routing::FallbackOutcome {
    use provenant_providers::ProviderError;
    match error {
        ProviderError::LoginRequired
        | ProviderError::CredentialUnavailable
        | ProviderError::CredentialNotFound
        | ProviderError::CredentialExpired => provenant_routing::FallbackOutcome::AuthRequired,
        _ => provenant_routing::FallbackOutcome::Other,
    }
}

/// Refresh failures retain login guidance only when the credential owner has
/// classified the cause. Other provider errors cannot supply diagnostic text.
pub(super) fn credential_failure_skip(
    route: &str,
    error: &provenant_providers::ProviderError,
    label: &str,
    directory: &Path,
) -> SkippedRoute {
    use provenant_providers::ProviderError;
    let mut skipped = skip(route, SkipKind::AuthRequired, "credential refresh failed");
    let cause = match error {
        ProviderError::CredentialNotFound => provenant_records::RouteExclusionReason::AuthRequired,
        ProviderError::CredentialExpired => provenant_records::RouteExclusionReason::LoginExpired,
        _ => return skipped,
    };
    skipped.set_cause(cause);
    skipped.detail = provenant_records::diagnostic::safe_text(&credential_failure_message(
        error, label, directory,
    ));
    skipped
}

// Resolve only PATH, leaving the rest of the daemon environment intact. The
// login shell runs from the account home, never the writer's checkout.
fn host_dependency_path(
    cancel: &provenant_core::CancelToken,
) -> Result<(std::ffi::OsString, &'static str), String> {
    let probe = || {
        let account = provenant_os::sys::account(provenant_os::sys::Account::Current)
            .map_err(|error| format!("dependency_path_unavailable: {error}"))?
            .ok_or("dependency_path_unavailable: host account is unavailable")?;
        let shell = if account.shell.is_empty() {
            Path::new("/bin/sh")
        } else {
            Path::new(&account.shell)
        };
        login_dependency_path(shell, Path::new(&account.home), cancel)
    };
    dependency_path_with_fallback(probe, std::env::var_os("PATH"), cancel)
}

pub(super) fn dependency_path_with_fallback(
    probe: impl FnOnce() -> Result<std::ffi::OsString, String>,
    daemon_path: Option<std::ffi::OsString>,
    cancel: &provenant_core::CancelToken,
) -> Result<(std::ffi::OsString, &'static str), String> {
    match probe() {
        Ok(path) if !cancel.is_cancelled() => Ok((path, "login_shell")),
        _ if cancel.is_cancelled() => Err("dependency PATH probe failed (cancelled)".into()),
        Err(error) => daemon_path
            .map(|path| (path, "daemon_fallback"))
            .ok_or(error),
        Ok(_) => unreachable!(),
    }
}

pub(super) fn login_dependency_path(
    shell: &Path,
    home: &Path,
    cancel: &provenant_core::CancelToken,
) -> Result<std::ffi::OsString, String> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    use std::os::unix::ffi::OsStringExt;
    let probe = || -> Result<std::ffi::OsString, String> {
        let argv = [
            shell.as_os_str().to_owned(),
            "-lc".into(),
            r#"printf '\0provenant-path\0%s\0' "$PATH""#.into(),
        ];
        let mut options = ExecOptions::new(&argv, home, std::time::Duration::from_secs(10));
        options.cancel = Some(cancel);
        options.process_group = true;
        options.cleanup_process_group = true;
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        let outcome = SystemExecutor::new(1)
            .map_err(|error| error.to_string())?
            .execute(options)
            .map_err(|error| error.to_string())?;
        if outcome.termination != Termination::Exit(0)
            || outcome.timed_out
            || outcome.cancelled
            || outcome.stdout.truncated
        {
            return Err("login-shell PATH probe failed, timed out or was cancelled".into());
        }
        let fields = outcome
            .stdout
            .bytes
            .split(|byte| *byte == 0)
            .collect::<Vec<_>>();
        let path = fields
            .windows(2)
            .find(|pair| pair[0] == b"provenant-path")
            .map(|pair| pair[1])
            .ok_or("login shell did not return PATH")?;
        Ok(std::ffi::OsString::from_vec(path.to_vec()))
    };
    if cancel.is_cancelled() {
        return Err("dependency PATH probe failed (cancelled)".into());
    }
    let result = probe();
    if cancel.is_cancelled() {
        return Err("dependency PATH probe failed (cancelled)".into());
    }
    result.map_err(|error| format!(
        "dependency_path_unavailable: {}: {error}; check the host login shell with `{} -lc 'printf %s \"$PATH\"'`",
        shell.display(), shell.display()
    ))
}

pub(super) fn dependency_preparation_refusal(
    error: String,
    enabled: bool,
    profile: &provenant_records::run::HarnessProfile,
) -> String {
    let origin = if profile.knobs.contains_key("lanes.prepare_deps") {
        format!("sealed profile {}", profile.name)
    } else {
        "compiled default".to_owned()
    };
    format!(
        "{error}; lanes.prepare_deps={enabled} ({origin}); set lanes.prepare_deps=false to skip dependency preparation"
    )
}

pub(super) fn prepare_writer_dependencies(
    worktree: &Path,
    enabled: bool,
    search_path: &dyn Fn() -> Result<(std::ffi::OsString, &'static str), String>,
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

    let (path, path_source) = search_path()?;
    let directories = std::env::split_paths(&path).collect::<Vec<_>>();
    let executable = find_command_on_path("pnpm", &directories).map_err(|_| {
        format!(
            "dependency_tool_not_found: {command} failed: pnpm executable was not found; searched PATH={} (source: {path_source}); install pnpm on the host, check `pnpm --version` in its login shell, then retry the dispatch",
            path.to_string_lossy()
        )
    })?;
    let mut env = std::env::vars_os()
        .filter(|(key, _)| key != "PATH")
        .collect::<Vec<_>>();
    env.push(("PATH".into(), path));
    let argv = [
        executable.as_os_str().to_owned(),
        "install".into(),
        "--frozen-lockfile".into(),
    ];
    let executor = provenant_os::exec::SystemExecutor::new(1)
        .map_err(|error| format!("{command} failed: {error}"))?;
    let mut options =
        provenant_os::exec::ExecOptions::new(&argv, worktree, DEPENDENCY_INSTALL_TIMEOUT);
    options.env = Some(&env);
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
        "path_source": path_source,
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
) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
    prepare_with_wait(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        |name| match registry.host {
            Some(host) if matches!(&task.route, run::RequestedRoute::Adapter { provider, .. } if AdapterSpec::get(provider).is_some_and(|spec| spec.named_entry == provenant_records::NamedEntry::Command)) => {
                find_command_on_path(name, &host.executable_search_path)
            }
            None if matches!(&task.route, run::RequestedRoute::Adapter { provider, .. } if AdapterSpec::get(provider).is_some_and(|spec| spec.named_entry == provenant_records::NamedEntry::Command)) =>
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
    .map_err(|error| error.into_refusal())
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
    #[error("{message}")]
    Refusal {
        message: String,
        cause: provenant_records::run::TerminalCause,
        /// Fixed-form text naming a configured value, never captured output.
        note: Option<String>,
    },
    #[error(transparent)]
    Prompt(#[from] LaunchPromptError),
    #[error(transparent)]
    Run(#[from] run::RunError),
    #[error("{message}")]
    Candidates {
        message: String,
        plans: Vec<provenant_confinement::PlanError>,
        skipped_routes: Vec<SkippedRoute>,
        fallback_outcome: Option<provenant_records::run::PreparationFailure>,
    },
    #[error("{0}")]
    Other(String),
}
impl PrepareError {
    fn into_refusal(self) -> run::PrepareRefusal {
        match self {
            Self::Refusal {
                message,
                cause,
                note,
            } => run::PrepareRefusal {
                terminal_cause: Some(cause),
                terminal_note: note,
                message,
                skipped_routes: Vec::new(),
                fallback_outcome: matches!(
                    cause,
                    provenant_records::run::TerminalCause::UnregisteredAccount
                        | provenant_records::run::TerminalCause::CommandUnavailable
                        | provenant_records::run::TerminalCause::HostNativeUnconfigured
                )
                .then_some(provenant_records::run::PreparationFailure::AdapterUnavailable),
            },
            Self::Candidates {
                message,
                skipped_routes,
                fallback_outcome,
                ..
            } => run::PrepareRefusal {
                terminal_cause: None,
                terminal_note: None,
                message,
                skipped_routes,
                fallback_outcome,
            },
            other => other.to_string().into(),
        }
    }
}
/// Retain a closed resolver classification, never its freely formatted error.
fn route_resolution_refusal(error: crate::route::DispatchError) -> PrepareError {
    use crate::route::{DispatchError, EdgeError};
    use provenant_records::run::TerminalCause;
    let message = error.to_string();
    let (cause, code) = match &error {
        DispatchError::Edge(error) => match error.as_ref() {
            EdgeError::AccountProfile {
                kind: crate::route::AccountProfileError::Unknown,
                ..
            } => (
                TerminalCause::UnknownPacingAccount,
                "unknown_pacing_account",
            ),
            EdgeError::Routing(error) => (TerminalCause::RouteResolutionRefused, error.code()),
            EdgeError::Unresolved { .. } => {
                (TerminalCause::RouteResolutionRefused, "unresolved_selector")
            }
            _ => (
                TerminalCause::RouteResolutionRefused,
                "route_configuration_or_observation",
            ),
        },
        DispatchError::Catalogue(error) => (TerminalCause::RouteResolutionRefused, error.code()),
        DispatchError::FamilySeparation(..) => {
            (TerminalCause::RouteResolutionRefused, "family_separation")
        }
    };
    PrepareError::Refusal {
        message,
        cause,
        note: Some(format!("route resolver classification: {code}")),
    }
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
        && AdapterSpec::get(provider)
            .is_some_and(|spec| spec.named_entry == provenant_records::NamedEntry::DelegatedAgent)
    {
        AdapterSpec::get(provider)
            .ok_or("unknown adapter")?
            .check_launch(&provenant_records::LaunchRequest {
                resume: resume.is_some(),
                write: run::Service::has_writes(task.writer.is_some(), &task.access.write_roots),
                network: task.access.network,
                skill_location: task.profile.require_skill_location,
                ..Default::default()
            })
            .map_err(|error| error.to_string())?;
        let roots = registry.roots.ok_or("routing context is unavailable")?;
        let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
            .map_err(|error| error.to_string())?;
        let native =
            config
                .domains
                .fabric
                .host_native
                .get(name)
                .ok_or_else(|| PrepareError::Refusal {
                    message: provenant_records::run::TerminalCause::HostNativeUnconfigured
                        .detail()
                        .into(),
                    cause: provenant_records::run::TerminalCause::HostNativeUnconfigured,
                    note: None,
                })?;
        let provenant_fabric_runtime::config::HostNativeConfig::ClaudeAgent {
            model,
            effort,
            description,
            prompt,
        } = native;
        let provider = model
            .split_once('/')
            .map(|(provider, _)| provider)
            .ok_or("host-native model has no provider")?;
        let mut delegated = task.clone();
        delegated.route = run::RequestedRoute::Model {
            extra: Default::default(),
            provider: provider.into(),
            model: model.clone(),
            effort: effort.clone(),
        };
        let mut launch = prepare_candidates(
            registry,
            REGISTRY,
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
        REGISTRY,
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

/// A mismatch is about which account is signed in, so only a check that names
/// the account may clear it; a plain "logged in" answer must not.
pub(super) fn login_check_can_clear(adapter: &str, mismatch: bool) -> bool {
    !mismatch || AdapterSpec::get(adapter).is_some_and(|spec| spec.login_check_names_account)
}

/// Only the already-selected account may be refreshed. A replacement cannot
/// grant another account's identity or revive a different catalogue revision.
fn stale_observation_index(
    route: &ResolvedRoute,
    availability: &[provenant_providers::AvailabilityRecord],
    now: i64,
    catalogue_revision: &str,
) -> Option<usize> {
    if !matches!(
        route.readiness,
        provenant_records::RouteReadiness::Unavailable {
            reason: provenant_records::RouteExclusionReason::StaleAvailability,
            ..
        }
    ) {
        return None;
    }
    let key = route.account_key.as_ref()?;
    availability.iter().position(|record| {
        record.adapter == route.adapter
            && record.account_key.as_ref() == Some(key)
            && (record.catalogue_revision != catalogue_revision
                || record.observed_unix_seconds > now
                || record.expires_unix_seconds <= now)
    })
}

fn retry_stale_selected(
    availability: &mut [provenant_providers::AvailabilityRecord],
    route: &ResolvedRoute,
    now: i64,
    catalogue_revision: &str,
    reprobe: impl FnOnce() -> Result<provenant_providers::AvailabilityRecord, AccountObservationError>,
    resolve: impl FnOnce(&[provenant_providers::AvailabilityRecord]) -> Option<ResolvedRoute>,
) -> Result<Option<ResolvedRoute>, AccountObservationError> {
    let Some(index) = stale_observation_index(route, availability, now, catalogue_revision) else {
        return Ok(None);
    };
    let old = availability[index].clone();
    let fresh = reprobe()?;
    if fresh.adapter != old.adapter
        || fresh.account_key != old.account_key
        || fresh.catalogue_revision != catalogue_revision
        || fresh.observed_unix_seconds > now
        || fresh.expires_unix_seconds <= now
    {
        return Ok(None);
    }
    availability[index] = fresh;
    let next = resolve(availability).and_then(|next| same_selected_ready(route, next));
    match next {
        Some(next) => Ok(Some(next)),
        None => {
            availability[index] = old;
            Ok(None)
        }
    }
}

fn same_selected_ready(route: &ResolvedRoute, next: ResolvedRoute) -> Option<ResolvedRoute> {
    if next.adapter != route.adapter
        || next.model != route.model
        || next.effort != route.effort
        || next.account_key != route.account_key
        || next.host != route.host
        || next.catalogue_revision != route.catalogue_revision
        || next.eligibility != route.eligibility
        || next.confidentiality != route.confidentiality
        || !matches!(
            next.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        )
    {
        return None;
    }
    // Preserve the route, alternatives and receipt while updating readiness.
    let mut original = route.clone();
    original.readiness = next.readiness;
    Some(original)
}

#[cfg(test)]
mod stale_readiness_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn stale_selected_account_is_reprobed_once_without_changing_route() {
        let route: ResolvedRoute = serde_json::from_value(serde_json::json!({
            "adapter":"claude", "model":"model", "effort":null,
            "catalogue_revision":"revision", "account_key":"account", "host":"local",
            "alternatives":[], "exclusions":[], "eligibility":"eligible",
            "readiness":{"status":"unavailable","reason":"stale_availability"},
            "confidentiality":{"evidence_ref":"evidence"}
        }))
        .unwrap();
        let stale = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "claude".into(),
            executable: "/private/TOKEN_SENTINEL".into(),
            version: "1".into(),
            last_update: None,
            fixture_ref: "installed/claude/version".into(),
            catalogue_revision: "revision".into(),
            account_key: Some("account".into()),
            observed_models: vec![],
            observed_unix_seconds: 10,
            expires_unix_seconds: 20,
        };
        let fresh = provenant_providers::AvailabilityRecord {
            observed_unix_seconds: 30,
            expires_unix_seconds: 330,
            ..stale.clone()
        };
        let calls = Cell::new(0);
        let mut records = vec![stale.clone()];
        let result = retry_stale_selected(
            &mut records,
            &route,
            30,
            "revision",
            || {
                calls.set(calls.get() + 1);
                Ok(fresh.clone())
            },
            |records| {
                assert_eq!(records[0].expires_unix_seconds, 330);
                let mut ready = route.clone();
                ready.extra.insert(
                    "redraw".into(),
                    serde_json::json!("must not replace original"),
                );
                ready.readiness = provenant_records::RouteReadiness::Ready {
                    extra: Default::default(),
                    availability_ref: "installed/claude/version".into(),
                    expires_unix_seconds: 330,
                };
                Some(ready)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(result.account_key, route.account_key);
        assert_eq!(result.extra, route.extra);
        assert!(matches!(
            result.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        assert_eq!(records[0], fresh);

        let mut records = vec![stale.clone()];
        let next = retry_stale_selected(
            &mut records,
            &route,
            30,
            "revision",
            || Ok(fresh),
            |_| {
                let mut other = route.clone();
                other.account_key = Some("other".into());
                other.readiness = provenant_records::RouteReadiness::Ready {
                    extra: Default::default(),
                    availability_ref: "installed/claude/version".into(),
                    expires_unix_seconds: 330,
                };
                Some(other)
            },
        )
        .unwrap();
        assert!(next.is_none());
        assert_eq!(records[0], stale);
        let calls = Cell::new(0);
        let failed = retry_stale_selected(
            &mut records,
            &route,
            30,
            "revision",
            || {
                calls.set(calls.get() + 1);
                Err(AccountObservationError::AuthRequired(
                    "login required".into(),
                ))
            },
            |_| panic!("a failed probe must not resolve"),
        );
        assert!(matches!(
            failed,
            Err(AccountObservationError::AuthRequired(_))
        ));
        assert_eq!(calls.get(), 1);
        assert_eq!(records[0], stale);
        let mut ready = route.clone();
        ready.readiness = provenant_records::RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "installed/claude/version".into(),
            expires_unix_seconds: 330,
        };
        let untouched = retry_stale_selected(
            &mut records,
            &ready,
            30,
            "revision",
            || panic!("fresh routes must not be re-probed"),
            |_| None,
        )
        .unwrap();
        assert!(untouched.is_none());
        assert_eq!(records[0], stale);
        let diagnostic = run::ErrorKind::StaleReadiness.diagnostic();
        assert_eq!(diagnostic.code, "STALE_READINESS");
        assert_eq!(diagnostic.category.as_deref(), Some("warning"));
        assert_eq!(
            run::ErrorKind::StaleReadiness.boundary_class(),
            provenant_records::diagnostic::BoundaryClass::None
        );
        assert!(
            diagnostic
                .next
                .as_deref()
                .unwrap()
                .contains("provenant quota tick")
        );
        assert!(
            !serde_json::to_string(&diagnostic)
                .unwrap()
                .contains("TOKEN_SENTINEL")
        );
    }

    #[test]
    fn refreshed_selected_route_rechecks_routing_and_access_guards() {
        use provenant_routing::{
            AccessContext, Catalogue, Fallback, ModelId, RouteChoice, RouteRequest, RoutingConfig,
            RoutingContext,
        };
        let catalogue = Catalogue::bundled().unwrap();
        let config = RoutingConfig::default();
        let request = RouteRequest {
            intent: Default::default(),
            choice: RouteChoice::Model(ModelId::parse("codex/gpt-6-luna").unwrap()),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        };
        let stale = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "codex".into(),
            executable: "/fixture/codex".into(),
            version: "1".into(),
            last_update: None,
            fixture_ref: "installed/codex/version".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("work".into()),
            observed_models: vec![],
            observed_unix_seconds: 10,
            expires_unix_seconds: 20,
        };
        let fresh = provenant_providers::AvailabilityRecord {
            observed_unix_seconds: 30,
            expires_unix_seconds: 330,
            ..stale.clone()
        };
        let access = [AccessContext {
            adapter: "codex",
            disclosure_ref: "grant",
            confinement_ref: "grant",
        }];
        let resolve = |records: &[provenant_providers::AvailabilityRecord],
                       access: &[AccessContext<'_>]| {
            let context = RoutingContext {
                choice_context: None,
                policy_account_hint: None,
                catalogue: &catalogue,
                config: &config,
                registry: REGISTRY,
                availability: records,
                quota: &[],
                capacity_cooldowns: &[],
                accounts: &[],
                quota_policies: &[],
                readiness: None,
                access,
                now: provenant_core::WallTime::from_micros(30_000_000).unwrap(),
                host: "local",
            };
            provenant_routing::resolve(&request, &context).unwrap()
        };
        // Availability expiry is advisory; it does not override current route guards.
        let route = resolve(std::slice::from_ref(&stale), &access);
        assert!(matches!(
            route.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        assert!(stale_observation_index(&route, &[stale], 30, catalogue.revision()).is_none());
        let ready = same_selected_ready(&route, resolve(std::slice::from_ref(&fresh), &access))
            .expect("same selected account remains eligible");
        assert_eq!(ready.account_key, route.account_key);
        assert!(matches!(
            ready.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        let denied = resolve(std::slice::from_ref(&fresh), &[]);
        assert!(matches!(
            denied.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::DisclosureDenied,
                ..
            }
        ));
        assert!(same_selected_ready(&route, denied).is_none());
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_candidates(
    registry: &Registry<'_>,
    provider_registry: &[provenant_providers::Registration],
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
        return prepare_command(registry, task, paths, resume, locate, provider, name);
    }
    let default_limit = provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES;
    check_launch_prompt(prompt_text, paths, None, default_limit, "compiled default")?;
    let loaded_config = registry
        .roots
        .map(|roots| provenant_config::read_with_domains(roots, &crate::domains::SECTIONS));
    const ARGV_LIMIT_KEY: &str = "domains.run.argv_prompt_limit_bytes";
    let (argv_limit, argv_origin, argv_origin_record) = match &loaded_config {
        Some(Ok(config)) => {
            let origin = config.origins.get(ARGV_LIMIT_KEY);
            let label = if matches!(origin, Some(provenant_config::Origin::User { .. })) {
                "host configuration"
            } else {
                "compiled default"
            };
            (
                config.domains.run.argv_prompt_limit_bytes,
                label,
                origin.map_or_else(
                    || serde_json::json!({"kind": "compiled"}),
                    |value| serde_json::json!(value),
                ),
            )
        }
        Some(Err(error)) => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: {ARGV_LIMIT_KEY} unavailable ({error}); using compiled default"
            );
            (
                default_limit,
                "compiled default",
                serde_json::json!({"kind": "compiled"}),
            )
        }
        None => (
            default_limit,
            "compiled default",
            serde_json::json!({"kind": "compiled"}),
        ),
    };
    if task.fallback.is_empty()
        && let run::RequestedRoute::Model { model, .. } = &task.route
        && let Some((adapter, _)) = model.split_once('/')
    {
        check_launch_prompt(prompt_text, paths, Some(adapter), argv_limit, argv_origin)?;
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
    let source = match writer.as_ref() {
        Some(writer) => writer.worktree.path().to_owned(),
        None => source_cwd(registry, task)?,
    };
    let isolated =
        writer.is_none() && task.access.source_root.is_some() && !task.access.source_live;
    let cwd = if isolated {
        super::source_tree::ensure_source_roots(&source, &task.access, assets)?;
        super::source_tree::snapshot_source(registry, task, paths, &source, &git)?
    } else {
        source.clone()
    };
    // Status is evidence for both the brief and a source refusal. Failure to
    // collect it must never masquerade as a clean predecessor worktree.
    let uncommitted = if let (Some(writer), Some(_)) = (&writer, &task.source_head) {
        writer
            .uncommitted_paths(&git)
            .map_err(|error| format!("cannot inspect uncommitted predecessor edits: {error}"))?
    } else {
        Vec::new()
    };
    if let Some(writer) = &writer {
        if let Some(source_head) = task.source_head.as_deref() {
            let predecessor = task
                .head
                .as_deref()
                .ok_or("missing predecessor head for writer continuation")?
                .parse()
                .map_err(|_| "invalid pinned writer predecessor head")?;
            let source = source_head
                .parse()
                .map_err(|_| "invalid pinned writer source")?;
            let lineage = writer.require_continuation_source(&git, source, predecessor);
            if !uncommitted.is_empty() {
                let detail = lineage.err().map_or_else(
                    || format!("writer continuation from {source} to {predecessor}"),
                    |error| error.to_string(),
                );
                return Err(format!(
                    "{detail}; uncommitted predecessor edits: {}",
                    uncommitted.join(", ")
                )
                .into());
            }
            lineage.map_err(|error| error.to_string())?;
        } else if let Some(head) = task.head.as_deref() {
            writer
                .require_source(head.parse().map_err(|_| "invalid pinned writer source")?)
                .map_err(|error| error.to_string())?;
        }
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
            Some(head) => run::GitContext::capture_at(&git, &source, base, head),
            None => run::GitContext::capture(&git, &source, base),
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
    let config = loaded_config
        .ok_or("routing context is unavailable")?
        .map_err(|error| error.to_string())?;
    // Continue the applied decision rather than redrawing the named pool
    // using the new operation seed.
    let mut request = match resume {
        Some(prior) => continuation_request(prior)?,
        None => route_request(task)?,
    };
    request.intent = route_request(task)?.intent;
    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
    let mut eligible = provenant_routing::probe_adapters(
        &request,
        &catalogue,
        &config.domains.routing,
        provider_registry,
    )
    .map_err(|e| e.to_string())?;
    if resume.is_none()
        && let Some(context) = task.context.as_ref()
        && let (Some(risk), Some(kind)) =
            (context.risk_tier.as_deref(), context.task_kind.as_deref())
        && let Ok(now) = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())
        && let Some(model) = config
            .domains
            .routing
            .preferred_model(&request, risk, kind, now)
        && let Some(registration) = provider_registry
            .iter()
            .find(|entry| entry.adapter.id() == model.adapter())
        && !eligible.contains(&registration.adapter.id())
    {
        eligible.push(registration.adapter.id());
    }
    // Exclude updates during preparation when the bounded wait succeeds.
    let clock = provenant_os::clock::SystemClock::new();
    use provenant_core::Clock;
    let fallback_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&fallback_cancel);
    let provider_leases =
        available_provider_leases(roots.state.as_path(), &eligible, wait, &clock, cancel)
            .map_err(|error| error.to_string())?;
    let eligible = |name: &str| provider_leases.is_empty() || provider_leases.contains_key(name);
    let settings = config.domains.accounts.with_derived_accounts();
    let host_writes = config.domains.run;
    let mut entries: Vec<_> = settings.entries.into_values().collect();
    let mut default_failures = std::collections::BTreeMap::new();
    for spec in ADAPTER_SPECS.iter().filter(|spec| eligible(spec.name)) {
        let Some((environment, fallback)) = spec.default_account_directory else {
            continue;
        };
        if entries.iter().any(|entry| entry.adapter == spec.name) {
            continue;
        }
        let directory = registry
            .host
            .and_then(|host| host.default_account_directories.get(spec.name).cloned())
            .or_else(|| std::env::var_os(environment).map(PathBuf::from))
            .unwrap_or_else(|| home.join(fallback));
        let mut entry = provenant_providers::accounts::Registration {
            adapter: spec.name.into(),
            label: "default".into(),
            key: String::new(),
            org_fingerprint: None,
            directory: directory.clone(),
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
        };
        let discovered = (|| -> Result<(), AccountObservationError> {
            let executable =
                locate(spec.name).map_err(AccountObservationError::VersionUnavailable)?;
            entry.directory = canonical(&directory)
                .map_err(|detail| default_directory_error(spec, &directory, detail))?;
            entry.key = provenant_providers::accounts::detect(
                &executor,
                spec.name,
                &executable,
                &home,
                &user,
                &entry.directory,
                executor.root(),
            )
            .map_err(|error| account_observation_error(&entry, &error))?;
            Ok(())
        })();
        match discovered {
            Ok(()) => entries.push(entry),
            Err(error) => {
                default_failures
                    .insert(spec.name.to_owned(), (error.skip_kind(), error.to_string()));
            }
        }
    }
    // These adapters use the CLI's existing local login and configuration.
    for spec in ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.local_login && eligible(spec.name))
    {
        let adapter = spec.name;
        if !entries.iter().any(|entry| entry.adapter == adapter) && locate(adapter).is_ok() {
            entries.push(provenant_providers::accounts::Registration {
                adapter: adapter.into(),
                label: spec.local_account.0.into(),
                key: spec.local_account.1.into(),
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
    let diagnostic_records = crate::quota::read_cache(roots).unwrap_or_default();
    let mut availability = Vec::new();
    let mut reasons: Vec<SkippedRoute> = Vec::new();
    let mut auth_failures = std::collections::BTreeMap::new();
    let mut observation_failures = std::collections::BTreeMap::new();
    let probe = Probe {
        roots: registry.roots,
        executor: &executor,
        home: &home,
        user: &user,
        scratch: &paths.scratch,
    };
    for entry in entries.iter().filter(|entry| eligible(&entry.adapter)) {
        match locate(&entry.adapter)
            .map_err(AccountObservationError::from)
            .and_then(|executable| {
                observe_account(entry, &executable, &probe, catalogue.revision(), timestamp)
            }) {
            Ok(mut record) => {
                let strategy = AdapterSpec::get(&entry.adapter)
                    .map_or(provenant_records::LoginReprobe::None, |spec| {
                        spec.login_reprobe
                    });
                if strategy != provenant_records::LoginReprobe::None {
                    let scope = provenant_quota::ReadinessScope {
                        host: provenant_quota::LOCAL_HOST.into(),
                        adapter: entry.adapter.clone(),
                        account_key: entry.key.clone(),
                        route: provenant_quota::ACCOUNT_ROUTE.into(),
                    };
                    let recorded =
                        crate::quota::read_readiness(roots.state.as_path(), &scope, timestamp);
                    let mismatch = recorded.reason
                        == Some(provenant_quota::ReadinessReason::AccountMismatch)
                        || diagnostic_records.iter().any(|r| {
                            r.account.adapter == entry.adapter
                                && r.account.account_key == entry.key
                                && r.last_error
                                    == Some(provenant_quota::ProbeFailure::AccountMismatch)
                        });
                    let auth_failure = mismatch
                        || matches!(
                            recorded.reason,
                            Some(
                                provenant_quota::ReadinessReason::SignedOut
                                    | provenant_quota::ReadinessReason::LoginExpired
                            )
                        );
                    if auth_failure && login_check_can_clear(&entry.adapter, mismatch) {
                        let executable = Path::new(&record.executable);
                        if strategy == provenant_records::LoginReprobe::ExecutableOnly {
                            // The version probe established executable availability,
                            // not login health. Let the selected launch check auth.
                            record.fixture_ref =
                                format!("installed/{}/login-unverified", entry.adapter);
                        } else {
                            let healthy = match strategy {
                                provenant_records::LoginReprobe::Codex => {
                                    codex_login_is_healthy(entry, executable, &probe)
                                }
                                provenant_records::LoginReprobe::Cursor => {
                                    cursor_login_is_healthy(entry, executable, &probe)
                                }
                                provenant_records::LoginReprobe::AccountIdentity => {
                                    provenant_providers::accounts::detect_for_registration(
                                        &executor,
                                        &entry.adapter,
                                        executable,
                                        &home,
                                        &user,
                                        &entry.directory,
                                        executor.root(),
                                    )
                                    .is_ok_and(|(key, _)| {
                                        key == entry.key || (!mismatch && entry.key == "local")
                                    })
                                }
                                provenant_records::LoginReprobe::None
                                | provenant_records::LoginReprobe::ExecutableOnly => false,
                            };
                            if healthy {
                                record.fixture_ref =
                                    format!("installed/{}/login-status", entry.adapter);
                            }
                        }
                    }
                }
                availability.push(record)
            }
            Err(AccountObservationError::AuthRequired(detail)) => {
                auth_failures.insert((entry.adapter.clone(), entry.key.clone()), detail.clone());
                observation_failures.insert((entry.adapter.clone(), entry.key.clone()), detail);
            }
            Err(error) => {
                observation_failures.insert(
                    (entry.adapter.clone(), entry.key.clone()),
                    format!("account {}: {error}", entry.label),
                );
            }
        }
    }
    // Availability is diagnostic evidence; admission uses the in-memory records.
    // A failed publication must not prevent a correctly prepared lane launching.
    let publish_availability =
        |records: &[provenant_providers::AvailabilityRecord]| -> Result<(), String> {
            let attempt_dir =
                Directory::open_anchor(paths.scratch.parent().ok_or("attempt path unavailable")?)
                    .map_err(|e| e.to_string())?;
            attempt_dir
                .atomic_write(
                    std::ffi::OsStr::new("availability.json"),
                    "availability",
                    &serde_json::to_vec(records).map_err(|e| e.to_string())?,
                    PublishMode::Replace,
                )
                .and_then(|p| p.require_durable())
                .map_err(|e| e.to_string())
        };
    let _ = publish_availability(&availability);
    let digest = task.authority_digest.to_string();
    let mut access = Vec::new();
    let mut plan_errors = std::collections::BTreeMap::new();
    let mut reported_plan_errors = std::collections::BTreeSet::new();
    for adapter in provider_registry
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
            let entry = matching.next().ok_or_else(|| PrepareError::Refusal {
                message: "the selected account is not registered for the requested route; run `provenant accounts list`".into(),
                cause: provenant_records::run::TerminalCause::UnregisteredAccount,
                note: None,
            })?;
            if matching.any(|other| other.key != entry.key || other.adapter != entry.adapter) {
                return Err(PrepareError::Other(
                    "account alias is ambiguous; use its exact registered key".into(),
                ));
            }
            Ok(entry.key.as_str())
        })
        .transpose()?
        .or(prior_account);
    let selected_adapter = resume
        .and_then(|prior| prior.applied_route.as_ref())
        .and_then(|route| route.provider.as_deref())
        .or_else(|| {
            selected.and_then(|key| {
                entries
                    .iter()
                    .find(|entry| {
                        entry.key == key
                            && requested_adapter.is_none_or(|adapter| entry.adapter == adapter)
                    })
                    .map(|entry| entry.adapter.as_str())
            })
        });
    let host = provenant_config::bind_host(roots)
        .map_err(|e| e.to_string())?
        .to_string();
    let rules = crate::route::task_exclusion_rules(&task.profile, roots)
        .map_err(|error| error.to_string())?;
    let family_constraints = match &task.route {
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
    };
    let resolve = |availability: &[provenant_providers::AvailabilityRecord],
                   rejected: &[provenant_routing::ModelId]| {
        crate::route::resolve_dispatch_rejecting(
            &request,
            roots,
            provider_registry,
            selected,
            (paths.operation_id.as_str(), paths.task_index),
            availability,
            &access,
            &host,
            family_constraints,
            &rules,
            rejected,
            task.context.as_ref().and_then(|context| {
                Some((context.risk_tier.as_deref()?, context.task_kind.as_deref()?))
            }),
        )
    };
    let resolved = resolve(&availability, &[]).map_err(route_resolution_refusal)?;
    let named_pool = resolved.pool_selection.is_some() && resume.is_none();
    if let Some(selection) = &resolved.pool_selection {
        for redraw in &selection.redraws {
            reasons.push(skip(
                &redraw.rejected_pick,
                SkipKind::Unavailable,
                &redraw.reason,
            ));
        }
    }
    // Candidate preparation happens before writer custody or guardian handoff.
    // Advance only for a typed provider outcome, within the sealed alternatives,
    // and never substitute a differently pinned account.
    let mut previous_outcome = provenant_routing::FallbackOutcome::Other;
    let mut queue: std::collections::VecDeque<_> = candidates(resolved).into();
    let mut rejected = Vec::new();
    let mut reprobed_accounts = std::collections::BTreeSet::new();
    let mut candidate = 0usize;
    let mut redraws = 0u8;
    loop {
        if queue.is_empty() {
            let allowed = provenant_routing::FallbackPolicy {
                outcome: previous_outcome,
                handoff: provenant_routing::FallbackHandoff::Local,
                custody_released: true,
                read_only: false,
                budget_remaining: named_pool && redraws < config.domains.routing.settlement_retries,
                pinned: false,
            }
            .allows_switch();
            if !allowed {
                if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired
                    && let Some(last) = reasons.last_mut()
                {
                    last.detail.push_str(if named_pool {
                        "; fallback retry budget exhausted"
                    } else {
                        "; no eligible fallback remains"
                    });
                }
                break;
            }
            let Ok(next) = resolve(&availability, &rejected) else {
                if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired
                    && let Some(last) = reasons.last_mut()
                {
                    last.detail.push_str("; no eligible fallback remains");
                }
                break;
            };
            redraws += 1;
            queue.extend(candidates(next));
        }
        let Some(mut route) = queue.pop_front() else {
            break;
        };
        let model =
            provenant_routing::ModelId::parse(&format!("{}/{}", route.adapter, route.model))
                .map_err(|error| error.to_string())?;
        if rejected.contains(&model) {
            continue;
        }
        let pinned = violates_account_pin(
            selected,
            selected_adapter,
            route.account_key.as_deref(),
            &route.adapter,
        );
        if candidate > 0
            && !(provenant_routing::FallbackPolicy {
                outcome: previous_outcome,
                handoff: provenant_routing::FallbackHandoff::Local,
                custody_released: true,
                read_only: false,
                budget_remaining: true,
                pinned,
            })
            .allows_switch()
        {
            if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired
                && let Some(last) = reasons.last_mut()
            {
                last.detail.push_str(if pinned {
                    "; fallback unavailable because the account is pinned"
                } else {
                    "; fallback unavailable under the route policy"
                });
            }
            break;
        }
        rejected.push(model);
        let current_candidate = candidate;
        candidate += 1;
        previous_outcome = provenant_routing::FallbackOutcome::Other;
        let label = format!("{}/{}", route.adapter, route.model);
        if !eligible(&route.adapter) {
            previous_outcome = provenant_routing::FallbackOutcome::AdapterUnavailable;
            reasons.push(skip(
                &label,
                SkipKind::Unavailable,
                "provider lease unavailable",
            ));
            continue;
        }
        if let Some(spec) = AdapterSpec::get(&route.adapter)
            && let Err(error) =
                request
                    .intent
                    .check_launch(spec, &route.model, route.effort.as_deref())
        {
            reasons.push(skip(&label, SkipKind::Refused, error.to_string()));
            continue;
        }
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
                provider_registry,
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
        let Some(registration) = provider_registry
            .iter()
            .find(|registration| registration.adapter.id() == route.adapter)
        else {
            previous_outcome = provenant_routing::FallbackOutcome::AdapterUnavailable;
            reasons.push(skip(&label, SkipKind::Unavailable, "unknown adapter"));
            continue;
        };
        let adapter = registration.adapter;
        if matches!(
            route.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::StaleAvailability,
                ..
            }
        ) {
            if let Some(key) = route.account_key.clone() {
                let mut exact = request.clone();
                exact.choice = provenant_routing::RouteChoice::Model(
                    provenant_routing::ModelId::parse(&label).map_err(|error| error.to_string())?,
                );
                exact.effort = route.effort.clone();
                exact.fallback = provenant_routing::Fallback::None;
                let revalidate = |records: &[provenant_providers::AvailabilityRecord]| {
                    crate::route::resolve_dispatch_rejecting(
                        &exact,
                        roots,
                        provider_registry,
                        Some(&key),
                        (paths.operation_id.as_str(), paths.task_index),
                        records,
                        &access,
                        &host,
                        family_constraints,
                        &rules,
                        &[],
                        task.context.as_ref().and_then(|context| {
                            Some((context.risk_tier.as_deref()?, context.task_kind.as_deref()?))
                        }),
                    )
                    .ok()
                };
                let observed_at = clock.unix_seconds();
                if let Some(index) = stale_observation_index(
                    &route,
                    &availability,
                    observed_at,
                    catalogue.revision(),
                ) {
                    if reprobed_accounts.insert((route.adapter.clone(), key.clone())) {
                        let old = availability[index].clone();
                        if let Some(entry) = entries.iter().find(|entry| {
                            entry.adapter == old.adapter
                                && Some(&entry.key) == old.account_key.as_ref()
                        }) {
                            let recheck_probe = Probe {
                                roots: None,
                                executor: probe.executor,
                                home: probe.home,
                                user: probe.user,
                                scratch: probe.scratch,
                            };
                            if let Ok(Some(rechecked)) = retry_stale_selected(
                                &mut availability,
                                &route,
                                observed_at,
                                catalogue.revision(),
                                || {
                                    locate(&entry.adapter)
                                        .map_err(AccountObservationError::from)
                                        .and_then(|executable| {
                                            observe_account(
                                                entry,
                                                &executable,
                                                &recheck_probe,
                                                catalogue.revision(),
                                                observed_at,
                                            )
                                        })
                                        .map(|mut fresh| {
                                            fresh.last_update = old.last_update.clone();
                                            fresh
                                        })
                                },
                                revalidate,
                            ) {
                                route = rechecked;
                                let _ = publish_availability(&availability);
                            }
                        }
                    }
                } else if let Some(rechecked) =
                    revalidate(&availability).and_then(|next| same_selected_ready(&route, next))
                {
                    // Another route of this account may reuse the already
                    // refreshed observation without running a second probe.
                    route = rechecked;
                }
            }
            if matches!(
                route.readiness,
                provenant_records::RouteReadiness::Unavailable {
                    reason: provenant_records::RouteExclusionReason::StaleAvailability,
                    ..
                }
            ) {
                let diagnostic = run::ErrorKind::StaleReadiness.diagnostic();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning [{}]: {}; next: {}",
                    diagnostic.code,
                    diagnostic
                        .detail
                        .as_deref()
                        .unwrap_or("account readiness is stale"),
                    diagnostic.next.as_deref().unwrap_or("re-probe the account")
                );
            }
        }
        if let provenant_records::RouteReadiness::Unavailable { reason, .. } = &route.readiness {
            if matches!(
                reason,
                provenant_records::RouteExclusionReason::UnknownAvailability
            ) {
                if let Some((kind, detail)) = default_failures.get(&route.adapter) {
                    previous_outcome = if *kind == SkipKind::AuthRequired {
                        provenant_routing::FallbackOutcome::AuthRequired
                    } else {
                        provenant_routing::FallbackOutcome::AdapterUnavailable
                    };
                    let mut skipped = skip(&label, *kind, detail);
                    skipped.set_cause(if *kind == SkipKind::AuthRequired {
                        provenant_records::RouteExclusionReason::AuthRequired
                    } else {
                        reason.clone()
                    });
                    if *kind == SkipKind::AuthRequired {
                        skipped.detail = provenant_records::diagnostic::safe_text(detail);
                    }
                    reasons.push(skipped);
                    continue;
                }
                let relevant: Vec<_> = entries
                    .iter()
                    .filter(|entry| {
                        entry.adapter == route.adapter
                            && selected
                                .or(route.account_key.as_deref())
                                .is_none_or(|key| key == entry.key)
                    })
                    .filter_map(|entry| {
                        observation_failures.get(&(entry.adapter.clone(), entry.key.clone()))
                    })
                    .collect();
                if unavailable_auth_detail(&route, selected, &entries, &auth_failures).is_none()
                    && !relevant.is_empty()
                {
                    previous_outcome = provenant_routing::FallbackOutcome::AdapterUnavailable;
                    let mut skipped = skip(
                        &label,
                        SkipKind::Account,
                        relevant.into_iter().cloned().collect::<Vec<_>>().join("; "),
                    );
                    skipped.set_cause(reason.clone());
                    reasons.push(skipped);
                    continue;
                }
            }
            let outcome = provenant_routing::FallbackOutcome::from_exclusion(reason);
            if outcome == provenant_routing::FallbackOutcome::AuthRequired {
                previous_outcome = outcome;
                let key = selected.or(route.account_key.as_deref());
                let detail = crate::quota::account_login_diagnostic(
                    entries.iter(),
                    &diagnostic_records,
                    &route.adapter,
                    key,
                    reason,
                );
                let mut skipped = skip(&label, SkipKind::AuthRequired, &detail);
                skipped.set_cause(reason.clone());
                // This detail is assembled from registered account facts, not
                // provider output. Remote projections still use typed facts.
                skipped.detail = provenant_records::diagnostic::safe_text(&detail);
                reasons.push(skipped);
                continue;
            }
            if matches!(
                reason,
                provenant_records::RouteExclusionReason::UnknownAvailability
            ) && let Some(detail) =
                unavailable_auth_detail(&route, selected, &entries, &auth_failures)
            {
                previous_outcome = provenant_routing::FallbackOutcome::AuthRequired;
                let mut skipped = skip(&label, SkipKind::AuthRequired, &detail);
                skipped.set_cause(provenant_records::RouteExclusionReason::AuthRequired);
                skipped.detail = provenant_records::diagnostic::safe_text(&detail);
                reasons.push(skipped);
                continue;
            }
            previous_outcome = outcome;
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
                    || {
                        route
                            .extra
                            .get("account_exclusions")
                            .and_then(|v| v.as_array())
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                            .unwrap_or_else(|| provenant_routing::exclusion_detail(reason))
                    },
                    |error| {
                        reported_plan_errors.insert(route.adapter.clone());
                        format!("confinement plan: {error}")
                    },
                );
            let mut skipped = skip(&label, SkipKind::Unavailable, detail);
            skipped.set_cause(reason.clone());
            reasons.push(skipped);
            continue;
        }
        if let Err(error) = crate::route::check_dispatch_candidate(roots, &route, provider_registry)
        {
            reasons.push(skip(&label, SkipKind::Unavailable, error));
            continue;
        }
        let entry = entries
            .iter()
            .find(|e| e.adapter == route.adapter && Some(&e.key) == route.account_key.as_ref());
        let account = match entry
            .filter(|entry| {
                AdapterSpec::get(&entry.adapter).is_none_or(|spec| {
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
                previous_outcome = provenant_routing::FallbackOutcome::AdapterUnavailable;
                reasons.push(skip(&label, SkipKind::Unavailable, error));
                continue;
            }
        };
        // Every lane stages temporary files under tmp for terminal cleanup.
        // A failed setup never poisons the next explicit fallback candidate.
        let staging = &paths.temporary;
        let scratch = staging.join(format!("candidate-{current_candidate}"));
        let private_state = paths
            .provider_state
            .join(format!("candidate-{current_candidate}"));
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
        let text = if !task.access.attachments.is_empty()
            && !AdapterSpec::get(adapter.id()).is_some_and(|spec| spec.launch.image_input)
        {
            let paths = task
                .access
                .attachments
                .iter()
                .map(|path| format!("- {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{text}\n\nAttached images (open these files if supported):\n{paths}")
        } else {
            text
        };
        let text = format!(
            "{}\n{text}",
            run::lane_preamble(
                &model_policy.guidance(),
                writer.is_some(),
                &private_state.join("handoff.json")
            )
        );
        let prompt = match launch_prompt(
            &text,
            paths,
            state_root,
            adapter.id(),
            argv_limit,
            argv_origin,
        ) {
            Ok(prompt) => prompt,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Refused, error.to_string()));
                continue;
            }
        };
        let child_state = state_root
            .join("children")
            .join(paths.attempt_id.to_string());
        let (plan, observed_head, repository, worktree_gitdir) = match confinement_request(
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
                        Some(writer.worktree.repo().private_git_dir().to_owned()),
                    )
                })
                .map_err(|error| error.to_string()),
            None => read_only_plan(
                roots,
                project_id,
                &source,
                &git,
                request,
                isolated,
                task.access.source_live,
            )
            .and_then(|mut planned| {
                if isolated {
                    planned.1 = Some(
                        task.head
                            .as_deref()
                            .ok_or("graph source head missing")?
                            .parse()
                            .map_err(|_| "invalid graph source head")?,
                    );
                    planned.3 = Some(cwd.join(".git"));
                } else {
                    require_source_head(task, planned.1)?;
                }
                Ok(planned)
            }),
        }) {
            Ok(planned) => planned,
            Err(error) => {
                reasons.push(skip(&label, SkipKind::Refused, error));
                continue;
            }
        };
        let adapter_spec = AdapterSpec::get(adapter.id());
        let refresh_token = adapter_spec.is_some_and(|spec| spec.launch_token_refresh);
        let refreshed = if refresh_token && fixture_token.is_none() {
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
                    previous_outcome = provenant_routing::FallbackOutcome::AuthRequired;
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
                    previous_outcome = provenant_routing::FallbackOutcome::AuthRequired;
                    reasons.push(credential_failure_skip(
                        &label,
                        &error,
                        entry.map_or("unknown", |e| e.label.as_str()),
                        account.directory(),
                    ));
                    continue;
                }
            }
        } else {
            None
        };
        let token = if refresh_token {
            fixture_token.or(refreshed.as_ref())
        } else {
            None
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
        let prepare = |provider_route: &provenant_records::ResolvedRoute| {
            adapter.prepare(PrepareInput {
                images: &task.access.attachments,
                command: None,
                route: provider_route,
                confined: &plan,
                prompt: &prompt,
                continuation: continuation.as_ref(),
                grant_digest: &grant_digest,
                executable: &executable,
                home: &home,
                user: &user,
                host_var: &|key| std::env::var_os(key),
                skill_stage_max_bytes: config.domains.lanes.skill_stage_max_bytes,
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
            })
        };
        let mut spec = match prepare(&provider_route) {
            Ok(spec) => spec,
            Err(error) => {
                previous_outcome = provider_preparation_fallback(&error);
                let kind = if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired {
                    SkipKind::AuthRequired
                } else {
                    SkipKind::Refused
                };
                reasons.push(skip(&label, kind, error.to_string()));
                continue;
            }
        };
        if !materialise_user_skills(&mut spec.user_skills, |stage, setup| {
            apply_setup_directory(&[staging], stage)
                .and_then(|_| apply_setup(setup, &[stage], None))
        }) {
            let skills = std::mem::take(&mut spec.user_skills);
            let mut without_skills = provider_route.clone();
            without_skills.set_user_skills(false);
            spec = match prepare(&without_skills) {
                Ok(spec) => spec,
                Err(error) => {
                    previous_outcome = provider_preparation_fallback(&error);
                    let kind =
                        if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired {
                            SkipKind::AuthRequired
                        } else {
                            SkipKind::Refused
                        };
                    reasons.push(skip(&label, kind, error.to_string()));
                    continue;
                }
            };
            spec.user_skills = skills;
        }
        if let Some(alias) = &paths.temporary_alias {
            alias_temporary_env(
                &mut spec.env,
                scratch,
                &alias.join(format!("candidate-{current_candidate}")),
            );
        }
        apply_lane_capabilities(&mut spec.profile.spec, &task.access);
        let common_git_path = repository
            .as_ref()
            .map(|path| path.to_path_buf())
            .transpose()
            .map_err(|error| error.to_string())?;
        let project_paths = super::project_paths::apply(
            &mut spec,
            &plan,
            super::project_paths::Context {
                cwd: &cwd,
                home: &home,
                common_git: common_git_path.as_deref(),
                state_root: roots.state.as_path(),
                worktree_gitdir: worktree_gitdir.as_deref(),
                writer: writer.is_some(),
                settings: config.domains.lanes.projects.get(&project_id_str),
                project: &project_id_str,
            },
        );
        configure_process_tree(&mut spec, paths)?;
        let build_settings = config.domains.fabric.lanes.build_settings();
        configure_build_slots(
            &mut spec,
            &build_settings,
            &home,
            scratch,
            &provenant_executable,
            roots.state.as_path(),
        );
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
        if let Some(adapter_spec) = adapter_spec {
            spec.configure_shell(scratch, adapter_spec);
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
        if git_context.is_some()
            && let Some(argument) = adapter_spec.and_then(|spec| spec.context_directory_arg)
        {
            // Scratch is already granted by the outer profile. Claude's file
            // tools also need it in their working directories to read context.
            spec.argv
                .splice(1..1, [argument.into(), scratch.as_os_str().to_owned()]);
        }
        spec.complete_tool_requirements(&plan, &home, &cwd);
        if isolated {
            super::source_tree::ensure_profile_isolated(&source, &spec.profile.spec)?;
            super::source_tree::ensure_view_read_only(
                &cwd,
                &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
            )?;
        }
        let transcript_account = account
            .as_ref()
            .filter(|_| adapter_spec.is_some_and(|spec| spec.transcript_account_root))
            .map(|account| account.directory());
        let mut setup_roots = vec![scratch, private_state, roots.state.as_path()];
        // Only directory creation uses these additional provider state roots;
        // generated files and links remain confined to attempt-owned roots.
        let xdg_data_home = spec
            .env
            .iter()
            .find(|(key, _)| key == "XDG_DATA_HOME")
            .map(|(_, value)| Path::new(value));
        match adapter_spec.map_or(provenant_records::SetupRoot::None, |spec| spec.setup_root) {
            provenant_records::SetupRoot::XdgDataHome => {
                if let Some(data) = xdg_data_home {
                    let existing = data
                        .ancestors()
                        .find(|path| path.is_dir())
                        .ok_or("provider data directory has no existing ancestor")?;
                    setup_roots.push(existing);
                }
            }
            provenant_records::SetupRoot::Home => setup_roots.push(&home),
            provenant_records::SetupRoot::None => {}
        }
        if let Err(error) = apply_setup(&spec.setup, &setup_roots, transcript_account) {
            reasons.push(skip(
                &label,
                SkipKind::Setup,
                format!("setup failed: {error}"),
            ));
            continue;
        }
        if AdapterSpec::get(&route.adapter).is_some_and(|spec| {
            spec.login_observation == provenant_records::LoginObservation::ExecutableOnly
        }) && availability.iter().any(|record| {
            record.adapter == route.adapter
                && record.account_key == route.account_key
                && record.fixture_ref == format!("installed/{}/login-unverified", route.adapter)
        }) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: OpenCode login status is unverified; the selected launch will check authentication"
            );
        }
        if let Some(warning) = image_input_warning(&route.adapter, &task.access.attachments) {
            let _ = writeln!(std::io::stderr().lock(), "{warning}");
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
                Some(
                    prepare_writer_dependencies(
                        writer.worktree.path(),
                        enabled,
                        &|| host_dependency_path(cancel),
                        cancel,
                    )
                    .map_err(|error| {
                        dependency_preparation_refusal(error, enabled, &task.profile)
                    })?,
                )
            }
            None => None,
        };
        let mut provider = launch_route_record(
            &route,
            entry.map(|e| e.label.as_str()),
            &task.route,
            resume.is_some(),
            redirected || current_candidate > 0,
        );
        if let Some(warning) = provider
            .extra
            .get("availability_warning")
            .and_then(|v| v.as_str())
        {
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
        }
        provider.extra.insert(
            "account_source".into(),
            account_source(resume.is_some(), task.account.is_some()).into(),
        );
        provider.extra.insert("project_paths".into(), project_paths);
        provider.extra.insert(
            "argv_prompt_limit".into(),
            serde_json::json!({
                "value": argv_limit, "key": ARGV_LIMIT_KEY, "origin": argv_origin_record.clone(),
            }),
        );
        if let Some(event) = &dependency_event {
            provider
                .extra
                .insert("dependency_preparation".into(), event.clone());
        }
        let mut skills =
            user_skills_exposure(user_skills_mode, user_skills_origin, &spec.user_skills);
        skills["stage_max_bytes"] = serde_json::json!({
            "value": config.domains.lanes.skill_stage_max_bytes,
            "origin": config.origins.get("domains.lanes.skill_stage_max_bytes"),
            "key": "domains.lanes.skill_stage_max_bytes",
        });
        provider.extra.insert("user_skills".into(), skills);
        provider.extra.insert(
            "tool_closure".into(),
            serde_json::Value::Array(
                spec.prepared_tools()
                    .iter()
                    .map(|tool| tool.receipt())
                    .collect(),
            ),
        );
        provider.extra.insert(
            "applied_access".into(),
            spec.applied_access().receipt(&home),
        );
        provider.extra.insert(
            "discovery_warnings".into(),
            serde_json::json!(discovery_warnings(&spec.omitted_discovery, &home, false)),
        );
        let stdin = match spec.stdin {
            Stdin::Bytes(bytes) => bytes,
            Stdin::Null => Vec::new(),
        };
        // Resolve gh credentials on the host, where a keychain login is
        // available. The token travels only in the live launch environment.
        if writer.is_none()
            && task.access.network(true) == NetworkAccess::Open
            && adapter_spec.is_some_and(|spec| spec.github_auth.is_some())
            && let Ok(gh) = locate("gh")
        {
            super::gh_auth::inject(&executor, &gh, &home, cancel, &mut spec.env);
            if adapter_spec.is_some_and(|spec| {
                spec.github_auth == Some(provenant_records::GithubAuth::ShellEnvironment)
            }) && spec.env.iter().any(|(key, _)| key == "GH_TOKEN")
            {
                super::gh_auth::codex_shell_environment(&spec.env, &mut spec.argv);
            }
        }
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
        fallback_outcome: match previous_outcome {
            provenant_routing::FallbackOutcome::AuthRequired => {
                Some(provenant_records::run::PreparationFailure::AuthRequired)
            }
            provenant_routing::FallbackOutcome::UsageLimited => {
                Some(provenant_records::run::PreparationFailure::UsageLimited)
            }
            provenant_routing::FallbackOutcome::AdapterUnavailable => {
                Some(provenant_records::run::PreparationFailure::AdapterUnavailable)
            }
            provenant_routing::FallbackOutcome::Other => None,
        },
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
        skipped_routes: reasons,
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
    let mut record = RouteRecord::from_resolved(route, account_label, source);
    if let provenant_records::RouteReadiness::Ready { extra, .. } = &route.readiness
        && let Some(state) = extra.get("availability_warning").and_then(|v| v.as_str())
    {
        let warning = format!(
            "account {} availability is {state}; the provider will check it",
            account_label
                .or(route.account_key.as_deref())
                .unwrap_or("unknown")
        );
        record
            .extra
            .insert("availability_warning".into(), warning.into());
    }
    record
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
            choice_context: None,
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
        let mut uncertain = resolved.clone();
        uncertain.readiness = provenant_records::RouteReadiness::Ready {
            extra: [("availability_warning".into(), serde_json::json!("Stale"))]
                .into_iter()
                .collect(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 99,
        };
        let warned = launch_route_record(&uncertain, Some("work"), &requested, false, false);
        assert!(
            warned.extra["availability_warning"]
                .as_str()
                .is_some_and(|warning| warning.contains("work") && warning.contains("Stale"))
        );
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

/// One warning line for an omitted grant, shown relative to the home directory.
pub(super) fn omission_warning(entry: &provenant_providers::OmittedGrant, home: &Path) -> String {
    let display = if entry
        .path
        .to_str()
        .is_none_or(|text| text.chars().any(char::is_control))
    {
        "<unrepresentable path; inspect on host>".into()
    } else {
        entry
            .path
            .strip_prefix(home)
            .map(|relative| format!("~/{}", relative.display()))
            .unwrap_or_else(|_| "<external path; inspect on host>".into())
    };
    if entry.source == provenant_providers::OmissionSource::ProviderPackage {
        return format!(
            "omitted shared provider package grant {display}; using the executable only"
        );
    }
    let guard = if provenant_confinement::refuses_lane_root(&entry.path, true, Some(home)) {
        "credential root guard"
    } else if !provenant_confinement::permissions::profile_path_representable(&entry.path) {
        "path cannot be represented in the sandbox profile"
    } else {
        "protected root guard"
    };
    format!("omitted discovered grant {display}: {guard}")
}

/// Warnings for omitted grants of one kind. Skill omissions belong under
/// `user_skills.warnings`; every other source is a generic launch-grant
/// warning recorded as `discovery_warnings` on the route record.
pub(super) fn discovery_warnings(
    omitted: &[provenant_providers::OmittedGrant],
    home: &Path,
    skills: bool,
) -> Vec<String> {
    omitted
        .iter()
        .filter(|entry| (entry.source == provenant_providers::OmissionSource::UserSkill) == skills)
        .map(|entry| omission_warning(entry, home))
        .collect()
}

/// Report the same provider-owned locations and grants used during prepare.
/// Materialise the captured tree; an I/O failure drops only optional skills.
fn materialise_user_skills(
    staged: &mut provenant_providers::StagedUserSkills,
    setup: impl FnOnce(&Path, &[SetupStep]) -> Result<(), String>,
) -> bool {
    let Some(root) = &staged.root else {
        return true;
    };
    if setup(root, staged.setup()).is_ok() {
        return true;
    }
    staged.discard();
    staged.warnings.push(provenant_providers::SkillWarning {
        path: PathBuf::new(),
        reason: "staging_failed",
    });
    false
}

fn user_skills_exposure(
    mode: provenant_records::UserSkillsMode,
    origin: serde_json::Value,
    staged: &provenant_providers::StagedUserSkills,
) -> serde_json::Value {
    let warnings: Vec<_> = staged
        .warnings
        .iter()
        .map(|warning| {
            serde_json::json!({
                "code": "user_skill_skipped", "reason": warning.reason, "path": warning.path.to_string_lossy(),
            })
        })
        .collect();
    serde_json::json!({
        "mode": mode,
        "exposed": staged.root.is_some(),
        "directory": staged.source,
        "staged_file_count": staged.file_count,
        "warnings": warnings,
        "source": origin,
    })
}

#[cfg(test)]
mod user_skills_tests {
    use super::*;
    #[test]
    fn user_skills_receipt_matches_each_prepared_adapter() -> Result<(), Box<dyn std::error::Error>>
    {
        use provenant_confinement::permissions::{
            AccessMode, NetworkAccess, PathGrant, PathScope, PlanPermissions,
        };
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        use provenant_core::{AttemptId, ProjectId, WorkScope};
        use provenant_providers::{LaneRuntime, PrepareInput, Prompt, SetupStep};
        use provenant_records::{
            ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness, UserSkillsMode,
        };
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let token = provenant_providers::claude::OauthToken::fixture()?;
        let prompt = Prompt::new("fixture".into());
        for adapter in provenant_records::ADAPTER_SPECS
            .iter()
            .filter(|entry| entry.skills.is_some())
        {
            let root = base.join(adapter.name);
            let home = root.join("home");
            let skills = match adapter.name {
                "claude" => home.join(".claude/skills"),
                "codex" => home.join(".agents/skills"),
                "cursor" => home.join(".cursor/skills"),
                "agy" => home.join(".gemini/skills"),
                "opencode" => home.join(".config/opencode/skills"),
                other => panic!("missing skills fixture for {other}"),
            };
            std::fs::create_dir_all(&home)?;
            let executable = base.join("package/bin/provider");
            std::fs::create_dir_all(executable.parent().ok_or("package")?)?;
            std::fs::write(&executable, b"fixture")?;
            let grant = |path| PathGrant {
                path,
                scope: PathScope::Subtree,
            };
            let auth = match adapter.name {
                "codex" | "cursor" => Some("login".into()),
                "agy" => Some("keychain".into()),
                _ => None,
            };
            let plan = ConfinedPlan::new(PlanRequest {
                scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
                permissions: PlanPermissions {
                    mode: AccessMode::ReadOnly,
                    read_only: None,
                    scratch: grant(root.join("scratch")),
                    private_state: Some(grant(root.join("state"))),
                    read_roots: vec![grant(root.join("repo"))],
                    protected_roots: vec![],
                    secret_roots: vec![],
                    selected_authentication: auth,
                    include_continuation: false,
                    write_roots: vec![],
                    network: NetworkAccess::Provider,
                    nested_confinement: false,
                },
            })?;
            let mut route = ResolvedRoute {
                extra: Default::default(),
                adapter: adapter.name.into(),
                model: "fixture".into(),
                effort: None,
                catalogue_revision: "fixture".into(),
                account_key: None,
                host: "fixture".into(),
                alternatives: Default::default(),
                exclusions: vec![],
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
            let provider = provenant_providers::REGISTRY
                .iter()
                .find(|entry| entry.adapter.id() == adapter.name)
                .ok_or("adapter")?
                .adapter;
            for writer in [false, true] {
                let mut permissions = plan.permissions().clone();
                if writer {
                    permissions.mode = AccessMode::WorktreeWrite(Box::new(serde_json::from_value(
                        serde_json::json!({
                            "git_grants_digest": vec![0; 32], "publishing": false,
                            "common_git_dir": root.join("repo/.git"),
                            "cargo_cache": {"path": root.join("cache"), "scope": "subtree"}
                        }),
                    )?));
                }
                let plan = ConfinedPlan::new(PlanRequest {
                    scope: plan.scope(),
                    codex: CodexPolicy::Clean,
                    extensions: ExtensionPolicy::Deny,
                    agy_state: None,
                    permissions,
                })?;
                for state in ["missing", "discover", "off"] {
                    if state == "discover" {
                        std::fs::create_dir_all(&skills)?;
                        std::fs::write(skills.join("SKILL.md"), b"fixture")?;
                    }
                    route.set_user_skills(state != "off");
                    let input = PrepareInput {
                        route: &route,
                        confined: &plan,
                        prompt: &prompt,
                        images: &[],
                        command: None,
                        continuation: None,
                        grant_digest: "fixture",
                        executable: &executable,
                        home: &home,
                        user: "fixture".as_ref(),
                        host_var: &|_| None,
                        skill_stage_max_bytes: 16 * 1024 * 1024,
                        account: None,
                        token: Some(&token),
                        cwd: &root.join("repo"),
                        git_context: false,
                        nonce: [9; 16],
                        provenant: LaneRuntime {
                            executable: &executable,
                            git: &executable,
                            state: &root,
                            child_state: &root,
                            home: &home,
                            depth: 1,
                            attempt: AttemptId::from_bytes([7; 16]),
                            assets: &root,
                        },
                    };
                    let spec = provider.prepare(input)?;
                    let mode = if state == "off" {
                        UserSkillsMode::Off
                    } else {
                        UserSkillsMode::Discover
                    };
                    let receipt =
                        user_skills_exposure(mode, serde_json::Value::Null, &spec.user_skills);
                    assert_eq!(
                        receipt["exposed"],
                        state == "discover",
                        "{} {state}",
                        adapter.name
                    );
                    if let Some(stage) = &spec.user_skills.root {
                        assert!(
                            spec.profile
                                .spec
                                .runtime_reads
                                .contains(&grant(stage.clone()))
                        );
                        assert!(
                            spec.profile
                                .spec
                                .runtime_reads
                                .iter()
                                .all(|read| read.path != skills
                                    && (read.scope != PathScope::Subtree
                                        || !skills.starts_with(&read.path))),
                            "{}: {:?}",
                            adapter.name,
                            spec.profile.spec.runtime_reads
                        );
                        assert!(spec.setup.iter().any(
                        |step| matches!(step, SetupStep::Symlink { target, .. } if target == stage)
                    ));
                    }
                }
                std::fs::remove_dir_all(&skills)?;
            }
        }
        Ok(())
    }

    #[test]
    fn shared_provider_package_omission_reports_narrow_fallback() {
        let entry = provenant_providers::OmittedGrant {
            path: PathBuf::from("/usr/local"),
            source: provenant_providers::OmissionSource::ProviderPackage,
        };
        let warnings = discovery_warnings(&[entry], Path::new("/fixture/home"), false);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("executable only"));
    }

    #[test]
    fn unrepresentable_discovery_reports_why_it_was_omitted() {
        use std::os::unix::ffi::OsStringExt;
        let entry = provenant_providers::OmittedGrant {
            path: PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/tool-\xff".to_vec())),
            source: provenant_providers::OmissionSource::Toolchain,
        };
        let warnings = discovery_warnings(&[entry], Path::new("/fixture/home"), false);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("cannot be represented"));
        let control = provenant_providers::OmittedGrant {
            path: PathBuf::from("/fixture/home/tool-\u{1}"),
            source: provenant_providers::OmissionSource::Toolchain,
        };
        let warning = omission_warning(&control, Path::new("/fixture/home"));
        assert!(warning.contains("unrepresentable path"));
        assert!(!warning.contains('\u{1}'));
    }

    #[test]
    fn staging_setup_failure_continues_without_skills() {
        let mut staged = provenant_providers::StagedUserSkills::default();
        staged.root = Some(PathBuf::from("/attempt/skills"));
        staged.source = Some("~/.agents/skills".into());
        staged.file_count = 3;
        assert!(!materialise_user_skills(&mut staged, |_, _| Err(
            "injected I/O failure".into()
        )));
        assert!(staged.root.is_none());
        assert_eq!(staged.file_count, 0);
        assert_eq!(staged.source.as_deref(), Some("~/.agents/skills"));
        assert!(
            staged
                .warnings
                .iter()
                .any(|warning| warning.reason == "staging_failed")
        );
        let record = user_skills_exposure(
            provenant_records::UserSkillsMode::Discover,
            serde_json::Value::Null,
            &staged,
        );
        assert_eq!(record["exposed"], false);
        assert_eq!(record["warnings"][0]["reason"], "staging_failed");
    }

    #[test]
    fn user_skills_receipt_reports_each_skipped_reason() {
        let mut staged = provenant_providers::StagedUserSkills::default();
        staged.root = Some(PathBuf::from("/attempt/skills"));
        for reason in ["credential_root", "cycle", "changed_during_copy"] {
            staged.warnings.push(provenant_providers::SkillWarning {
                path: PathBuf::from(reason),
                reason,
            });
        }
        let receipt = user_skills_exposure(
            provenant_records::UserSkillsMode::Discover,
            serde_json::Value::Null,
            &staged,
        );
        assert_eq!(receipt["exposed"], true);
        for (warning, reason) in receipt["warnings"].as_array().unwrap().iter().zip([
            "credential_root",
            "cycle",
            "changed_during_copy",
        ]) {
            assert_eq!(warning["code"], "user_skill_skipped");
            assert_eq!(warning["reason"], reason);
        }
    }

    #[test]
    fn receipt_uses_prepared_skill_facts() {
        let staged = provenant_providers::StagedUserSkills::default();
        let origin = serde_json::json!({"key": "domains.lanes.user_skills", "origin": "compiled"});
        let record = user_skills_exposure(
            provenant_records::UserSkillsMode::Discover,
            origin.clone(),
            &staged,
        );
        assert_eq!(record["exposed"], false);
        assert_eq!(record["staged_file_count"], 0);
        assert_eq!(record["source"], origin);
        let off = user_skills_exposure(
            provenant_records::UserSkillsMode::Off,
            serde_json::Value::Null,
            &staged,
        );
        assert_eq!(off["exposed"], false);
        assert_eq!(off["staged_file_count"], 0);
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn unsupported_image_input_warns_without_refusing() {
        let images = vec![PathBuf::from("/outside/image.png")];
        let warning = image_input_warning("cursor", &images).unwrap();
        assert!(warning.contains("cursor") && warning.contains("image_input"));
        assert!(image_input_warning("claude", &images).is_none());
        assert!(image_input_warning("cursor", &[]).is_none());
    }
}

#[cfg(test)]
mod refusal_cause_tests {
    use super::*;

    #[test]
    fn resolver_refusal_retains_closed_classification_without_error_text() {
        let error = crate::route::DispatchError::Catalogue(
            provenant_routing::RoutingError::UnsupportedEffort {
                model: "PROVIDER_SECRET_SENTINEL".into(),
                effort: "PROVIDER_SECRET_SENTINEL".into(),
                support: "PROVIDER_SECRET_SENTINEL".into(),
            },
        );
        let refusal = route_resolution_refusal(error).into_refusal();
        assert_eq!(
            refusal.terminal_cause,
            Some(provenant_records::run::TerminalCause::RouteResolutionRefused)
        );
        assert_eq!(
            refusal.terminal_note.as_deref(),
            Some("route resolver classification: unsupported_effort")
        );
        assert!(refusal.skipped_routes.is_empty());
        assert!(
            !refusal
                .terminal_note
                .unwrap()
                .contains("PROVIDER_SECRET_SENTINEL")
        );
    }
}
