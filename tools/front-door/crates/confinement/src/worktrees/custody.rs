//! Writer custody, locking and terminal checks.
use super::*;

/// A writer must pass this check before its terminal result is accepted.
/// Index and working-tree cleanliness do not prove a Git operation has ended.
pub fn check_writer_terminal(repo: &VerifiedRepo) -> Result<(), WorktreeError> {
    check_writer_terminal_with_expected_merge(repo, None)
}

/// Accept only the merge metadata restored from a verified private writer
/// view. Other Git operations remain terminal refusals.
pub fn check_writer_terminal_with_merge(
    repo: &VerifiedRepo,
    merge_head: &[u8],
) -> Result<(), WorktreeError> {
    check_writer_terminal_with_expected_merge(repo, Some(merge_head))
}

fn check_writer_terminal_with_expected_merge(
    repo: &VerifiedRepo,
    expected_merge: Option<&[u8]>,
) -> Result<(), WorktreeError> {
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
        if marker == "MERGE_HEAD"
            && let Some(expected) = expected_merge
        {
            let actual = repo
                .private_dir()
                .read_stable(OsStr::new(marker), 1024 * 1024)?;
            if actual != expected {
                return Err(WorktreeError::GitOperationInProgress);
            }
            continue;
        }
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

/// Recheck that a verified worktree is clean while the caller holds writer
/// custody. This probes the worktree's Git configuration before running status.
pub fn require_clean_worktree(
    context: &WorktreeContext<'_>,
    worktree: &mut VerifiedWorktree,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    context
        .utility
        .probe_worktree(context.executor, worktree, context.utility_budget, cancel)?;
    match worktree_cleanliness_at(context, worktree.repo(), cancel)? {
        WorktreeCleanliness::Clean => Ok(()),
        WorktreeCleanliness::Ignored => Err(WorktreeError::IgnoredPaths(String::new())),
        WorktreeCleanliness::Dirty(paths) => Err(WorktreeError::Dirty(paths)),
        WorktreeCleanliness::GitOperationInProgress => Err(WorktreeError::GitOperationInProgress),
        WorktreeCleanliness::Unknown => Err(WorktreeError::UnknownOccupancy {
            path: worktree.path().to_path_buf(),
            detail: "worktree cleanliness could not be verified".into(),
        }),
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
    let directory = project_state.open_verified_directory(&name)?;
    provenant_supervisor::custody::prepare_writer_observation(&directory)
        .map_err(|_| WorktreeError::UnknownWriter)?;
    Ok(directory)
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
    lock_worktree_impl(context, entry, cancel, preview, false)
}

/// Launch admission runs before taking the repository registry lock.
pub(super) fn lock_worktree_for_launch(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
) -> Result<LockedWorktree, WorktreeError> {
    lock_worktree_impl(context, entry, cancel, false, true)
}

fn lock_worktree_impl(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
    preview: bool,
    reobserve: bool,
) -> Result<LockedWorktree, WorktreeError> {
    if entry.locked || entry.prunable {
        return Err(WorktreeError::GitEntryHeld {
            locked: entry.locked,
        });
    }
    let verified = verify_probed_worktree(context, &entry.path, cancel)?;
    let writer = if preview {
        cache_writer_probe(context.project_state, verified.path()).map_err(custody_refusal)?
    } else {
        let state = writer_directory(context.project_state, verified.path())?;
        let observation = provenant_supervisor::custody::writer_admission_guard(&state)
            .map_err(|_| WorktreeError::UnknownWriter)?;
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
        drop(observation);
        match retained_import(&state) {
            Ok(true) => {
                return Err(WorktreeError::UnknownOccupancy {
                    path: verified.path().to_path_buf(),
                    detail: "unresolved retained import fence".into(),
                });
            }
            Ok(false) => {}
            Err(detail) => {
                return Err(WorktreeError::UnknownOccupancy {
                    path: verified.path().to_path_buf(),
                    detail,
                });
            }
        }
        Some(writer)
    };
    let canonical = canonicalize(verified.path())?;
    if !preview {
        context.workers.invalidate();
    }
    let (mut observations, details) = if reobserve {
        let keep_going = || {
            !cancel.is_cancelled()
                && context
                    .lock_budget
                    .remaining(context.clock.mono_now())
                    .as_micros()
                    > 0
        };
        let result = context
            .workers
            .observe_roots_while(std::slice::from_ref(&canonical), &keep_going);
        if cancel.is_cancelled() {
            return Err(WorktreeError::Cancelled);
        }
        result.ok_or(WorktreeError::LockBusy)?
    } else {
        context
            .workers
            .observe_roots_with_details(std::slice::from_ref(&canonical))
    };
    if reobserve && let Some(observed) = observations.get(&canonical).cloned() {
        let state = reobserve_held_path(
            context.workers,
            &canonical,
            observed,
            context.lock_budget,
            context.clock,
            cancel,
            &provenant_core::ThreadPause,
        )?;
        observations.insert(canonical.clone(), state);
    }
    // The writer lock establishes exclusion.
    // A census gap does not make a free lease live; only a positive
    // observation of a live process refuses. Landing also needs this root
    // observed free, which a process the kernel refuses to describe does not
    // prevent.
    match observations.get(&canonical) {
        Some(Occupancy::Unknown { evidence, .. }) if context.workers.requires_complete_census() => {
            Err(WorktreeError::IncompleteCensus {
                detail: details
                    .get(&canonical)
                    .cloned()
                    .unwrap_or_else(|| evidence.clone()),
            })
        }
        Some(Occupancy::Available | Occupancy::Unknown { .. }) => {
            Ok(LockedWorktree { verified, writer })
        }
        Some(Occupancy::Observed { .. }) if !reobserve => {
            Err(live_refusal(context.workers, &canonical))
        }
        Some(Occupancy::Observed { process }) => Err(process
            .clone()
            .map_or(WorktreeError::LiveWriter, WorktreeError::LiveProcess)),
        Some(Occupancy::Held) => Err(WorktreeError::LockBusy),
        None if context.workers.requires_complete_census() => {
            Err(WorktreeError::IncompleteCensus {
                detail: context.workers.incomplete_census_detail(),
            })
        }
        None => Ok(LockedWorktree { verified, writer }),
    }
}

/// Only complete evidence of a free root clears an observed holder. A gap,
/// cancellation or deadline cannot turn that positive evidence into custody.
fn reobserve_held_path(
    workers: &dyn WorkerObserver,
    root: &Path,
    mut observed: Occupancy,
    budget: WaitBudget,
    clock: &dyn Clock,
    cancel: &CancelToken,
    pause: &dyn provenant_core::Pause,
) -> Result<Occupancy, WorktreeError> {
    for _ in 0..3 {
        if !matches!(&observed, Occupancy::Observed { process: Some(process) }
            if process.kind == ProcessEvidenceKind::OtherHeldPath)
        {
            break;
        }
        if cancel.is_cancelled() {
            return Err(WorktreeError::Cancelled);
        }
        let remaining = budget.remaining(clock.mono_now()).as_micros();
        if remaining == 0 {
            break;
        }
        pause.wait(
            provenant_core::DurationMicros::from_micros(remaining.min(100_000)),
            cancel,
        );
        if cancel.is_cancelled() {
            return Err(WorktreeError::Cancelled);
        }
        let keep_going =
            || !cancel.is_cancelled() && budget.remaining(clock.mono_now()).as_micros() > 0;
        if !keep_going() {
            break;
        }
        workers.invalidate();
        let Some((states, _)) = workers.observe_roots_while(&[root.to_path_buf()], &keep_going)
        else {
            if cancel.is_cancelled() {
                return Err(WorktreeError::Cancelled);
            }
            break;
        };
        if cancel.is_cancelled() {
            return Err(WorktreeError::Cancelled);
        }
        if !keep_going() {
            break;
        }
        match states.get(root) {
            Some(state @ (Occupancy::Available | Occupancy::Observed { .. } | Occupancy::Held)) => {
                observed = state.clone()
            }
            Some(Occupancy::Unknown { .. }) | None => break,
        }
    }
    Ok(observed)
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

pub(crate) fn combine_occupancy(path: &Path, state: Occupancy, native: Occupancy) -> Occupancy {
    match (state, native) {
        (Occupancy::Held, _) => Occupancy::Held,
        (unknown @ Occupancy::Unknown { .. }, _) => unknown,
        (observed @ Occupancy::Observed { .. }, _) => observed,
        (Occupancy::Available, Occupancy::Observed { process }) => Occupancy::Observed { process },
        (Occupancy::Available, Occupancy::Held) => Occupancy::census_unknown(
            path.to_path_buf(),
            "census reported contention without a held writer lease",
        ),
        (Occupancy::Available, Occupancy::Unknown { evidence, .. }) => {
            Occupancy::census_unknown(path.to_path_buf(), evidence)
        }
        (Occupancy::Available, Occupancy::Available) => Occupancy::Available,
    }
}

/// Convert a failed custody probe into the corresponding worktree refusal.
pub fn custody_refusal(state: Occupancy) -> WorktreeError {
    match state {
        Occupancy::Held => WorktreeError::LockBusy,
        Occupancy::Unknown {
            scope, evidence, ..
        } => WorktreeError::UnknownOccupancy {
            path: scope,
            detail: evidence,
        },
        Occupancy::Observed { .. } => WorktreeError::LiveWriter,
        Occupancy::Available => WorktreeError::UnknownWriter,
    }
}

/// Identify the attempt recorded in the held writer slot, when unambiguous.
pub fn writer_holder(project_state: &Directory, path: &Path) -> Option<provenant_core::AttemptId> {
    let slot = PathBuf::from(state_dir_name(path));
    let listing = provenant_supervisor::custody::list_writer_reservations(project_state).ok()?;
    let mut matches = listing
        .reservations
        .iter()
        .filter(|record| record.custody_directory() == Some(slot.as_path()));
    let holder = matches.next()?.scope.attempt;
    matches.next().is_none().then_some(holder)
}

/// Initialise a selected admission slot's observer gate, including legacy
/// slots. Read-only inventory never calls this mutating upgrade.
pub fn prepare_writer_probe(project_state: &Directory, path: &Path) -> Result<(), WorktreeError> {
    writer_directory(project_state, path).map(|_| ())
}

/// Observe exclusion under the short probe gate. Writer acquisition waits for
/// this gate rather than mistaking the observer for a live writer.
pub(crate) fn disk_writer_state(project_state: &Directory, path: &Path) -> Occupancy {
    let directory = match project_state.open_verified_directory(&state_dir_name(path)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Occupancy::Available,
        Err(error) => return Occupancy::custody_unknown(path.to_path_buf(), error.to_string()),
    };
    // Retained imports remain fences even when no writer inode survived.
    match retained_import(&directory) {
        Ok(true) => {
            return Occupancy::custody_unknown(
                path.to_path_buf(),
                "unresolved retained import fence",
            );
        }
        Ok(false) => {}
        Err(detail) => return Occupancy::custody_unknown(path.to_path_buf(), detail),
    }
    if directory
        .metadata_at(OsStr::new(WRITER_LOCK), false)
        .is_err_and(|error| error.is_not_found())
    {
        return Occupancy::Available;
    }
    let _probe = match provenant_supervisor::custody::writer_observation_guard(&directory) {
        Ok(guard) => guard,
        Err(error) => return Occupancy::custody_unknown(path.to_path_buf(), error.to_string()),
    };
    match cache_writer_probe_under_gate(project_state, path) {
        Ok(guard) => {
            drop(guard);
            Occupancy::Available
        }
        Err(state) => state,
    }
}

/// Probe an existing exclusion slot without creating state during preview.
pub fn cache_writer_probe(
    project_state: &Directory,
    path: &Path,
) -> Result<Option<LockGuard>, Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let directory = match project_state.open_verified_directory(&state_dir_name(path)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(unknown(error.to_string())),
    };
    // No inode to probe means no short-lived observer can conflict with us.
    // Still scan retained evidence in the ordinary raw helper.
    let gate = match directory.metadata_at(OsStr::new(WRITER_LOCK), false) {
        Err(error) if error.is_not_found() => {
            return match retained_import(&directory).map_err(unknown)? {
                true => Err(unknown("unresolved retained import fence".into())),
                false => Ok(None),
            };
        }
        Err(error) => return Err(unknown(error.to_string())),
        Ok(_) => provenant_supervisor::custody::writer_observation_guard(&directory)
            .map_err(|error| unknown(error.to_string()))?,
    };
    let result = cache_writer_probe_under_gate(project_state, path);
    drop(gate);
    result
}

fn cache_writer_probe_under_gate(
    project_state: &Directory,
    path: &Path,
) -> Result<Option<LockGuard>, Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let directory = match project_state.open_verified_directory(&state_dir_name(path)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(unknown(format!("writer slot could not be opened: {error}"))),
    };
    let guard = match directory.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive) {
        Ok(guard) => Some(guard),
        Err(error) if error.is_not_found() => None,
        Err(FsError::LockTimeout { .. }) => return Err(Occupancy::Held),
        Err(error) => {
            return Err(unknown(format!(
                "writer lock could not be checked: {error}"
            )));
        }
    };
    if retained_import(&directory).map_err(unknown)? {
        return Err(unknown("unresolved retained import fence".into()));
    }
    Ok(guard)
}

// An interrupted import retains both its reservation and marker after the
// process releases writer.lock. A resolved import may leave the marker as
// historical evidence, so only the pair fences the slot.
fn retained_import(slot: &Directory) -> Result<bool, String> {
    fn exists(dir: &Directory, name: &str) -> Result<bool, String> {
        match dir.metadata_at(OsStr::new(name), false) {
            Ok(_) => Ok(true),
            Err(error) if error.is_not_found() => Ok(false),
            Err(error) => Err(format!("retained fence could not be checked: {error}")),
        }
    }
    if exists(slot, "reservation.json")? && exists(slot, "import-retained.json")? {
        return Ok(true);
    }
    for (index, name) in slot
        .entries()
        .map_err(|error| format!("retained fences could not be listed: {error}"))?
        .enumerate()
    {
        if index >= 10_000 {
            return Err("retained fence scan exceeded its entry limit".into());
        }
        let name =
            name.map_err(|error| format!("retained fence entry could not be read: {error}"))?;
        let metadata = slot
            .metadata_at(&name, false)
            .map_err(|error| format!("retained fence entry could not be checked: {error}"))?;
        if metadata.kind != provenant_os::fs::FileKind::Directory {
            continue;
        }
        let attempt = slot
            .open_verified_directory(&name)
            .map_err(|error| format!("retained fence directory could not be opened: {error}"))?;
        if exists(&attempt, "reservation.json")? && exists(&attempt, "import-retained.json")? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Hold writer exclusion for cache mutation. Membership comes from the
/// caller's Git inventory; the slot supplies only exclusion.
pub fn cache_writer_guard(project_state: &Directory, path: &Path) -> Result<LockGuard, Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let directory = writer_directory(project_state, path)
        .map_err(|error| unknown(format!("writer slot could not be opened: {error}")))?;
    let observation = provenant_supervisor::custody::writer_admission_guard(&directory)
        .map_err(|error| unknown(error.to_string()))?;
    let guard = directory
        .try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => Occupancy::Held,
            error => unknown(format!("writer lock could not be acquired: {error}")),
        })?;
    drop(observation);
    if retained_import(&directory).map_err(unknown)? {
        return Err(unknown("unresolved retained import fence".into()));
    }
    Ok(guard)
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
        let summary = dirty_paths_summary(&paths);
        assert!(summary.contains("new name"));
        assert!(summary.contains("old name"));
    }

    #[test]
    fn summary_lists_dirty_paths_and_counts_the_rest() {
        assert_eq!(dirty_paths_summary_bytes(&[]), "");
        let one = dirty_paths_summary_bytes(&[b"a.rs", b"b/c.rs"]);
        for path in ["a.rs", "b/c.rs"] {
            assert_eq!(one.matches(path).count(), 1, "{one}");
        }
        let dedup = dirty_paths_summary_bytes(&[b"a.rs", b"a.rs", b"b.rs"]);
        for path in ["a.rs", "b.rs"] {
            assert_eq!(dedup.matches(path).count(), 1, "{dedup}");
        }
        let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n:02}").into_bytes()).collect();
        let many_refs: Vec<&[u8]> = many.iter().map(Vec::as_slice).collect();
        let summary = dirty_paths_summary_bytes(&many_refs);
        assert!(summary.len() <= 1024);
        let listed = (0..15)
            .filter(|n| summary.contains(&format!("f{n:02}")))
            .count();
        let omitted: usize = summary
            .split_whitespace()
            .rev()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(listed + omitted, many.len(), "{summary}");
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

#[cfg(test)]
mod retry_tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime, Pause, TimeError, WallTime};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct Time(AtomicU64);
    impl Clock for Time {
        fn wall_now(&self) -> Result<WallTime, TimeError> {
            WallTime::from_micros(0)
        }
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(self.0.load(Ordering::SeqCst))
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError> {
            self.0.store(deadline.as_micros(), Ordering::SeqCst);
            Ok(())
        }
    }
    impl Pause for Time {
        fn wait(&self, delay: DurationMicros, _: &CancelToken) {
            self.0.fetch_add(delay.as_micros(), Ordering::SeqCst);
        }
    }
    struct Retry<'a> {
        result: Option<Occupancy>,
        time: &'a Time,
        census_micros: u64,
        calls: AtomicUsize,
    }
    impl WorkerObserver for Retry<'_> {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.time.0.fetch_add(self.census_micros, Ordering::SeqCst);
            self.result
                .clone()
                .map(|state| (roots[0].clone(), state))
                .into_iter()
                .collect()
        }
    }
    fn holder() -> Occupancy {
        Occupancy::Observed {
            process: Some(ObservedProcess::new(
                42,
                Some("holder".into()),
                ProcessEvidenceKind::OtherHeldPath,
            )),
        }
    }

    #[test]
    fn held_path_requires_positive_free_evidence_after_retry() {
        let root = Path::new("/worktree");
        for result in [
            Some(Occupancy::census_unknown(root.into(), "incomplete census")),
            None,
            Some(holder()),
            Some(Occupancy::Available),
        ] {
            let time = Time(AtomicU64::new(0));
            let workers = Retry {
                result: result.clone(),
                time: &time,
                census_micros: 0,
                calls: AtomicUsize::new(0),
            };
            let budget = WaitBudget::up_to(time.mono_now(), DurationMicros::from_micros(1_000_000));
            let observed = reobserve_held_path(
                &workers,
                root,
                holder(),
                budget,
                &time,
                &CancelToken::new(),
                &time,
            )
            .unwrap();
            if result == Some(Occupancy::Available) {
                assert_eq!(observed, Occupancy::Available);
            } else {
                assert_eq!(observed, holder());
            }
        }
    }

    #[test]
    fn held_path_retry_bounds_pause_and_rejects_census_after_deadline() {
        for (budget_micros, census_micros, elapsed, calls) in [
            (0, 0, 0, 0),
            (50_000, 0, 50_000, 0),
            (150_000, 60_000, 160_000, 1),
            (150_000, 0, 100_000, 1),
        ] {
            let time = Time(AtomicU64::new(0));
            let workers = Retry {
                result: Some(Occupancy::Available),
                time: &time,
                census_micros,
                calls: AtomicUsize::new(0),
            };
            let budget =
                WaitBudget::up_to(time.mono_now(), DurationMicros::from_micros(budget_micros));
            let observed = reobserve_held_path(
                &workers,
                Path::new("/worktree"),
                holder(),
                budget,
                &time,
                &CancelToken::new(),
                &time,
            )
            .unwrap();
            assert_eq!(
                observed,
                if budget_micros > 100_000 && census_micros == 0 {
                    Occupancy::Available
                } else {
                    holder()
                }
            );
            assert_eq!(time.0.load(Ordering::SeqCst), elapsed);
            assert_eq!(workers.calls.load(Ordering::SeqCst), calls);
        }
    }
}
