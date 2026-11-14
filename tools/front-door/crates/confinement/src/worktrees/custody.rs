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

#[derive(Clone, Debug, Serialize)]
pub struct SubmoduleEvidence {
    pub common_git_dir: PathBuf,
    pub private_git_dir: PathBuf,
    pub path: PathBuf,
    pub head: GitOid,
    pub evidence: provenant_records::custody::LandedEvidence,
}

/// Only stores under this project's common Git directory are in cleanup scope.
/// Their refs/objects survive removal of a linked checkout. Every initialized
/// child must be clean and independently retained; detached unpushed heads keep.
pub fn submodule_evidence(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<Vec<SubmoduleEvidence>, WorktreeError> {
    let mut evidence = Vec::new();
    inspect_submodules(
        context,
        repo,
        repo.root_path(),
        repo.private_git_dir(),
        0,
        false,
        &mut Vec::new(),
        &mut evidence,
        cancel,
    )?;
    Ok(evidence)
}

pub(super) fn hold_submodules(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<(Vec<LockGuard>, Vec<SubmoduleEvidence>), WorktreeError> {
    let mut guards = Vec::new();
    let mut evidence = Vec::new();
    inspect_submodules(
        context,
        repo,
        repo.root_path(),
        repo.private_git_dir(),
        0,
        true,
        &mut guards,
        &mut evidence,
        cancel,
    )?;
    Ok((guards, evidence))
}

#[allow(clippy::too_many_arguments)]
fn inspect_submodules(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    removed_root: &Path,
    removed_admin: &Path,
    depth: usize,
    hold: bool,
    guards: &mut Vec<LockGuard>,
    evidence: &mut Vec<SubmoduleEvidence>,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    if depth >= 32 {
        return Err(WorktreeError::UnknownWriter);
    }
    let index = run_git(
        context,
        repo,
        GitRead::IndexEntries { paths: Vec::new() },
        cancel,
    )?;
    if exit_code(&index)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    for entry in
        parse_index_entries(&index.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
        if entry.mode != 0o160000 {
            continue;
        }
        let relative = Path::new(OsStr::from_bytes(entry.path));
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(WorktreeError::InvalidOutput);
        }
        let path = repo.root_path().join(relative);
        let unknown = |detail: &str| WorktreeError::UnknownOccupancy {
            path: path.clone(),
            detail: detail.into(),
        };
        let mut directory = repo.root_dir().try_clone()?;
        let mut absent = false;
        for component in relative.components() {
            match directory.open_verified_directory(component.as_os_str()) {
                Ok(child) => directory = child,
                Err(error) if error.is_not_found() => {
                    absent = true;
                    break;
                }
                Err(_) => {
                    return Err(unknown(
                        "submodule path cannot be observed without following links",
                    ));
                }
            }
        }
        if absent {
            continue;
        }
        match directory.metadata_at(OsStr::new(".git"), false) {
            Err(error) if error.is_not_found() => {
                // Git ignores submodule contents in parent status. An uninitialised
                // directory with manually placed files must therefore be kept.
                if directory.entries()?.next().is_some() {
                    return Err(unknown("uninitialised submodule contains untracked data"));
                }
                continue;
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let child = context.utility.open_read_repo(
            context.executor,
            directory.current_path()?,
            context.utility_budget,
            cancel,
        )?;
        let child = &child.0;
        if child.common_git_dir().starts_with(removed_root)
            || child.private_git_dir().starts_with(removed_root)
            || child.common_git_dir().starts_with(removed_admin)
            || child.private_git_dir().starts_with(removed_admin)
            || !child
                .common_git_dir()
                .starts_with(context.repo.common_git_dir())
        {
            return Err(unknown(
                "submodule Git custody is outside the current project's retained store",
            ));
        }
        if hold {
            let guard = lock_repository(
                child.common_dir(),
                LockMode::Shared,
                WaitBudget::nonblocking(context.clock.mono_now()),
                cancel,
                context.clock,
            )
            .map_err(|error| match error {
                FsError::LockTimeout { .. } => WorktreeError::LockBusy,
                error => error.into(),
            })?;
            guards.push(guard);
        }
        check_writer_terminal(child)?;
        inspect_submodules(
            context,
            child,
            removed_root,
            removed_admin,
            depth + 1,
            hold,
            guards,
            evidence,
            cancel,
        )?;
        let status = run_git(context, child, GitRead::StatusIncludingIgnored, cancel)?;
        if exit_code(&status)? != 0 {
            return Err(unknown("submodule status is unavailable"));
        }
        let status =
            parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
        if !status.is_empty() {
            return Err(unknown(
                "submodule has dirty, untracked or ignored working data",
            ));
        }
        let head = run_git(context, child, GitRead::RevParseHead, cancel)?;
        if exit_code(&head)? != 0 {
            return Err(unknown("submodule HEAD is unavailable"));
        }
        let head = parse_oid(trim_ascii(&head.stdout.bytes))?;
        let mut references = BTreeMap::new();
        for read in [GitRead::BranchTips, GitRead::RemoteBranchTips] {
            let refs = run_git(context, child, read, cancel)?;
            if exit_code(&refs)? != 0 {
                return Err(unknown("submodule retained refs are unavailable"));
            }
            references.extend(parse_branch_tips(&refs.stdout.bytes)?);
        }
        let references: Vec<_> = references
            .into_iter()
            .map(|(reference, commit)| EvidenceRef {
                reference,
                commit,
                integration: false,
            })
            .collect();
        match super::evidence::removal_evidence_in(context, child, head, &references, cancel)? {
            RemovalEvidence::Proven(proof) => evidence.push(SubmoduleEvidence {
                common_git_dir: child.common_git_dir().to_owned(),
                private_git_dir: child.private_git_dir().to_owned(),
                path: path.clone(),
                head,
                evidence: proof,
            }),
            _ => {
                return Err(unknown(
                    "submodule HEAD is not independently subsumed by a retained ref",
                ));
            }
        }
    }
    Ok(())
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
    submodule_evidence(context, repo, cancel)?;
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

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_WRITER_GATE_INITIALISATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Canonical paths locate exclusion slots, never identity records. Keep slots
/// after removal so concurrent callers cannot lock different inodes.
pub(super) fn writer_directory(
    project_state: &Directory,
    path: &Path,
) -> Result<Directory, WorktreeError> {
    let name = state_dir_name(path);
    match project_state.open_verified_directory(&name) {
        Ok(directory) => {
            drop(
                provenant_supervisor::custody::writer_observation_guard(&directory)
                    .map_err(|_| WorktreeError::UnknownWriter)?,
            );
            return Ok(directory);
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    // Publish only a fully initialised slot. A crash during setup leaves an
    // unpublished staging directory rather than a permanently unusable slot.
    let stage = provenant_os::fs::temporary_name()?;
    project_state.mkdir(&stage, 0o700)?;
    let staged = project_state.open_verified_directory(&stage)?;
    let payload = provenant_os::fs::PayloadTree::claim(project_state, &stage, &staged.metadata()?)?;
    let initialise = || -> Result<(), WorktreeError> {
        #[cfg(test)]
        if FAIL_WRITER_GATE_INITIALISATION.with(|failure| failure.replace(false)) {
            return Err(WorktreeError::UnknownWriter);
        }
        provenant_supervisor::custody::prepare_writer_observation(&staged)
            .map_err(|_| WorktreeError::UnknownWriter)?;
        staged.sync()?;
        Ok(())
    };
    if let Err(error) = initialise() {
        // Preserve the initial error if cleanup also fails. A hard crash can
        // leave this unpublished directory, but it contains no reservation.
        let _ = payload.unlink(OsStr::new("writer-probe.lock"));
        let _ = project_state.rmdir(&stage);
        return Err(error);
    }
    match project_state.rename_noreplace(&stage, project_state, &name) {
        Ok(()) => project_state.sync()?,
        Err(error) => {
            // This unpublished private tree cannot be a live writer slot.
            payload.unlink(OsStr::new("writer-probe.lock"))?;
            project_state.rmdir(&stage)?;
            project_state.sync()?;
            if !error.is_exists() {
                return Err(error.into());
            }
        }
    }
    let directory = project_state.open_verified_directory(&name)?;
    drop(
        provenant_supervisor::custody::writer_observation_guard(&directory)
            .map_err(|_| WorktreeError::UnknownWriter)?,
    );
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
///
/// `requested` is the persisted path the slot is later located from, without
/// inventorying slots. Custody is refused unless it is still the canonical
/// path: a component moved behind a symlink since the request was stored
/// would otherwise put the reservation where no reader looks.
pub fn writer_custody(
    project_state: &Directory,
    worktree: &VerifiedWorktree,
    requested: &Path,
    branch: &GitRefName,
    base: GitOid,
    worktree_list: impl FnOnce() -> Result<Vec<u8>, WorktreeError>,
) -> Result<(Directory, WriterBinding), WorktreeError> {
    worktree.repo().revalidate()?;
    let canonical = canonicalize(worktree.path())?;
    if canonical != requested {
        return Err(WorktreeError::IdentityChanged);
    }
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
    let writer =
        if preview {
            cache_writer_probe(context.project_state, verified.path()).map_err(custody_refusal)?
        } else {
            let state = writer_directory(context.project_state, verified.path())?;
            let observation = provenant_supervisor::custody::writer_admission_guard(&state)
                .map_err(|error| match error {
                    provenant_supervisor::custody::CustodyError::Busy => WorktreeError::LockBusy,
                    _ => WorktreeError::UnknownWriter,
                })?;
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
            retained_import(&state).map_err(|detail| WorktreeError::UnknownOccupancy {
                path: verified.path().to_path_buf(),
                detail,
            })?;
            Some(writer)
        };
    let canonical = canonicalize(verified.path())?;
    if !preview {
        context.workers.invalidate();
    }
    if reobserve {
        use provenant_records::policy::{Effect, Facts};
        let state = context.project_state.current_path()?;
        let host = state
            .parent()
            .and_then(Path::parent)
            .ok_or(WorktreeError::InvalidRequest)?;
        let policy = provenant_config::policy::read(
            host,
            &context.binding.project_id().to_string(),
            &Default::default(),
        );
        let (observed, _) = context
            .workers
            .observe_roots_with_details(std::slice::from_ref(&canonical));
        let effect = policy.check("WRITER_CENSUS", Facts {
            violated: !matches!(observed.get(&canonical), Some(Occupancy::Available)),
            detail: "process census reports an occupant or incomplete observation; exclusive writer lease is held".into(),
        });
        match effect {
            Effect::Proceed => {}
            Effect::Warn(message) => {
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
            }
            Effect::Refuse(_) => {
                return Err(live_refusal(context.workers, &canonical));
            }
        }
        if cancel.is_cancelled() {
            return Err(WorktreeError::Cancelled);
        }
        return Ok(LockedWorktree { verified, writer });
    }
    let (observations, details) = context
        .workers
        .observe_roots_with_details(std::slice::from_ref(&canonical));
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

pub(super) fn repository_lock<'a>(
    context: &WorktreeContext<'a>,
    mode: LockMode,
    cancel: &CancelToken,
) -> Result<LockGuard, WorktreeError> {
    lock_repository(
        context.repo.common_dir(),
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

/// Observe exclusion under the short probe gate. Writer acquisition waits for
/// this gate rather than mistaking the observer for a live writer.
pub(crate) fn disk_writer_state(project_state: &Directory, path: &Path) -> Occupancy {
    let directory = match project_state.open_verified_directory(&state_dir_name(path)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Occupancy::Available,
        Err(error) => return Occupancy::custody_unknown(path.to_path_buf(), error.to_string()),
    };
    // Retained imports remain fences even when no writer inode survived.
    if let Err(detail) = retained_import(&directory) {
        return Occupancy::custody_unknown(path.to_path_buf(), detail);
    }
    if directory
        .metadata_at(OsStr::new(WRITER_LOCK), false)
        .is_err_and(|error| error.is_not_found())
    {
        return Occupancy::Available;
    }
    let _probe = match provenant_supervisor::custody::writer_observation_guard(&directory) {
        Ok(guard) => guard,
        Err(provenant_supervisor::custody::CustodyError::Busy) => return Occupancy::Held,
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
            retained_import(&directory).map_err(unknown)?;
            return Ok(None);
        }
        Err(error) => return Err(unknown(error.to_string())),
        Ok(_) => provenant_supervisor::custody::writer_observation_guard(&directory)
            .map_err(|error| observation_occupancy(path, error))?,
    };
    let result = cache_writer_probe_under_gate(project_state, path);
    drop(gate);
    result
}

fn observation_occupancy(
    path: &Path,
    error: provenant_supervisor::custody::CustodyError,
) -> Occupancy {
    match error {
        provenant_supervisor::custody::CustodyError::Busy => Occupancy::Held,
        error => Occupancy::custody_unknown(path.to_path_buf(), error.to_string()),
    }
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
    retained_import(&directory).map_err(unknown)?;
    Ok(guard)
}

// Supervisor validates exact scoped settlement and run-owned release evidence.
// Observation never removes reservations or private Git.
fn retained_import(slot: &Directory) -> Result<(), String> {
    match provenant_supervisor::custody::pending_import(slot) {
        Ok(None) => Ok(()),
        Ok(Some(fence)) => Err(format!(
            "unresolved retained import fence: {}",
            provenant_supervisor::custody::CustodyError::ImportRetained {
                fence: Box::new(fence)
            }
        )),
        Err(error) => Err(format!(
            "retained import fence could not be checked: {error}"
        )),
    }
}

/// Hold writer exclusion for cache mutation. Membership comes from the
/// caller's Git inventory; the slot supplies only exclusion.
pub fn cache_writer_guard(project_state: &Directory, path: &Path) -> Result<LockGuard, Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let directory = writer_directory(project_state, path)
        .map_err(|error| unknown(format!("writer slot could not be opened: {error}")))?;
    let observation = provenant_supervisor::custody::writer_admission_guard(&directory)
        .map_err(|error| observation_occupancy(path, error))?;
    let guard = directory
        .try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => Occupancy::Held,
            error => unknown(format!("writer lock could not be acquired: {error}")),
        })?;
    drop(observation);
    retained_import(&directory).map_err(unknown)?;
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
