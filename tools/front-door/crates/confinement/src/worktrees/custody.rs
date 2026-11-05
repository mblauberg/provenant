//! Writer custody, locking and terminal checks.
use super::*;

/// A writer must pass this check before its terminal result is accepted.
/// Index and working-tree cleanliness do not prove a Git operation has ended.
pub fn check_writer_terminal(repo: &VerifiedRepo) -> Result<(), WorktreeError> {
    repo.revalidate()?;
    for marker in [
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "sequencer",
    ] {
        match repo.private_dir().metadata_at(OsStr::new(marker), false) {
            Ok(_) => return Err(WorktreeError::GitOperationInProgress),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    repo.revalidate()?;
    Ok(())
}

/// A gitlink only runs a nested clean filter when its checkout exists. An
/// uninitialised gitlink has no `.git` marker, so status cannot follow it into
/// lane-controlled configuration and the tree can be reported as it is.
pub(super) fn initialised_gitlink(root: &Path, path: &[u8]) -> Result<bool, WorktreeError> {
    let relative = Path::new(OsStr::from_bytes(path));
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(WorktreeError::InvalidOutput);
    }
    match std::fs::symlink_metadata(root.join(relative).join(".git")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(WorktreeError::UnknownWriter),
    }
}

const DIRTY_PATHS_LISTED: usize = 12;
const DIRTY_SUMMARY_BYTES: usize = 1024;

pub(super) fn dirty_paths_summary(paths: &[StatusPath]) -> String {
    let raw: Vec<&[u8]> = paths
        .iter()
        .flat_map(|path| std::iter::once(path.path.as_slice()).chain(path.original_path.as_deref()))
        .collect();
    dirty_paths_summary_bytes(&raw)
}

fn dirty_paths_summary_bytes(paths: &[&[u8]]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for path in paths {
        let lossy = String::from_utf8_lossy(path).into_owned();
        if seen.insert(lossy.clone()) {
            unique.push(lossy);
        }
    }
    // List whole names only, within the count and byte budgets, so the
    // omission count is never cut off. The first name is always listed,
    // shortened only if it alone exceeds the budget.
    let mut summary = String::new();
    let mut listed = 0;
    for name in unique.iter().take(DIRTY_PATHS_LISTED) {
        let separator = if listed == 0 { "" } else { ", " };
        if listed > 0 && summary.len() + separator.len() + name.len() > DIRTY_SUMMARY_BYTES {
            break;
        }
        summary.push_str(separator);
        summary.push_str(name);
        listed += 1;
    }
    summary.truncate(summary.floor_char_boundary(DIRTY_SUMMARY_BYTES));
    let more = unique.len() - listed;
    if more > 0 {
        summary.push_str(&format!(" and {more} more"));
    }
    summary
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WorktreeCleanliness {
    Clean,
    Ignored,
    Dirty(String),
    GitOperationInProgress,
    Unknown,
}

pub(super) fn worktree_cleanliness_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCleanliness, WorktreeError> {
    match check_writer_terminal(repo) {
        Ok(()) => {}
        Err(WorktreeError::GitOperationInProgress) => {
            return Ok(WorktreeCleanliness::GitOperationInProgress);
        }
        Err(error) => return Err(error),
    }
    let index = run_git(
        context,
        repo,
        GitRead::IndexEntries { paths: Vec::new() },
        cancel,
    )?;
    if exit_code(&index)? != 0 {
        return Ok(WorktreeCleanliness::Unknown);
    }
    for entry in
        parse_index_entries(&index.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
        if entry.mode == 0o160000 && initialised_gitlink(repo.root_path(), entry.path)? {
            return Ok(WorktreeCleanliness::Unknown);
        }
    }
    let outcome = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Ok(WorktreeCleanliness::Unknown);
    }
    let records =
        parse_porcelain_v2_z(&outcome.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    if records.is_empty() {
        return Ok(WorktreeCleanliness::Clean);
    }
    let dirty_records: Vec<_> = records
        .into_iter()
        .filter(|record| record.kind != StatusKind::Ignored)
        .collect();
    if dirty_records.is_empty() {
        Ok(WorktreeCleanliness::Ignored)
    } else {
        Ok(WorktreeCleanliness::Dirty(dirty_paths_summary(
            &dirty_records,
        )))
    }
}

pub(super) fn worktree_dirty_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCondition, WorktreeError> {
    Ok(match worktree_cleanliness_at(context, repo, cancel)? {
        WorktreeCleanliness::Clean => WorktreeCondition::Clean,
        WorktreeCleanliness::Ignored => WorktreeCondition::Ignored,
        WorktreeCleanliness::Dirty(_) => WorktreeCondition::Dirty,
        WorktreeCleanliness::GitOperationInProgress => WorktreeCondition::GitOperationInProgress,
        WorktreeCleanliness::Unknown => WorktreeCondition::Unknown,
    })
}

pub(super) struct LockedWorktree {
    pub(super) verified: VerifiedWorktree,
    pub(super) writer: Option<LockGuard>,
}

/// Canonical paths locate exclusion slots, never identity records. Keep slots
/// after removal so concurrent callers cannot lock different inodes.
pub(super) fn writer_directory(
    project_state: &Directory,
    path: &Path,
) -> Result<Directory, WorktreeError> {
    let name = state_dir_name(path);
    match project_state.mkdir(&name, 0o700) {
        Ok(()) => project_state.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(project_state.open_verified_directory(&name)?)
}

/// Verify a registered worktree and probe its own effective Git configuration,
/// so every host Git operation on it neutralises what lanes can influence
/// there (an `onbranch:` include of a lane-writable file, for one).
pub(super) fn verify_probed_worktree(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<VerifiedWorktree, WorktreeError> {
    let mut tree = verify_worktree(context.repo, path)?;
    context
        .utility
        .probe_worktree(context.executor, &mut tree, context.utility_budget, cancel)?;
    Ok(tree)
}

/// Git's inventory and linked-worktree verification establish membership.
/// Supervisor keeps per-attempt process evidence in the exclusion slot.
pub fn writer_custody(
    project_state: &Directory,
    worktree: &VerifiedWorktree,
    branch: &GitRefName,
    base: GitOid,
    worktree_list: impl FnOnce() -> Result<Vec<u8>, WorktreeError>,
) -> Result<(Directory, WriterBinding), WorktreeError> {
    worktree.repo().revalidate()?;
    let canonical = canonicalize(worktree.path())?;
    let entries = parse_worktree_list(&worktree_list()?)?;
    let entry = entries
        .iter()
        .find(|entry| !entry.bare && canonicalize(&entry.path).ok().as_deref() == Some(&canonical))
        .ok_or(WorktreeError::NotFound)?;
    if entry.locked || entry.prunable {
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    if entry.branch.as_ref() != Some(branch) {
        return Err(WorktreeError::IdentityChanged);
    }
    let state = writer_directory(project_state, &canonical)?;
    let binding = WriterBinding::from_verified(
        worktree.repo().root_dir(),
        worktree.repo().private_dir(),
        branch.as_str(),
        base,
    )
    .map_err(|_| WorktreeError::IdentityChanged)?;
    Ok((state, binding))
}

pub(super) fn lock_worktree_for_mutation(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
) -> Result<LockedWorktree, WorktreeError> {
    lock_worktree(context, entry, cancel, false)
}

pub(super) fn lock_worktree(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
    preview: bool,
) -> Result<LockedWorktree, WorktreeError> {
    if entry.locked || entry.prunable {
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    let verified = verify_probed_worktree(context, &entry.path, cancel)?;
    let writer = if preview {
        cache_writer_probe(context.project_state, verified.path()).map_err(|state| match state {
            WorkerState::Busy => WorktreeError::LockBusy,
            _ => WorktreeError::UnknownWriter,
        })?
    } else {
        let state = writer_directory(context.project_state, verified.path())?;
        let writer = match state.lock(
            OsStr::new(WRITER_LOCK),
            LockMode::Exclusive,
            WaitBudget::nonblocking(context.clock.mono_now()),
            cancel,
            context.clock,
        ) {
            Ok(guard) => guard,
            Err(FsError::LockTimeout { .. }) => return Err(WorktreeError::LockBusy),
            Err(FsError::Cancelled { .. }) => return Err(WorktreeError::Cancelled),
            Err(_) => return Err(WorktreeError::UnknownWriter),
        };
        Some(writer)
    };
    let canonical = canonicalize(verified.path())?;
    if !preview {
        context.workers.invalidate();
    }
    let (observations, details) = context
        .workers
        .observe_roots_with_details(std::slice::from_ref(&canonical));
    // The writer lock establishes exclusion.
    // A census gap does not make a free lease live; only a positive
    // observation of a live process refuses. Landing also needs this root
    // observed free, which a process the kernel refuses to describe does not
    // prevent.
    match observations
        .get(&canonical)
        .copied()
        .unwrap_or(WorkerState::Unknown)
    {
        WorkerState::Unknown if context.workers.requires_complete_census() => {
            Err(WorktreeError::IncompleteCensus {
                detail: details
                    .get(&canonical)
                    .cloned()
                    .unwrap_or_else(|| context.workers.incomplete_census_detail()),
            })
        }
        WorkerState::Free | WorkerState::Unknown => Ok(LockedWorktree { verified, writer }),
        WorkerState::Live => Err(live_refusal(context.workers, &canonical)),
        WorkerState::Busy => Err(WorktreeError::LockBusy),
    }
}

pub(super) fn repository_lock<'a>(
    context: &WorktreeContext<'a>,
    mode: LockMode,
    cancel: &CancelToken,
) -> Result<LockGuard, WorktreeError> {
    context
        .project_state
        .lock(
            OsStr::new(REPOSITORY_LOCK),
            mode,
            context.lock_budget,
            cancel,
            context.clock,
        )
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => WorktreeError::RegistryLockTimeout,
            other => other.into(),
        })
}

pub(super) fn validate_binding(context: &WorktreeContext<'_>) -> Result<(), WorktreeError> {
    if context.binding.common_dir() != context.repo.common_git_dir() {
        return Err(RepoError::InvalidRegistration.into());
    }
    Ok(())
}

pub(crate) fn combine_worker_state(state: WorkerState, native: WorkerState) -> WriterState {
    match (state, native) {
        // A held lease is authoritative even when the census also sees a
        // process. Preserve contention so callers can name the lease holder.
        (WorkerState::Busy, _) => WriterState::Busy,
        (WorkerState::Live, _) | (_, WorkerState::Live) => WriterState::Live,
        (_, WorkerState::Busy) => WriterState::Busy,
        (WorkerState::Unknown, _) | (_, WorkerState::Unknown) => WriterState::Unknown,
        (WorkerState::Free, WorkerState::Free) => WriterState::Free,
    }
}

/// Observe exclusion without creating state during listing or disk reports.
pub(crate) fn disk_writer_state(project_state: &Directory, path: &Path) -> WorkerState {
    match cache_writer_probe(project_state, path) {
        Ok(_) => WorkerState::Free,
        Err(state) => state,
    }
}

/// Probe an existing exclusion slot without creating state during preview.
pub fn cache_writer_probe(
    project_state: &Directory,
    path: &Path,
) -> Result<Option<LockGuard>, WorkerState> {
    let directory = match project_state.open_verified_directory(&state_dir_name(path)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(_) => return Err(WorkerState::Unknown),
    };
    match directory.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive) {
        Ok(guard) => Ok(Some(guard)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(FsError::LockTimeout { .. }) => Err(WorkerState::Busy),
        Err(_) => Err(WorkerState::Unknown),
    }
}

/// Hold writer exclusion for cache mutation. Membership comes from the
/// caller's Git inventory; the slot supplies only exclusion.
pub fn cache_writer_guard(
    project_state: &Directory,
    path: &Path,
) -> Result<LockGuard, WorkerState> {
    let directory = writer_directory(project_state, path).map_err(|_| WorkerState::Unknown)?;
    directory
        .try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => WorkerState::Busy,
            _ => WorkerState::Unknown,
        })
}

#[cfg(test)]
mod dirty_tests {
    use super::*;

    #[test]
    fn summary_includes_both_rename_paths() {
        let paths = [StatusPath {
            kind: StatusKind::RenameOrCopy,
            path: b"new name".to_vec(),
            original_path: Some(b"old name".to_vec()),
        }];
        assert_eq!(dirty_paths_summary(&paths), "new name, old name");
    }

    #[test]
    fn summary_lists_dirty_paths_and_counts_the_rest() {
        assert_eq!(dirty_paths_summary_bytes(&[]), "");
        let one = dirty_paths_summary_bytes(&[b"a.rs", b"b/c.rs"]);
        assert_eq!(one, "a.rs, b/c.rs");
        let dedup = dirty_paths_summary_bytes(&[b"a.rs", b"a.rs", b"b.rs"]);
        assert_eq!(dedup, "a.rs, b.rs");
        let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n}").into_bytes()).collect();
        let many_refs: Vec<&[u8]> = many.iter().map(Vec::as_slice).collect();
        let summary = dirty_paths_summary_bytes(&many_refs);
        assert!(
            summary.contains("f11") && !summary.contains("f12"),
            "{summary}"
        );
        assert!(summary.ends_with(" and 3 more"), "{summary}");
        let long = vec![b'x'; 4000];
        let long_summary = dirty_paths_summary_bytes(&[&long]);
        assert!(long_summary.len() <= 1024);
        // Long names that overflow the byte budget are left whole and counted.
        let wide: Vec<Vec<u8>> = (0..12)
            .map(|n| format!("{n:02}{}", "y".repeat(200)).into_bytes())
            .collect();
        let wide_refs: Vec<&[u8]> = wide.iter().map(Vec::as_slice).collect();
        let summary = dirty_paths_summary_bytes(&wide_refs);
        let (names, rest) = summary.split_once(" and ").expect("omission count");
        let listed: Vec<_> = names.split(", ").collect();
        assert!(listed.iter().all(|name| name.len() == 202), "{summary}");
        assert_eq!(rest, format!("{} more", 12 - listed.len()), "{summary}");
    }
}
