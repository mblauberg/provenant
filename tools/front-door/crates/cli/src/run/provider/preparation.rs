use super::*;

/// Resolve a provider or named command through the filtered requester PATH.
pub(super) fn preparation_executable(
    path: &run::FilteredLanePath,
    name: &str,
) -> Result<PathBuf, String> {
    let name = AdapterSpec::get(name).map_or(name, |spec| spec.binary);
    path.resolve(name)
}

fn include_local_login(
    spec: &AdapterSpec,
    entries: &[provenant_providers::accounts::Registration],
    selected: Option<&str>,
    selected_adapter: Option<&str>,
    retained_identity: Option<RetainedIdentity<'_>>,
) -> bool {
    if !spec.local_login
        || selected_adapter.is_some_and(|adapter| adapter != spec.name)
        || retained_identity
            .is_some_and(|identity| identity.account_key != Some(spec.local_account.1))
    {
        return false;
    }
    let has_entry = entries.iter().any(|entry| entry.adapter == spec.name);
    if !has_entry {
        return true;
    }

    // A registered-account adapter's local identity participates in the same
    // per-account observation and quota scheduling as its other accounts.
    if !spec.is_unregistered_local(spec.local_account.1) {
        return false;
    }
    selected.is_none_or(|selector| {
        selector == spec.local_account.1
            || (selector == spec.local_account.0
                && !entries.iter().any(|entry| {
                    entry.adapter == spec.name && (entry.label == selector || entry.key == selector)
                }))
    })
}

fn effective_account_selector<'a>(
    requested: Option<&'a str>,
    prior_account: Option<&'a str>,
    continuing: bool,
) -> Option<&'a str> {
    if continuing { prior_account } else { requested }
}

fn select_account<'a>(
    entries: &'a [provenant_providers::accounts::Registration],
    selector: &str,
    requested_adapter: Option<&str>,
) -> Result<&'a provenant_providers::accounts::Registration, PrepareError> {
    // Exact keys identify credentials; only use labels when no exact key
    // exists. This keeps the synthetic `local` identity distinct from any
    // registered account that happens to use the same label.
    let candidates = |by_key: bool| {
        entries.iter().filter(move |entry| {
            (if by_key {
                entry.key == selector
            } else {
                entry.label == selector
            }) && requested_adapter.is_none_or(|adapter| entry.adapter == adapter)
        })
    };
    let by_key = entries.iter().any(|entry| {
        entry.key == selector && requested_adapter.is_none_or(|adapter| entry.adapter == adapter)
    });
    let mut matching = candidates(by_key);
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
    Ok(entry)
}

fn select_retained_account<'a>(
    entries: &'a [provenant_providers::accounts::Registration],
    identity: RetainedIdentity<'_>,
) -> Result<Option<&'a provenant_providers::accounts::Registration>, PrepareError> {
    let Some(key) = identity.account_key else {
        return Ok(None);
    };
    entries
        .iter()
        .find(|entry| entry.adapter == identity.provider && entry.key == key)
        .map(Some)
        .ok_or_else(|| PrepareError::Refusal {
            message: "the prior attempt's account is not registered for its applied provider; cannot continue it".into(),
            cause: provenant_records::run::TerminalCause::UnregisteredAccount,
            note: None,
        })
}

const DEPENDENCY_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Repeated preparation must never reuse a lane's mutable Git state. Keep the
/// allocation number separate from the route index used for fallback receipts.
pub(super) fn fresh_scratch_directory(
    root: &Path,
    prefix: &str,
    mut candidate: Option<usize>,
) -> Result<(Directory, String), String> {
    let parent = Directory::open_anchor(root).map_err(|error| error.to_string())?;
    loop {
        let name = match candidate {
            Some(candidate) => format!("{prefix}-{candidate}"),
            None => prefix.to_owned(),
        };
        match parent.mkdir(std::ffi::OsStr::new(&name), 0o700) {
            Ok(()) => {
                parent.sync().map_err(|error| error.to_string())?;
                let directory = parent
                    .open_verified_directory(std::ffi::OsStr::new(&name))
                    .map_err(|error| error.to_string())?;
                return Ok((directory, name));
            }
            Err(error) if error.is_exists() => {
                candidate = Some(match candidate {
                    Some(candidate) => candidate.checked_add(1).ok_or("scratch exhausted")?,
                    None => 1,
                });
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn uniform_candidate_boundary(
    count: usize,
    boundaries: &std::collections::BTreeMap<usize, provenant_records::diagnostic::BoundaryClass>,
) -> Option<provenant_records::diagnostic::BoundaryClass> {
    if count == 0 || boundaries.len() != count {
        return None;
    }
    let mut classes = boundaries.values().copied();
    let first = classes.next()?;
    classes.all(|class| class == first).then_some(first)
}

fn uniform_candidate_resource(
    count: usize,
    resources: &std::collections::BTreeMap<usize, provenant_records::diagnostic::ResourceFact>,
) -> Option<provenant_records::diagnostic::ResourceFact> {
    if count == 0 || resources.len() != count {
        return None;
    }
    let mut facts = resources.values().copied();
    let first = facts.next()?;
    facts.all(|fact| fact == first).then_some(first)
}

fn attach_candidate_guard_facts(
    reasons: &mut [SkippedRoute],
    boundaries: &std::collections::BTreeMap<usize, provenant_records::diagnostic::BoundaryClass>,
    resources: &std::collections::BTreeMap<usize, provenant_records::diagnostic::ResourceFact>,
) -> Result<(), PrepareError> {
    for (index, reason) in reasons.iter_mut().enumerate() {
        reason
            .set_guard_facts(
                boundaries.get(&index).copied(),
                resources.get(&index).copied(),
            )
            .map_err(|error| PrepareError::Other(error.to_string()))?;
    }
    Ok(())
}

fn resolve_pacing_source(
    fact: provenant_records::diagnostic::ResourceFact,
    adapter: &str,
    origins: &std::collections::BTreeMap<String, provenant_config::Origin>,
) -> provenant_records::diagnostic::ResourceFact {
    use provenant_records::diagnostic::{PacingReserveSetting, PacingSettingSource, ResourceFact};
    let ResourceFact::PacingReserve {
        percent,
        setting,
        origin: PacingSettingSource::Unresolved,
    } = fact
    else {
        return fact;
    };
    let field = match setting {
        PacingReserveSetting::AdapterFiveHour => "five_hour_percent",
        PacingReserveSetting::AdapterSevenDay => "seven_day_percent",
        _ => return fact,
    };
    let key = format!("domains.routing.pacing_reserve.{adapter}.{field}");
    let origin = if matches!(
        origins.get(&key),
        Some(provenant_config::Origin::User { .. })
    ) {
        PacingSettingSource::Configuration
    } else {
        PacingSettingSource::ShippedDefault
    };
    ResourceFact::PacingReserve {
        percent,
        setting,
        origin,
    }
}

pub(super) fn image_input_warning(adapter: &str, attachments: &[PathBuf]) -> Option<String> {
    (!attachments.is_empty()
        && !AdapterSpec::get(adapter).is_some_and(|spec| spec.launch.image_input))
    .then(|| {
        format!(
            "warning: adapter {adapter} does not support image_input; attachments remain readable"
        )
    })
}

fn push_launch_warning(route: &mut ResolvedRoute, warning: impl Into<String>) {
    let warning = warning.into();
    let warning = if warning.starts_with("warning:") {
        warning
    } else {
        format!("warning: {warning}")
    };
    if !route.diagnostics.launch_warnings.contains(&warning) {
        route.diagnostics.launch_warnings.push(warning);
    }
}

/// Warn when an ignore file the routed adapter declares, in the writer's
/// checkout, hides the writer's worktree from that adapter's agent.
pub(super) fn ignore_file_warnings(
    adapter: &str,
    task: &run::TaskRequest,
    state: &Path,
) -> Vec<String> {
    match (&task.writer, AdapterSpec::get(adapter)) {
        (Some(writer), Some(spec)) => {
            crate::ignore_files::launch_warnings(&writer.project, &writer.worktree, spec, state)
        }
        _ => Vec::new(),
    }
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
    let kind = if matches!(
        error,
        ProviderError::LoginRequired
            | ProviderError::CredentialNotFound
            | ProviderError::CredentialExpired
    ) {
        SkipKind::AuthRequired
    } else {
        SkipKind::Refused
    };
    let mut skipped = skip(route, kind, "credential refresh failed");
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

pub(super) fn dependency_preparation_warning(
    error: String,
    enabled: bool,
    profile: &provenant_records::run::HarnessProfile,
) -> String {
    let origin = if profile
        .knobs
        .get("lanes.prepare_deps")
        .and_then(serde_json::Value::as_bool)
        .is_some()
    {
        format!("sealed profile {}", profile.name)
    } else {
        "compiled default".to_owned()
    };
    format!(
        "{error}; run `pnpm install --frozen-lockfile` in the worktree to inspect or repair dependencies; lanes.prepare_deps={enabled} ({origin}); set lanes.prepare_deps=false to skip dependency preparation"
    )
}

pub(super) fn dependency_preparation_event(
    result: Result<serde_json::Value, String>,
    enabled: bool,
    profile: &provenant_records::run::HarnessProfile,
    policy: &provenant_records::policy::Policy,
) -> Option<serde_json::Value> {
    match result {
        Ok(event) => Some(event),
        Err(error) => {
            let warning = provenant_confinement::redact::bounded_line(
                &dependency_preparation_warning(error, enabled, profile),
                4096,
            );
            match policy.check(
                "PREPARATION_DEPENDENCY_PREPARATION_EVENT_NONE",
                provenant_records::policy::Facts::violation(
                    "dependency preparation event records an advisory install failure",
                ),
            ) {
                provenant_records::policy::Effect::Warn(_) => Some(serde_json::json!({
                    "command": "pnpm install --frozen-lockfile",
                    "outcome": "warning",
                    "reason": warning,
                })),
                provenant_records::policy::Effect::Proceed
                | provenant_records::policy::Effect::Refuse(_) => None,
            }
        }
    }
}

#[test]
fn dependency_setup_failure_warns_and_retains_redacted_reason() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let policy = provenant_records::policy::Policy::default();
    let event = dependency_preparation_event(
        Err(format!("missing pnpm {secret}")),
        true,
        &provenant_records::run::HarnessProfile::default(),
        &policy,
    )
    .expect("warning policy retains the event");
    assert_eq!(event["outcome"], "warning");
    let reason = event["reason"].as_str().unwrap();
    assert!(!reason.contains(secret));
    let success = serde_json::json!({"outcome": "installed"});
    assert_eq!(
        dependency_preparation_event(
            Ok(success.clone()),
            true,
            &provenant_records::run::HarnessProfile::default(),
            &policy
        ),
        Some(success)
    );
    let rule = policy.rule("PREPARATION_DEPENDENCY_PREPARATION_EVENT_NONE");
    assert_eq!(
        rule.rule.code,
        "PREPARATION_DEPENDENCY_PREPARATION_EVENT_NONE"
    );
    assert_eq!(
        rule.rule.strength,
        provenant_records::policy::Strength::Warn
    );
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
    // Install output is arbitrary subprocess content and must not enter a
    // warning or retained launch event. Keep only the process outcome below.
    options.stderr = provenant_os::exec::StreamPolicy::Discard;
    use provenant_os::exec::Executor;
    let outcome = executor
        .execute(options)
        .map_err(|error| format!("{command} failed: {error}"))?;
    let elapsed_micros = outcome.elapsed_micros;
    if outcome.termination != provenant_os::exec::Termination::Exit(0)
        || outcome.timed_out
        || outcome.cancelled
    {
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
        return Err(format!("{command} failed ({reason})"));
    }
    Ok(serde_json::json!({
        "command": command,
        "outcome": "installed",
        "path_source": path_source,
        "elapsed_micros": elapsed_micros,
    }))
}

fn prepare_writer_dependencies_with_policy(
    worktree: &Path,
    enabled: bool,
    search_path: &dyn Fn() -> Result<(std::ffi::OsString, &'static str), String>,
    cancel: &provenant_core::CancelToken,
    profile: &provenant_records::run::HarnessProfile,
    policy: &provenant_records::policy::Policy,
) -> Result<serde_json::Value, String> {
    let result = prepare_writer_dependencies(worktree, enabled, search_path, cancel);
    if let Err(error) = &result {
        let detail = provenant_confinement::redact::bounded_line(
            &dependency_preparation_warning(error.clone(), enabled, profile),
            4096,
        );
        if let provenant_records::policy::Effect::Warn(message) = policy.check(
            "PREPARATION_PREPARE_WRITER_DEPENDENCIES_NONE",
            provenant_records::policy::Facts::violation(detail),
        ) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
        }
    }
    result
}

pub(crate) fn prepare(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    wait: Option<u64>,
) -> Result<run::PreparedLaunch, Box<run::PrepareRefusal>> {
    prepare_inner(registry, task, prompt_text, paths, resume, None, None, wait)
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
    let budget = provenant_core::WaitBudget::up_to(
        clock.mono_now(),
        provenant_core::DurationMicros::from_micros(wait.unwrap_or(30).min(30) * 1_000_000),
    );
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
    Unsupported(#[from] provenant_records::UnsupportedLaunch),
    #[error(transparent)]
    Permission(#[from] provenant_confinement::permissions::PermissionError),
    #[error(transparent)]
    Run(#[from] run::RunError),
    #[error(transparent)]
    Provider(#[from] provenant_providers::ProviderError),
    #[error(transparent)]
    Receipt(#[from] serde_json::Error),
    #[error(transparent)]
    ReadOnly(#[from] ReadOnlyPlanError),
    #[error(transparent)]
    SourceView(super::source_tree::SourceViewError),
    #[error("write grants overlap a host index")]
    HostIndexOverlap,
    #[error("write grants overlap the host target seed store")]
    TargetSeedOverlap,
    #[error("{0}")]
    GraphInputsAuthority(String),
    #[error("{0}")]
    SourceProfileIsolated(String),
    #[error("{0}")]
    SourceViewReadOnly(String),
    #[error(transparent)]
    SourceRoots(#[from] super::source_tree::SourceRootOverlap),
    #[error("{0}")]
    WriterContinuation(String),
    #[error(transparent)]
    SourceHead(#[from] SourceHeadError),
    #[error(transparent)]
    SourceCwd(#[from] SourceCwdError),
    #[error(transparent)]
    ContinuationRequest(#[from] ContinuationRequestError),
    #[error("{message}")]
    Candidates {
        message: String,
        plans: Vec<provenant_confinement::PlanError>,
        candidate_boundary: Option<provenant_records::diagnostic::BoundaryClass>,
        candidate_resource: Option<provenant_records::diagnostic::ResourceFact>,
        skipped_routes: Vec<SkippedRoute>,
        fallback_outcome: Option<provenant_records::run::PreparationFailure>,
    },
    #[error("{error}")]
    RouteResolution {
        error: crate::route::DispatchError,
        checks: Vec<SkippedRoute>,
    },
    #[error("{0}")]
    Other(String),
}

fn continuation_skip(
    route: &str,
    guard: provenant_records::diagnostic::ContinuationGuard,
) -> SkippedRoute {
    let mut skipped = skip(route, SkipKind::Refused, "");
    skipped.set_continuation_guard(guard);
    skipped
}

fn candidate_refusal_message(
    skipped_routes: &[SkippedRoute],
    fallback_outcome: Option<provenant_records::run::PreparationFailure>,
) -> Option<String> {
    if skipped_routes.is_empty() {
        return None;
    }
    let mut reasons = skipped_routes
        .iter()
        .map(SkippedRoute::describe)
        .collect::<Vec<_>>();
    if fallback_outcome == Some(provenant_records::run::PreparationFailure::AuthRequired) {
        reasons.push("no eligible fallback remains".into());
    }
    Some(reasons.join("; "))
}

fn policy_code_for_boundary(
    class: provenant_records::diagnostic::BoundaryClass,
) -> Option<&'static str> {
    use provenant_records::diagnostic::BoundaryClass as B;
    match class {
        B::Credentials => Some("PREPARATION_PREPARE_WITH_CREDENTIALS"),
        B::LaneGit => Some("VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION"),
        B::Integrity => Some("PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY"),
        B::Authority => Some("PREPARATION_PREPARE_CANDIDATES_AUTHORITY"),
        B::None => None,
    }
}

fn provider_policy_code(error: &provenant_providers::ProviderError) -> Option<String> {
    error.policy_code().map(str::to_owned).or_else(|| {
        matches!(
            error,
            provenant_providers::ProviderError::PromptTooLarge { .. }
        )
        .then(|| "ARGV_PROMPT".to_owned())
    })
}

fn policy_code_for_resource(
    resource: Option<provenant_records::diagnostic::ResourceFact>,
) -> Option<String> {
    match resource {
        Some(provenant_records::diagnostic::ResourceFact::ArgvPromptLimit { .. }) => {
            Some("ARGV_PROMPT".into())
        }
        _ => None,
    }
}
impl PrepareError {
    pub(super) fn registered_project_inventory(error: provenant_config::ConfigError) -> Self {
        match ReadOnlyPlanError::from(error) {
            error @ ReadOnlyPlanError::ProjectBindingDirectory(_) => Self::ReadOnly(error),
            error => Self::Other(format!("cannot read registered project inventory: {error}")),
        }
    }

    pub(super) fn into_refusal(self) -> Box<run::PrepareRefusal> {
        match self {
            Self::RouteResolution { error, checks } => {
                let mut refusal = route_resolution_refusal(error).into_refusal();
                // These checks explain why adapters were denied access; they
                // do not establish the cause of an unrelated router failure.
                refusal.skipped_routes = checks;
                refusal
            }
            Self::Refusal {
                message,
                cause,
                note,
            } => Box::new(run::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                policy_code: None,
                resource: None,
                terminal_cause: cause,
                terminal_note: note,
                message,
                skipped_routes: Vec::new(),
                fallback_outcome: match cause {
                    provenant_records::run::TerminalCause::AccountLoginRequired => {
                        Some(provenant_records::run::PreparationFailure::AuthRequired)
                    }
                    provenant_records::run::TerminalCause::UnregisteredAccount
                    | provenant_records::run::TerminalCause::CommandUnavailable
                    | provenant_records::run::TerminalCause::HostNativeUnconfigured => {
                        Some(provenant_records::run::PreparationFailure::AdapterUnavailable)
                    }
                    _ => None,
                },
            }),
            Self::SourceView(error) => {
                let detail = error.to_string();
                let binding_refusal = matches!(
                    &error.cause,
                    super::source_tree::SourceCause::ProjectBindingDirectory(_)
                );
                let cause = if binding_refusal {
                    provenant_records::run::TerminalCause::ProjectBindingDirectoryUnsafe
                } else {
                    provenant_records::run::TerminalCause::SourceViewRefused
                };
                let mut refusal = run::PrepareRefusal::boxed(cause, detail.clone());
                refusal.terminal_note = Some(detail);
                if binding_refusal {
                    refusal.boundary_class =
                        provenant_records::diagnostic::BoundaryClass::Authority;
                    let mut skipped = skip(
                        "project binding state",
                        SkipKind::Refused,
                        "project binding state directory is unsafe",
                    );
                    skipped.confinement_guard = Some(
                        provenant_records::diagnostic::ConfinementGuard::ProjectBindingDirectoryUnsafe,
                    );
                    refusal.skipped_routes.push(skipped);
                }
                refusal
            }
            Self::Candidates {
                message,
                skipped_routes,
                fallback_outcome,
                plans,
                candidate_boundary,
                candidate_resource,
            } => {
                // Only classify the aggregate when every refused candidate
                // has a typed plan failure and all establish the same boundary.
                let boundary_class = if let Some(class) = candidate_boundary {
                    class
                } else if !plans.is_empty()
                    && plans.len() == skipped_routes.len()
                    && skipped_routes.iter().all(|route| {
                        route.cause
                            == Some(provenant_records::RouteExclusionReason::ConfinementUnavailable)
                    })
                {
                    let mut classes = plans.iter().map(|error| error.boundary_class());
                    match classes.next() {
                        Some(first) if classes.all(|class| class == first) => first,
                        _ => provenant_records::diagnostic::BoundaryClass::None,
                    }
                } else {
                    provenant_records::diagnostic::BoundaryClass::None
                };
                let continuation_rule = skipped_routes.first().and_then(|first| {
                    let rule = first.continuation_guard?.rule;
                    skipped_routes
                        .iter()
                        .all(|route| {
                            route
                                .continuation_guard
                                .is_some_and(|guard| guard.rule == rule)
                        })
                        .then_some(rule.code().to_owned())
                });
                let policy_code = continuation_rule.or_else(|| {
                    policy_code_for_boundary(boundary_class)
                        .map(str::to_owned)
                        .or_else(|| policy_code_for_resource(candidate_resource))
                });
                let message =
                    candidate_refusal_message(&skipped_routes, fallback_outcome).unwrap_or(message);
                let mut refusal = Box::new(run::PrepareRefusal {
                    boundary_class,
                    policy_code,
                    resource: candidate_resource,
                    terminal_cause: provenant_records::run::TerminalCause::RouteResolutionRefused,
                    terminal_note: None,
                    message,
                    skipped_routes,
                    fallback_outcome,
                });
                if refusal.auth_required() {
                    refusal.terminal_cause =
                        provenant_records::run::TerminalCause::AccountLoginRequired;
                }
                refusal
            }
            Self::SourceRoots(error) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    error.to_string(),
                );
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Authority;
                refusal.policy_code = Some("SOURCE_TREE_ENSURE_SOURCE_ROOTS_AUTHORITY".into());
                refusal.terminal_note = Some(error.to_string());
                refusal
            }
            Self::SourceCwd(error) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    error.to_string(),
                );
                refusal.boundary_class = error.boundary_class();
                refusal.policy_code = if refusal.boundary_class
                    == provenant_records::diagnostic::BoundaryClass::Authority
                {
                    Some("CONFINEMENT_SOURCE_CWD_AUTHORITY".into())
                } else {
                    policy_code_for_boundary(refusal.boundary_class).map(str::to_owned)
                };
                refusal
            }
            Self::Permission(error) => PlanCheckError::Permission(error).into_refusal(),
            Self::Unsupported(error) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    error.to_string(),
                );
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Authority;
                refusal.policy_code = Some("PREPARATION_PREPARE_CANDIDATES_AUTHORITY".into());
                refusal
            }
            Self::ContinuationRequest(error) => {
                let guard = error.guard();
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    guard.to_string(),
                );
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Authority;
                refusal.policy_code = Some(guard.rule.code().into());
                refusal
                    .skipped_routes
                    .push(continuation_skip("continuation", guard));
                refusal
            }
            Self::SourceHead(error) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    error.to_string(),
                );
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Authority;
                refusal.policy_code = Some("CONFINEMENT_REQUIRE_SOURCE_HEAD_AUTHORITY".into());
                refusal
            }
            Self::ReadOnly(error) => PlanCheckError::ReadOnly(error).into_refusal(),
            Self::Run(run::RunError::Confinement(error)) => {
                PlanCheckError::Permission(error).into_refusal()
            }
            Self::Run(run::RunError::Plan(error)) => PlanCheckError::Plan(error).into_refusal(),
            Self::Run(error) => {
                let boundary_class = error.boundary_class();
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::HandoffRefused,
                    error.to_string(),
                );
                refusal.boundary_class = boundary_class;
                refusal.policy_code = policy_code_for_boundary(boundary_class).map(str::to_owned);
                refusal
            }
            Self::Provider(error) => {
                let policy_code = provider_policy_code(&error);
                let boundary_class = error.boundary_class();
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::HandoffRefused,
                    error.to_string(),
                );
                refusal.boundary_class = boundary_class;
                refusal.policy_code = policy_code
                    .or_else(|| policy_code_for_boundary(boundary_class).map(str::to_owned));
                refusal.resource = error.resource_fact();
                if let provenant_providers::ProviderError::ContinuationRefused(guard) = error {
                    refusal
                        .skipped_routes
                        .push(continuation_skip("continuation", guard));
                }
                refusal
            }
            Self::Prompt(error) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::StoredPromptUnreadable,
                    error.to_string(),
                );
                let policy_code = match &error {
                    LaunchPromptError::Provider(error) => provider_policy_code(error),
                    _ => None,
                };
                if let Some(policy_code) = policy_code {
                    refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::None;
                    refusal.policy_code = Some(policy_code);
                    refusal.terminal_cause =
                        provenant_records::run::TerminalCause::AccessPolicyRefused;
                } else {
                    refusal.boundary_class = match &error {
                        LaunchPromptError::TooLarge => {
                            provenant_records::diagnostic::BoundaryClass::Integrity
                        }
                        _ => provenant_records::diagnostic::BoundaryClass::None,
                    };
                    refusal.policy_code =
                        policy_code_for_boundary(refusal.boundary_class).map(str::to_owned);
                }
                refusal.resource = error.resource_fact();
                refusal
            }
            error @ (Self::HostIndexOverlap | Self::TargetSeedOverlap) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    error.to_string(),
                );
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Integrity;
                refusal.policy_code = Some(
                    match &error {
                        Self::HostIndexOverlap => {
                            "CONFINEMENT_HOST_INDEXES_PROTECTED_DATA_INTEGRITY"
                        }
                        Self::TargetSeedOverlap => {
                            "CONFINEMENT_TARGET_SEEDS_PROTECTED_DATA_INTEGRITY"
                        }
                        _ => return refusal,
                    }
                    .into(),
                );
                refusal
            }
            error @ (Self::GraphInputsAuthority(_)
            | Self::SourceProfileIsolated(_)
            | Self::SourceViewReadOnly(_)
            | Self::WriterContinuation(_)) => {
                let mut refusal = run::PrepareRefusal::boxed(
                    provenant_records::run::TerminalCause::AccessPolicyRefused,
                    error.to_string(),
                );
                refusal.boundary_class = provenant_records::diagnostic::BoundaryClass::Authority;
                refusal.policy_code = Some(
                    match &error {
                        Self::GraphInputsAuthority(_) => {
                            "PREPARATION_CHECK_GRAPH_INPUTS_ROOT_AUTHORITY"
                        }
                        Self::SourceProfileIsolated(_) => {
                            "SOURCE_TREE_ENSURE_PROFILE_ISOLATED_AUTHORITY"
                        }
                        Self::SourceViewReadOnly(_) => {
                            "SOURCE_TREE_ENSURE_VIEW_READ_ONLY_AUTHORITY"
                        }
                        Self::WriterContinuation(_) => {
                            "CONFINEMENT_WRITER_SOURCE_CONTEXT_AUTHORITY"
                        }
                        _ => return refusal,
                    }
                    .into(),
                );
                refusal
            }
            other => run::PrepareRefusal::boxed(
                provenant_records::run::TerminalCause::HandoffRefused,
                other.to_string(),
            ),
        }
    }
}

#[cfg(test)]
#[test]
// Spec: docs/specs/fabric.md#session-ready-steering
fn preparation_preserves_typed_boundaries_without_classifying_other_errors() {
    use provenant_records::diagnostic::BoundaryClass;
    let error = run::RunError::Confinement(
        provenant_confinement::permissions::PermissionError::CredentialRoot,
    );
    let refusal = PrepareError::Run(error).into_refusal();
    assert_eq!(refusal.boundary_class, BoundaryClass::Credentials);
    let permission = PrepareError::Permission(
        provenant_confinement::permissions::PermissionError::GitWriteAuthorityMissing,
    )
    .into_refusal();
    assert_eq!(permission.boundary_class, BoundaryClass::LaneGit);
    assert_eq!(
        permission.skipped_routes[0].plan_error,
        Some(provenant_records::diagnostic::PlanFailure::Permission)
    );
    assert_eq!(
        permission.skipped_routes[0].guard_boundary,
        Some(BoundaryClass::LaneGit)
    );
    assert!(permission.terminal_note.is_some());
    let prompt = PrepareError::Prompt(LaunchPromptError::Provider(
        provenant_providers::ProviderError::PromptTooLarge {
            limit: 32,
            origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
        },
    ))
    .into_refusal();
    assert_eq!(prompt.boundary_class, BoundaryClass::None);
    assert_eq!(prompt.policy_code.as_deref(), Some("ARGV_PROMPT"));
    assert_eq!(
        PrepareError::Prompt(LaunchPromptError::TooLarge)
            .into_refusal()
            .boundary_class,
        BoundaryClass::Integrity
    );
    for error in [SourceHeadError::Missing, SourceHeadError::Invalid] {
        let refusal = PrepareError::SourceHead(error).into_refusal();
        assert_eq!(refusal.boundary_class, BoundaryClass::Authority);
    }
    for error in [
        ContinuationRequestError::MissingRoute,
        ContinuationRequestError::MissingModel,
        ContinuationRequestError::MissingProvider,
        ContinuationRequestError::InvalidModel("TOKEN_SENTINEL /private/source".into()),
        ContinuationRequestError::ProviderMismatch,
    ] {
        let guard = error.guard();
        let refusal = PrepareError::ContinuationRequest(error).into_refusal();
        assert_eq!(refusal.boundary_class, BoundaryClass::Authority);
        assert_eq!(refusal.policy_code.as_deref(), Some(guard.rule.code()));
        assert_eq!(refusal.skipped_routes[0].continuation_guard, Some(guard));
        let error = run::RunError::ContinuationRefused { refusal };
        let reason = error.to_string();
        assert!(reason.contains(guard.rule.code()));
        assert!(reason.contains(guard.field.name()));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/source"));
    }
    let source =
        PrepareError::SourceCwd(SourceCwdError::Authority("source identity changed".into()))
            .into_refusal();
    assert_eq!(source.boundary_class, BoundaryClass::Authority);
    let source_roots =
        PrepareError::SourceRoots(super::source_tree::SourceRootOverlap::Assets).into_refusal();
    assert_eq!(source_roots.boundary_class, BoundaryClass::Authority);
    assert_eq!(
        source_roots.terminal_note.as_deref(),
        Some("product assets overlap the live graph source checkout")
    );
    let unavailable =
        PrepareError::SourceCwd(SourceCwdError::Operational("cwd unavailable".into()))
            .into_refusal();
    assert_eq!(unavailable.boundary_class, BoundaryClass::None);
    let unsupported = PrepareError::Unsupported(provenant_records::UnsupportedLaunch {
        adapter: "fixture",
        field: provenant_records::LaunchField::Write,
    })
    .into_refusal();
    assert_eq!(unsupported.boundary_class, BoundaryClass::Authority);
    let other = PrepareError::Other("TOKEN_SENTINEL".into()).into_refusal();
    assert_eq!(other.boundary_class, BoundaryClass::None);
    for error in [
        PrepareError::GraphInputsAuthority("invalid graph inputs root".into()),
        PrepareError::SourceProfileIsolated("source grant overlaps".into()),
        PrepareError::WriterContinuation("uncommitted predecessor edits".into()),
    ] {
        assert_eq!(
            error.into_refusal().boundary_class,
            BoundaryClass::Authority
        );
    }
    for error in [
        PrepareError::HostIndexOverlap,
        PrepareError::TargetSeedOverlap,
    ] {
        let refusal = error.into_refusal();
        assert_eq!(refusal.boundary_class, BoundaryClass::Integrity);
        assert!(refusal.message.contains("overlap"));
    }
    let candidate = || {
        let mut route = skip("fixture/model", SkipKind::Unavailable, "plan refused");
        route.set_cause(provenant_records::RouteExclusionReason::ConfinementUnavailable);
        route
    };
    let aggregate = |skipped_routes| {
        PrepareError::Candidates {
            message: "candidate refusal".into(),
            plans: vec![provenant_confinement::PlanError::InvalidRoots],
            candidate_boundary: None,
            candidate_resource: None,
            skipped_routes,
            fallback_outcome: None,
        }
        .into_refusal()
    };
    assert_eq!(
        aggregate(vec![candidate()]).boundary_class,
        BoundaryClass::Authority
    );
    let unrelated = skip("other/model", SkipKind::Unavailable, "not available");
    assert_eq!(
        aggregate(vec![candidate(), unrelated]).boundary_class,
        BoundaryClass::None
    );
    let guard = provenant_records::diagnostic::ContinuationGuard {
        rule: provenant_records::diagnostic::ContinuationRule::RetainedRoute,
        field: provenant_records::diagnostic::ContinuationField::Effort,
        issue: provenant_records::diagnostic::ContinuationIssue::Mismatch,
    };
    let refused = aggregate(vec![continuation_skip("codex/gpt-6-luna", guard)]);
    assert_eq!(refused.skipped_routes[0].continuation_guard, Some(guard));
    assert_eq!(refused.policy_code.as_deref(), Some(guard.rule.code()));
    assert!(refused.message.contains(guard.rule.code()));
    assert!(refused.message.contains(guard.field.name()));
    let mut boundaries = std::collections::BTreeMap::new();
    boundaries.insert(0, BoundaryClass::Authority);
    assert_eq!(
        uniform_candidate_boundary(1, &boundaries),
        Some(BoundaryClass::Authority)
    );
    assert_eq!(uniform_candidate_boundary(2, &boundaries), None);
    boundaries.insert(1, BoundaryClass::Credentials);
    assert_eq!(uniform_candidate_boundary(2, &boundaries), None);
    let mut resources = std::collections::BTreeMap::new();
    let fact = provenant_records::diagnostic::ResourceFact::ArgvPromptLimit {
        limit: 3,
        origin: provenant_records::diagnostic::SettingSource::Configuration,
    };
    resources.insert(0, fact);
    assert_eq!(uniform_candidate_resource(1, &resources), Some(fact));
    assert_eq!(uniform_candidate_resource(2, &resources), None);
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
            EdgeError::Routing(error) => {
                let cause = match error {
                    provenant_routing::RoutingError::NoEligiblePoolMember {
                        cause: Some(reason),
                        ..
                    } => match reason {
                        provenant_records::RouteExclusionReason::AuthRequired
                        | provenant_records::RouteExclusionReason::LoginExpired => {
                            TerminalCause::AccountLoginRequired
                        }
                        provenant_records::RouteExclusionReason::UnknownAccount => {
                            TerminalCause::UnregisteredAccount
                        }
                        _ => TerminalCause::RouteResolutionRefused,
                    },
                    _ => TerminalCause::RouteResolutionRefused,
                };
                (cause, error.code())
            }
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

impl From<super::source_tree::SourceViewError> for PrepareError {
    fn from(error: super::source_tree::SourceViewError) -> Self {
        Self::SourceView(error)
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

#[test]
fn mixed_candidate_guard_facts_survive_fallback_without_source_text()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::{BoundaryClass, ResourceFact, SettingSource};
    let mut reasons = vec![
        skip("a", SkipKind::Refused, "TOKEN_SENTINEL"),
        skip("b", SkipKind::Refused, "TOKEN_SENTINEL"),
    ];
    let boundaries = [(0, BoundaryClass::Credentials)].into_iter().collect();
    let resources = [(
        1,
        ResourceFact::ArgvPromptLimit {
            limit: 3,
            origin: SettingSource::Configuration,
        },
    )]
    .into_iter()
    .collect();
    attach_candidate_guard_facts(&mut reasons, &boundaries, &resources)?;
    assert_eq!(reasons[0].guard_boundary, Some(BoundaryClass::Credentials));
    assert!(
        reasons[0]
            .diagnostic()
            .detail
            .as_deref()
            .is_none_or(|text| !text.contains("TOKEN_SENTINEL"))
    );
    let persisted = serde_json::to_vec(&reasons)?;
    let restored: Vec<SkippedRoute> = serde_json::from_slice(&persisted)?;
    let projected = restored[1].diagnostic();
    assert_eq!(projected.code, "REFUSED");
    let detail = projected.detail.ok_or("missing resource detail")?;
    assert!(detail.contains("domains.run.argv_prompt_limit_bytes"));
    assert!(detail.contains("host configuration"));
    assert!(!detail.contains("TOKEN_SENTINEL"));
    assert!(uniform_candidate_boundary(2, &boundaries).is_none());
    assert!(uniform_candidate_resource(2, &resources).is_none());
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PlanCheckError {
    #[error(transparent)]
    Plan(#[from] provenant_confinement::PlanError),
    #[error(transparent)]
    Permission(#[from] provenant_confinement::permissions::PermissionError),
    #[error(transparent)]
    ReadOnly(#[from] ReadOnlyPlanError),
    #[error(transparent)]
    SourceHead(#[from] SourceHeadError),
}

impl PlanCheckError {
    fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        match self {
            Self::Plan(error) => error.boundary_class(),
            Self::Permission(error) => error.boundary_class(),
            Self::ReadOnly(error) => error.boundary_class(),
            Self::SourceHead(_) => provenant_records::diagnostic::BoundaryClass::Authority,
        }
    }

    fn policy_code(&self) -> Option<&'static str> {
        use provenant_confinement::{PlanError, permissions::PermissionError};
        let permission = match self {
            Self::Permission(error) | Self::Plan(PlanError::Permission(error)) => Some(error),
            _ => None,
        };
        if let Some(permission) = permission {
            fn root(error: &PermissionError) -> &PermissionError {
                match error {
                    PermissionError::ConfigOrigin { source, .. } => root(source),
                    other => other,
                }
            }
            return Some(match root(permission) {
                PermissionError::CredentialRoot
                | PermissionError::CredentialRootAt { .. }
                | PermissionError::CredentialGrant { .. }
                | PermissionError::CredentialStore { .. } => {
                    "VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_CREDENTIALS"
                }
                PermissionError::AnchorMismatch | PermissionError::AnchorMismatchAt { .. } => {
                    "VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_CREDENTIALS_2"
                }
                PermissionError::InvalidPath(_) | PermissionError::InvalidReadOnlyPolicy => {
                    "VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_AUTHORITY_2"
                }
                _ => match permission.boundary_class() {
                    provenant_records::diagnostic::BoundaryClass::Credentials => {
                        "VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_CREDENTIALS"
                    }
                    provenant_records::diagnostic::BoundaryClass::LaneGit => {
                        "VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION"
                    }
                    provenant_records::diagnostic::BoundaryClass::Integrity => {
                        "PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY"
                    }
                    provenant_records::diagnostic::BoundaryClass::Authority => {
                        "PREPARATION_PREPARE_CANDIDATES_AUTHORITY"
                    }
                    provenant_records::diagnostic::BoundaryClass::None => {
                        return None;
                    }
                },
            });
        }
        Some(match self {
            Self::ReadOnly(_) => "CONFINEMENT_READ_ONLY_PLAN_AUTHORITY",
            Self::SourceHead(_) => "CONFINEMENT_REQUIRE_SOURCE_HEAD_AUTHORITY",
            Self::Plan(
                PlanError::InvalidRoots | PlanError::UnconfinedExtensions | PlanError::Home(_),
            ) => "VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_AUTHORITY_2",
            Self::Plan(_) => "PREPARATION_PREPARE_CANDIDATES_AUTHORITY",
            Self::Permission(_) => return None,
        })
    }

    fn skipped(&self, route: &str) -> SkippedRoute {
        use provenant_confinement::{PlanError, permissions::PermissionError};
        use provenant_records::diagnostic::PlanFailure;
        let permission = match self {
            Self::Permission(error) | Self::Plan(PlanError::Permission(error)) => Some(error),
            _ => None,
        };
        fn permission_failure(error: &PermissionError) -> PlanFailure {
            match error {
                PermissionError::ConfigOrigin { source, .. } => permission_failure(source),
                PermissionError::ProtectedOverlap => PlanFailure::ProtectedOverlap,
                PermissionError::InvalidPath(_) => PlanFailure::InvalidPath,
                PermissionError::CredentialRoot
                | PermissionError::CredentialRootAt { .. }
                | PermissionError::CredentialGrant { .. }
                | PermissionError::CredentialStore { .. } => PlanFailure::CredentialRoot,
                PermissionError::AnchorMismatch | PermissionError::AnchorMismatchAt { .. } => {
                    PlanFailure::AnchorMismatch
                }
                PermissionError::BroadRoot => PlanFailure::BroadRoot,
                _ => PlanFailure::Permission,
            }
        }
        let failure = match self {
            Self::Permission(error) | Self::Plan(PlanError::Permission(error)) => {
                permission_failure(error)
            }
            Self::Plan(PlanError::InvalidRoots) => PlanFailure::InvalidRoots,
            Self::Plan(PlanError::UnconfinedExtensions) => PlanFailure::UnconfinedExtensions,
            Self::Plan(PlanError::Home(_)) => PlanFailure::Home,
            Self::ReadOnly(_) => PlanFailure::ReadOnly,
            Self::SourceHead(_) => PlanFailure::SourceHead,
        };
        let mut skipped = skip(route, SkipKind::Refused, failure.detail());
        skipped.plan_error = Some(failure);
        skipped.guard_boundary = Some(self.boundary_class());
        skipped.guard_resource = permission.and_then(PermissionError::resource_fact);
        skipped.confinement_guard = Some(self.guard_fact());
        skipped.set_cause(provenant_records::RouteExclusionReason::ConfinementUnavailable);
        skipped
    }

    fn into_refusal(self) -> Box<run::PrepareRefusal> {
        let skipped = self.skipped("preparation");
        let mut refusal = run::PrepareRefusal::boxed(
            provenant_records::run::TerminalCause::AccessPolicyRefused,
            skipped.detail.clone(),
        );
        refusal.boundary_class = self.boundary_class();
        refusal.policy_code = self.policy_code().map(str::to_owned);
        refusal.terminal_note = Some(skipped.detail.clone());
        refusal.resource = skipped.guard_resource;
        refusal.skipped_routes.push(skipped);
        if let Self::ReadOnly(ReadOnlyPlanError::ProjectBindingDirectory(diagnostic)) = self {
            refusal.terminal_cause =
                provenant_records::run::TerminalCause::ProjectBindingDirectoryUnsafe;
            refusal.message = diagnostic
                .detail
                .unwrap_or_else(|| "project binding state directory is unsafe".into());
            refusal.terminal_note = diagnostic.next;
        }
        refusal
    }

    /// A closed guard fact can leave the host; paths and captured errors cannot.
    fn guard_fact(&self) -> provenant_records::diagnostic::ConfinementGuard {
        use provenant_confinement::PlanError;
        use provenant_confinement::permissions::{AvailabilityReason, PermissionError};
        use provenant_records::diagnostic::ConfinementGuard as G;
        fn permission(error: &PermissionError) -> G {
            match error {
                PermissionError::ConfigOrigin { source, .. } => permission(source),
                PermissionError::InvalidReadOnlyPolicy => G::InvalidReadOnlyPolicy,
                PermissionError::InvalidPath(_) => G::InvalidPermissionPath,
                PermissionError::BroadRoot => G::PermissionRootTooBroad,
                PermissionError::CredentialRoot
                | PermissionError::CredentialRootAt { .. }
                | PermissionError::CredentialGrant { .. }
                | PermissionError::CredentialStore { .. } => G::CredentialRootRefused,
                PermissionError::ProtectedOverlap => G::ProtectedPathOverlap,
                PermissionError::AnchorMismatch | PermissionError::AnchorMismatchAt { .. } => {
                    G::PathAnchorChanged
                }
                PermissionError::GitControlHardLinked(_) => G::GitControlHardLinked,
                PermissionError::NonUtf8Path => G::PathNotUtf8,
                PermissionError::InvalidFixture => G::InvalidProfileFixture,
                PermissionError::PermissionNotRecorded => G::PermissionNotRecorded,
                PermissionError::ProcessPolicyMissing => G::ProcessPolicyMissing,
                PermissionError::GitWriteAuthorityMissing => G::GitWriteAuthorityMissing,
                PermissionError::NestedConfinementUnsupported => G::NestedConfinementUnsupported,
                PermissionError::ProfileTooLarge { .. } => G::ProfileLimitExceeded,
                PermissionError::GrantLimitExceeded { .. } => G::GrantLimitExceeded,
                PermissionError::GitControlEntriesExceeded { .. } => G::GitControlEntriesExceeded,
                PermissionError::BackendUnavailable => G::SandboxBackendUnavailable,
                PermissionError::BackendNotAvailable(reason) => match reason {
                    AvailabilityReason::UnsupportedPlatform => G::UnsupportedSandboxPlatform,
                    AvailabilityReason::SandboxExecutableMissing => G::SandboxExecutableMissing,
                    AvailabilityReason::SandboxExecutableUntrusted => G::SandboxExecutableUntrusted,
                    AvailabilityReason::ProbeFailed => G::SandboxProbeFailed,
                },
                PermissionError::Cancelled => G::SandboxProbeCancelled,
                PermissionError::Filesystem(_) => G::ConfinementFilesystemFailed,
                PermissionError::ProfileTransport(_) => G::ProfileTransportFailed,
                PermissionError::Execution(_) => G::SandboxExecutionFailed,
                PermissionError::Repository(_) => G::RepositoryAuthorityRevalidationFailed,
                PermissionError::HeadRead(_) => G::GitHeadReadFailed,
                PermissionError::CacheSweepIo(_) | PermissionError::CacheSweepFilesystem { .. } => {
                    G::CacheSweepFailed
                }
                PermissionError::CacheSweepDepth => G::CacheSweepDepthExceeded,
            }
        }
        fn plan(error: &PlanError) -> G {
            match error {
                PlanError::Permission(error) => permission(error),
                PlanError::InvalidRoots => G::InvalidPlanRoots,
                PlanError::UnconfinedExtensions => G::UnconfinedExtensions,
                PlanError::Home(error) => match error {
                    provenant_os::home::HomeError::InvalidRecipe => G::InvalidPrivateHomeRecipe,
                    provenant_os::home::HomeError::UnsafeEntry => G::UnsafePrivateHomeEntry,
                    provenant_os::home::HomeError::TooLarge => G::PrivateHomeLimitExceeded,
                    provenant_os::home::HomeError::UnsupportedState => {
                        G::PrivateHomeStateUnsupported
                    }
                    provenant_os::home::HomeError::Conflict { .. } => G::PrivateHomeConflict,
                    provenant_os::home::HomeError::DurabilityUnconfirmed(_) => {
                        G::PrivateHomeDurabilityUnconfirmed
                    }
                    provenant_os::home::HomeError::Filesystem(_) => G::PrivateHomeFilesystemFailed,
                    provenant_os::home::HomeError::Read(_) => G::PrivateHomeReadFailed,
                },
            }
        }
        match self {
            Self::Permission(error) => permission(error),
            Self::Plan(error) => plan(error),
            Self::SourceHead(error) => match error {
                SourceHeadError::Missing => G::SourceHeadMissing,
                SourceHeadError::Invalid => G::SourceHeadInvalid,
                SourceHeadError::Moved(_) => G::SourceHeadMoved,
            },
            Self::ReadOnly(error) => match error {
                ReadOnlyPlanError::Plan(error) => plan(error),
                ReadOnlyPlanError::Run(_) => G::ReadOnlyRunRefused,
                ReadOnlyPlanError::BindingMismatch => G::ProjectBindingMismatch,
                ReadOnlyPlanError::MissingBinding => G::ProjectBindingMissing,
                ReadOnlyPlanError::PinnedSourceRequiresGit => G::PinnedSourceRequiresGit,
                ReadOnlyPlanError::Other(_) => G::ReadOnlyPreparationFailed,
                ReadOnlyPlanError::ProjectBindingDirectory(_) => G::ProjectBindingDirectoryUnsafe,
            },
        }
    }
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

#[cfg(any(test, feature = "fixture"))]
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
    prepare_inner(
        registry,
        task,
        prompt_text,
        paths,
        resume,
        Some(&locate),
        fixture_token,
        wait,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_inner(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt_text: &str,
    paths: &run::LaunchPaths,
    resume: Option<&run::AttemptRecord>,
    locate_override: Option<&ExecutableLocator<'_>>,
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
            })?;
        let roots = registry.roots.ok_or("routing context is unavailable")?;
        let config =
            crate::config_reads::read_for(roots, "run").map_err(|error| error.to_string())?;
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
            required_family: task.route.required_family().map(str::to_owned),
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
            resume,
            locate_override,
            fixture_token,
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
        launch.route.host_native = Some(name.clone());
        return Ok(launch);
    }
    prepare_candidates(
        registry,
        REGISTRY,
        task,
        prompt_text,
        paths,
        resume,
        locate_override,
        fixture_token,
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

/// The models a route's account reports when the requested model is not among
/// them. Accounts that report no models place no restriction.
fn unoffered_model(
    availability: &[provenant_providers::AvailabilityRecord],
    route: &ResolvedRoute,
    now: i64,
) -> Option<Vec<String>> {
    let key = route.account_key.as_deref()?;
    if !AdapterSpec::get(&route.adapter).is_some_and(|spec| spec.offered_models) {
        return None;
    }
    let record = availability.iter().find(|record| {
        record.adapter == route.adapter
            && record.account_key.as_deref() == Some(key)
            && record.expires_unix_seconds > now
            && !record.observed_models.is_empty()
    })?;
    (!provenant_providers::model_matches_offered(&route.model, &record.observed_models))
        .then(|| record.observed_models.clone())
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
    fn model_outside_the_accounts_offered_list_is_refused_with_that_list() {
        let route: ResolvedRoute = serde_json::from_value(serde_json::json!({
            "selection_context":{"workload":"general","writes":false},
            "diagnostics":serde_json::to_value(provenant_records::route_selection::RouteDiagnostics::default()).unwrap(),
            "adapter":"agy", "model":"gemini-3.8-pro", "effort":null,
            "catalogue_revision":"revision", "account_key":"account", "host":"local",
            "alternatives":[], "exclusions":[], "eligibility":"eligible",
            "readiness":{"status":"unavailable","reason":"stale_availability"},
            "confidentiality":{"evidence_ref":"evidence"}
        }))
        .unwrap();
        let record = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "agy".into(),
            executable: "/bin/agy".into(),
            version: "1".into(),
            last_update: None,
            fixture_ref: "installed/agy/version".into(),
            catalogue_revision: "revision".into(),
            account_key: Some("account".into()),
            plan: None,
            observed_models: vec!["Gemini Flash".into(), "GPT-OSS 120B (Medium)".into()],
            observed_unix_seconds: 10,
            expires_unix_seconds: 400,
        };
        assert_eq!(
            unoffered_model(std::slice::from_ref(&record), &route, 20),
            Some(record.observed_models.clone())
        );
        let mut flash = route.clone();
        flash.model = "gemini-3.8-flash".into();
        assert_eq!(
            unoffered_model(std::slice::from_ref(&record), &flash, 20),
            None
        );
        let unknown = provenant_providers::AvailabilityRecord {
            plan: None,
            observed_models: vec![],
            ..record
        };
        assert_eq!(unoffered_model(&[unknown], &route, 20), None);
    }

    #[test]
    fn stale_selected_account_is_reprobed_once_without_changing_route() {
        let route: ResolvedRoute = serde_json::from_value(serde_json::json!({
            "selection_context":{"workload":"general","writes":false},
            "diagnostics":serde_json::to_value(provenant_records::route_selection::RouteDiagnostics::default()).unwrap(),
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
            plan: None,
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
                    availability_warning: None,
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
                    availability_warning: None,
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
            availability_warning: None,
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
            plan: None,
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
                selection_evidence: None,
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
                reason: provenant_records::RouteExclusionReason::ConfinementUnavailable,
                ..
            }
        ));
        assert!(same_selected_ready(&route, denied).is_none());
    }

    #[test]
    fn claude_lane_token_loss_reprobes_the_same_account_without_mutating_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::{
            CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Fake {
            account_directory: std::path::PathBuf,
            signed_in: AtomicBool,
        }
        impl Executor for Fake {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                let output = if options.argv[1] == "auth" {
                    assert_eq!(options.argv[2], "status");
                    assert_eq!(options.argv[3], "--json");
                    assert!(options.env.unwrap().iter().any(|(key, value)| {
                        key == "CLAUDE_CONFIG_DIR" && value == self.account_directory.as_os_str()
                    }));

                    if self.signed_in.load(Ordering::Relaxed) {
                        br#"{"loggedIn":true,"email":"account@example.test"}"#.as_slice()
                    } else {
                        br#"{"loggedIn":false}"#.as_slice()
                    }
                } else if options.argv[1] == "--version" {
                    b"Claude 2.0\n".as_slice()
                } else {
                    panic!("unexpected account probe: {:?}", options.argv);
                };
                let stdout = CapturedStream {
                    bytes: output.to_vec(),
                    total_bytes: output.len() as u64,
                    ..Default::default()
                };
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout,
                    stderr: CapturedStream::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }

        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let account_directory = home.join(".claude-work");
        let state = root.join("state");
        std::fs::create_dir_all(&account_directory)?;
        std::fs::create_dir_all(&state)?;
        let token_path = account_directory.join(provenant_providers::claude::LANE_TOKEN_FILE);
        std::fs::write(&token_path, "registered-token-sentinel\n")?;
        let token_before = std::fs::read(&token_path)?;
        let key = provenant_providers::identity::fingerprint("account@example.test")
            .ok_or("account fingerprint")?;
        let entry: provenant_providers::accounts::Registration =
            serde_json::from_value(serde_json::json!({
                "adapter":"claude", "label":"work", "key":key.clone(),
                "directory":account_directory
            }))?;
        let fake = Fake {
            account_directory: entry.directory.clone(),
            signed_in: AtomicBool::new(true),
        };
        let executor = provenant_providers::host::HostProbeExecutor::new(&fake, &state)?;
        let probe = Probe {
            roots: None,
            executor: &executor,
            home: &home,
            user: std::ffi::OsStr::new("fixture"),
            scratch: temp.path(),
        };
        let route: ResolvedRoute = serde_json::from_value(serde_json::json!({
            "selection_context":{"workload":"general","writes":false},
            "diagnostics":serde_json::to_value(provenant_records::route_selection::RouteDiagnostics::default()).unwrap(),
            "adapter":"claude", "model":"claude/example", "effort":null,
            "catalogue_revision":"revision", "account_key":key.clone(), "host":"local",
            "alternatives":[], "exclusions":[], "eligibility":"eligible",
            "readiness":{"status":"unavailable","reason":"stale_availability"},
            "confidentiality":{"evidence_ref":"evidence"}
        }))?;
        use provenant_run::Providers as _;
        let prior_route = run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/example".into()),
            account_key: Some(entry.key.clone()),
            ..Default::default()
        };
        let mut exit = crate::run::tests::clean_exit();
        exit.termination = Some(Termination::Exit(1));
        let prior = crate::run::Registry::default().decode_captures(
            &prior_route,
            include_bytes!(
                "../../../../../tests/fixtures/providers/claude/live-auth-required.jsonl"
            ),
            b"",
            &exit,
            temp.path(),
            None,
        )?;
        assert_eq!(
            prior.terminal_cause,
            Some(run::TerminalCause::LaneTokenLost)
        );
        let stale = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "claude".into(),
            executable: "/fixture/claude".into(),
            version: "old".into(),
            last_update: None,
            fixture_ref: "installed/claude/version".into(),
            catalogue_revision: "revision".into(),
            account_key: Some(entry.key.clone()),
            plan: None,
            observed_models: vec![],
            observed_unix_seconds: 10,
            expires_unix_seconds: 20,
        };

        // This is the successor probe after the prior writer stopped with
        // LaneTokenLost: a healthy account refreshes readiness on the same route.
        let mut availability = vec![stale.clone()];
        let selected = retry_stale_selected(
            &mut availability,
            &route,
            30,
            "revision",
            || observe_account(&entry, Path::new("/fixture/claude"), &probe, "revision", 30),
            |records| {
                assert_eq!(records[0].account_key.as_deref(), Some(entry.key.as_str()));
                let mut ready = route.clone();
                ready.readiness = provenant_records::RouteReadiness::Ready {
                    availability_warning: None,
                    extra: Default::default(),
                    availability_ref: records[0].fixture_ref.clone(),
                    expires_unix_seconds: records[0].expires_unix_seconds,
                };
                Some(ready)
            },
        )?
        .ok_or("healthy selected account was not reusable")?;
        assert_eq!(selected.adapter, route.adapter);
        assert_eq!(selected.account_key.as_deref(), Some(entry.key.as_str()));
        assert_eq!(
            availability[0].account_key.as_deref(),
            Some(entry.key.as_str())
        );

        // If the account itself has since signed out, the next fresh observation
        // becomes an account-login refusal rather than another lane-token stop.
        assert_eq!(std::fs::read(&token_path)?, token_before);
        fake.signed_in.store(false, Ordering::Relaxed);
        let failed = observe_account(&entry, Path::new("/fixture/claude"), &probe, "revision", 31)
            .expect_err("signed-out account must not be available");
        let AccountObservationError::AuthRequired(detail) = failed else {
            panic!("signed-out account lost its typed login outcome: {failed:?}");
        };
        let failures = std::collections::BTreeMap::from([(
            (entry.adapter.clone(), entry.key.clone()),
            detail.clone(),
        )]);
        let failed_route = unavailable_auth_detail(
            &route,
            Some(entry.key.as_str()),
            std::slice::from_ref(&entry),
            &failures,
        )
        .ok_or("selected account login failure was not retained")?;
        let mut skipped = skip("claude/example", SkipKind::AuthRequired, failed_route);
        skipped.set_cause(provenant_records::RouteExclusionReason::AuthRequired);
        let refusal = PrepareError::Candidates {
            message: detail,
            plans: Vec::new(),
            candidate_boundary: None,
            candidate_resource: None,
            skipped_routes: vec![skipped],
            fallback_outcome: Some(provenant_records::run::PreparationFailure::AuthRequired),
        }
        .into_refusal();
        assert!(refusal.auth_required());
        assert_eq!(
            refusal.terminal_cause,
            provenant_records::run::TerminalCause::AccountLoginRequired
        );
        assert_eq!(
            refusal.skipped_routes[0].cause,
            Some(provenant_records::RouteExclusionReason::AuthRequired)
        );

        assert_eq!(std::fs::read(&token_path)?, token_before);
        Ok(())
    }
}

/// Name, by registered label only, each account an unpinned route passed over
/// with its own reason. Routing records opaque keys; a key without a
/// registration is never shown, since a key can carry a sign-in identity.
fn name_skipped_accounts(
    mut skipped: SkippedRoute,
    route: &provenant_records::ResolvedRoute,
    entries: &[provenant_providers::accounts::Registration],
) -> SkippedRoute {
    let provenant_records::RouteReadiness::Unavailable {
        skipped_accounts, ..
    } = &route.readiness
    else {
        return skipped;
    };
    let accounts: Vec<_> = skipped_accounts
        .iter()
        .cloned()
        .map(|account| provenant_records::SkippedAccount {
            extra: Default::default(),
            account: entries
                .iter()
                .find(|entry| entry.adapter == route.adapter && entry.key == account.account)
                .map_or_else(
                    || "unregistered account".into(),
                    |entry| entry.label.clone(),
                ),
            reason: account.reason,
        })
        .collect();
    if accounts.is_empty() {
        return skipped;
    }
    skipped.detail = format!(
        "{}; accounts skipped: {}",
        skipped.detail,
        provenant_records::SkippedAccount::describe(&accounts)
    );
    skipped.skipped_accounts = accounts;
    skipped
}

fn setup_skip(route: &str, detail: impl Into<String>) -> SkippedRoute {
    let mut skipped = skip(route, SkipKind::Setup, detail);
    skipped.set_cause(provenant_records::RouteExclusionReason::SetupFailed);
    skipped
}

fn record_admission_redraws(
    resolved: &provenant_records::ResolvedRoute,
    reasons: &mut Vec<SkippedRoute>,
    rejected: &mut Vec<provenant_routing::ModelId>,
) -> Result<(), PrepareError> {
    if let Some(selection) = &resolved.pool_selection {
        for redraw in &selection.redraws {
            let model = provenant_routing::ModelId::parse(&redraw.rejected_pick)
                .map_err(|error| error.to_string())?;
            if rejected.contains(&model) {
                continue;
            }
            rejected.push(model);
            let cause = redraw
                .cause
                .clone()
                .unwrap_or(provenant_records::RouteExclusionReason::AdmissionRefused);
            let mut skipped = skip(&redraw.rejected_pick, SkipKind::Unavailable, &redraw.reason);
            skipped.set_cause(cause);
            reasons.push(skipped);
        }
    }
    Ok(())
}

fn next_permitted_candidate(
    queue: &mut std::collections::VecDeque<provenant_records::ResolvedRoute>,
    rejected: &mut Vec<provenant_routing::ModelId>,
    reasons: &mut Vec<SkippedRoute>,
    pin: (Option<&str>, Option<&str>),
    switched: bool,
    outcome: provenant_routing::FallbackOutcome,
) -> Result<Option<provenant_records::ResolvedRoute>, PrepareError> {
    while let Some(route) = queue.pop_front() {
        let label = format!("{}/{}", route.adapter, route.model);
        let model = provenant_routing::ModelId::parse(&label).map_err(|error| error.to_string())?;
        if rejected.contains(&model) {
            continue;
        }
        rejected.push(model);
        let pinned =
            violates_account_pin(pin.0, pin.1, route.account_key.as_deref(), &route.adapter);
        let allowed = provenant_routing::FallbackPolicy {
            outcome,
            handoff: provenant_routing::FallbackHandoff::Local,
            custody_released: true,
            budget_remaining: true,
            pinned,
        }
        .allows_preparation_switch();
        if switched && !allowed {
            let mut skipped = skip(&label, SkipKind::Account, "account pinned");
            skipped.set_cause(provenant_records::RouteExclusionReason::AccountPinned);
            reasons.push(skipped);
            continue;
        }
        return Ok(Some(route));
    }
    Ok(None)
}

/// Availability left once every account that admission refused for its
/// allowance is removed, so a redraw can only land on an untried account.
fn without_refused_accounts(
    availability: &[provenant_providers::AvailabilityRecord],
    refused: &std::collections::BTreeSet<(String, String)>,
) -> Vec<provenant_providers::AvailabilityRecord> {
    availability
        .iter()
        .filter(|record| {
            !refused.contains(&(
                record.adapter.clone(),
                record.account_key.clone().unwrap_or_default(),
            ))
        })
        .cloned()
        .collect()
}

fn redraw_candidates(
    result: Result<provenant_records::ResolvedRoute, crate::route::DispatchError>,
    reasons: &mut Vec<SkippedRoute>,
    rejected: &mut Vec<provenant_routing::ModelId>,
) -> Result<Vec<provenant_records::ResolvedRoute>, PrepareError> {
    match result {
        Ok(resolved) => {
            record_admission_redraws(&resolved, reasons, rejected)?;
            Ok(candidates(resolved))
        }
        Err(crate::route::DispatchError::Edge(error))
            if matches!(
                error.as_ref(),
                crate::route::EdgeError::Routing(
                    provenant_routing::RoutingError::NoEligiblePoolMember { .. }
                )
            ) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(PrepareError::RouteResolution {
            error,
            checks: std::mem::take(reasons),
        }),
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
    locate_override: Option<&ExecutableLocator<'_>>,
    fixture_token: Option<&provenant_providers::claude::OauthToken>,
    wait: Option<u64>,
) -> Result<run::PreparedLaunch, PrepareError> {
    if let run::RequestedRoute::Adapter { provider, name, .. } = &task.route {
        return prepare_command(
            registry,
            task,
            paths,
            resume,
            locate_override,
            provider,
            name,
        );
    }
    let policy_project = task
        .profile
        .policy_project
        .map(|project| project.to_string())
        .or_else(|| registry.project_id.map(|project| project.to_string()))
        .unwrap_or_default();
    let launch_policy = registry
        .roots
        .map_or_else(provenant_records::policy::Policy::default, |roots| {
            run::launch_policy(roots.state.as_path(), &policy_project, task)
        });
    // This is the raw, recorded prompt. It must fit the same bound as the
    // validated prompt artifact, before any host preamble is added.
    if prompt_text.len() as u64 > super::PROMPT_LIMIT {
        return Err(PrepareError::Prompt(LaunchPromptError::TooLarge));
    }
    let default_limit = provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES;
    let loaded_config = registry
        .roots
        .map(|roots| crate::config_reads::read_for(roots, "run"));
    const ARGV_LIMIT_KEY: &str = "domains.run.argv_prompt_limit_bytes";
    let (argv_limit, argv_origin, argv_origin_record) = match &loaded_config {
        Some(Ok(config)) => {
            let origin = config.origins.get(ARGV_LIMIT_KEY);
            let label = if matches!(origin, Some(provenant_config::Origin::User { .. })) {
                provenant_records::diagnostic::SettingSource::Configuration
            } else {
                provenant_records::diagnostic::SettingSource::ShippedDefault
            };
            (
                config.domains.run.argv_prompt_limit_bytes,
                label,
                origin_receipt(origin),
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
                provenant_records::diagnostic::SettingSource::ShippedDefault,
                origin_receipt(None),
            )
        }
        None => (
            default_limit,
            provenant_records::diagnostic::SettingSource::ShippedDefault,
            origin_receipt(None),
        ),
    };
    if let run::RequestedRoute::Model { model, .. } = &task.route
        && let Some((adapter, _)) = model.split_once('/')
    {
        check_launch_prompt(
            prompt_text,
            paths,
            Some(adapter),
            argv_limit,
            argv_origin,
            &launch_policy,
        )?;
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
    )
    .map_err(PrepareError::GraphInputsAuthority)?;
    let writer = match &task.writer {
        Some(request) => Some(run::prepare_writer_with_facts(
            request,
            roots.state.as_path(),
            project_id,
            &registry.repo_facts,
        )?),
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
    let fallback_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&fallback_cancel);
    let git_executor = provenant_os::exec::SystemExecutor::new(1).map_err(|e| e.to_string())?;
    let project_binding = super::confinement::registered_project_binding(roots, project_id)?;
    let requester_cwd = registry.cwd.unwrap_or(&source);
    let lane_path = match writer.as_ref() {
        Some(writer) => {
            let excluded_roots = writer
                .lane_path_roots()
                .into_iter()
                .chain(std::iter::once(writer.worktree.path()))
                .collect::<Vec<_>>();
            run::FilteredLanePath::for_requester_excluding_roots(registry.host, &excluded_roots)
                .map_err(|error| error.to_string())?
        }
        None => crate::run::filtered_requester_lane_path(
            registry.host,
            requester_cwd,
            &source,
            &provenant_confinement::UtilityRuntime::read_only(&git, roots.state.as_path())
                .map_err(|error| error.to_string())?,
            &git_executor,
            cancel,
            project_binding.as_ref(),
        )?,
    };
    let locate = |name: &str| match locate_override {
        Some(locate) => locate(name),
        None => preparation_executable(&lane_path, name),
    };
    let writer_context = writer
        .as_ref()
        .map(|writer| writer_source_context(writer, task, &git, &launch_policy))
        .transpose()?
        .flatten();
    // Let the host login lookup diagnose missing credentials, including relocated
    // stores and keychain logins, rather than guessing from the ambient home.
    let publishing = publishing_requested(
        task.access.network(true) == NetworkAccess::Open,
        &task.profile.knobs,
    );
    let publishing_gh = if publishing {
        match publishing_executable(&locate) {
            Ok(gh) => Some(gh),
            Err(cause) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: GitHub publishing unavailable: {cause}"
                );
                None
            }
        }
    } else {
        None
    };
    let git_context = task
        .base
        .as_deref()
        .map(|base| match task.head.as_deref() {
            Some(head) => {
                run::GitContext::capture_at(&git, &source, roots.state.as_path(), base, head)
            }
            None => run::GitContext::capture(&git, &source, roots.state.as_path(), base),
        })
        .transpose()?;
    let user = provenant_os::sys::account(provenant_os::sys::Account::Current)
        .map_err(|error| error.to_string())?
        .ok_or("current user identity is unavailable")?
        .name;

    // Nested attempts live in a lane-writable child store. Unconfined provider
    // commands, including status, version and token refresh, get a fresh cwd
    // under the host configuration root retained in the launch context.
    let executor =
        provenant_providers::host::HostProbeExecutor::new(&git_executor, roots.state.as_path())
            .map_err(|e| e.to_string())?
            .with_search_path(lane_path.path());
    let mut config = loaded_config
        .ok_or("routing context is unavailable")?
        .map_err(|error| error.to_string())?;
    config.domains.routing.launch_policy = launch_policy.clone();
    // Continue the applied decision rather than redrawing the named pool
    // using the new operation seed.
    let (mut request, retained_identity) = match resume {
        Some(prior) => {
            let (request, identity) = continuation_request(prior, &launch_policy)?;
            (request, Some(identity))
        }
        None => (route_request(task)?, None),
    };
    let task_request = route_request(task)?;
    request.intent = task_request.intent;
    request.confidential = task_request.confidential;
    let requested_adapter = match &task.route {
        run::RequestedRoute::Adapter { provider, .. } => Some(provider.as_str()),
        run::RequestedRoute::Model { model, .. } => {
            model.split_once('/').map(|(adapter, _)| adapter)
        }
        run::RequestedRoute::Named { .. } | run::RequestedRoute::Review { .. } => None,
    };
    let selection_adapter = retained_identity
        .map(|identity| identity.provider)
        .or(requested_adapter);
    // Continue with the applied identity as a provider/key pair. In particular,
    // a local key such as `local` is not unique across adapters.
    let effective_account_selector = effective_account_selector(
        task.account.as_deref(),
        retained_identity.and_then(|identity| identity.account_key),
        resume.is_some(),
    );
    let catalogue = provenant_routing::Catalogue::bundled().map_err(|e| e.to_string())?;
    let mut eligible = provenant_routing::probe_adapters(
        &request,
        &catalogue,
        &config.domains.routing,
        provider_registry,
    )
    .map_err(|e| e.to_string())?;
    if let Some(identity) = retained_identity {
        eligible.retain(|adapter| *adapter == identity.provider);
    }
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
    // Hold shared fences throughout preparation. An exhausted wait never grants access.
    let clock = provenant_os::clock::SystemClock::new();
    use provenant_core::Clock;
    let provider_leases =
        available_provider_leases(roots.state.as_path(), &eligible, wait, &clock, cancel)
            .map_err(|error| error.to_string())?;
    let eligible = |name: &str| provider_leases.contains_key(name);
    let settings = config.domains.accounts.unique_accounts();
    let cargo_cache = writer
        .as_ref()
        .map(|writer| {
            provenant_confinement::permissions::worktree_cargo_cache_path(&home, &writer.worktree)
        })
        .transpose()?;
    let mut lane_roots = vec![source.as_path(), cwd.as_path(), paths.scratch.as_path()];
    lane_roots.extend(cargo_cache.as_deref());
    let credential_roots = credential_roots(registry, &home, &settings, &lane_roots)?;
    let github_roots = github_roots(registry);
    let protected_worktrees = protected_worktrees(registry, &source, &git)?;
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
        let host_directory = registry
            .host
            .and_then(|host| host.default_account_directories.get(spec.name).cloned());
        let environment_directory = host_provider_var(registry, environment);
        let configured_by = match (host_directory.as_ref(), environment_directory.as_ref()) {
            (Some(directory), Some(value)) if directory.as_os_str() == value => Some(environment),
            (Some(_), _) => Some("host account-directory configuration"),
            (None, Some(_)) => Some(environment),
            (None, None) => None,
        };
        let directory = host_directory
            .or_else(|| environment_directory.map(PathBuf::from))
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
            entry.directory = canonical(&directory).map_err(|detail| {
                default_directory_error(spec, &directory, detail, configured_by)
            })?;
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
                let configured_directory =
                    matches!(&error, AccountObservationError::ConfiguredDirectory(_));
                default_failures.insert(
                    spec.name.to_owned(),
                    (error.skip_kind(), error.to_string(), configured_directory),
                );
            }
        }
    }
    // These adapters use the CLI's existing local login and configuration.
    for spec in ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.local_login && eligible(spec.name))
    {
        let adapter = spec.name;
        if include_local_login(
            spec,
            &entries,
            effective_account_selector,
            selection_adapter,
            retained_identity,
        ) && locate(adapter).is_ok()
            && !entries.iter().any(|entry| {
                entry.adapter == adapter
                    && (entry.key == spec.local_account.1 || entry.directory == home)
            })
        {
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
    // Enumerate only actual identities; never derive a second plan from a login.
    let settings = provenant_providers::accounts::Settings {
        entries: entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| (i.to_string(), entry))
            .collect(),
        ..Default::default()
    }
    .unique_accounts();
    let entries: Vec<_> = settings.entries.into_values().collect();
    // Resolve the exact continuation identity before observing account state.
    let selected_entry = match retained_identity {
        Some(identity) => select_retained_account(&entries, identity)?,
        None => effective_account_selector
            .map(|selector| select_account(&entries, selector, requested_adapter))
            .transpose()?,
    };
    let selected = selected_entry.map(|entry| entry.key.as_str());
    let selected_adapter = retained_identity
        .map(|identity| identity.provider)
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
    let timestamp = clock.unix_seconds();
    let diagnostic_records = crate::quota::read_cache(roots).unwrap_or_default();
    let mut availability = Vec::new();
    let mut reasons: Vec<SkippedRoute> = Vec::new();
    let mut reason_boundaries = std::collections::BTreeMap::new();
    let mut reason_resources = std::collections::BTreeMap::new();
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
                        extra: Default::default(),
                        host: provenant_quota::LOCAL_HOST.into(),
                        adapter: entry.adapter.clone(),
                        account_key: entry.key.clone(),
                        route: provenant_quota::ACCOUNT_ROUTE.into(),
                    };
                    let recorded =
                        crate::quota::read_readiness(roots.state.as_path(), &scope, timestamp);
                    // Staleness removes an old auth failure from admission,
                    // but the executable-only OpenCode probe still cannot
                    // establish login health. Keep that distinction visible
                    // as a launch warning instead of silently losing the
                    // reason when `read_readiness` projects it to `Stale`.
                    let persisted_auth_failure = crate::quota::recorded_readiness_file(
                        roots.state.as_path(),
                        &entry.adapter,
                        &entry.key,
                    )
                    .ok()
                    .flatten()
                    .is_some_and(|file| {
                        file.records.iter().any(|record| {
                            record.scope.host == provenant_quota::LOCAL_HOST
                                && record.scope.route == provenant_quota::ACCOUNT_ROUTE
                                && recorded.reason == Some(provenant_quota::ReadinessReason::Stale)
                                && recorded.observed_scope.as_ref() == Some(&record.scope)
                                && recorded.observed_unix_seconds
                                    == Some(record.observed_unix_seconds)
                                && matches!(
                                    record.reason,
                                    Some(
                                        provenant_quota::ReadinessReason::SignedOut
                                            | provenant_quota::ReadinessReason::LoginExpired
                                    )
                                )
                        })
                    });
                    let mismatch = recorded.reason
                        == Some(provenant_quota::ReadinessReason::AccountMismatch)
                        || diagnostic_records.iter().any(|r| {
                            r.account.adapter == entry.adapter
                                && r.account.account_key == entry.key
                                && r.last_error
                                    == Some(provenant_quota::ProbeFailure::AccountMismatch)
                        });
                    let auth_failure = mismatch
                        || diagnostic_records.iter().any(|record| {
                            record.account.adapter == entry.adapter
                                && record.account.account_key == entry.key
                                && (record
                                    .last_error
                                    .is_some_and(provenant_quota::ProbeFailure::needs_login)
                                    || record.snapshot.as_ref().is_some_and(|snapshot| {
                                        snapshot.status
                                            == provenant_quota::QuotaStatus::AuthRequired
                                    }))
                        })
                        || matches!(
                            recorded.reason,
                            Some(
                                provenant_quota::ReadinessReason::SignedOut
                                    | provenant_quota::ReadinessReason::LoginExpired
                            )
                        )
                        || persisted_auth_failure;
                    // A version-only probe cannot clear an authentication refusal.
                    if auth_failure
                        && login_check_can_clear(&entry.adapter, mismatch)
                        && strategy != provenant_records::LoginReprobe::ExecutableOnly
                    {
                        let executable = Path::new(&record.executable);
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
                host_roots: Some(roots),
                cwd: &cwd,
                home: &home,
                state: roots.state.as_path(),
                credential_roots: &credential_roots,
                github_roots: &github_roots,
                protected_worktrees: &protected_worktrees,
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
            InventoryContext::Preflight,
        )
        .map_err(PlanCheckError::from)
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
    let host = provenant_config::bind_host(roots)
        .map_err(|e| e.to_string())?
        .to_string();
    let rules = crate::route::task_exclusion_rules(&task.profile, roots)
        .map_err(|error| error.to_string())?;
    let required_family = task.route.required_family();
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
    let routing_evidence = crate::route::selection_evidence(
        roots,
        &home,
        &source,
        task.context
            .as_ref()
            .and_then(|context| context.role.as_deref()),
    );
    let resolve = |availability: &[provenant_providers::AvailabilityRecord],
                   rejected: &[provenant_routing::ModelId]| {
        crate::route::resolve_dispatch_rejecting_family(
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
            required_family,
            Some(&routing_evidence),
            Some(&launch_policy),
        )
        .map(|mut resolved| {
            // The router cannot observe host fences. Preserve the known
            // preparation refusal in its pool receipt instead of unknown
            // availability for providers we deliberately did not probe.
            if let Some(selection) = &mut resolved.pool_selection {
                for candidate in &mut selection.candidates {
                    if candidate
                        .model
                        .split_once('/')
                        .is_some_and(|(adapter, _)| !eligible(adapter))
                    {
                        candidate.readiness =
                            Some(provenant_records::RouteReadiness::Unavailable {
                                skipped_accounts: Vec::new(),
                                extra: Default::default(),
                                reason: provenant_records::RouteExclusionReason::ProviderUpdating,
                            });
                        candidate.exclusion = Some("provider update in progress".into());
                    }
                }
            }
            resolved
        })
    };
    let resolved = resolve(&availability, &[]).map_err(|error| PrepareError::RouteResolution {
        error,
        checks: plan_errors
            .iter()
            .map(|(adapter, error)| error.skipped(adapter))
            .collect(),
    })?;
    let named_pool = resolved.pool_selection.is_some() && resume.is_none();
    let mut rejected = Vec::new();
    record_admission_redraws(&resolved, &mut reasons, &mut rejected)?;
    // Candidate preparation happens before writer custody or guardian handoff,
    // so no excluded candidate has run. Advance past every exclusion, whatever
    // its cause, through the sealed alternatives and a named pool's remaining
    // members, and never substitute a differently pinned account. Settlement
    // retries bound launched attempts, not candidates that never launched.
    let mut previous_outcome = provenant_routing::FallbackOutcome::Other;
    let mut queue: std::collections::VecDeque<_> = candidates(resolved).into();
    let mut reprobed_accounts = std::collections::BTreeSet::new();
    let mut refused_accounts = std::collections::BTreeSet::<(String, String)>::new();
    let mut candidate = 0usize;
    // A login resolved for one route serves every later route and the reader path.
    let mut gh_login: super::gh_auth::LoginCache = None;
    // Whether a workflow in this checkout runs on pull request label events;
    // the gh wrapper then says a label change starts CI (#3270).
    let label_ci_warning =
        publishing_gh.is_some() && provenant_config::label_events_trigger_ci(&cwd);
    loop {
        if queue.is_empty() {
            let next = if named_pool {
                match redraw_candidates(
                    resolve(
                        &without_refused_accounts(&availability, &refused_accounts),
                        &rejected,
                    ),
                    &mut reasons,
                    &mut rejected,
                ) {
                    Ok(next) => next,
                    Err(PrepareError::RouteResolution { error, mut checks }) => {
                        attach_candidate_guard_facts(
                            &mut checks,
                            &reason_boundaries,
                            &reason_resources,
                        )?;
                        return Err(PrepareError::RouteResolution { error, checks });
                    }
                    Err(error) => return Err(error),
                }
            } else {
                Vec::new()
            };
            // Each redraw excludes every candidate already tried, so a pool
            // ends when no untried member remains.
            queue.extend(next.into_iter().filter(|route| {
                provenant_routing::ModelId::parse(&format!("{}/{}", route.adapter, route.model))
                    .is_ok_and(|model| !rejected.contains(&model))
            }));
            if queue.is_empty() {
                if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired
                    && let Some(last) = reasons.last_mut()
                {
                    last.detail.push_str("; no eligible fallback remains");
                }
                break;
            }
        }
        let Some(mut route) = next_permitted_candidate(
            &mut queue,
            &mut rejected,
            &mut reasons,
            (selected, selected_adapter),
            candidate > 0,
            previous_outcome,
        )?
        else {
            continue;
        };
        let current_candidate = candidate;
        candidate += 1;
        previous_outcome = provenant_routing::FallbackOutcome::Other;
        let label = format!("{}/{}", route.adapter, route.model);
        if !eligible(&route.adapter) {
            previous_outcome = provenant_routing::FallbackOutcome::AdapterUnavailable;
            let mut skipped = skip(&label, SkipKind::Unavailable, "provider updating");
            skipped.set_cause(provenant_records::RouteExclusionReason::ProviderUpdating);
            skipped.detail = provenant_records::diagnostic::safe_text(&format!(
                "provider update in progress for {}; wait for the update to finish or select another eligible route",
                route.adapter
            ));
            reasons.push(skipped);
            continue;
        }
        if let Some(spec) = AdapterSpec::get(&route.adapter)
            && let Err(error) =
                request
                    .intent
                    .check_launch(spec, &route.model, route.effort.as_deref())
        {
            reason_boundaries.insert(
                reasons.len(),
                provenant_records::diagnostic::BoundaryClass::Authority,
            );
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
            reason_boundaries.insert(
                reasons.len(),
                provenant_records::diagnostic::BoundaryClass::Authority,
            );
            reasons.push(skip(&label, SkipKind::Refused, "read-only model policy"));
            continue;
        }
        if let Some(offered) = unoffered_model(&availability, &route, clock.unix_seconds()) {
            reason_boundaries.insert(
                reasons.len(),
                provenant_records::diagnostic::BoundaryClass::Authority,
            );
            reasons.push(skip(
                &label,
                SkipKind::Refused,
                format!(
                    "model `{}` is not offered by this account; offered models: {}",
                    route.model,
                    offered.join(", ")
                ),
            ));
            continue;
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
                    crate::route::resolve_dispatch_rejecting_family(
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
                        required_family,
                        Some(&routing_evidence),
                        Some(&launch_policy),
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
                if let Some((kind, detail, configured_directory)) =
                    default_failures.get(&route.adapter)
                {
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
                    if *kind == SkipKind::AuthRequired || *configured_directory {
                        skipped.detail = provenant_records::diagnostic::safe_text(detail);
                    }
                    reasons.push(name_skipped_accounts(skipped, &route, &entries));
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
                    reasons.push(name_skipped_accounts(skipped, &route, &entries));
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
                reasons.push(name_skipped_accounts(skipped, &route, &entries));
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
                reasons.push(name_skipped_accounts(skipped, &route, &entries));
                continue;
            }
            previous_outcome = outcome;
            if matches!(
                reason,
                provenant_records::RouteExclusionReason::ConfinementUnavailable
            ) && let Some(error) = plan_errors.get(route.adapter.as_str())
            {
                reported_plan_errors.insert(route.adapter.clone());
                reason_boundaries.insert(reasons.len(), error.boundary_class());
                let mut skipped = error.skipped(&label);
                // Keep the router's readiness classification alongside the plan fact.
                skipped.kind = SkipKind::Unavailable;
                skipped.set_cause(reason.clone());
                reasons.push(name_skipped_accounts(skipped, &route, &entries));
                continue;
            }
            if matches!(
                reason,
                provenant_records::RouteExclusionReason::PacingReserve
            ) && let Some(fact) = route.diagnostics.pacing_reserve_fact
                && matches!(
                    fact,
                    provenant_records::diagnostic::ResourceFact::PacingReserve { .. }
                )
            {
                reason_resources.insert(
                    reasons.len(),
                    resolve_pacing_source(fact, &route.adapter, &config.origins),
                );
            }
            let mut skipped = skip(
                &label,
                SkipKind::Unavailable,
                provenant_routing::exclusion_detail(reason),
            );
            skipped.set_cause(reason.clone());
            reasons.push(name_skipped_accounts(skipped, &route, &entries));
            continue;
        }
        if let Err(error) =
            crate::route::check_dispatch_candidate(roots, &route, provider_registry, &launch_policy)
        {
            // Admission refuses an account whose allowance is spent. Another
            // account may still serve the same model, so move on to it once per
            // account, never past a pinned one.
            let spent = matches!(
                &error,
                crate::route::EdgeError::Quota(provenant_quota::QuotaError::AllowanceExhausted)
            );
            let refused_key = route
                .account_key
                .clone()
                .filter(|_| spent && selected.is_none());
            let Some(key) = refused_key else {
                reasons.push(skip(&label, SkipKind::Unavailable, error.to_string()));
                continue;
            };
            let name = entries
                .iter()
                .find(|entry| entry.adapter == route.adapter && entry.key == key)
                .map_or_else(
                    || "unregistered account".to_owned(),
                    |entry| entry.label.clone(),
                );
            let mut skipped = skip(
                &label,
                SkipKind::Account,
                format!("account {name}: {error}"),
            );
            skipped.set_cause(provenant_records::RouteExclusionReason::Exhausted);
            reasons.push(skipped);
            previous_outcome = provenant_routing::FallbackOutcome::from_exclusion(
                &provenant_records::RouteExclusionReason::Exhausted,
            );
            refused_accounts.insert((route.adapter.clone(), key));
            let remaining = without_refused_accounts(&availability, &refused_accounts);
            if let Ok(model) = provenant_routing::ModelId::parse(&label)
                && remaining
                    .iter()
                    .any(|record| record.adapter == route.adapter)
            {
                rejected.retain(|tried| *tried != model);
            }
            if let Ok(next) = resolve(&remaining, &rejected) {
                queue.extend(candidates(next));
            }
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
        // Reader snapshots require empty private Git state. Writers retain
        // their existing candidate directory for idempotent preparation.
        let scratch_claim = if writer.is_none() {
            fresh_scratch_directory(staging, "candidate", Some(current_candidate))
        } else {
            let name = format!("candidate-{current_candidate}");
            apply_setup_directory(&[staging], &staging.join(&name))
                .map(|directory| (directory, name))
        };
        // Retain the handle: a parent lane can rename tmp before context is
        // published, so later writes must still use the held directory.
        let (scratch_directory, scratch_name) = match scratch_claim {
            Ok(scratch) => scratch,
            Err(error) => {
                reasons.push(setup_skip(&label, error));
                continue;
            }
        };
        let scratch = scratch_directory
            .current_path()
            .map_err(|error| error.to_string())?;
        let private_state = paths.provider_state.join(&scratch_name);
        if let Err(error) = apply_setup(
            &[SetupStep::CreateDir(private_state.clone())],
            &[&paths.provider_state],
            None,
        ) {
            reasons.push(setup_skip(&label, error));
            continue;
        }
        let continuation = match resume {
            Some(prior) => match continuation_for_policy(
                prior,
                &route,
                &task.authority_digest.to_string(),
                ContinuationState {
                    state_root,
                    config_root: roots.state.as_path(),
                    private_state: &private_state,
                },
                &launch_policy,
            ) {
                Ok(continuation) => Some(continuation),
                Err(error) => {
                    let class = error.boundary_class();
                    if class != provenant_records::diagnostic::BoundaryClass::None {
                        reason_boundaries.insert(reasons.len(), class);
                    }
                    if let Some(fact) = error.resource_fact() {
                        reason_resources.insert(reasons.len(), fact);
                    }
                    reasons.push(match error.guard() {
                        Some(guard) => continuation_skip(&label, guard),
                        None => skip(&label, SkipKind::Refused, error.to_string()),
                    });
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
        let text = match &writer_context {
            Some(context) => format!("{context}\n\n{text}"),
            None => text,
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
        let mut request = match confinement_request(
            project_id,
            ConfinementRoots {
                host_roots: Some(roots),
                cwd: &cwd,
                home: &home,
                state: roots.state.as_path(),
                credential_roots: &credential_roots,
                github_roots: &github_roots,
                protected_worktrees: &protected_worktrees,
                scratch,
                private_state,
                assets,
                prompt_file: None,
            },
            writer.as_ref(),
            &task.access,
            &host_writes,
            adapter.id(),
            publishing_gh.is_some(),
            InventoryContext::Launch(task, &format!("{}/{}", route.adapter, route.model)),
        )
        .map_err(PlanCheckError::from)
        {
            Ok(request) => request,
            Err(error) => {
                reason_boundaries.insert(reasons.len(), error.boundary_class());
                reasons.push(error.skipped(&label));
                continue;
            }
        };
        let handoff = private_state.join("handoff.json");
        let writable_handoff = handoff_writable(&request.permissions);
        let limits = run::lane_limits(
            task,
            state_root,
            &project_id.to_string(),
            adapter.id(),
            run::HostCaps {
                runtime_seconds: config
                    .providers
                    .get(adapter.id())
                    .and_then(|provider| provider.runtime_deadline_seconds),
            },
        );
        let lanes = provenant_config::LaneBuildSettings::read(state_root).unwrap_or_default();
        let total = provenant_os::sys::resources::memory().map_or(0, |memory| memory.total_bytes);
        let extras = run::PreambleExtras {
            verification: Some(run::VerificationResolution::resolve(
                &task.profile,
                prompt_text,
            )),
            build_cap: Some(run::BuildCap {
                slots: lanes.build_limit(total),
                source: lanes.build_limit_source(),
            }),
        };
        let text = format!(
            "{}\n{text}",
            run::lane_preamble_with(
                &model_policy.guidance(),
                &limits,
                writer
                    .is_some()
                    .then(|| run::ResolvedCiScope::resolve(&task.profile)),
                writable_handoff.then_some(handoff.as_path()),
                &extras,
            )
        );
        let prompt = match launch_prompt(
            &text,
            paths,
            state_root,
            adapter.id(),
            argv_limit,
            argv_origin,
            &launch_policy,
        ) {
            Ok(prompt) => prompt,
            Err(error) => {
                if let Some(fact) = error.resource_fact() {
                    reason_resources.insert(reasons.len(), fact);
                }
                reasons.push(skip(&label, SkipKind::Refused, error.to_string()));
                continue;
            }
        };
        // Only the spilled prompt path was unknown when the request was built.
        // Add its exact read grant without recomputing host policy and Git roots.
        if let Some(path) = prompt.file() {
            let grant = PathGrant {
                path: path.to_owned(),
                scope: PathScope::Exact,
            };
            if let Some(policy) = request.permissions.policy.as_mut()
                && let Err(error) = policy
                    .mandatory_mut()
                    .add_state_read(grant.clone())
                    .map_err(PlanCheckError::from)
            {
                reason_boundaries.insert(reasons.len(), error.boundary_class());
                reasons.push(error.skipped(&label));
                continue;
            }
            request.permissions.read_roots.push(grant);
        }
        let planned = match writer.as_ref() {
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
                .map_err(PlanCheckError::from),
            None => read_only_plan(
                roots,
                project_id,
                &source,
                &cwd,
                &git,
                request,
                isolated,
                task.access.source_live,
            )
            .map_err(PlanCheckError::from)
            .and_then(|mut planned| {
                if isolated {
                    planned.1 = Some(source_head(task)?);
                    planned.3 = Some(cwd.join(".git"));
                } else {
                    require_source_head(task, planned.1)?;
                }
                Ok(planned)
            }),
        };
        let (mut plan, observed_head, repository, worktree_gitdir) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                reason_boundaries.insert(reasons.len(), error.boundary_class());
                reasons.push(error.skipped(&label));
                continue;
            }
        };
        let credential_roots = plan.permissions().credential_roots.clone();
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
                    previous_outcome = provenant_routing::FallbackOutcome::Other;
                    reasons.push(skip(&label, SkipKind::Refused, error));
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
                    previous_outcome = provider_preparation_fallback(&error);
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
        let model_id =
            provenant_routing::ModelId::parse(&label).map_err(|error| error.to_string())?;
        let launch_model = catalogue.launch_model(&model_id, route.effort.as_deref());
        let provider_model = (launch_model != route.model).then_some(launch_model);
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
            provenant_records::run::UserSkillsSourceReceipt {
                route_policy: Some(effective.sources.clone()),
                ..Default::default()
            }
        } else {
            let key = if project_override.is_some() {
                project_key.as_str()
            } else {
                "domains.lanes.user_skills"
            };
            provenant_records::run::UserSkillsSourceReceipt {
                key: Some(key.into()),
                origin: Some(origin_receipt(config.origins.get(key))),
                ..Default::default()
            }
        };
        provider_route.set_user_skills(user_skills_mode.is_discover());
        let nonce = entropy_128().map_err(|error| error.to_string())?;
        let grant_digest = task.authority_digest.to_string();
        let sandbox = sandbox_mode(
            &host_writes,
            &format!("{}/{}", route.adapter, route.model),
            task,
            &plan,
        );
        let prepare = |provider_route: &provenant_records::ResolvedRoute| {
            adapter.prepare(PrepareInput {
                credential_roots: credential_roots.clone(),
                images: &task.access.attachments,
                policy: &launch_policy,
                command: None,
                route: provider_route,
                provider_model: provider_model.as_deref(),
                confined: &plan,
                prompt: &prompt,
                continuation: continuation.as_ref(),
                grant_digest: &grant_digest,
                executable: &executable,
                home: &home,
                user: &user,
                host_var: &|key| {
                    if key == provenant_core::consumer_env::SANDBOX {
                        Some(sandbox.env_value().into())
                    } else if key == "PATH" {
                        Some(lane_path.path().to_os_string())
                    } else {
                        host_provider_var(registry, key)
                    }
                },
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
                let class = error.boundary_class();
                if class != provenant_records::diagnostic::BoundaryClass::None {
                    reason_boundaries.insert(reasons.len(), class);
                }
                if let Some(fact) = error.resource_fact() {
                    reason_resources.insert(reasons.len(), fact);
                }
                let kind = if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired {
                    SkipKind::AuthRequired
                } else {
                    SkipKind::Refused
                };
                reasons.push(match error {
                    provenant_providers::ProviderError::ContinuationRefused(guard) => {
                        continuation_skip(&label, guard)
                    }
                    _ => skip(&label, kind, error.to_string()),
                });
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
                    let class = error.boundary_class();
                    if class != provenant_records::diagnostic::BoundaryClass::None {
                        reason_boundaries.insert(reasons.len(), class);
                    }
                    if let Some(fact) = error.resource_fact() {
                        reason_resources.insert(reasons.len(), fact);
                    }
                    let kind =
                        if previous_outcome == provenant_routing::FallbackOutcome::AuthRequired {
                            SkipKind::AuthRequired
                        } else {
                            SkipKind::Refused
                        };
                    reasons.push(match error {
                        provenant_providers::ProviderError::ContinuationRefused(guard) => {
                            continuation_skip(&label, guard)
                        }
                        _ => skip(&label, kind, error.to_string()),
                    });
                    continue;
                }
            };
            spec.user_skills = skills;
        }
        let native_root = match &spec.session_state {
            provenant_providers::SessionStateLocation::AttemptPrivate => None,
            provenant_providers::SessionStateLocation::Native { root }
                if !sandbox.effective && root.is_absolute() =>
            {
                Some(root.clone())
            }
            _ => {
                reasons.push(skip(
                    &label,
                    SkipKind::Refused,
                    "provider session-state location does not match the prepared sandbox or is not an absolute native root",
                ));
                reason_boundaries.insert(
                    reasons.len() - 1,
                    provenant_records::diagnostic::BoundaryClass::Authority,
                );
                continue;
            }
        };
        if let Some(prior) = resume {
            let prior_native = prior
                .applied_route
                .as_ref()
                .is_some_and(|applied| prior_uses_native_session_state(applied, &route.adapter));
            if prior_native != native_root.is_some() {
                reasons.push(continuation_skip(
                    &label,
                    provenant_records::diagnostic::ContinuationGuard {
                        rule: provenant_records::diagnostic::ContinuationRule::NativeSession,
                        field: provenant_records::diagnostic::ContinuationField::SessionState,
                        issue: provenant_records::diagnostic::ContinuationIssue::Mismatch,
                    },
                ));
                reason_boundaries.insert(
                    reasons.len() - 1,
                    provenant_records::diagnostic::BoundaryClass::Authority,
                );
                continue;
            }
            if let Some(root) = native_root.as_deref()
                && let Err(error) = verify_native_continuation(prior, &route, root, state_root)
            {
                let class = error.boundary_class();
                reasons.push(match error.guard() {
                    Some(guard) => continuation_skip(&label, guard),
                    None => skip(&label, SkipKind::Refused, error.to_string()),
                });
                if class != provenant_records::diagnostic::BoundaryClass::None {
                    reason_boundaries.insert(reasons.len() - 1, class);
                }
                continue;
            }
        }
        if let Some(alias) = &paths.temporary_alias {
            alias_temporary_env(&mut spec.env, scratch, &alias.join(&scratch_name));
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
                credential_roots: &credential_roots,
                common_git: common_git_path.as_deref(),
                state_root: roots.state.as_path(),
                worktree_gitdir: worktree_gitdir.as_deref(),
                writer: writer.is_some(),
                settings: config.domains.lanes.projects.get(&project_id_str),
                project: &project_id_str,
                policy: &launch_policy,
            },
        )?;
        let process_tree_read = configure_process_tree(&mut spec, paths)?;
        let build_settings = config.domains.fabric.lanes.build_settings();
        spec.configure_cargo_sources(
            build_settings.cargo_source_writes,
            plan.permissions().network,
        );
        let coordination_reads = configure_build_slots(
            &mut spec,
            &build_settings,
            &source,
            &home,
            scratch,
            &provenant_executable,
            roots.state.as_path(),
            &task.profile.knobs,
            &launch_policy,
        )?;
        if let Some(policy) = plan.policy_mut() {
            for root in std::iter::once(process_tree_read).chain(coordination_reads) {
                policy.mandatory_mut().add_state_read(root)?;
            }
        }
        if !host_indexes_protected(
            roots.state.as_path(),
            &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
        ) {
            let effect = launch_policy.check(
                "CONFINEMENT_HOST_INDEXES_PROTECTED_DATA_INTEGRITY",
                provenant_records::policy::Facts::violation("write grants overlap host indexes"),
            );
            if effect.blocks() {
                return Err(PrepareError::HostIndexOverlap);
            }
        }
        if !target_seeds_protected(
            roots.state.as_path(),
            &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
        ) {
            let effect = launch_policy.check(
                "CONFINEMENT_TARGET_SEEDS_PROTECTED_DATA_INTEGRITY",
                provenant_records::policy::Facts::violation("write grants overlap target seeds"),
            );
            if effect.blocks() {
                return Err(PrepareError::TargetSeedOverlap);
            }
        }
        if let Some(head) = &task.head {
            spec.env
                .push((provenant_core::consumer_env::LANE_HEAD.into(), head.into()));
            if let Some(base) = &task.base {
                spec.env
                    .push((provenant_core::consumer_env::LANE_BASE.into(), base.into()));
            }
        }
        set_cargo_profile(&mut spec.env, &build_settings, !sandbox.effective);
        spec.env.extend(
            model_policy
                .launch_env(adapter.id())
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        if sandbox.effective {
            set_cargo_jobs(
                &mut spec.env,
                registry
                    .host
                    .and_then(|host| host.cargo_build_jobs.as_deref()),
                build_settings.jobs(),
            );
        }
        if let Some(adapter_spec) = adapter_spec {
            spec.configure_shell(scratch, adapter_spec);
        }
        let mut gh_attempted = false;
        // The one GitHub credential decision for this lane (gh_auth), with
        // its delivery; the receipt records it below.
        let mut gh_selection = super::gh_auth::Selection::new(
            super::gh_auth::Delivery::None(
                if publishing {
                    "GitHub access not requested by this adapter"
                } else {
                    "publishing off or network closed"
                }
                .into(),
            ),
            false,
        );
        let gh_permitted = github_auth_permitted(&plan, &spec.profile.spec);
        if publishing && !gh_permitted {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: {label}: GitHub publishing credentials withheld by the access policy"
            );
            gh_selection = super::gh_auth::Selection::new(
                if sandbox.effective {
                    super::gh_auth::Delivery::None("withheld by the access policy".into())
                } else {
                    // Nothing is injected, but an unsandboxed lane can still
                    // read the user's login stores itself.
                    super::gh_auth::Delivery::Ambient
                },
                false,
            );
        }
        let gh_policy = match publishing_gh
            .as_ref()
            .map(|_| provenant_config::GithubPolicy::read(&roots.state, project_id))
        {
            None => provenant_config::GithubPolicy::default(),
            Some(Ok(policy)) => policy,
            // Sandboxed publishing refuses an unreadable policy: its selected
            // credential restrictions cannot be applied.
            Some(Err(error)) if sandbox.effective => {
                return Err(launch_policy.boundary_error(
                    "PREPARATION_PREPARE_WITH_CREDENTIALS",
                    provenant_records::policy::Facts::violation(error.to_string()),
                    error.to_string().into(),
                ));
            }
            Some(Err(error)) => {
                if let Some(message) = launch_policy
                    .check(
                        "PREPARATION_GITHUB_POLICY_NONE",
                        provenant_records::policy::Facts::violation(format!(
                            "{label}: GitHub policy unreadable, the gh wrapper applies no deny list: {error}"
                        )),
                    )
                    .message()
                {
                    let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
                }
                provenant_config::GithubPolicy::default()
            }
        };
        let gh_budget = limits.gh_invocations.as_ref();
        let gh_shim = provenant_providers::GhShim {
            policy: &gh_policy,
            budget: gh_budget.map_or(0, |limit| limit.value),
            budget_source: gh_budget.map_or("default", |limit| limit.source.as_str()),
            label_ci_warning,
        };
        if let Some(gh) = publishing_gh
            .as_ref()
            .filter(|_| sandbox.effective && gh_permitted)
        {
            // Resolve the login before composing gh's wrapper: token launches
            // use a private empty config directory, independent of host links.
            gh_attempted = true;
            let delivery = match super::gh_auth::inject(
                &executor,
                gh,
                &home,
                &|key| host_provider_var(registry, key),
                cancel,
                &mut gh_login,
                &mut spec.env,
            ) {
                Ok(origin) => super::gh_auth::Delivery::Injected(origin),
                Err(warning) => {
                    let _ = writeln!(std::io::stderr().lock(), "warning: {label}: {warning}");
                    super::gh_auth::Delivery::None("login lookup failed".into())
                }
            };
            let publishing = spec.configure_publishing(scratch, &git, gh, &home, &gh_shim);
            if let Err(error) = publishing {
                let class = error.boundary_class();
                if class != provenant_records::diagnostic::BoundaryClass::None {
                    reason_boundaries.insert(reasons.len(), class);
                }
                reasons.push(setup_skip(&label, error.to_string()));
                continue;
            }
            gh_selection = super::gh_auth::Selection::new(delivery, true);
        } else if let Some(gh) = publishing_gh.as_ref().filter(|_| !sandbox.effective) {
            // An unsandboxed lane keeps the user's own gh login; the counting
            // wrapper still fronts gh on its PATH. A wrapper that cannot be
            // installed leaves gh unmediated, which the receipt says.
            let mediated = match spec.configure_gh_shim(scratch, gh, &gh_shim) {
                Ok(()) => true,
                Err(error) => {
                    if let Some(message) = launch_policy
                        .check(
                            "PREPARATION_GH_SHIM_NONE",
                            provenant_records::policy::Facts::violation(format!(
                                "{label}: gh wrapper unavailable, gh runs uncounted: {error}"
                            )),
                        )
                        .message()
                    {
                        let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
                    }
                    false
                }
            };
            gh_selection =
                super::gh_auth::Selection::new(super::gh_auth::Delivery::Ambient, mediated);
        }
        if git_context.is_some()
            && let Some(argument) = adapter_spec.and_then(|spec| spec.context_directory_arg)
        {
            // Scratch is already granted by the outer profile. Claude's file
            // tools also need it in their working directories to read context.
            spec.argv
                .splice(1..1, [argument.into(), scratch.as_os_str().to_owned()]);
        }
        if writer.is_none() {
            super::command::scratch_cargo_output(&mut spec.env, scratch);
        }
        spec.complete_tool_requirements(&plan, &cwd)?;
        if isolated {
            super::source_tree::ensure_profile_isolated(&source, &spec.profile.spec)
                .map_err(PrepareError::SourceProfileIsolated)?;
            super::source_tree::ensure_view_read_only(
                &cwd,
                &provenant_confinement::write_grants(plan.permissions(), &spec.profile),
            )
            .map_err(PrepareError::SourceViewReadOnly)?;
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
            reasons.push(setup_skip(&label, format!("setup failed: {error}")));
            continue;
        }
        if let Some(warning) = image_input_warning(&route.adapter, &task.access.attachments) {
            push_launch_warning(&mut route, warning);
        }
        for warning in ignore_file_warnings(&route.adapter, task, roots.state.as_path()) {
            push_launch_warning(&mut route, warning);
        }
        if matches!(
            &task_request.choice,
            provenant_routing::RouteChoice::Model(_)
        ) && let Some(scope) = config.providers.get(&route.adapter)
            && let Some(warning) = scope.pin_warning(&route.adapter, &route.model)
        {
            push_launch_warning(&mut route, warning);
        }
        if route.eligibility == provenant_records::RouteEligibility::UnverifiedModel {
            let warning = format!(
                "model {}/{} is not in the catalogue; the provider will check it",
                route.adapter, route.model
            );
            push_launch_warning(&mut route, warning);
        }
        for warning in config.domains.routing.pool_effort_warnings(&catalogue) {
            push_launch_warning(&mut route, warning);
        }
        if let Some((fallback, reason)) = &fallback_dropped {
            push_launch_warning(
                &mut route,
                format!(
                    "{label}: model policy fallback {} ({reason}); dropped",
                    fallback.as_str()
                ),
            );
        }
        if writer.is_some()
            && let Some(advisory) = config
                .domains
                .routing
                .opus_writer_advisory(&catalogue, &route)
        {
            push_launch_warning(&mut route, advisory);
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
                let result = prepare_writer_dependencies_with_policy(
                    writer.worktree.path(),
                    enabled,
                    &|| host_dependency_path(cancel),
                    cancel,
                    &task.profile,
                    &launch_policy,
                );
                // Cancellation remains terminal; dependency setup is otherwise
                // advisory and the provider may install or repair it in the lane.
                if cancel.is_cancelled() {
                    return Err(PrepareError::Other(
                        "dependency preparation cancelled".into(),
                    ));
                }
                dependency_preparation_event(result, enabled, &task.profile, &launch_policy)
            }
            None => None,
        };
        let mut provider = launch_route_record(
            &route,
            entry.map(|e| e.label.as_str()),
            &task.route,
            resume.is_some(),
            current_candidate > 0,
        );
        provider.account_source = Some(if resume.is_some() {
            provenant_records::run::AccountSource::Continuation
        } else if task.account.is_some() {
            provenant_records::run::AccountSource::Pin
        } else {
            provenant_records::run::AccountSource::Automatic
        });
        if refreshed
            .as_ref()
            .is_some_and(|token| token.is_lane_token())
        {
            provider.lane_token = Some(true);
            provider.registered_lane_token = Some(true);
        }
        provider.project_paths = Some(serde_json::from_value(project_paths)?);
        provider.argv_prompt_limit = Some(provenant_records::run::ArgvPromptLimitReceipt {
            value: argv_limit as u64,
            key: ARGV_LIMIT_KEY.into(),
            origin: argv_origin_record.clone(),
            extra: Default::default(),
        });
        if let Some(event) = &dependency_event {
            provider.dependency_preparation = Some(serde_json::from_value(event.clone())?);
        }
        let mut skills =
            user_skills_exposure(user_skills_mode, user_skills_origin, &spec.user_skills);
        skills.stage_max_bytes = Some(provenant_records::run::ArgvPromptLimitReceipt {
            value: config.domains.lanes.skill_stage_max_bytes,
            key: "domains.lanes.skill_stage_max_bytes".into(),
            origin: origin_receipt(config.origins.get("domains.lanes.skill_stage_max_bytes")),
            extra: Default::default(),
        });
        provider.user_skills = Some(skills);
        provider.cargo_home = serde_json::from_value(spec.cargo_home_receipt())?;
        provider.tool_closure = Some(serde_json::from_value(serde_json::Value::Array(
            spec.prepared_tools()
                .iter()
                .map(|tool| tool.receipt())
                .collect(),
        ))?);
        provider.applied_access = Some(serde_json::from_value(
            spec.applied_access()
                .receipt(&home, spec.credential_roots()),
        )?);
        provider.discovery_warnings = Some(discovery_warnings(
            &spec.omitted_discovery,
            &home,
            spec.credential_roots(),
            false,
        ));
        let cargo_home = spec
            .cargo_home()
            .cloned()
            .ok_or_else(|| PrepareError::Other("host Cargo binding was not finalised".into()))?;
        let stdin = match spec.stdin {
            Stdin::Bytes(bytes) => bytes,
            Stdin::Null => Vec::new(),
        };
        // Resolve gh credentials on the host, where a keychain login is
        // available. The token travels only in the live launch environment.
        let reader_gh = sandbox.effective
            && github_auth_permitted(&plan, &spec.profile.spec)
            && adapter_spec.is_some_and(|spec| spec.github_auth.is_some());
        if let Some(gh) = publishing_gh
            .clone()
            .or_else(|| reader_gh.then(|| locate("gh").ok()).flatten())
            && reader_gh
            && !gh_attempted
        {
            let delivery = match super::gh_auth::inject(
                &executor,
                &gh,
                &home,
                &|key| host_provider_var(registry, key),
                cancel,
                &mut gh_login,
                &mut spec.env,
            ) {
                Ok(origin) => super::gh_auth::Delivery::Injected(origin),
                Err(warning) => {
                    let _ = writeln!(std::io::stderr().lock(), "warning: {label}: {warning}");
                    super::gh_auth::Delivery::None("login lookup failed".into())
                }
            };
            gh_selection = super::gh_auth::Selection::new(delivery, false);
        }
        provider.github_credential = Some(serde_json::from_value(gh_selection.receipt())?);
        if writer.is_some() {
            let scope = run::ResolvedCiScope::resolve(&task.profile);
            provider.ci_scope = Some(provenant_records::run::CiScopeReceipt {
                value: scope.value.as_str().into(),
                source: scope.source,
                key: "lanes.ci_scope".into(),
                extra: Default::default(),
            });
        }
        gh_selection.finalise_environment(&mut spec.env);
        if adapter_spec.is_some_and(|spec| {
            spec.github_auth == Some(provenant_records::GithubAuth::ShellEnvironment)
        }) && (!sandbox.effective || spec.env.iter().any(|(key, _)| key == "GH_TOKEN"))
        {
            super::gh_auth::codex_shell_environment(&spec.env, &mut spec.argv);
        }
        attach_candidate_guard_facts(&mut reasons, &reason_boundaries, &reason_resources)?;
        if let Some(policy) = plan.policy_mut() {
            if let Some(stage) = &spec.user_skills.root {
                policy.mandatory_mut().add_state_read_exec(PathGrant {
                    path: stage.clone(),
                    scope: PathScope::Subtree,
                })?;
            }
            if resume.is_some() {
                // The selected adapter, not optional lane reads, declares the
                // exact retained inputs needed to continue its prior session.
                for root in &spec.profile.spec.continuation_reads {
                    policy.mandatory_mut().add_state_read(root.clone())?;
                }
            }
        }
        // Carry the exact continuation expectation already bound into the
        // provider's shared stream decoder. Admission seals it with this
        // candidate route so the live and terminal host decoders can prove
        // that a resumed child stayed in its predecessor session.
        let continuation_expectation = match (resume.is_some(), spec.stream.expected_session()) {
            (true, Some(session)) => Some(run::ContinuationExpectation {
                session: session.to_owned(),
            }),
            (true, None) => {
                return Err(PrepareError::Refusal {
                    message: "provider prepared a resumed invocation without its retained session binding".into(),
                    cause: provenant_records::run::TerminalCause::HandoffRefused,
                    note: None,
                });
            }
            (false, _) => None,
        };
        apply_sandbox_mode(task, sandbox.clone(), &mut spec.env, &mut provider);
        // Publish the route/root binding only when this candidate is ready to
        // return as the applied launch. A refused fallback candidate must not
        // leave terminal settlement observing a different native account.
        if let Some(root) = native_root.as_deref()
            && let Err(error) = write_native_session_location(&paths.provider_state, root, &route)
        {
            reasons.push(setup_skip(&label, error));
            continue;
        }
        return Ok(run::PreparedLaunch {
            limits,
            continuation_expectation,
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
                cargo_home,
                cargo_target_seed: config.domains.fabric.lanes.cargo_seed.enabled,
                state_root: roots.state.as_path().to_path_buf(),
                sandbox,
                backend: None,
            }),
        });
    }
    attach_candidate_guard_facts(&mut reasons, &reason_boundaries, &reason_resources)?;
    let candidate_boundary = uniform_candidate_boundary(reasons.len(), &reason_boundaries);
    let candidate_resource = uniform_candidate_resource(reasons.len(), &reason_resources);
    Err(PrepareError::Candidates {
        fallback_outcome: match previous_outcome {
            provenant_routing::FallbackOutcome::AuthRequired => {
                Some(provenant_records::run::PreparationFailure::AuthRequired)
            }
            provenant_routing::FallbackOutcome::UsageLimited => {
                Some(provenant_records::run::PreparationFailure::UsageLimited)
            }
            provenant_routing::FallbackOutcome::AdapterUnavailable
            | provenant_routing::FallbackOutcome::ModelUnavailable => {
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
                PlanCheckError::Permission(error) => Some(error.into()),
                PlanCheckError::ReadOnly(_) | PlanCheckError::SourceHead(_) => None,
            })
            .collect(),
        candidate_boundary,
        candidate_resource,
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

fn origin_receipt(
    origin: Option<&provenant_config::Origin>,
) -> provenant_records::run::OriginReceipt {
    origin
        .map(provenant_config::Origin::receipt)
        .unwrap_or_default()
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
    if let provenant_records::RouteReadiness::Ready {
        availability_warning: Some(state),
        ..
    } = &route.readiness
    {
        let warning = format!(
            "account {} availability is {state}; the provider will check it",
            account_label
                .or(route.account_key.as_deref())
                .unwrap_or("unknown")
        );
        record.availability_warning = Some(warning);
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
            selection_evidence: None,
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
        let mut resolved = provenant_routing::resolve(&request, &context)?;
        resolved
            .diagnostics
            .launch_warnings
            .push("warning: fixture launch diagnostic".into());
        let requested = run::RequestedRoute::Named {
            extra: Default::default(),
            required_family: None,
            name: "fixture".into(),
            effort: None,
        };
        let record = launch_route_record(&resolved, Some("work"), &requested, false, false);
        let wire = serde_json::to_value(&record)?;
        assert_eq!(wire["model"], "codex/gpt-6.1-sol");
        assert_eq!(wire["effort"], "high");
        assert_eq!(wire["account_label"], "work");
        assert_eq!(wire["source"], "named_route");
        assert_eq!(
            wire["launch_warnings"][0],
            "warning: fixture launch diagnostic"
        );
        let mut uncertain = resolved.clone();
        uncertain.readiness = provenant_records::RouteReadiness::Ready {
            availability_warning: Some("Stale".into()),
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 99,
        };
        let warned = launch_route_record(&uncertain, Some("work"), &requested, false, false);
        assert!(
            warned
                .availability_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("work") && warning.contains("Stale"))
        );
        let pinned = run::RequestedRoute::Model {
            extra: Default::default(),
            required_family: None,
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
pub(super) fn omission_warning(
    entry: &provenant_providers::OmittedGrant,
    home: &Path,
    credential_roots: &provenant_core::CredentialRoots,
) -> String {
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
    let guard = if let Some(root) =
        credential_roots.refusal(&entry.path, provenant_core::CredentialScope::Subtree, true)
    {
        format!("credential root guard (source: {})", root.source)
    } else if !provenant_confinement::permissions::profile_path_representable(&entry.path) {
        "path cannot be represented in the sandbox profile".to_owned()
    } else {
        "protected root guard".to_owned()
    };
    format!("omitted discovered grant {display}: {guard}")
}

/// Warnings for omitted grants of one kind. Skill omissions belong under
/// `user_skills.warnings`; every other source is a generic launch-grant
/// warning recorded as `discovery_warnings` on the route record.
pub(super) fn discovery_warnings(
    omitted: &[provenant_providers::OmittedGrant],
    home: &Path,
    credential_roots: &provenant_core::CredentialRoots,
    skills: bool,
) -> Vec<String> {
    omitted
        .iter()
        .filter(|entry| (entry.source == provenant_providers::OmissionSource::UserSkill) == skills)
        .map(|entry| omission_warning(entry, home, credential_roots))
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
    source: provenant_records::run::UserSkillsSourceReceipt,
    staged: &provenant_providers::StagedUserSkills,
) -> provenant_records::run::UserSkillsReceipt {
    let warnings: Vec<_> = staged
        .warnings
        .iter()
        .map(|warning| provenant_records::run::UserSkillWarningReceipt {
            code: "user_skill_skipped".into(),
            reason: warning.reason.into(),
            path: warning.path.to_string_lossy().into_owned(),
            extra: Default::default(),
        })
        .collect();
    provenant_records::run::UserSkillsReceipt {
        mode,
        exposed: staged.root.is_some(),
        directory: staged.source.clone(),
        staged_file_count: staged.file_count,
        warnings,
        source,
        stage_max_bytes: None,
        extra: Default::default(),
    }
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
                "codex" | "cursor" | "opencode" => Some("login".into()),
                "agy" => Some("keychain".into()),
                _ => None,
            };
            let plan = ConfinedPlan::new(PlanRequest {
                scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
                permissions: PlanPermissions {
                    credential_roots: provenant_core::CredentialRoots::for_home(&home),
                    policy: None,
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
                selection_context: Default::default(),
                diagnostics: Default::default(),
                user_skills: true,
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
                    availability_warning: None,
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
                    let launch_policy = provenant_records::policy::Policy::default();
                    let input = PrepareInput {
                        credential_roots: provenant_core::CredentialRoots::for_home(&home),
                        route: &route,
                        provider_model: None,
                        policy: &launch_policy,
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
                    let receipt = serde_json::to_value(user_skills_exposure(
                        mode,
                        Default::default(),
                        &spec.user_skills,
                    ))?;
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
        let roots = provenant_core::CredentialRoots::for_home(Path::new("/fixture/home"));
        let warnings = discovery_warnings(&[entry], Path::new("/fixture/home"), &roots, false);
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
        let roots = provenant_core::CredentialRoots::for_home(Path::new("/fixture/home"));
        let warnings = discovery_warnings(&[entry], Path::new("/fixture/home"), &roots, false);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("cannot be represented"));
        let control = provenant_providers::OmittedGrant {
            path: PathBuf::from("/fixture/home/tool-\u{1}"),
            source: provenant_providers::OmissionSource::Toolchain,
        };
        let roots = provenant_core::CredentialRoots::for_home(Path::new("/fixture/home"));
        let warning = omission_warning(&control, Path::new("/fixture/home"), &roots);
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
        let record = serde_json::to_value(user_skills_exposure(
            provenant_records::UserSkillsMode::Discover,
            Default::default(),
            &staged,
        ))
        .unwrap();
        assert_eq!(record["exposed"], false);
        assert_eq!(record["warnings"][0]["reason"], "staging_failed");
    }

    #[test]
    fn user_skills_receipt_reports_each_skipped_reason() {
        let mut staged = provenant_providers::StagedUserSkills::default();
        staged.root = Some(PathBuf::from("/attempt/skills"));
        // Keep every staging eligibility outcome visible through the stable
        // user_skill_skipped receipt code, not just the common link cases.
        let skipped_reasons = [
            "credential_root",
            "cycle",
            "depth_limit",
            "entry_limit",
            "size_limit",
            "staging_failed",
            "unsupported_type",
            "changed_during_copy",
        ];
        for reason in skipped_reasons {
            staged.warnings.push(provenant_providers::SkillWarning {
                path: PathBuf::from(reason),
                reason,
            });
        }
        let receipt = serde_json::to_value(user_skills_exposure(
            provenant_records::UserSkillsMode::Discover,
            Default::default(),
            &staged,
        ))
        .unwrap();
        assert_eq!(receipt["exposed"], true);
        for (warning, reason) in receipt["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .zip(skipped_reasons)
        {
            assert_eq!(warning["code"], "user_skill_skipped");
            assert_eq!(warning["reason"], reason);
        }
    }

    #[test]
    fn receipt_uses_prepared_skill_facts() {
        let staged = provenant_providers::StagedUserSkills::default();
        let origin = provenant_records::run::UserSkillsSourceReceipt {
            key: Some("domains.lanes.user_skills".into()),
            origin: Some(provenant_records::run::OriginReceipt::Compiled {
                profile: None,
                extra: Default::default(),
            }),
            ..Default::default()
        };
        let record = serde_json::to_value(user_skills_exposure(
            provenant_records::UserSkillsMode::Discover,
            origin.clone(),
            &staged,
        ))
        .unwrap();
        assert_eq!(record["exposed"], false);
        assert_eq!(record["staged_file_count"], 0);
        assert_eq!(record["source"], serde_json::to_value(&origin).unwrap());
        let off = serde_json::to_value(user_skills_exposure(
            provenant_records::UserSkillsMode::Off,
            Default::default(),
            &staged,
        ))
        .unwrap();
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
            provenant_records::run::TerminalCause::RouteResolutionRefused
        );
        assert_eq!(
            refusal.terminal_note.as_deref(),
            Some("route resolver classification: unsupported_effort")
        );
        assert!(refusal.skipped_routes.is_empty());
        // Spec: docs/specs/retry-owner.md#admission-is-distinct-from-observation
        for (reason, expected) in [
            (
                provenant_records::RouteExclusionReason::AuthRequired,
                provenant_records::run::PreparationFailure::AuthRequired,
            ),
            (
                provenant_records::RouteExclusionReason::LoginExpired,
                provenant_records::run::PreparationFailure::AuthRequired,
            ),
            (
                provenant_records::RouteExclusionReason::UnknownAccount,
                provenant_records::run::PreparationFailure::AdapterUnavailable,
            ),
        ] {
            let error = crate::route::DispatchError::from(crate::route::EdgeError::Routing(
                provenant_routing::RoutingError::NoEligiblePoolMember {
                    exclusions: vec!["PROVIDER_SECRET_SENTINEL".into()],
                    cause: Some(reason),
                },
            ));
            assert_eq!(
                route_resolution_refusal(error)
                    .into_refusal()
                    .fallback_outcome,
                Some(expected)
            );
        }
        assert!(
            !refusal
                .terminal_note
                .unwrap()
                .contains("PROVIDER_SECRET_SENTINEL")
        );
    }
}

#[test]
fn oversized_argv_prompt_spills_to_private_attempt_state() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_records::diagnostic::SettingSource;
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let paths = run::LaunchPaths {
        attempt_id: provenant_core::AttemptId::from_bytes([1; 16]),
        operation_id: provenant_core::OperationId::parse("spill-limit")?,
        task_index: 0,
        lane_depth: 1,
        deadline_unix_ms: u64::MAX,
        scratch: root.join("scratch"),
        provider_state: root.join("provider-state"),
        temporary: root.join("tmp"),
        temporary_alias: None,
    };
    let policy = provenant_records::policy::Policy::default();
    let text = "PROMPT_SECRET_SENTINEL";
    let prompt = launch_prompt(
        text,
        &paths,
        &root,
        "cursor",
        1,
        SettingSource::Configuration,
        &policy,
    )?;
    let file = prompt
        .file()
        .ok_or("oversized argv prompt was not spilled")?;
    assert_eq!(std::fs::read_to_string(file)?, text);
    assert!(!prompt.text().contains(text));
    Ok(())
}

#[cfg(test)]
mod candidate_walk_regressions {
    use super::*;
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
        RouteSelectionReceipt, RouteSelectionRedraw,
    };

    fn route(model: &str, account: &str) -> ResolvedRoute {
        ResolvedRoute {
            selection_context: Default::default(),
            diagnostics: Default::default(),
            user_skills: true,
            extra: Default::default(),
            adapter: "cursor".into(),
            model: model.into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some(account.into()),
            host: "fixture".into(),
            alternatives: Default::default(),
            exclusions: vec![],
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::stale(),
            confidentiality: RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        }
    }

    #[test]
    fn pin_incompatible_candidate_does_not_hide_later_compatible_candidate()
    -> Result<(), PrepareError> {
        let mut queue = [route("wrong", "other"), route("right", "pinned")].into();
        let mut rejected = vec![];
        let mut reasons = vec![];
        let selected = next_permitted_candidate(
            &mut queue,
            &mut rejected,
            &mut reasons,
            (Some("pinned"), Some("cursor")),
            true,
            provenant_routing::FallbackOutcome::Other,
        )?
        .expect("compatible route");
        assert_eq!(selected.model, "right");
        assert_eq!(reasons[0].route, "cursor/wrong");
        assert_eq!(
            reasons[0].cause,
            Some(provenant_records::RouteExclusionReason::AccountPinned)
        );
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn refused_accounts_leave_only_untried_accounts() {
        let record = |key: &str| provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "claude".into(),
            executable: "/fixture/claude".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "fixture".into(),
            catalogue_revision: "fixture".into(),
            account_key: Some(key.into()),
            plan: None,
            observed_models: vec![],
            observed_unix_seconds: 0,
            expires_unix_seconds: 1,
        };
        let all = [record("spent"), record("ready")];
        let mut refused = std::collections::BTreeSet::new();
        refused.insert(("claude".to_owned(), "spent".to_owned()));
        let left = without_refused_accounts(&all, &refused);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].account_key.as_deref(), Some("ready"));
        refused.insert(("claude".to_owned(), "ready".to_owned()));
        assert!(without_refused_accounts(&all, &refused).is_empty());
    }

    #[test]
    fn redraw_receipt_and_admission_error_survive_candidate_walk() -> Result<(), PrepareError> {
        let mut stale = route("stale", "pinned");
        stale.pool_selection = Some(RouteSelectionReceipt {
            selection_rule: provenant_records::route_selection::SelectionRule::WeightedDraw,
            considered_exclusions: Vec::new(),
            exploration: provenant_records::route_selection::Exploration {
                extra: Default::default(),
                selected: false,
                percent: 0,
                enabled: false,
                source: "fixture".into(),
            },
            weight_source: provenant_records::route_selection::WeightSource::Catalogue,
            account_load: None,
            extra: Default::default(),
            pool: "fixture".into(),
            candidates: vec![],
            roll: 0,
            total_weight: 1,
            pick: "cursor/stale".into(),
            reason: None,
            redraws: vec![RouteSelectionRedraw {
                cause: Some(provenant_records::RouteExclusionReason::Exhausted),
                extra: Default::default(),
                rejected_pick: "cursor/spent".into(),
                roll: 0,
                total_weight: 1,
                reason: "private detail".into(),
            }],
        });
        let mut reasons = vec![];
        let mut rejected = vec![];
        let next = redraw_candidates(Ok(stale), &mut reasons, &mut rejected)?;
        assert_eq!(next[0].model, "stale");
        assert!(matches!(
            next[0].readiness,
            RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::StaleAvailability,
                ..
            }
        ));
        assert_eq!(reasons[0].route, "cursor/spent");
        assert_eq!(
            reasons[0].cause,
            Some(provenant_records::RouteExclusionReason::Exhausted)
        );
        assert!(!reasons[0].detail.contains("private detail"));
        let error = crate::route::DispatchError::from(crate::route::EdgeError::Quota(
            provenant_quota::QuotaError::AllowanceExhausted,
        ));
        let refusal = redraw_candidates(Err(error), &mut reasons, &mut rejected).unwrap_err();
        assert!(
            matches!(refusal, PrepareError::RouteResolution { error: crate::route::DispatchError::Edge(error), checks } if matches!(*error, crate::route::EdgeError::Quota(provenant_quota::QuotaError::AllowanceExhausted)) && checks.iter().any(|check| check.route == "cursor/spent" && check.cause == Some(provenant_records::RouteExclusionReason::Exhausted)))
        );
        Ok(())
    }
}

#[cfg(test)]
mod route_launch_boundary_regressions {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn agy_entry(
        label: &str,
        key: &str,
        directory: &Path,
    ) -> provenant_providers::accounts::Registration {
        provenant_providers::accounts::Registration {
            adapter: "agy".into(),
            label: label.into(),
            key: key.into(),
            org_fingerprint: None,
            directory: directory.into(),
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
        }
    }

    // Spec: docs/specs/routing.md#accounts-and-credentials
    #[test]
    fn agy_local_login_routes_beside_registered_accounts_and_is_not_a_root() -> Result<(), String> {
        let spec = AdapterSpec::get("agy").ok_or("agy adapter missing")?;
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let home = std::fs::canonicalize(temp.path()).map_err(|error| error.to_string())?;
        let registered = agy_entry("work", "agy-registered", &home.join(".agy-work"));
        assert!(include_local_login(
            spec,
            std::slice::from_ref(&registered),
            None,
            Some("agy"),
            None
        ));
        assert!(include_local_login(
            spec,
            std::slice::from_ref(&registered),
            Some(spec.local_account.1),
            Some("agy"),
            None
        ));
        assert!(include_local_login(
            spec,
            std::slice::from_ref(&registered),
            Some(spec.local_account.0),
            Some("agy"),
            None
        ));
        assert!(!include_local_login(
            spec,
            std::slice::from_ref(&registered),
            Some(spec.local_account.1),
            Some("cursor"),
            None
        ));
        let cursor = AdapterSpec::get("cursor").ok_or("cursor adapter missing")?;
        assert!(include_local_login(
            cursor,
            &[],
            Some(cursor.local_account.1),
            Some("cursor"),
            None
        ));
        assert!(!include_local_login(
            cursor,
            &[],
            Some(cursor.local_account.1),
            Some("agy"),
            None
        ));
        let resumed_local =
            effective_account_selector(Some("agy-registered"), Some(spec.local_account.1), true);
        assert_eq!(resumed_local, Some(spec.local_account.1));
        assert!(include_local_login(
            spec,
            std::slice::from_ref(&registered),
            resumed_local,
            Some("agy"),
            Some(RetainedIdentity {
                provider: "agy",
                account_key: Some(spec.local_account.1),
            })
        ));
        assert!(!include_local_login(
            spec,
            &[],
            None,
            Some("agy"),
            Some(RetainedIdentity {
                provider: "agy",
                account_key: None,
            })
        ));
        assert_eq!(
            effective_account_selector(Some("agy-registered"), Some(spec.local_account.1), false),
            Some("agy-registered")
        );
        assert_eq!(
            effective_account_selector(Some("agy-registered"), None, true),
            None
        );

        let mut settings = provenant_providers::accounts::Settings::default();
        settings.entries.insert("work".into(), registered.clone());
        let roots = credential_roots(&Registry::default(), &home, &settings, &[])?;
        // HOME-rooted accounts protect client state and declared credentials,
        // while ordinary HOME children remain available to independent lanes.
        for relative in [
            ".gemini/unlisted",
            "state.vscdb",
            "state.vscdb-wal",
            "Library/Keychains/login.keychain-db",
        ] {
            assert!(roots.refuses(
                &registered.directory.join(relative),
                provenant_core::CredentialScope::Exact,
                true
            ));
        }
        assert!(!roots.refuses(
            &registered.directory.join("workspace"),
            provenant_core::CredentialScope::Subtree,
            true,
        ));
        assert!(
            credential_roots(
                &Registry::default(),
                &home,
                &settings,
                &[&registered.directory.join("workspace")],
            )
            .is_ok()
        );
        assert!(roots.entries().iter().any(|root| {
            root.source.starts_with("account registry: agy/work")
                && root.path == registered.directory.join(".gemini")
                && root.scope == provenant_core::CredentialScope::Subtree
        }));
        assert!(
            !roots
                .entries()
                .iter()
                .any(|root| root.source.starts_with("account registry: agy/default"))
        );

        let mut candidates = vec![registered.clone()];
        candidates.push(agy_entry(spec.local_account.0, spec.local_account.1, &home));
        assert_eq!(
            select_account(&candidates, spec.local_account.1, Some("agy"))
                .map_err(|error| error.to_string())?
                .key,
            spec.local_account.1
        );
        assert_eq!(
            select_account(&candidates, spec.local_account.0, Some("agy"))
                .map_err(|error| error.to_string())?
                .key,
            spec.local_account.1
        );
        assert_eq!(
            select_account(&candidates, "agy-registered", Some("agy"))
                .map_err(|error| error.to_string())?
                .key,
            "agy-registered"
        );

        let labelled_collision = agy_entry("local", "agy-other", &home.join(".agy-other"));
        let mut colliding_candidates = vec![labelled_collision];
        colliding_candidates.push(agy_entry(spec.local_account.0, spec.local_account.1, &home));
        assert_eq!(
            select_account(&colliding_candidates, spec.local_account.1, Some("agy"))
                .map_err(|error| error.to_string())?
                .key,
            spec.local_account.1
        );
        Ok(())
    }

    // Spec: docs/specs/routing.md#accounts-and-credentials
    #[test]
    fn continuation_account_selection_uses_the_retained_provider_key_pair() {
        let home = PathBuf::from("/fixture/home");
        let agy_local = agy_entry("default", "local", &home);
        let mut cursor_local = agy_entry("default", "local", &home);
        cursor_local.adapter = "cursor".into();
        let entries = [cursor_local, agy_local.clone()];
        let selected = select_retained_account(
            &entries,
            RetainedIdentity {
                provider: "agy",
                account_key: Some("local"),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.adapter, "agy");

        let no_account = select_retained_account(
            &entries,
            RetainedIdentity {
                provider: "agy",
                account_key: None,
            },
        )
        .unwrap();
        assert!(no_account.is_none());

        let mut cursor_only = agy_entry("local", "cursor-only", &home);
        cursor_only.adapter = "cursor".into();
        let mismatch_entries = [agy_local.clone(), cursor_only];
        let mismatch = select_retained_account(
            &mismatch_entries,
            RetainedIdentity {
                provider: "agy",
                account_key: Some("cursor-only"),
            },
        );
        assert!(matches!(
            mismatch,
            Err(PrepareError::Refusal {
                cause: provenant_records::run::TerminalCause::UnregisteredAccount,
                ..
            })
        ));

        let mut same_provider_label = agy_entry("cursor-only", "agy-other-key", &home);
        same_provider_label.label = "cursor-only".into();
        let mismatch_entries = [agy_local, same_provider_label];
        let mismatch = select_retained_account(
            &mismatch_entries,
            RetainedIdentity {
                provider: "agy",
                account_key: Some("cursor-only"),
            },
        );
        assert!(matches!(
            mismatch,
            Err(PrepareError::Refusal {
                cause: provenant_records::run::TerminalCause::UnregisteredAccount,
                ..
            })
        ));
    }

    // Spec: docs/specs/supervisor.md#native-environment-and-sandbox-tool-stores
    #[test]
    fn requester_path_filter_and_named_lookup_share_one_owner() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = std::fs::canonicalize(temp.path()).map_err(|error| error.to_string())?;
        let project = root.join("project");
        let worktree = project.join(".worktrees/selected");
        let bin = root.join("requester-bin");
        std::fs::create_dir_all(&worktree).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&bin).map_err(|error| error.to_string())?;
        let executable = bin.join("fixture-command");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").map_err(|error| error.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let mut host = run::HostLaunchContext::from_environment(
            &root.join("state"),
            &project,
            "/usr/bin/git".into(),
        );
        let supplied =
            std::env::join_paths([bin.as_os_str(), project.as_os_str(), worktree.as_os_str()])
                .map_err(|error| error.to_string())?;
        host.lane_path = Some(supplied.as_os_str().as_bytes().to_vec());

        let filtered = run::FilteredLanePath::for_requester(Some(&host), &project, &worktree)
            .map_err(|error| error.to_string())?;
        let directories = std::env::split_paths(filtered.path()).collect::<Vec<_>>();
        assert_eq!(directories, [bin]);
        assert_eq!(filtered.resolve("fixture-command")?, executable);
        Ok(())
    }
}

#[test]
// Spec: docs/specs/state.md#roots-and-identities
fn binding_refusal_preserves_local_repair_and_closed_route_guard() {
    let error = ReadOnlyPlanError::from(
        provenant_config::ConfigError::ProjectBindingDirectoryUnsafe {
            path: std::path::PathBuf::from("/private/test-binding-state"),
            mode: 0o755,
            owner_matches: true,
        },
    );
    let refusal = PlanCheckError::ReadOnly(error).into_refusal();
    assert!(refusal.message.contains("/private/test-binding-state"));
    assert!(
        refusal
            .terminal_note
            .as_deref()
            .is_some_and(|note| note.contains("chmod 700"))
    );
    assert_eq!(
        refusal.boundary_class,
        provenant_records::diagnostic::BoundaryClass::Authority
    );
    assert_eq!(
        refusal.policy_code.as_deref(),
        Some("CONFINEMENT_READ_ONLY_PLAN_AUTHORITY")
    );
    assert_eq!(refusal.skipped_routes.len(), 1);
    assert!(
        !refusal.skipped_routes[0]
            .detail
            .contains("/private/test-binding-state")
    );
    assert_eq!(
        refusal.skipped_routes[0].confinement_guard,
        Some(provenant_records::diagnostic::ConfinementGuard::ProjectBindingDirectoryUnsafe)
    );
    assert_eq!(
        refusal.terminal_cause,
        provenant_records::run::TerminalCause::ProjectBindingDirectoryUnsafe
    );
}
