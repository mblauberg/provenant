//! Bounded decoding of Git tree entries for an attempt-owned source view.
use provenant_confinement::repo::RepoPath;
use provenant_core::GitOid;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
};

/// One file's ceiling. It leaves room for the batch header under the 64 MiB
/// capture ceiling of a single utility read.
const MAX_OBJECT_BYTES: u64 = 60 * 1024 * 1024;
/// Bytes of blob content requested by one `cat-file --batch` process.
const BATCH_BYTES: u64 = 32 * 1024 * 1024;
const BATCH_OBJECTS: usize = 4096;
const BATCH_HEADER_ALLOWANCE: u64 = 160;

/// Host limits for one view, from `domains.run.source_view_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SourceLimits {
    pub seconds: u64,
    pub entries: usize,
    pub bytes: u64,
}

impl SourceLimits {
    pub(super) fn from_config(config: &provenant_confinement::run_config::RunConfig) -> Self {
        Self {
            seconds: config.source_view_seconds,
            entries: usize::try_from(config.source_view_entries).unwrap_or(usize::MAX),
            bytes: config.source_view_bytes,
        }
    }
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self::from_config(&provenant_confinement::run_config::RunConfig::default())
    }
}

/// The step of building the view that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceStep {
    Repository,
    TreeListing,
    TreeValidation,
    ObjectRead,
    TemporaryDirectory,
    IndexBuild,
}

impl SourceStep {
    fn label(self) -> &'static str {
        match self {
            Self::Repository => "repository check",
            Self::TreeListing => "tree listing",
            Self::TreeValidation => "tree validation",
            Self::ObjectRead => "object read",
            Self::TemporaryDirectory => "temporary directory",
            Self::IndexBuild => "index build",
        }
    }
}

/// A fixed-form cause: never Git output, file contents or credential values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SourceCause {
    TimedOut,
    OutputLimit(&'static str),
    GitExit(i64),
    Signalled(i64),
    Cancelled,
    Refused(&'static str),
    Failed(&'static str),
}

impl std::fmt::Display for SourceCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str(
                "timed out (raise domains.run.source_view_seconds if the host is slow or loaded)",
            ),
            Self::OutputLimit(stream) => write!(f, "{stream} exceeded its output limit"),
            Self::GitExit(status) => write!(f, "Git exited with status {status}"),
            Self::Signalled(signal) => write!(f, "Git was ended by signal {signal}"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Refused(reason) | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("pinned source view failed at {}: {cause}", step.label())]
pub(super) struct SourceViewError {
    pub step: SourceStep,
    pub cause: SourceCause,
}

impl SourceViewError {
    fn new(step: SourceStep, cause: SourceCause) -> Self {
        Self { step, cause }
    }
    fn failed(step: SourceStep, reason: &'static str) -> Self {
        Self::new(step, SourceCause::Failed(reason))
    }
    fn refused(step: SourceStep, reason: &'static str) -> Self {
        Self::new(step, SourceCause::Refused(reason))
    }
    fn filesystem(_: impl std::fmt::Display) -> Self {
        Self::failed(
            SourceStep::TemporaryDirectory,
            "filesystem operation failed",
        )
    }
    fn utility(step: SourceStep) -> impl Fn(provenant_confinement::utility::UtilityError) -> Self {
        move |error| Self::new(step, utility_cause(&error))
    }
}

fn utility_cause(error: &provenant_confinement::utility::UtilityError) -> SourceCause {
    use provenant_confinement::utility::UtilityError as E;
    match error {
        E::TimedOut => SourceCause::TimedOut,
        E::Truncated { stream } => SourceCause::OutputLimit(stream),
        E::Signalled { signal } => SourceCause::Signalled(*signal),
        E::Cancelled => SourceCause::Cancelled,
        E::Incomplete => SourceCause::Failed("Git output was incomplete"),
        E::LaneConfiguration { .. } => {
            SourceCause::Refused("a lane-writable Git configuration cannot be neutralised")
        }
        E::Repo(_) => SourceCause::Failed("repository could not be verified"),
        E::Execution(_) => SourceCause::Failed("Git could not be run"),
        _ => SourceCause::Failed("Git utility refused the read"),
    }
}

fn exit_cause(termination: provenant_os::exec::Termination) -> Option<SourceCause> {
    use provenant_os::exec::Termination;
    match termination {
        Termination::Exit(0) => None,
        Termination::Exit(status) => Some(SourceCause::GitExit(status)),
        Termination::Signal(signal) => Some(SourceCause::Signalled(signal)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceKind {
    File,
    Executable,
    Symlink,
}

#[derive(Debug)]
pub(super) struct SourceEntry {
    pub path: RepoPath,
    pub oid: GitOid,
    pub kind: SourceKind,
    pub size: u64,
}

/// `git ls-tree -r -l -z` is byte-oriented: paths need not be UTF-8.
/// Refuse unsupported objects rather than quietly presenting a partial tree.
pub(super) fn parse_source_tree(
    bytes: &[u8],
    limits: &SourceLimits,
) -> Result<Vec<SourceEntry>, &'static str> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err("source tree listing is incomplete");
    }
    let mut entries = Vec::new();
    let mut paths = HashSet::new();
    let mut total = 0u64;
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("invalid source tree entry")?;
        let mut fields = record[..tab]
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty());
        let mode = fields.next().ok_or("invalid source tree mode")?;
        let kind = match mode {
            b"100644" => SourceKind::File,
            b"100755" => SourceKind::Executable,
            b"120000" => SourceKind::Symlink,
            _ => return Err("source tree contains an unsupported entry"),
        };
        if fields.next() != Some(b"blob".as_slice()) {
            return Err("source tree contains an unsupported object");
        }
        let oid = std::str::from_utf8(fields.next().ok_or("missing source object")?)
            .map_err(|_| "invalid source object")?
            .parse::<GitOid>()
            .map_err(|_| "invalid source object")?;
        let size = std::str::from_utf8(fields.next().ok_or("missing source size")?)
            .map_err(|_| "invalid source size")?
            .parse::<u64>()
            .map_err(|_| "invalid source size")?;
        if fields.next().is_some() {
            return Err("invalid source tree entry");
        }
        let path = RepoPath::new(PathBuf::from(OsString::from_vec(
            record[tab + 1..].to_vec(),
        )))
        .map_err(|_| "unsafe source path")?;
        if path.as_path().components().any(|component| {
            component
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(b".git")
        }) {
            return Err("source tree contains a reserved Git path");
        }
        if !paths.insert(path.as_path().to_owned()) {
            return Err("duplicate source path");
        }
        if size > MAX_OBJECT_BYTES {
            return Err("a source file exceeds the 60 MiB per-file limit");
        }
        total = total
            .checked_add(size)
            .ok_or("source tree exceeds domains.run.source_view_bytes")?;
        if total > limits.bytes || entries.len() >= limits.entries {
            return Err("source tree exceeds domains.run.source_view_entries or source_view_bytes");
        }
        entries.push(SourceEntry {
            path,
            oid,
            kind,
            size,
        });
    }
    Ok(entries)
}

/// Publish a verified tree below a host-owned empty directory without following
/// any tree-provided symlink. The caller supplies bounded, batched Git object
/// reads: `read_blobs` returns the content of each requested object, in order.
pub(super) fn materialise_source_tree(
    root: &provenant_os::fs::Directory,
    entries: &[SourceEntry],
    mut read_blobs: impl FnMut(&[GitOid], u64) -> Result<Vec<Vec<u8>>, SourceViewError>,
) -> Result<(), SourceViewError> {
    use std::io::Write;
    use std::path::{Component, Path};

    let unsafe_link =
        || SourceViewError::refused(SourceStep::TreeValidation, "unsafe source symlink");
    let size_changed =
        || SourceViewError::refused(SourceStep::ObjectRead, "source object size changed");
    // Resolve against the complete, immutable Git tree, not the host filesystem.
    // A later symlink may change the meaning of an earlier link's target.
    let mut links = HashMap::new();
    let symlinks: Vec<&SourceEntry> = entries
        .iter()
        .filter(|entry| entry.kind == SourceKind::Symlink)
        .collect();
    for group in batches(&symlinks) {
        let oids: Vec<GitOid> = group.iter().map(|entry| entry.oid).collect();
        let bytes = read_blobs(&oids, group.iter().map(|entry| entry.size).sum())?;
        for (entry, bytes) in group.iter().zip(bytes) {
            if u64::try_from(bytes.len()).ok() != Some(entry.size) {
                return Err(size_changed());
            }
            let target = PathBuf::from(OsString::from_vec(bytes));
            if target.as_os_str().is_empty()
                || target.as_os_str().as_encoded_bytes().contains(&0)
                || target.is_absolute()
            {
                return Err(unsafe_link());
            }
            links.insert(entry.path.as_path().to_owned(), target);
        }
    }
    // On macOS the source volume can match ASCII names without regard to
    // case. Non-ASCII filesystem equivalence is volume-dependent; fail closed
    // if it could affect a parent traversal after a named component.
    let non_ascii = links.iter().any(|(path, target)| {
        !path.as_os_str().as_encoded_bytes().is_ascii()
            || !target.as_os_str().as_encoded_bytes().is_ascii()
    });
    if cfg!(target_os = "macos") && non_ascii {
        for target in links.values() {
            let mut named = false;
            for component in target.components() {
                match component {
                    Component::Normal(_) => named = true,
                    Component::ParentDir if named => return Err(unsafe_link()),
                    _ => {}
                }
            }
        }
    }
    let folded: HashMap<_, _> = links
        .iter()
        .map(|(path, target)| {
            (
                path.as_os_str().as_encoded_bytes().to_ascii_lowercase(),
                target,
            )
        })
        .collect();
    for path in links.keys() {
        let mut resolved = PathBuf::new();
        let mut pending: VecDeque<OsString> = path
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        let mut expansions = 0;
        while let Some(component) = pending.pop_front() {
            match Path::new(&component).components().next() {
                Some(Component::CurDir) => {}
                Some(Component::ParentDir) => {
                    if !resolved.pop() {
                        return Err(unsafe_link());
                    }
                }
                Some(Component::Normal(name)) => {
                    resolved.push(name);
                    let target = if cfg!(target_os = "macos") {
                        folded
                            .get(&resolved.as_os_str().as_encoded_bytes().to_ascii_lowercase())
                            .copied()
                    } else {
                        links.get(&resolved)
                    };
                    if let Some(target) = target {
                        expansions += 1;
                        if expansions > 40 {
                            return Err(unsafe_link());
                        }
                        resolved.pop();
                        for part in target.components().rev() {
                            pending.push_front(part.as_os_str().to_owned());
                        }
                    }
                }
                _ => return Err(unsafe_link()),
            }
        }
    }
    let place = |entry: &SourceEntry, bytes: &[u8]| -> Result<(), SourceViewError> {
        if u64::try_from(bytes.len()).ok() != Some(entry.size) {
            return Err(size_changed());
        }
        let invalid =
            || SourceViewError::refused(SourceStep::TreeValidation, "invalid source path");
        let mut parent = root.try_clone().map_err(SourceViewError::filesystem)?;
        let path = entry.path.as_path();
        let name = path.file_name().ok_or_else(invalid)?;
        if let Some(ancestors) = path.parent() {
            for component in ancestors.components() {
                let Component::Normal(name) = component else {
                    return Err(invalid());
                };
                match parent.mkdir(name, 0o700) {
                    Ok(()) => {}
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(SourceViewError::filesystem(error)),
                }
                parent = parent
                    .open_verified_directory(name)
                    .map_err(SourceViewError::filesystem)?;
            }
        }
        match entry.kind {
            SourceKind::File | SourceKind::Executable => {
                // The attempt-owned tree is private until it is complete.
                // Exclusive creation also permits tracked names such as Cargo.lock.
                let mut file = parent
                    .create_new(name)
                    .map_err(SourceViewError::filesystem)?;
                file.write_all(bytes).map_err(SourceViewError::filesystem)?;
                if entry.kind == SourceKind::Executable {
                    file.make_owner_executable()
                        .map_err(SourceViewError::filesystem)?;
                }
            }
            SourceKind::Symlink => {
                let target = PathBuf::from(OsString::from_vec(bytes.to_vec()));
                parent
                    .symlink(Path::new(&target), name)
                    .map_err(SourceViewError::filesystem)?;
            }
        }
        Ok(())
    };
    let files: Vec<&SourceEntry> = entries
        .iter()
        .filter(|entry| entry.kind != SourceKind::Symlink)
        .collect();
    for group in batches(&files) {
        // Identical blobs are read once per batch.
        let mut unique: Vec<GitOid> = Vec::new();
        let mut seen = HashSet::new();
        let mut expected = 0u64;
        for entry in &group {
            if seen.insert(entry.oid) {
                unique.push(entry.oid);
                expected = expected.saturating_add(entry.size);
            }
        }
        let contents = read_blobs(&unique, expected)?;
        let by_oid: HashMap<GitOid, Vec<u8>> = unique.into_iter().zip(contents).collect();
        for entry in group {
            place(entry, by_oid.get(&entry.oid).ok_or_else(size_changed)?)?;
        }
    }
    for entry in symlinks {
        let target = links
            .get(entry.path.as_path())
            .ok_or_else(unsafe_link)?
            .as_os_str()
            .as_encoded_bytes();
        place(entry, target)?;
    }
    Ok(())
}

/// Consecutive entries grouped so one batched read stays inside its capture
/// budget: at most `BATCH_BYTES` of content and `BATCH_OBJECTS` objects.
fn batches<'a>(entries: &[&'a SourceEntry]) -> Vec<Vec<&'a SourceEntry>> {
    let mut groups: Vec<Vec<&SourceEntry>> = Vec::new();
    let mut bytes = 0u64;
    for entry in entries {
        match groups.last_mut() {
            Some(group)
                if group.len() < BATCH_OBJECTS
                    && bytes.saturating_add(entry.size) <= BATCH_BYTES =>
            {
                group.push(entry);
                bytes += entry.size;
            }
            _ => {
                groups.push(vec![entry]);
                bytes = entry.size;
            }
        }
    }
    groups
}

/// Decode `git cat-file --batch` output for exactly `oids`, in order.
pub(super) fn parse_blob_batch(
    oids: &[GitOid],
    bytes: &[u8],
) -> Result<Vec<Vec<u8>>, SourceViewError> {
    let malformed = || {
        SourceViewError::failed(
            SourceStep::ObjectRead,
            "Git returned a malformed object stream",
        )
    };
    let mut rest = bytes;
    let mut out = Vec::with_capacity(oids.len());
    for oid in oids {
        let newline = rest
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(malformed)?;
        let header = std::str::from_utf8(&rest[..newline]).map_err(|_| malformed())?;
        rest = &rest[newline + 1..];
        let mut fields = header.split(' ');
        if fields.next() != Some(oid.to_string().as_str()) {
            return Err(malformed());
        }
        match (fields.next(), fields.next(), fields.next()) {
            (Some("missing"), None, None) => {
                return Err(SourceViewError::refused(
                    SourceStep::ObjectRead,
                    "pinned source object is unavailable",
                ));
            }
            (Some("blob"), Some(size), None) => {
                let size = size.parse::<usize>().map_err(|_| malformed())?;
                if rest.len() <= size || rest[size] != b'\n' {
                    return Err(malformed());
                }
                out.push(rest[..size].to_vec());
                rest = &rest[size + 1..];
            }
            _ => return Err(malformed()),
        }
    }
    if rest.is_empty() {
        Ok(out)
    } else {
        Err(malformed())
    }
}

fn overlaps_source(
    source: &std::path::Path,
    grant: &provenant_confinement::permissions::PathGrant,
) -> bool {
    use provenant_confinement::permissions::PathScope;
    let path = std::fs::canonicalize(&grant.path).unwrap_or_else(|_| grant.path.clone());
    path.starts_with(source) || (grant.scope == PathScope::Subtree && source.starts_with(path))
}

#[derive(Debug, thiserror::Error)]
pub(super) enum SourceRootOverlap {
    #[error("product assets overlap the live graph source checkout")]
    Assets,
    #[error("an access grant overlaps the live graph source checkout")]
    Access,
    #[error("a system read grant overlaps the live graph source checkout")]
    SystemRead,
}

/// Explicit grants must not reopen the live source beside the pinned view.
pub(super) fn ensure_source_roots(
    source: &std::path::Path,
    access: &super::run::LaneAccess,
    assets: &std::path::Path,
) -> Result<(), SourceRootOverlap> {
    use provenant_confinement::permissions::{PathGrant, PathScope};
    let asset = PathGrant {
        path: assets.to_owned(),
        scope: PathScope::Subtree,
    };
    if overlaps_source(source, &asset) {
        return Err(SourceRootOverlap::Assets);
    }
    if access
        .read_roots
        .iter()
        .chain(&access.write_roots)
        .chain(&access.inputs_root)
        .any(|path| {
            overlaps_source(
                source,
                &PathGrant {
                    path: path.clone(),
                    scope: PathScope::Subtree,
                },
            )
        })
        || access.attachments.iter().any(|path| {
            overlaps_source(
                source,
                &PathGrant {
                    path: path.clone(),
                    scope: PathScope::Exact,
                },
            )
        })
    {
        return Err(SourceRootOverlap::Access);
    }
    if cfg!(target_os = "macos")
        && ["/usr/share", "/private/var/db/timezone"]
            .iter()
            .any(|path| source.starts_with(path))
    {
        return Err(SourceRootOverlap::SystemRead);
    }
    Ok(())
}

pub(super) fn ensure_profile_isolated(
    source: &std::path::Path,
    profile: &provenant_confinement::permissions::ProviderProfileSpec,
) -> Result<(), String> {
    let grants = profile
        .runtime_reads
        .iter()
        .chain(&profile.process_executables)
        .chain(&profile.continuation_reads)
        .chain(&profile.state_writes)
        .chain(&profile.tool_writes)
        .chain(&profile.tool_exec_roots)
        .chain(
            profile
                .authentication
                .iter()
                .flat_map(|auth| auth.roots.iter().chain(&auth.in_place_writes)),
        );
    if grants
        .into_iter()
        .any(|grant| overlaps_source(source, grant))
    {
        return Err("a provider grant overlaps the live graph source checkout".into());
    }
    Ok(())
}

/// No provider-owned write grant may cover the immutable view.
pub(super) fn ensure_view_read_only(
    view: &std::path::Path,
    grants: &[provenant_confinement::permissions::WriteGrant],
) -> Result<(), String> {
    use provenant_confinement::permissions::PathGrant;
    if grants.iter().any(|grant| {
        overlaps_source(
            view,
            &PathGrant {
                path: grant.path.clone(),
                scope: grant.scope,
            },
        )
    }) {
        return Err("a provider write grant overlaps the pinned source view".into());
    }
    Ok(())
}

/// Materialise a pinned Git commit into an attempt-private, read-only view.
/// The Git object reader is host-owned and applies the same bounded repository
/// checks as the ordinary read-only preparation path. A failure names the step
/// and a fixed-form cause; it never carries Git output or file contents.
pub(super) fn snapshot_source(
    registry: &super::Registry<'_>,
    task: &super::run::TaskRequest,
    paths: &super::run::LaunchPaths,
    source: &std::path::Path,
    git: &std::path::Path,
) -> Result<PathBuf, SourceViewError> {
    use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime, run_utility};
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor};
    use provenant_os::fs::{Directory, PublishMode};
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, time::Duration};

    use SourceStep as Step;
    let repository = |reason| SourceViewError::refused(Step::Repository, reason);
    let roots = registry
        .roots
        .ok_or_else(|| repository("routing context is unavailable"))?;
    let project = registry
        .project_id
        .ok_or_else(|| repository("selected project identity is unavailable"))?;
    let head = task
        .head
        .as_deref()
        .ok_or_else(|| repository("graph source head missing"))?
        .parse::<GitOid>()
        .map_err(|_| repository("invalid graph source head"))?;
    let limits = provenant_confinement::run_config::RunConfig::read(roots.state.as_path())
        .map(|config| SourceLimits::from_config(&config))
        .unwrap_or_default();
    let host_home = super::host_git_home(roots.state.as_path()).map_err(|_| {
        SourceViewError::failed(Step::TemporaryDirectory, "host Git home unavailable")
    })?;
    let utility = UtilityRuntime::new(git, host_home.path())
        .map_err(SourceViewError::utility(Step::Repository))?;
    let executor = SystemExecutor::new(1)
        .map_err(|_| SourceViewError::failed(Step::Repository, "executor unavailable"))?;
    let clock = provenant_os::clock::SystemClock::new();
    let deadline = WaitBudget::cli(clock.mono_now(), limits.seconds)
        .map_err(|_| SourceViewError::failed(Step::Repository, "invalid time limit"))?;
    let remaining = |step: SourceStep| -> Result<Duration, SourceViewError> {
        let duration = Duration::from_micros(deadline.remaining(clock.mono_now()).as_micros());
        if duration.is_zero() {
            Err(SourceViewError::new(step, SourceCause::TimedOut))
        } else {
            Ok(duration)
        }
    };
    // One shared deadline bounds the whole view; no process gets a smaller
    // fixed share, so a slow disk costs time rather than a refusal.
    let budget = |step: SourceStep, stdout: usize| -> Result<UtilityBudget, SourceViewError> {
        UtilityBudget::new(
            remaining(step)?,
            Duration::from_secs(2),
            Duration::from_secs(2),
            stdout,
            1024 * 1024,
        )
        .map_err(SourceViewError::utility(step))
    };
    const LISTING_LIMIT: usize = 64 * 1024 * 1024;
    let local_cancel = provenant_core::CancelToken::new();
    let cancel = registry.cancel.unwrap_or(&local_cancel);
    let repo = utility
        .open_repo(
            &executor,
            source,
            budget(Step::Repository, 64 * 1024 * 1024)?,
            cancel,
        )
        .map_err(SourceViewError::utility(Step::Repository))?;
    let location = provenant_config::ProjectLocation::new(repo.common_git_dir())
        .map_err(|_| SourceViewError::failed(Step::Repository, "project location invalid"))?;
    let binding = provenant_config::lookup_project(roots, &location)
        .map_err(|_| SourceViewError::failed(Step::Repository, "project binding unreadable"))?
        .ok_or_else(|| repository("dispatch project binding missing"))?;
    if binding.project_id() != project {
        return Err(repository(
            "source repository does not match dispatch project",
        ));
    }
    let tree = run_utility(
        &executor,
        GitRead::AllTreeEntries { oid: head },
        &repo,
        &utility,
        budget(Step::TreeListing, LISTING_LIMIT)?,
        cancel,
    )
    .map_err(SourceViewError::utility(Step::TreeListing))?;
    if let Some(cause) = exit_cause(tree.termination) {
        return Err(SourceViewError::new(Step::TreeListing, cause));
    }
    let entries = parse_source_tree(&tree.stdout.bytes, &limits)
        .map_err(|reason| SourceViewError::refused(Step::TreeValidation, reason))?;
    let temporary =
        Directory::open_anchor(&paths.temporary).map_err(SourceViewError::filesystem)?;
    let nonce = provenant_os::fs::entropy_128().map_err(SourceViewError::filesystem)?;
    let name = format!("source-{}", provenant_core::hex(&nonce));
    temporary
        .mkdir(OsStr::new(&name), 0o700)
        .map_err(SourceViewError::filesystem)?;
    let root = temporary
        .open_verified_directory(OsStr::new(&name))
        .map_err(SourceViewError::filesystem)?;
    materialise_source_tree(&root, &entries, |oids, content| {
        let stdout = content
            .saturating_add(BATCH_HEADER_ALLOWANCE * oids.len() as u64)
            .saturating_add(1024)
            .min(LISTING_LIMIT as u64);
        let object = run_utility(
            &executor,
            GitRead::BlobBatch {
                oids: oids.to_vec(),
            },
            &repo,
            &utility,
            budget(
                Step::ObjectRead,
                usize::try_from(stdout).unwrap_or(LISTING_LIMIT),
            )?,
            cancel,
        )
        .map_err(SourceViewError::utility(Step::ObjectRead))?;
        if let Some(cause) = exit_cause(object.termination) {
            return Err(SourceViewError::new(Step::ObjectRead, cause));
        }
        parse_blob_batch(oids, &object.stdout.bytes)
    })?;
    // Give Git-aware tools an isolated detached HEAD and access to the same
    // verified object store. No branch or mutable checkout index is copied.
    let objects = repo.common_git_dir().join("objects");
    if objects.as_os_str().as_bytes().contains(&b'\n')
        || objects.as_os_str().as_bytes().contains(&b'\r')
    {
        return Err(SourceViewError::refused(
            Step::TemporaryDirectory,
            "source object location cannot be represented safely",
        ));
    }
    let layout = || -> Result<(), SourceViewError> {
        root.mkdir(OsStr::new(".git"), 0o700)
            .map_err(SourceViewError::filesystem)?;
        let git_dir = root
            .open_verified_directory(OsStr::new(".git"))
            .map_err(SourceViewError::filesystem)?;
        git_dir
            .mkdir(OsStr::new("objects"), 0o700)
            .map_err(SourceViewError::filesystem)?;
        git_dir
            .mkdir(OsStr::new("refs"), 0o700)
            .map_err(SourceViewError::filesystem)?;
        let objects_dir = git_dir
            .open_verified_directory(OsStr::new("objects"))
            .map_err(SourceViewError::filesystem)?;
        objects_dir
            .mkdir(OsStr::new("info"), 0o700)
            .map_err(SourceViewError::filesystem)?;
        let info = objects_dir
            .open_verified_directory(OsStr::new("info"))
            .map_err(SourceViewError::filesystem)?;
        let mut alternate = objects.as_os_str().as_bytes().to_vec();
        alternate.push(b'\n');
        info.atomic_write(
            OsStr::new("alternates"),
            ".source-",
            &alternate,
            PublishMode::NoReplace,
        )
        .map_err(SourceViewError::filesystem)?;
        git_dir
            .atomic_write(
                OsStr::new("HEAD"),
                ".source-",
                format!("{head}\n").as_bytes(),
                PublishMode::NoReplace,
            )
            .map_err(SourceViewError::filesystem)?;
        let config = if matches!(head, GitOid::Sha256(_)) {
            "[core]\n\trepositoryformatversion = 1\n\tbare = false\n[extensions]\n\tobjectFormat = sha256\n"
        } else {
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n"
        };
        git_dir
            .atomic_write(
                OsStr::new("config"),
                ".source-",
                config.as_bytes(),
                PublishMode::NoReplace,
            )
            .map_err(SourceViewError::filesystem)?;
        Ok(())
    };
    layout()?;
    // Populate only the synthetic index. The synthetic Git configuration is
    // host-written, and this process receives no user configuration or env.
    let snapshot = paths.temporary.join(name);
    let argv: Vec<OsString> = vec![
        git.as_os_str().to_owned(),
        "--git-dir".into(),
        snapshot.join(".git").into_os_string(),
        "--work-tree".into(),
        snapshot.clone().into_os_string(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        "core.sparseCheckout=false".into(),
        "read-tree".into(),
        head.to_string().into(),
    ];
    let env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), host_home.path().as_os_str().to_owned()),
        ("TMPDIR".into(), host_home.path().as_os_str().to_owned()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_NO_REPLACE_OBJECTS".into(), "1".into()),
        ("GIT_GRAFT_FILE".into(), "/dev/null".into()),
        ("GIT_NO_LAZY_FETCH".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_ATTR_NOSYSTEM".into(), "1".into()),
    ];
    let mut options = ExecOptions::new(&argv, host_home.path(), remaining(Step::IndexBuild)?);
    options.env = Some(&env);
    options.cancel = Some(cancel);
    let index = SourceViewError::utility(Step::IndexBuild);
    repo.revalidate().map_err(|error| index(error.into()))?;
    let indexed = executor
        .execute(options)
        .map_err(|error| index(error.into()))?;
    repo.revalidate().map_err(|error| index(error.into()))?;
    let cause = if indexed.timed_out {
        Some(SourceCause::TimedOut)
    } else if indexed.cancelled {
        Some(SourceCause::Cancelled)
    } else if indexed.drain_truncated {
        Some(SourceCause::OutputLimit("output"))
    } else {
        exit_cause(indexed.termination)
    };
    if let Some(cause) = cause {
        return Err(SourceViewError::new(Step::IndexBuild, cause));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_tree_materialisation_is_independent_of_a_moving_checkout()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let checkout = temp.path().join("checkout");
        let snapshot = temp.path().join("snapshot");
        std::fs::create_dir(&checkout)?;
        std::fs::create_dir(&snapshot)?;
        std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(checkout.join("subject"), "later checkout")?;
        let oid = "1".repeat(40);
        let listing = format!("100644 blob {oid} 14\tsubject\0100644 blob {oid} 14\tCargo.lock\0");
        let entries = parse_source_tree(listing.as_bytes(), &SourceLimits::default())?;
        let root = provenant_os::fs::Directory::open_anchor(&snapshot)?;
        materialise_source_tree(&root, &entries, |oids, _| {
            Ok(oids.iter().map(|_| b"pinned subject".to_vec()).collect())
        })?;
        std::fs::rename(&checkout, temp.path().join("moved"))?;
        std::fs::create_dir(&checkout)?;
        std::fs::write(checkout.join("subject"), "replacement")?;
        assert_eq!(std::fs::read(snapshot.join("subject"))?, b"pinned subject");
        let unsafe_link = format!("120000 blob {oid} 9\tlink\0");
        let entries = parse_source_tree(unsafe_link.as_bytes(), &SourceLimits::default())?;
        assert!(
            materialise_source_tree(&root, &entries, |oids, _| {
                Ok(oids.iter().map(|_| b"../escape".to_vec()).collect())
            })
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn source_tree_symlinks_stay_inside_through_chains() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        // Expanding a directory link before .. is necessary to detect escape.
        for (links, safe) in [
            (vec![("src/current", "../shared/current")], true),
            (vec![("src/current", "../../outside")], false),
            (
                vec![("src/current", "../alias"), ("alias", "shared/current")],
                true,
            ),
            (
                vec![("src/current", "../alias"), ("alias", "../outside")],
                false,
            ),
            (
                vec![("dir", "nested/deeper"), ("link", "dir/../../../outside")],
                false,
            ),
            (vec![("a", "b"), ("b", "a")], false),
            (vec![("link", "/outside")], false),
        ] {
            let temp = tempfile::tempdir()?;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
            std::fs::create_dir(temp.path().join("shared"))?;
            std::fs::write(temp.path().join("shared/current"), b"inside")?;
            let root = provenant_os::fs::Directory::open_anchor(temp.path())?;
            let mut blobs = HashMap::new();
            let mut listing = String::new();
            for (index, (path, target)) in links.iter().enumerate() {
                let oid = format!("{:040x}", index + 1);
                listing.push_str(&format!("120000 blob {oid} {}\t{path}\0", target.len()));
                blobs.insert(oid.parse::<GitOid>()?, target.as_bytes().to_vec());
            }
            let entries = parse_source_tree(listing.as_bytes(), &SourceLimits::default())?;
            let result = materialise_source_tree(&root, &entries, |oids, _| {
                oids.iter()
                    .map(|oid| {
                        blobs.get(oid).cloned().ok_or_else(|| {
                            SourceViewError::refused(SourceStep::ObjectRead, "missing blob")
                        })
                    })
                    .collect()
            });
            assert_eq!(result.is_ok(), safe, "{links:?}: {result:?}");
            if safe {
                assert_eq!(std::fs::read(temp.path().join("src/current"))?, b"inside");
                for (path, target) in links {
                    assert_eq!(
                        std::fs::read_link(temp.path().join(path))?,
                        PathBuf::from(target)
                    );
                }
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn source_tree_refuses_case_aliased_directory_escape() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let root = provenant_os::fs::Directory::open_anchor(temp.path())?;
        let first = "1".repeat(40);
        let second = "2".repeat(40);
        let listing = format!("120000 blob {first} 1\tdir\0120000 blob {second} 14\tlink\0");
        let entries = parse_source_tree(listing.as_bytes(), &SourceLimits::default())?;
        let result = materialise_source_tree(&root, &entries, |oids, _| {
            Ok(oids
                .iter()
                .map(|oid| {
                    if oid.to_string() == first {
                        b".".to_vec()
                    } else {
                        b"DIR/../outside".to_vec()
                    }
                })
                .collect())
        });
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn source_tree_refuses_grants_that_reopen_the_live_checkout() {
        use provenant_confinement::permissions::{PathGrant, PathScope, ProviderProfileSpec};
        let source = std::path::Path::new("/fixture/project/writer");
        let mut access = super::super::run::LaneAccess::default();
        assert!(
            ensure_source_roots(source, &access, std::path::Path::new("/fixture/assets")).is_ok()
        );
        access.read_roots.push("/fixture/project".into());
        assert!(
            ensure_source_roots(source, &access, std::path::Path::new("/fixture/assets")).is_err()
        );
        access.read_roots.clear();
        assert!(ensure_source_roots(source, &access, source).is_err());
        let mut profile = ProviderProfileSpec::default();
        profile.runtime_reads.push(PathGrant {
            path: "/fixture/project".into(),
            scope: PathScope::Exact,
        });
        assert!(ensure_profile_isolated(source, &profile).is_ok());
        profile.runtime_reads[0].scope = PathScope::Subtree;
        assert!(ensure_profile_isolated(source, &profile).is_err());
        profile.runtime_reads[0] = PathGrant {
            path: "/fixture/project/writer/file".into(),
            scope: PathScope::Exact,
        };
        assert!(ensure_profile_isolated(source, &profile).is_err());
        let view = std::path::Path::new("/fixture/state/source");
        let mut write = provenant_confinement::WriteGrant {
            source: provenant_confinement::WriteSource::Scratch,
            path: "/fixture/state/scratch".into(),
            scope: PathScope::Subtree,
        };
        assert!(ensure_view_read_only(view, &[write.clone()]).is_ok());
        write.path = "/fixture/state".into();
        assert!(ensure_view_read_only(view, &[write]).is_err());
    }

    #[test]
    fn source_tree_rejects_traversal_duplicates_and_unsupported_objects() {
        let oid = "1".repeat(40);
        let record =
            |mode: &str, kind: &str, name: &str| format!("{mode} {kind} {oid} 3\t{name}\0");
        let valid = format!(
            "{}{}",
            record("100644", "blob", "src/a"),
            record("100755", "blob", "run")
        );
        let parsed = parse_source_tree(valid.as_bytes(), &SourceLimits::default()).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path.as_path(), std::path::Path::new("src/a"));
        assert_eq!(parsed[0].size, 3);
        assert_eq!(parsed[0].oid.to_string(), oid);
        assert_eq!(parsed[1].kind, SourceKind::Executable);
        for bad in [
            record("100644", "blob", "../escape"),
            record("100644", "blob", ".GiT/config"),
            record("160000", "commit", "submodule"),
            format!(
                "{}{}",
                record("100644", "blob", "same"),
                record("100644", "blob", "same")
            ),
        ] {
            assert!(
                parse_source_tree(bad.as_bytes(), &SourceLimits::default()).is_err(),
                "{bad:?}"
            );
        }
        assert!(
            parse_source_tree(
                &valid.as_bytes()[..valid.len() - 1],
                &SourceLimits::default()
            )
            .is_err()
        );
    }
}
