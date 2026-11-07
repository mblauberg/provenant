use super::*;
use provenant_records::diagnostic::ContinuationCopyLimit;

#[derive(Debug, thiserror::Error)]
pub(super) enum ContinuationRequestError {
    #[error(
        "the prior attempt recorded no applied route; run provenant run with its result as context"
    )]
    MissingRoute,
    #[error(
        "the prior attempt recorded no applied model; run provenant run with its result as context"
    )]
    MissingModel,
    #[error("{0}; run provenant run with the prior result as context")]
    InvalidModel(String),
    #[error(
        "the prior attempt's applied provider and model differ; run provenant run with its result as context"
    )]
    ProviderMismatch,
}

/// Recheck current readiness for the exact route the provider session used.
/// Resolving an exact model bypasses named-pool selection and quota redraws.
pub(super) fn continuation_request(
    prior: &run::AttemptRecord,
) -> Result<provenant_routing::RouteRequest, ContinuationRequestError> {
    use provenant_routing::{Fallback, ModelId, RouteChoice};

    let applied = prior
        .applied_route
        .as_ref()
        .ok_or(ContinuationRequestError::MissingRoute)?;
    let model = applied
        .model
        .as_deref()
        .ok_or(ContinuationRequestError::MissingModel)?;
    let model = ModelId::parse(model)
        .map_err(|error| ContinuationRequestError::InvalidModel(error.to_string()))?;
    if applied.provider.as_deref() != Some(model.adapter()) {
        return Err(ContinuationRequestError::ProviderMismatch);
    }
    Ok(provenant_routing::RouteRequest {
        intent: Default::default(),
        choice: RouteChoice::Model(model),
        effort: applied.effort.clone(),
        fallback: Fallback::None,
        confidential: false,
    })
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContinuationError {
    #[error("route differs from the prior attempt's provider, model or account")]
    RouteMismatch,
    #[error("the prior attempt recorded no provider session")]
    MissingSession,
    #[error("the prior attempt recorded no applied route")]
    MissingRoute,
    #[error(transparent)]
    Provider(#[from] provenant_providers::ProviderError),
    #[error(transparent)]
    StateCopy(#[from] StateCopyError),
    #[error("{0}")]
    Other(String),
}

impl ContinuationError {
    pub(crate) fn resource_fact(&self) -> Option<provenant_records::diagnostic::ResourceFact> {
        match self {
            Self::StateCopy(error) => error.resource,
            _ => None,
        }
    }

    pub(super) fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass;
        match self {
            Self::RouteMismatch | Self::MissingSession | Self::MissingRoute => {
                BoundaryClass::Authority
            }
            Self::Provider(error) => error.boundary_class(),
            Self::StateCopy(error) => error.class,
            Self::Other(_) => BoundaryClass::None,
        }
    }
}

#[test]
fn continuation_route_mismatch_is_a_typed_authority_boundary() {
    use provenant_records::diagnostic::BoundaryClass;
    assert_eq!(
        ContinuationError::RouteMismatch.boundary_class(),
        BoundaryClass::Authority
    );
    for error in [
        ContinuationError::MissingSession,
        ContinuationError::MissingRoute,
        ContinuationError::Provider(provenant_providers::ProviderError::IdentityMismatch),
        ContinuationError::Provider(provenant_providers::ProviderError::UnknownIdentity),
    ] {
        assert_eq!(error.boundary_class(), BoundaryClass::Authority);
    }
    assert_eq!(
        ContinuationError::Other("unavailable".into()).boundary_class(),
        BoundaryClass::None
    );
}

impl From<String> for ContinuationError {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

impl From<&str> for ContinuationError {
    fn from(value: &str) -> Self {
        Self::Other(value.into())
    }
}

/// The continuation of `prior` on this candidate route. It must be the very
/// provider, model and account the prior attempt ran with. The prior attempt's
/// retained private state is copied into the new attempt's own, so the new
/// attempt never writes into settled history.
pub(super) fn continuation_for(
    prior: &run::AttemptRecord,
    route: &ResolvedRoute,
    grant_digest: &str,
    state_root: &Path,
    config_root: &Path,
    private_state: &Path,
) -> Result<provenant_providers::Continuation, ContinuationError> {
    let session = prior
        .session
        .as_deref()
        .ok_or(ContinuationError::MissingSession)?;
    let applied = prior
        .applied_route
        .as_ref()
        .ok_or(ContinuationError::MissingRoute)?;
    if applied.provider.as_deref() != Some(route.adapter.as_str())
        || applied.model.as_deref() != Some(format!("{}/{}", route.adapter, route.model).as_str())
        || applied.account_key != route.account_key
    {
        return Err(ContinuationError::RouteMismatch);
    }
    let continuation =
        provenant_providers::Continuation::retained(&route.adapter, session, route, grant_digest)?;
    let prior_state = state_root
        .join("runs")
        .join(prior.run_id.to_string())
        .join("attempts")
        .join(prior.attempt_id.to_string())
        .join("provider-state");
    let policy = StateCopyPolicy::read(config_root);
    let located = if route.adapter == "opencode" {
        super::observation::find_opencode_session(&prior_state, session).ok()
    } else {
        find_session_state_within(&prior_state, session, policy.limits)
            .map_err(|exhausted| StateCopyError::resource(policy.fact(exhausted.limit(), None)))?
    };
    match located {
        // Codex's SQLite index holds absolute rollout paths. Rebuild it from
        // the copied sessions instead of resolving back into settled history.
        Some(retained) => copy_state_classified(
            &retained,
            private_state,
            policy,
            AdapterSpec::get(&route.adapter).is_some_and(|spec| spec.copy_sessions_only),
        )?,
        None => return Err(missing_continuation(&route.adapter)),
    }
    Ok(continuation)
}

fn missing_continuation(provider: &str) -> ContinuationError {
    if matches!(provider, "cursor" | "opencode" | "claude") {
        format!("retained provider continuation is missing or expired; {provider} sessions now require attempt-private state, so host-backed sessions from before this release cannot resume; start a new lane").into()
    } else {
        "retained provider continuation is missing or expired; start a new lane".into()
    }
}

/// Configurable budgets shared by retained-state search and copy. A leftover
/// process from the prior attempt may still grow the tree. These budgets bound
/// host work without changing the anchored filesystem or hard-link guards.
#[derive(Clone, Copy)]
pub(super) struct StateLimits {
    pub(super) depth: u32,
    pub(super) entries: u32,
    pub(super) bytes: u64,
}
pub(super) const RETAINED_STATE: StateLimits = StateLimits {
    depth: 32,
    entries: 100_000,
    bytes: 4 << 30,
};

#[derive(Clone, Copy)]
struct StateCopyPolicy {
    limits: StateLimits,
    origins: [provenant_records::diagnostic::SettingSource; 3],
}
impl StateCopyPolicy {
    fn shipped(limits: StateLimits) -> Self {
        Self {
            limits,
            origins: [provenant_records::diagnostic::SettingSource::ShippedDefault; 3],
        }
    }
    fn read(state_root: &Path) -> Self {
        use provenant_records::diagnostic::SettingSource;
        match provenant_confinement::run_config::RunConfig::read_with_keys(state_root) {
            Ok((config, keys)) => {
                let origins = [
                    "continuation_copy_limit_bytes",
                    "continuation_copy_entries",
                    "continuation_copy_depth",
                ]
                .map(|key| {
                    if keys.contains(key) {
                        SettingSource::Configuration
                    } else {
                        SettingSource::ShippedDefault
                    }
                });
                Self {
                    limits: StateLimits {
                        bytes: config.continuation_copy_limit_bytes,
                        entries: config.continuation_copy_entries,
                        depth: config.continuation_copy_depth,
                    },
                    origins,
                }
            }
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: continuation copy settings unavailable ({error}); using compiled defaults"
                );
                Self::shipped(RETAINED_STATE)
            }
        }
    }
    fn fact(
        self,
        setting: ContinuationCopyLimit,
        available_bytes: Option<u64>,
    ) -> provenant_records::diagnostic::ResourceFact {
        use provenant_records::diagnostic::ResourceFact;
        let (limit, origin) = match setting {
            ContinuationCopyLimit::Bytes => (self.limits.bytes, self.origins[0]),
            ContinuationCopyLimit::Entries => (u64::from(self.limits.entries), self.origins[1]),
            ContinuationCopyLimit::Depth => (u64::from(self.limits.depth), self.origins[2]),
        };
        ResourceFact::ContinuationCopy {
            limit,
            setting,
            origin,
            available_bytes,
        }
    }
}

/// The names in `directory`, each spending one of `entries`, or `None` once
/// they run out. Listing first closes the stream before any descent.
pub(super) fn listed(
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
pub(super) fn open_local_directory(
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
/// named for `session`, searched within the configured retained-state budgets. The prior lane
/// wrote this tree, so the path must already be canonical and every directory
/// is opened from its parent's handle, never through a link or a mount.
pub(crate) fn continuation_available(
    prior: &run::AttemptRecord,
    state_root: &Path,
    config_root: &Path,
) -> Result<(), ContinuationError> {
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
    let policy = StateCopyPolicy::read(config_root);
    if provider == "opencode" {
        return super::observation::find_opencode_session(&state, session)
            .map(|_| ())
            .map_err(|_| missing_continuation(provider));
    }
    match find_session_state_within(&state, session, policy.limits) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(missing_continuation(provider)),
        Err(exhausted) => {
            Err(StateCopyError::resource(policy.fact(exhausted.limit(), None)).into())
        }
    }
}

#[cfg(test)]
pub(super) fn find_session_state(
    provider_state: &Path,
    session: &str,
) -> Result<Option<PathBuf>, SearchExhausted> {
    find_session_state_within(provider_state, session, RETAINED_STATE)
}

/// An unfinished bounded search cannot establish that a session is missing.
#[derive(Debug, PartialEq)]
pub(super) enum SearchExhausted {
    Entries,
    Depth,
}
impl SearchExhausted {
    fn limit(self) -> ContinuationCopyLimit {
        match self {
            Self::Entries => ContinuationCopyLimit::Entries,
            Self::Depth => ContinuationCopyLimit::Depth,
        }
    }
}

pub(super) fn find_session_state_within(
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
            Ok(None) => return Err(SearchExhausted::Entries),
            Err(_) => return Ok(false),
        };
        let mut depth_exhausted = false;
        for name in names {
            if name.to_string_lossy().contains(session) {
                return Ok(true);
            }
            if let Some(child) = open_local_directory(dir, &name, device) {
                if let Some(remaining) = depth.checked_sub(1) {
                    match holds(&child, session, remaining, device, budget) {
                        Ok(true) => return Ok(true),
                        Ok(false) => {}
                        Err(SearchExhausted::Depth) => depth_exhausted = true,
                        Err(error) => return Err(error),
                    }
                } else {
                    depth_exhausted = true;
                }
            }
        }
        if depth_exhausted {
            Err(SearchExhausted::Depth)
        } else {
            Ok(false)
        }
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
        Ok(None) => return Err(SearchExhausted::Entries),
        Err(_) => return Ok(None),
    };
    let mut depth_exhausted = false;
    for name in names {
        if name.to_string_lossy().starts_with("candidate-")
            && let Some(candidate) = open_local_directory(&root, &name, device)
        {
            match holds(&candidate, session, limits.depth, device, &mut budget) {
                Ok(true) => return Ok(Some(provider_state.join(name))),
                Ok(false) => {}
                Err(SearchExhausted::Depth) => depth_exhausted = true,
                Err(error) => return Err(error),
            }
        }
    }
    if depth_exhausted {
        Err(SearchExhausted::Depth)
    } else {
        Ok(None)
    }
}

/// Copy regular files and directories within the selected budgets. GC tombstones,
/// symlinks and other entries are skipped. For Codex, copy only session trees:
/// setup recreates caches and Codex rebuilds its index with local rollout paths.
/// A leftover process may still change the tree, so every entry is opened from
/// its parent's handle without following a link, and the opened descriptor is
/// checked and copied.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct StateCopyError {
    message: String,
    class: provenant_records::diagnostic::BoundaryClass,
    resource: Option<provenant_records::diagnostic::ResourceFact>,
}
impl StateCopyError {
    fn authority(message: &str) -> Self {
        Self {
            message: message.into(),
            class: provenant_records::diagnostic::BoundaryClass::Authority,
            resource: None,
        }
    }
    fn resource(fact: provenant_records::diagnostic::ResourceFact) -> Self {
        let diagnostic = fact.project(provenant_records::diagnostic::Diagnostic::new("REFUSED"));
        Self {
            message: format!(
                "{}; {}",
                diagnostic.detail.unwrap_or_default(),
                diagnostic.next.unwrap_or_default()
            ),
            class: provenant_records::diagnostic::BoundaryClass::None,
            resource: Some(fact),
        }
    }
}
impl From<String> for StateCopyError {
    fn from(message: String) -> Self {
        Self {
            message,
            class: provenant_records::diagnostic::BoundaryClass::None,
            resource: None,
        }
    }
}
impl From<&str> for StateCopyError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

#[cfg(test)]
pub(super) fn copy_state(from: &Path, to: &Path, sessions_only: bool) -> Result<(), String> {
    copy_state_within(from, to, RETAINED_STATE, sessions_only)
}

#[cfg(test)]
pub(super) fn copy_state_within(
    from: &Path,
    to: &Path,
    limits: StateLimits,
    sessions_only: bool,
) -> Result<(), String> {
    copy_state_classified(from, to, StateCopyPolicy::shipped(limits), sessions_only)
        .map_err(|error| error.to_string())
}

fn copy_state_classified(
    from: &Path,
    to: &Path,
    policy: StateCopyPolicy,
    sessions_only: bool,
) -> Result<(), StateCopyError> {
    let limits = policy.limits;
    let failed =
        |detail: &dyn std::fmt::Display| format!("copying retained provider state: {detail}");
    // The anchor opens without following its final component; this stat only
    // names the common refusal.
    if !std::fs::symlink_metadata(from)
        .map_err(|error| failed(&error))?
        .is_dir()
    {
        return Err(StateCopyError::authority(
            "retained provider state is not an anchored directory",
        ));
    }
    let source = Directory::open_anchor(from).map_err(|error| {
        if error.is_symlink()
            || error.raw_os_error() == Some(libc::ENOTDIR)
            || matches!(error, FsError::RaceRefused { .. })
        {
            StateCopyError::authority("retained provider state is not an anchored directory")
        } else {
            failed(&error).into()
        }
    })?;
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(failed(&"the destination has no parent directory").into());
    };
    std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
    let parent = Directory::open_anchor(parent).map_err(|error| failed(&error))?;
    // Free space caps the budget, so state the filesystem cannot hold is
    // refused before it fills the disk.
    let available_bytes = parent.available_bytes().map_err(|error| failed(&error))?;
    let bytes = limits.bytes.min(available_bytes);
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
    let copied = copied.map_err(|refusal| {
        let class = refusal.boundary_class();
        let message = match refusal {
            CopyRefusal::HardLink => "retained provider state holds a hard-linked file".into(),
            CopyRefusal::Limit(setting) => {
                let available =
                    (setting == ContinuationCopyLimit::Bytes).then_some(available_bytes);
                return StateCopyError::resource(policy.fact(setting, available));
            }
            CopyRefusal::CrossDevice => {
                "retained provider state crosses a filesystem boundary".into()
            }
            CopyRefusal::Fs(error) => failed(&error),
        };
        StateCopyError {
            message,
            class,
            resource: None,
        }
    });
    if let Err(refusal) = &copied
        && let Err(error) = remove_staged(&parent, &staging)
    {
        return Err(StateCopyError {
            message: format!("{refusal}; removing its staged copy: {error}"),
            class: refusal.class,
            resource: refusal.resource,
        });
    }
    copied
}

#[test]
#[allow(clippy::print_stderr, reason = "report an unavailable test fixture")]
fn retained_state_hard_link_is_an_integrity_refusal() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::BoundaryClass;
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    let external = temp.path().join("external");
    std::fs::write(&external, "state")?;
    if let Err(error) = std::fs::hard_link(&external, source.join("link")) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!("hard-link fixture unavailable: {error}");
            return Ok(());
        }
        return Err(error.into());
    }
    let error = copy_state_classified(
        &source,
        &temp.path().join("copy"),
        StateCopyPolicy::shipped(RETAINED_STATE),
        false,
    )
    .unwrap_err();
    assert_eq!(error.class, BoundaryClass::Integrity);
    assert_eq!(
        error.to_string(),
        "retained provider state holds a hard-linked file"
    );
    Ok(())
}

/// Remove a refused copy's staging tree. The host made it fresh and private,
/// so it may dispose of lock-named entries the copy wrote there.
pub(super) fn remove_staged(parent: &Directory, name: &std::ffi::OsStr) -> Result<(), FsError> {
    fn remove_contents(tree: &PayloadTree) -> Result<(), FsError> {
        for entry in tree
            .entries()?
            .collect::<std::result::Result<Vec<_>, _>>()?
        {
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
pub(super) struct Budget {
    pub(super) entries: u32,
    pub(super) bytes: u64,
    pub(super) device: u64,
}

pub(super) enum CopyRefusal {
    HardLink,
    // A resource refusal always names its configurable setting. The owner
    // supplies that setting's value and source when projecting the error.
    Limit(ContinuationCopyLimit),
    CrossDevice,
    Fs(FsError),
}

impl CopyRefusal {
    fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass;
        match self {
            Self::HardLink => BoundaryClass::Integrity,
            Self::CrossDevice => BoundaryClass::Authority,
            Self::Limit(_) | Self::Fs(_) => BoundaryClass::None,
        }
    }
}

impl From<FsError> for CopyRefusal {
    fn from(error: FsError) -> Self {
        Self::Fs(error)
    }
}

/// The entry changed type or vanished after it was listed. A leftover process
/// may churn the tree; skipping such an entry copies nothing it names.
pub(super) fn changed_entry(error: &FsError) -> bool {
    error.is_symlink()
        || error.is_not_found()
        || matches!(error, FsError::NotRegular { .. })
        // A directory now a file, or a file now a socket (Linux, then Darwin).
        || matches!(
            error.raw_os_error(),
            Some(libc::ENOTDIR | libc::ENXIO | libc::EOPNOTSUPP)
        )
}

pub(super) fn credential_failure_message(
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
        if let Some(directory) = provenant_records::diagnostic::shell_path(
            directory,
            provenant_records::diagnostic::Destination::Local,
        ) {
            message.push_str(&format!(
                "; sign in again with CLAUDE_CONFIG_DIR={directory} claude auth login"
            ));
        } else {
            message.push_str("; check the registered account directory before signing in again");
        }
    }
    message
}

// Lets a test change an entry between its stat and its open.
#[cfg(test)]
pub(super) type OpenHook = Box<dyn FnMut(&std::ffi::OsStr)>;
#[cfg(test)]
thread_local! {
    pub(super) static BEFORE_OPEN: std::cell::RefCell<Option<OpenHook>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn before_open(_name: &std::ffi::OsStr) {
    #[cfg(test)]
    BEFORE_OPEN.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(_name);
        }
    });
}

pub(super) fn copy_tree(
    from: &Directory,
    to: &Directory,
    depth: u32,
    budget: &mut Budget,
    sessions_only: bool,
) -> Result<(), CopyRefusal> {
    for name in listed(from, &mut budget.entries)?
        .ok_or(CopyRefusal::Limit(ContinuationCopyLimit::Entries))?
    {
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
                let depth = depth
                    .checked_sub(1)
                    .ok_or(CopyRefusal::Limit(ContinuationCopyLimit::Depth))?;
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
                    return Err(CopyRefusal::Limit(ContinuationCopyLimit::Bytes));
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

#[test]
fn retained_state_copy_refusals_require_inventoried_settings_and_followable_remedies()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::{
        BoundaryClass, ContinuationCopyLimit, Diagnostic, ResourceFact, SettingSource,
    };
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let retained = base.join("TOKEN_SENTINEL");
    let source = retained.join("candidate-0");
    std::fs::create_dir_all(source.join("a/b"))?;
    std::fs::write(source.join("a/b/session"), "state")?;
    let inventory = include_str!("../../../../run/assets/launch-guards.toml")
        .parse::<toml_edit::DocumentMut>()?;
    let guards = inventory["guard"].as_array_of_tables().unwrap();
    // Exercise real refusal paths rather than constructing their resource facts.
    for (field, value, expected_setting) in [
        (
            "continuation_copy_limit_bytes",
            4,
            ContinuationCopyLimit::Bytes,
        ),
        (
            "continuation_copy_entries",
            1,
            ContinuationCopyLimit::Entries,
        ),
        ("continuation_copy_depth", 1, ContinuationCopyLimit::Depth),
    ] {
        for configured in [false, true] {
            let policy = if configured {
                std::fs::write(
                    base.join("config.toml"),
                    format!("schema_version=1\n[domains.run]\n{field}={value}\n"),
                )?;
                StateCopyPolicy::read(&base)
            } else {
                let limits = match expected_setting {
                    ContinuationCopyLimit::Bytes => StateLimits {
                        bytes: value,
                        ..RETAINED_STATE
                    },
                    ContinuationCopyLimit::Entries => StateLimits {
                        entries: value as u32,
                        ..RETAINED_STATE
                    },
                    ContinuationCopyLimit::Depth => StateLimits {
                        depth: value as u32,
                        ..RETAINED_STATE
                    },
                };
                StateCopyPolicy::shipped(limits)
            };
            let target = base.join(format!("copy-{field}-{configured}"));
            let error = copy_state_classified(&source, &target, policy, false).unwrap_err();
            let continuation = ContinuationError::StateCopy(error);
            assert_eq!(continuation.boundary_class(), BoundaryClass::None);
            let fact = continuation
                .resource_fact()
                .expect("a class-none policy refusal must carry a configurable resource fact");
            assert!(
                matches!(fact, ResourceFact::ContinuationCopy { setting, limit, origin, .. }
                if setting == expected_setting && limit == value && origin == if configured { SettingSource::Configuration } else { SettingSource::ShippedDefault })
            );
            let diagnostic = fact.project(Diagnostic::new("REFUSED"));
            let detail = diagnostic.detail.as_deref().unwrap();
            assert!(detail.contains(expected_setting.setting()));
            assert!(detail.contains(if configured {
                "host configuration"
            } else {
                "shipped default"
            }));
            assert!(!detail.contains("TOKEN_SENTINEL"));
            assert!(
                diagnostic
                    .next
                    .as_deref()
                    .unwrap()
                    .contains(expected_setting.setting())
            );
            assert!(
                guards.iter().any(|row| {
                    row.get("site").and_then(|v| v.as_str())
                        == Some("cli/src/run/provider/continuation.rs#copy_state_classified")
                        && row.get("boundary").and_then(|v| v.as_str())
                            == Some("configurable_default")
                        && row.get("code").and_then(|v| v.as_str())
                            == Some(diagnostic.code.as_str())
                        && row.get("setting").and_then(|v| v.as_str())
                            == Some(expected_setting.setting())
                }),
                "actual refusal lacks an inventory classification: {field}"
            );
            assert!(!target.exists(), "no partial provider state may survive");
            // The search uses the same policy and retains the precise exhausted budget.
            if expected_setting != ContinuationCopyLimit::Bytes {
                let exhausted =
                    find_session_state_within(&retained, "session", policy.limits).unwrap_err();
                let search_error = StateCopyError::resource(policy.fact(exhausted.limit(), None));
                assert!(
                    matches!(search_error.resource, Some(ResourceFact::ContinuationCopy { setting, .. }) if setting == expected_setting)
                );
                assert!(
                    guards.iter().any(|row| {
                        row.get("site").and_then(|v| v.as_str())
                            == Some(
                                "cli/src/run/provider/continuation.rs#find_session_state_within",
                            )
                            && row.get("setting").and_then(|v| v.as_str())
                                == Some(expected_setting.setting())
                            && row.get("boundary").and_then(|v| v.as_str())
                                == Some("configurable_default")
                            && row.get("code").and_then(|v| v.as_str()) == Some("REFUSED")
                    }),
                    "search refusal is missing from inventory"
                );
            }
            // Follow the printed setting remedy against the very same tree.
            std::fs::write(
                base.join("config.toml"),
                format!("schema_version=1\n[domains.run]\n{field}=16\n"),
            )?;
            copy_state_classified(&source, &target, StateCopyPolicy::read(&base), false)?;
            assert_eq!(std::fs::read(target.join("a/b/session"))?, b"state");
            assert_eq!(
                find_session_state_within(
                    &retained,
                    "session",
                    StateCopyPolicy::read(&base).limits
                ),
                Ok(Some(source.clone()))
            );
        }
    }
    Ok(())
}

#[test]
fn retained_state_copy_refuses_redirected_roots_as_authority()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_records::diagnostic::BoundaryClass;
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join("session"), "state")?;
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&source, &alias)?;
    let file = temp.path().join("file");
    std::fs::write(&file, "state")?;
    for from in [alias, file] {
        let target = temp.path().join("copy");
        let error = copy_state_classified(
            &from,
            &target,
            StateCopyPolicy::shipped(RETAINED_STATE),
            false,
        )
        .unwrap_err();
        assert_eq!(error.class, BoundaryClass::Authority);
        assert!(error.resource.is_none());
        assert!(!target.exists());
    }
    Ok(())
}

#[test]
fn session_search_continues_after_unrelated_depth_exhaustion()
-> Result<(), Box<dyn std::error::Error>> {
    for separate_candidates in [false, true] {
        let temp = tempfile::tempdir()?;
        let state = std::fs::canonicalize(temp.path())?;
        let parent = if separate_candidates {
            state.clone()
        } else {
            state.join("candidate-0")
        };
        std::fs::create_dir_all(&parent)?;
        // Use the filesystem's actual directory order to put the exhausted
        // subtree first, without relying on alphabetical or creation order.
        for name in ["candidate-first", "candidate-second"] {
            std::fs::create_dir(parent.join(name))?;
        }
        let names = Directory::open_anchor(&parent)?
            .entries()?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(names.len(), 2);
        let deep = parent.join(&names[0]);
        let reachable = parent.join(&names[1]);
        std::fs::create_dir_all(deep.join("a/b/c"))?;
        std::fs::write(reachable.join("fixture-session"), "state")?;
        let limits = StateLimits {
            depth: 1,
            ..RETAINED_STATE
        };
        let expected = if separate_candidates {
            reachable.clone()
        } else {
            parent.clone()
        };
        assert_eq!(
            find_session_state_within(&state, "fixture-session", limits),
            Ok(Some(expected))
        );
        std::fs::remove_file(reachable.join("fixture-session"))?;
        assert_eq!(
            find_session_state_within(&state, "fixture-session", limits),
            Err(SearchExhausted::Depth)
        );
    }
    Ok(())
}
