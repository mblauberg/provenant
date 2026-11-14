use super::*;

/// Host context for fresh account identity and version probes.
pub(super) struct Probe<'a> {
    pub(super) roots: Option<&'a provenant_core::Roots>,
    pub(super) executor: &'a provenant_providers::host::HostProbeExecutor<'a>,
    pub(super) home: &'a Path,
    pub(super) user: &'a std::ffi::OsStr,
    pub(super) scratch: &'a Path,
}

/// Each explicit fallback candidate has its own private state, and only the
/// launched one selects Codex's rollout home. Exactly one distinct home may match.
pub(crate) fn observe_codex_rollout(
    provider_state: &Path,
    thread: &str,
) -> Result<provenant_providers::codex_rollout::Turn, provenant_providers::ProviderError> {
    use provenant_providers::ProviderError;
    let entries = std::fs::read_dir(provider_state).map_err(|_| ProviderError::UnknownIdentity)?;
    let mut observed = Err(ProviderError::UnknownIdentity);
    let mut homes = std::collections::BTreeSet::new();
    // Each candidate may also have one host-owned native-home selector.
    for entry in entries.take(128) {
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
        let candidate = provider_state.join(&name);
        let directory =
            Directory::open_anchor(provider_state).map_err(|_| ProviderError::UnknownIdentity)?;
        let home = match directory.read_stable(
            std::ffi::OsStr::new(&format!(
                "codex-observation-{}.json",
                name.to_string_lossy()
            )),
            16 * 1024,
        ) {
            Ok(bytes) => {
                #[derive(serde::Deserialize)]
                struct NativeHome {
                    schema_version: u64,
                    home: provenant_records::StructuredPathWire,
                }
                let record: NativeHome =
                    serde_json::from_slice(&bytes).map_err(|_| ProviderError::UnknownIdentity)?;
                if record.schema_version != 1 {
                    return Err(ProviderError::UnknownIdentity);
                }
                let home = record
                    .home
                    .to_path_buf()
                    .map_err(|_| ProviderError::UnknownIdentity)?;
                if !home.is_absolute() {
                    return Err(ProviderError::UnknownIdentity);
                }
                home
            }
            Err(error) if error.is_not_found() => candidate,
            Err(_) => return Err(ProviderError::UnknownIdentity),
        };
        // Native fallback candidates may select the same account home.
        if !homes.insert(home.clone()) {
            continue;
        }
        match provenant_providers::codex_rollout::observe(&home, thread) {
            Ok(_) if observed.is_ok() => return Err(ProviderError::IdentityMismatch),
            Ok(turn) => observed = Ok(turn),
            Err(ProviderError::UnknownIdentity) => {}
            Err(error) => return Err(error),
        }
    }
    observed
}

/// Find the one private OpenCode database that identifies this session.
pub(crate) fn observe_opencode_session(
    provider_state: &Path,
    session: &str,
) -> Result<(PathBuf, String), provenant_providers::ProviderError> {
    let candidate = find_opencode_session(provider_state, session)?;
    let model =
        provenant_providers::opencode::observe_model(&candidate.join("data/opencode"), session)?;
    Ok((candidate, model))
}

/// Locate by session identity independently of answering-model telemetry.
pub(crate) fn find_opencode_session(
    provider_state: &Path,
    session: &str,
) -> Result<PathBuf, provenant_providers::ProviderError> {
    use provenant_providers::ProviderError;
    let entries = std::fs::read_dir(provider_state).map_err(|_| ProviderError::UnknownIdentity)?;
    let mut found = None;
    for entry in entries.take(64) {
        let entry = entry.map_err(|_| ProviderError::UnknownIdentity)?;
        let name = entry.file_name();
        let valid = name
            .to_str()
            .and_then(|n| n.strip_prefix("candidate-"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !valid || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let candidate = entry.path();
        let data = candidate.join("data/opencode");
        if ![
            candidate.as_path(),
            candidate.join("data").as_path(),
            data.as_path(),
            data.join("opencode.db").as_path(),
        ]
        .iter()
        .all(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.file_type().is_symlink()))
        {
            continue;
        }
        match provenant_providers::opencode::session_exists(&data, session) {
            Ok(()) if found.is_none() => found = Some(candidate),
            Ok(()) => return Err(ProviderError::IdentityMismatch),
            Err(ProviderError::UnknownIdentity) => {}
            Err(error) => return Err(error),
        }
    }
    found.ok_or(ProviderError::UnknownIdentity)
}

pub(super) fn detect_failure(
    entry: &provenant_providers::accounts::Registration,
    error: &provenant_providers::ProviderError,
) -> String {
    let mut message = format!("{} account {}: {error}", entry.adapter, entry.label);
    if !matches!(error, provenant_providers::ProviderError::ProbeDirectory(_)) {
        message.push_str("; re-register the signed-in identity");
    }
    message
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AccountObservationError {
    #[error("{0}")]
    AuthRequired(String),
    #[error("{0}")]
    VersionUnavailable(String),
    #[error("{0}")]
    Other(String),
}
impl From<String> for AccountObservationError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}
impl From<&str> for AccountObservationError {
    fn from(message: &str) -> Self {
        Self::Other(message.into())
    }
}

impl AccountObservationError {
    pub(super) fn skip_kind(&self) -> SkipKind {
        match self {
            Self::AuthRequired(_) => SkipKind::AuthRequired,
            Self::VersionUnavailable(_) => SkipKind::Unavailable,
            Self::Other(_) => SkipKind::Account,
        }
    }
}

pub(super) fn default_directory_error(
    spec: &AdapterSpec,
    path: &Path,
    detail: String,
) -> AccountObservationError {
    if std::fs::symlink_metadata(path)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        AccountObservationError::AuthRequired(format!(
            "default {} login is missing; run `{}`",
            spec.name, spec.login_hint
        ))
    } else {
        AccountObservationError::Other(detail)
    }
}

pub(super) fn account_observation_error(
    entry: &provenant_providers::accounts::Registration,
    error: &provenant_providers::ProviderError,
) -> AccountObservationError {
    use provenant_providers::ProviderError;
    if matches!(error, ProviderError::RefreshDeferred) {
        return AccountObservationError::Other("provider CLI deferred token refresh".into());
    }
    if matches!(
        error,
        ProviderError::LoginRequired
            | ProviderError::CredentialUnavailable
            | ProviderError::CredentialNotFound
            | ProviderError::CredentialExpired
    ) {
        let repair = if matches!(error, ProviderError::CredentialExpired) {
            crate::quota::login_expired_repair(Some(entry))
        } else {
            crate::quota::login_repair(Some(entry))
        };
        AccountObservationError::AuthRequired(format!(
            "{} account {} needs sign-in; {repair}",
            entry.adapter, entry.label,
        ))
    } else {
        AccountObservationError::Other(detect_failure(entry, error))
    }
}

/// An absent availability row retains the login failure of every eligible account.
pub(super) fn unavailable_auth_detail(
    route: &ResolvedRoute,
    selected: Option<&str>,
    entries: &[provenant_providers::accounts::Registration],
    failures: &std::collections::BTreeMap<(String, String), String>,
) -> Option<String> {
    let relevant: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry.adapter == route.adapter
                && selected
                    .or(route.account_key.as_deref())
                    .is_none_or(|key| key == entry.key)
                && AdapterSpec::get(&entry.adapter).is_none_or(|spec| {
                    spec.account_serves_model(entry.plan.as_deref(), &route.model)
                })
        })
        .collect();
    if relevant.is_empty() {
        return None;
    }
    relevant
        .into_iter()
        .map(|entry| {
            failures
                .get(&(entry.adapter.clone(), entry.key.clone()))
                .cloned()
        })
        .collect::<Option<Vec<_>>>()
        .map(|details| details.join("; "))
}

/// A bounded login check in the selected Codex home. Provider output is discarded.
pub(super) fn codex_login_is_healthy(
    entry: &provenant_providers::accounts::Registration,
    executable: &Path,
    probe: &Probe<'_>,
) -> bool {
    matches!(
        provenant_providers::host::local_signin(
            provenant_records::LoginReprobe::Codex,
            probe.executor,
            executable,
            probe.home,
            &entry.directory,
            probe.scratch,
        ),
        Some(provenant_providers::host::LocalSignin::SignedIn)
    )
}

/// Cursor's status command uses the host login; never substitute it for a
/// registration pointing at another directory.
pub(super) fn cursor_login_is_healthy(
    entry: &provenant_providers::accounts::Registration,
    executable: &Path,
    probe: &Probe<'_>,
) -> bool {
    matches!(
        provenant_providers::host::local_signin(
            provenant_records::LoginReprobe::Cursor,
            probe.executor,
            executable,
            probe.home,
            &entry.directory,
            probe.scratch,
        ),
        Some(provenant_providers::host::LocalSignin::SignedIn)
    )
}

pub(super) fn observe_account(
    entry: &provenant_providers::accounts::Registration,
    executable: &Path,
    probe: &Probe<'_>,
    catalogue_revision: &str,
    timestamp: i64,
) -> Result<provenant_providers::AvailabilityRecord, AccountObservationError> {
    let Probe {
        roots,
        executor,
        home,
        user,
        scratch,
    } = *probe;
    let directory = canonical(&entry.directory)?;
    if directory != entry.directory
        || AdapterSpec::get(&entry.adapter)
            .and_then(|spec| spec.account_directory_prefix)
            .is_some_and(|prefix| directory != home.join(format!("{prefix}{}", entry.label)))
    {
        return Err(format!(
            "{} account {} has no fixed canonical identity directory",
            entry.adapter, entry.label
        )
        .into());
    }
    // Adapters without registered accounts, and an unregistered local login
    // (agy's synthetic `local` entry), have no registered identity to probe.
    let (detected, org) = if AdapterSpec::get(&entry.adapter)
        .is_none_or(|spec| !spec.registered_account || spec.is_unregistered_local(&entry.key))
    {
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
        .map_err(|error| account_observation_error(entry, &error))?
    };
    if detected != entry.key {
        return Err(format!(
            "{} account {} changed identity; re-register it",
            entry.adapter, entry.label
        )
        .into());
    }
    match entry.check_org(org.as_deref()) {
        Err(_) => {
            return Err(format!(
                "{} account {} changed identity; re-register it",
                entry.adapter, entry.label
            )
            .into());
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
            "provider version is unavailable (termination={:?}, timed_out={}, cancelled={}, drain_truncated={}, overflow_stopped={}, stdout_truncated={}, stdout_read_failed={})",
            out.termination,
            out.timed_out,
            out.cancelled,
            out.drain_truncated,
            out.overflow_stopped,
            out.stdout.truncated,
            out.stdout.read_failed
        )
    };
    if out.termination != provenant_os::exec::Termination::Exit(0)
        || out.timed_out
        || out.cancelled
        || out.drain_truncated
        || out.overflow_stopped
        || out.stdout.truncated
        || out.stdout.read_failed
    {
        return Err(AccountObservationError::VersionUnavailable(unavailable()));
    }
    let version = std::str::from_utf8(&out.stdout.bytes)
        .map_err(|_| "invalid provider version")?
        .trim()
        .to_owned();
    if version.is_empty() {
        return Err(AccountObservationError::VersionUnavailable(format!(
            "{}: empty version output",
            unavailable()
        )));
    }
    // An adapter flagged `offered_models` reads the offer list its usage probe cached.
    let observed_models =
        if AdapterSpec::get(&entry.adapter).is_some_and(|spec| spec.offered_models) {
            provenant_providers::agy::cached_models(executor.root(), &directory)
        } else {
            Vec::new()
        };
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
        plan: entry.plan.clone(),
        observed_models,
        observed_unix_seconds: timestamp,
        expires_unix_seconds: timestamp.saturating_add(300),
    })
}

pub(super) fn candidates(resolved: ResolvedRoute) -> Vec<ResolvedRoute> {
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

pub(super) fn with_alternative(
    base: &ResolvedRoute,
    alternative: &RouteAlternative,
) -> ResolvedRoute {
    ResolvedRoute {
        extra: alternative.extra.clone(),
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

pub(super) fn adapter_route(route: &ResolvedRoute) -> ResolvedRoute {
    let mut provider_route = route.clone();
    provider_route.alternatives = Default::default();
    provider_route
}

/// Record this dispatch's credential authority durably beside the attempt
/// before any refresh may act on it.
pub(super) fn credential_authority(
    host_root: &Path,
    paths: &run::LaunchPaths,
    login: &provenant_providers::claude::Login<'_>,
) -> Result<provenant_providers::claude::CredentialAuthority, String> {
    let authority = provenant_providers::claude::CredentialAuthority::for_dispatch(
        paths.attempt_id.to_string(),
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

#[derive(Debug, thiserror::Error)]
pub(super) enum LaunchPromptError {
    #[error("invalid input: recorded prompt exceeds 1 MiB; shorten the prompt")]
    TooLarge,
    #[error(transparent)]
    Provider(#[from] provenant_providers::ProviderError),
    #[error("{0}")]
    Other(String),
}
impl LaunchPromptError {
    pub(super) fn resource_fact(&self) -> Option<provenant_records::diagnostic::ResourceFact> {
        match self {
            Self::Provider(error) => error.resource_fact(),
            _ => None,
        }
    }
}
impl From<String> for LaunchPromptError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}
impl From<&str> for LaunchPromptError {
    fn from(message: &str) -> Self {
        Self::Other(message.into())
    }
}

pub(super) fn check_launch_prompt(
    text: &str,
    paths: &run::LaunchPaths,
    adapter: Option<&str>,
    argv_limit: usize,
    argv_origin: provenant_records::diagnostic::SettingSource,
    policy: &provenant_records::policy::Policy,
) -> Result<(), LaunchPromptError> {
    if paths.lane_depth > 1
        && adapter
            .and_then(AdapterSpec::get)
            .is_some_and(|spec| spec.argv_prompt)
    {
        Prompt::new(text.to_owned())
            .with_argv_limit(argv_limit, argv_origin)
            .check_argv(policy)?;
    }
    Ok(())
}

pub(super) fn launch_prompt(
    text: &str,
    paths: &run::LaunchPaths,
    state_root: &Path,
    adapter: &str,
    argv_limit: usize,
    argv_origin: provenant_records::diagnostic::SettingSource,
    policy: &provenant_records::policy::Policy,
) -> Result<Prompt, LaunchPromptError> {
    if text.len() as u64 > super::PROMPT_LIMIT {
        // Admission bounds the caller's raw prompt, not host additions.
        if let provenant_records::policy::Effect::Warn(message) = policy.check(
            "OBSERVATION_LAUNCH_PROMPT_NONE",
            provenant_records::policy::Facts::violation(
                "host-added launch prompt text exceeds 1 MiB",
            ),
        ) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
        }
    }
    check_launch_prompt(text, paths, Some(adapter), argv_limit, argv_origin, policy)?;
    let prompt = Prompt::new(text.to_owned()).with_argv_limit(argv_limit, argv_origin);
    if !AdapterSpec::get(adapter).is_some_and(|spec| spec.argv_prompt)
        || !prompt.argv_limit_exceeded()
        || paths.lane_depth > 1
    {
        // Nested lanes cannot publish into the parent-writable attempt. Their
        // policy check above decides whether an oversized inline argument may
        // proceed; top-level launches spill whenever the argument exceeds its
        // configured byte threshold.
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
    let prompt = Prompt::from_file(attempt.join(name)).with_argv_limit(argv_limit, argv_origin);
    // Check the small file-reference argument before publishing the prompt so
    // a strict, unusually small limit cannot leave an orphaned artifact.
    prompt.check_argv(policy)?;
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
        Err(error) => return Err(error.to_string().into()),
    }
    Ok(prompt)
}
