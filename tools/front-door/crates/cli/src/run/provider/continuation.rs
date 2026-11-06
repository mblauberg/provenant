use super::*;

/// Recheck current readiness for the exact route the provider session used.
/// Resolving an exact model bypasses named-pool selection and quota redraws.
pub(super) fn continuation_request(
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
        intent: Default::default(),
        choice: RouteChoice::Model(model),
        effort: applied.effort.clone(),
        fallback: Fallback::None,
        confidential: false,
    })
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ContinuationError {
    #[error("route differs from the prior attempt's provider, model or account")]
    RouteMismatch,
    #[error("{0}")]
    Other(String),
}

impl ContinuationError {
    pub(super) fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass;
        match self {
            Self::RouteMismatch => BoundaryClass::Authority,
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
    private_state: &Path,
) -> Result<provenant_providers::Continuation, ContinuationError> {
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
        return Err(ContinuationError::RouteMismatch);
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
        Ok(Some(retained)) => copy_state(&retained, private_state, AdapterSpec::get(&route.adapter).is_some_and(|spec| spec.copy_sessions_only))?,
        // These adapters retain sessions in their existing user data stores.
        Ok(None) if AdapterSpec::get(&route.adapter).is_some_and(|spec| spec.user_sessions) => {}
        Ok(None) => return Err("the prior attempt's retained provider state is missing or has expired; dispatch a new lane with its retained result or transcripts as context".into()),
        // An unfinished search proves nothing missing, whatever the adapter.
        Err(SearchExhausted) => {
            return Err(format!(
                "the prior attempt's retained provider state exceeds {} entries",
                RETAINED_STATE.entries
            ).into());
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
/// named for `session`, searched within `RETAINED_STATE`. The prior lane
/// wrote this tree, so the path must already be canonical and every directory
/// is opened from its parent's handle, never through a link or a mount.
pub(crate) fn continuation_available(
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

pub(super) fn find_session_state(
    provider_state: &Path,
    session: &str,
) -> Result<Option<PathBuf>, SearchExhausted> {
    find_session_state_within(provider_state, session, RETAINED_STATE)
}

/// The session search spent its entry budget before it finished.
#[derive(Debug, PartialEq)]
pub(super) struct SearchExhausted;

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
pub(super) fn copy_state(from: &Path, to: &Path, sessions_only: bool) -> Result<(), String> {
    copy_state_within(from, to, RETAINED_STATE, sessions_only)
}

pub(super) fn copy_state_within(
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
pub(super) fn remove_staged(parent: &Directory, name: &std::ffi::OsStr) -> Result<(), FsError> {
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
pub(super) struct Budget {
    pub(super) entries: u32,
    pub(super) bytes: u64,
    pub(super) device: u64,
}

pub(super) enum CopyRefusal {
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
