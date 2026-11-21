//! Writer custody, locking and terminal checks.
use super::*;

const INCARNATION: &str = "provenant-custody-generation";

/// Observe the immutable checkout incarnation without changing it. Missing
/// identity is explicit; a path-only subject can never alias retired custody.
pub fn worktree_subject(
    repo: &VerifiedRepo,
) -> Result<provenant_records::custody::Subject, WorktreeError> {
    repo.revalidate()?;
    let bytes = repo
        .private_dir()
        .read_stable(OsStr::new(INCARNATION), 256)?;
    let generation = std::str::from_utf8(&bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let path = repo
        .root_path()
        .to_str()
        .ok_or(WorktreeError::InvalidRequest)?;
    let subject = provenant_records::custody::Subject::worktree(path, generation);
    if subject.worktree_identity().is_none() {
        return Err(WorktreeError::InvalidOutput);
    }
    repo.revalidate()?;
    Ok(subject)
}

/// Publish once in the private Git directory. Removing and recreating a linked
/// checkout removes this marker, so even the same path receives fresh custody.
/// No-replace publication lets concurrent observers share one generation.
pub fn acquire_worktree_subject(
    repo: &VerifiedRepo,
) -> Result<provenant_records::custody::Subject, WorktreeError> {
    repo.revalidate()?;
    match repo
        .private_dir()
        .metadata_at(OsStr::new(INCARNATION), false)
    {
        Ok(_) => {
            repo.private_dir().sync()?;
            return worktree_subject(repo);
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    let generation = provenant_os::fs::temporary_name()?;
    match repo.private_dir().atomic_write(
        OsStr::new(INCARNATION),
        ".custody-",
        generation.as_bytes(),
        PublishMode::NoReplace,
    ) {
        Ok(publication) => publication
            .require_durable()
            .map_err(|_| WorktreeError::RegistrationUnconfirmed)?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    repo.private_dir().sync()?;
    worktree_subject(repo)
}

/// Native worktree mutations and removal require completed Git operations.
/// Index and working-tree cleanliness alone do not prove completion.
pub fn check_writer_terminal(repo: &VerifiedRepo) -> Result<(), WorktreeError> {
    check_writer_terminal_with_expected_merge(repo, None)
}

/// Accept only the explicitly expected merge marker. Other Git operations
/// remain unfinished.
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
    check_git_operation_terminal(repo.private_dir(), expected_merge)?;
    repo.revalidate()?;
    Ok(())
}

/// Inspect the Git state owned by a writer, including an attempt-private view.
pub fn check_git_operation_terminal(
    private: &Directory,
    expected_merge: Option<&[u8]>,
) -> Result<(), WorktreeError> {
    for marker in [
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "sequencer",
        "BISECT_START",
    ] {
        if marker == "MERGE_HEAD"
            && let Some(expected) = expected_merge
        {
            let actual = private.read_stable(OsStr::new(marker), 1024 * 1024)?;
            if actual != expected {
                return Err(WorktreeError::GitOperationInProgress);
            }
            continue;
        }
        match private.metadata_at(OsStr::new(marker), false) {
            Ok(_) => return Err(WorktreeError::GitOperationInProgress),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
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
        &mut evidence,
        cancel,
    )?;
    Ok(evidence)
}

pub(super) fn verify_removal_submodules(
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
        &mut evidence,
        cancel,
    )?;
    Ok(evidence)
}

#[allow(clippy::too_many_arguments)]
fn inspect_submodules(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    removed_root: &Path,
    removed_admin: &Path,
    depth: usize,
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
        check_writer_terminal(child)?;
        inspect_submodules(
            context,
            child,
            removed_root,
            removed_admin,
            depth + 1,
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
            let command = read.command_label();
            let refs = run_git(context, child, read, cancel)?;
            if exit_code(&refs)? != 0 {
                return Err(unknown("submodule retained refs are unavailable"));
            }
            references.extend(parse_branch_tips(&refs.stdout.bytes).map_err(|detail| {
                git_read_failure(&command, &refs, format!("output: {detail}"))
            })?);
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
            LandingAssessment::Proven(proof) => evidence.push(SubmoduleEvidence {
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
    dirty_paths_summary_bytes(&raw).text
}

struct DirtyPathsSummary {
    text: String,
    #[cfg(test)]
    listed: usize,
    #[cfg(test)]
    omitted: usize,
    #[cfg(test)]
    total: usize,
}

fn dirty_paths_summary_bytes(paths: &[&[u8]]) -> DirtyPathsSummary {
    if paths.is_empty() {
        return DirtyPathsSummary {
            text: String::new(),
            #[cfg(test)]
            listed: 0,
            #[cfg(test)]
            omitted: 0,
            #[cfg(test)]
            total: 0,
        };
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
    // Reserve room for the widest omission suffix so the whole text, suffix
    // included, stays within the byte budget.
    let reserve = if unique.len() > 1 {
        format!(" and {} more", unique.len()).len()
    } else {
        0
    };
    let budget = DIRTY_SUMMARY_BYTES.saturating_sub(reserve);
    let mut summary = String::new();
    let mut listed = 0;
    for name in unique.iter().take(DIRTY_PATHS_LISTED) {
        let separator = if listed == 0 { "" } else { ", " };
        if listed > 0 && summary.len() + separator.len() + name.len() > budget {
            break;
        }
        summary.push_str(separator);
        summary.push_str(name);
        listed += 1;
    }
    summary.truncate(summary.floor_char_boundary(budget));
    let more = unique.len() - listed;
    if more > 0 {
        summary.push_str(&format!(" and {more} more"));
    }
    DirtyPathsSummary {
        text: summary,
        #[cfg(test)]
        listed,
        #[cfg(test)]
        omitted: more,
        #[cfg(test)]
        total: unique.len(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WorktreeCleanliness {
    Clean,
    Ignored,
    Dirty(String, bool),
    GitOperationInProgress,
    Unknown,
}

pub(super) fn worktree_cleanliness_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCleanliness, WorktreeError> {
    let cleanliness = native_worktree_cleanliness_at(context, repo, cancel)?;
    require_attempt_indexes_clean(context, repo, cancel)?;
    Ok(cleanliness)
}

fn native_worktree_cleanliness_at(
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
        let protected = dirty_records.iter().any(|record| {
            std::iter::once(record.path.as_slice())
                .chain(record.original_path.as_deref())
                .any(|path| writer_edit_is_protected(&String::from_utf8_lossy(path)))
        });
        Ok(WorktreeCleanliness::Dirty(
            dirty_paths_summary(&dirty_records),
            protected,
        ))
    }
}

/// Run only after the caller has exclusive worktree occupancy. A native index
/// can be clean while a crashed or unreconciled attempt still has staged paths
/// in its private index, so removal must account for every durable record.
fn require_attempt_indexes_clean(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    let path = repo.root_path().to_path_buf();
    let slot_name = state_dir_name(repo.root_path());
    let slot = match context.project_state.open_verified_directory(&slot_name) {
        Ok(slot) => slot,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(_) => {
            return Err(private_attempt_index_unknown(
                &path,
                "private attempt index custody could not be opened",
            ));
        }
    };
    let records = provenant_supervisor::custody::recorded_attempt_indexes(&slot).map_err(|_| {
        private_attempt_index_unknown(&path, "private attempt index custody could not be verified")
    })?;
    for record in records {
        if let Err(error) = check_git_operation_terminal(&record.refs, None) {
            return match error {
                WorktreeError::GitOperationInProgress => Err(WorktreeError::Dirty(format!(
                    "attempt {} has an unfinished Git operation",
                    record.scope.attempt
                ))),
                error => Err(error),
            };
        }

        let head =
            provenant_os::git::private_branch_tip(&record.refs, &record.branch).map_err(|_| {
                private_attempt_index_unknown(
                    &path,
                    "private attempt branch tip could not be verified",
                )
            })?;
        record
            .refs
            .read_stable(OsStr::new("index"), 64 * 1024 * 1024)
            .map_err(|_| {
                private_attempt_index_unknown(&path, "private attempt index could not be read")
            })?;
        let index = record
            .refs
            .current_path()
            .map_err(|_| {
                private_attempt_index_unknown(&path, "private attempt index path changed")
            })?
            .join("index");
        let outcome = run_git(context, repo, GitRead::AttemptIndex { index, head }, cancel)
            .map_err(|_| {
                private_attempt_index_unknown(&path, "private attempt index could not be compared")
            })?;
        match exit_code(&outcome).map_err(|_| {
            private_attempt_index_unknown(&path, "private attempt index result was incomplete")
        })? {
            0 | 1 => {}
            _ => {
                return Err(private_attempt_index_unknown(
                    &path,
                    "private attempt branch objects are unavailable",
                ));
            }
        }
        let staged: Vec<_> = outcome
            .stdout
            .bytes
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .collect();
        if !staged.is_empty() {
            let summary = dirty_paths_summary_bytes(&staged).text;
            return Err(WorktreeError::Dirty(format!(
                "attempt {} staged paths: {summary}",
                record.scope.attempt
            )));
        }
    }
    Ok(())
}

fn private_attempt_index_unknown(path: &Path, detail: &str) -> WorktreeError {
    WorktreeError::UnknownOccupancy {
        path: path.to_path_buf(),
        detail: detail.to_owned(),
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
        WorktreeCleanliness::Dirty(paths, _) => Err(WorktreeError::Dirty(paths)),
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
    Ok(
        match native_worktree_cleanliness_at(context, repo, cancel)? {
            WorktreeCleanliness::Clean => WorktreeCondition::Clean,
            WorktreeCleanliness::Ignored => WorktreeCondition::Ignored,
            WorktreeCleanliness::Dirty(..) => WorktreeCondition::Dirty,
            WorktreeCleanliness::GitOperationInProgress => {
                WorktreeCondition::GitOperationInProgress
            }
            WorktreeCleanliness::Unknown => WorktreeCondition::Unknown,
        },
    )
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
    writer_directory_inner(project_state, path, true)
}

fn writer_directory_inner(
    project_state: &Directory,
    path: &Path,
    observe: bool,
) -> Result<Directory, WorktreeError> {
    let name = state_dir_name(path);
    match project_state.open_verified_directory(&name) {
        Ok(directory) => {
            if observe {
                drop(
                    provenant_supervisor::custody::writer_observation_guard(&directory)
                        .map_err(|_| WorktreeError::UnknownWriter)?,
                );
            }
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
    if observe {
        drop(
            provenant_supervisor::custody::writer_observation_guard(&directory)
                .map_err(|_| WorktreeError::UnknownWriter)?,
        );
    }
    Ok(directory)
}

/// Name the Git operation left in progress in a worktree, for advisory
/// stranded-work reports. Only the same markers the terminal check uses are
/// inspected; no operation content is read and nothing is changed.
pub fn in_progress_git_operation(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<Option<&'static str>, WorktreeError> {
    let tree = verify_probed_worktree(context, path, cancel)?;
    let private = tree.repo().private_dir();
    for (marker, label) in [
        ("MERGE_HEAD", "merge"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
        ("sequencer", "sequencer"),
        ("BISECT_START", "bisect"),
    ] {
        match private.metadata_at(OsStr::new(marker), false) {
            Ok(_) => return Ok(Some(label)),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
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
    // A registered checkout may have been created directly by Git. Give it
    // the same durable incarnation identity as a Provenant-created tree
    // before deriving the writer's shared Git-state authority.
    acquire_worktree_subject(worktree.repo())?;
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

/// Launch admission reobserves occupancy after taking worktree custody.
pub(super) fn lock_worktree_for_launch(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    cancel: &CancelToken,
) -> Result<LockedWorktree, WorktreeError> {
    lock_worktree_impl(context, entry, cancel, false, true)
        .map_err(|error| name_writer_lease(context.project_state, &entry.path, error))
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
            // Observation probes already treat reservations as durable
            // recovery evidence. Repeat that fence after taking exclusive
            // custody so a crashed writer cannot be mistaken for a clean tree
            // by a mutating operation such as worktree removal.
            outstanding_reservation(&state, verified.path()).map_err(custody_refusal)?;
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
            Effect::Refuse(_) | Effect::Stop(_) => {
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

pub(super) fn validate_binding(context: &WorktreeContext<'_>) -> Result<(), WorktreeError> {
    if context.binding.common_dir() != context.repo.common_git_dir() {
        return Err(RepoError::InvalidRegistration.into());
    }
    let layout_identity = context.layout.directory().metadata()?;
    if let Some(project) = context.layout.project() {
        let owner = VerifiedRepo::open_structure_only(project)?;
        if owner.private_git_dir() != context.binding.common_dir()
            || owner.common_git_dir() != context.binding.common_dir()
            || !owner
                .common_identity()
                .same_identity(&context.repo.common_identity())
            || !owner.root_identity().same_identity(&layout_identity)
        {
            return Err(RepoError::InvalidRegistration.into());
        }
    } else if context.layout.root() != context.binding.common_dir()
        || !layout_identity.same_identity(&context.repo.common_identity())
    {
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

/// Attach the durable holder to a writer-bind refusal. A missing or ambiguous
/// reservation remains the original refusal, never guessed ownership.
pub fn name_writer_lease(
    project_state: &Directory,
    path: &Path,
    error: WorktreeError,
) -> WorktreeError {
    if !matches!(
        error,
        WorktreeError::LockBusy
            | WorktreeError::UnknownWriter
            | WorktreeError::UnknownOccupancy { .. }
            | WorktreeError::WriterBusy { .. }
    ) {
        return error;
    }
    let Ok(slot) = project_state.open_verified_directory(&state_dir_name(path)) else {
        return error;
    };
    // An unresolved import is distinct from a held lease and must retain its
    // recovery guidance even when its reservation identifies one attempt.
    if retained_import(&slot).is_err() {
        return error;
    }
    let Ok(listing) = provenant_supervisor::custody::list_writer_reservations_in_slot(&slot) else {
        return error;
    };
    if !listing.issues.is_empty() || listing.reservations.len() != 1 {
        return error;
    }
    let reservation = &listing.reservations[0];
    WorktreeError::WriterLeaseHeld {
        holder: reservation.scope.attempt,
        branch: reservation
            .branch
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        worktree: path.to_path_buf(),
        age: reservation.age.clone().unwrap_or_else(|| "unknown".into()),
        live: reservation.live,
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
        if let Err(state) = outstanding_reservation(&directory, path) {
            return state;
        }
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
            outstanding_reservation(&directory, path)?;
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
    outstanding_reservation(&directory, path)?;
    Ok(guard)
}

/// A reservation is durable recovery evidence even after its writer lock has
/// disappeared. Observation cannot decide whether it belongs to a live,
/// unattended, or currently unobservable writer, so it must never project the
/// slot as free or remove the evidence. Run-owned reconciliation settles it.
fn outstanding_reservation(slot: &Directory, path: &Path) -> Result<(), Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let listing = provenant_supervisor::custody::list_writer_reservations_in_slot(slot)
        .map_err(|error| unknown(format!("writer reservations could not be checked: {error}")))?;
    if let Some(issue) = listing.issues.first() {
        return Err(unknown(format!(
            "writer reservation evidence could not be verified at {}: {}; inspect the attempt and run `provenant reconcile` before retrying",
            issue.reservation.display(),
            issue.reason
        )));
    }
    if let Some(reservation) = listing.reservations.first() {
        let attempt = reservation.scope.attempt;
        return Err(unknown(format!(
            "writer attempt {attempt} has an unsettled reservation; run `provenant reconcile {attempt}` before retrying"
        )));
    }
    Ok(())
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

// A claim validates the published gate when it acquires it below. Looking at
// it here would turn the observer we intend to queue behind into a refusal.
pub(super) fn claim_writer_directory(
    project_state: &Directory,
    path: &Path,
) -> Result<Directory, WorktreeError> {
    writer_directory_inner(project_state, path, false)
}

/// Owner claims queue behind brief observations, but still refuse a live writer
/// immediately. A report's temporary probe is not writer custody (#3338).
pub(super) fn claim_writer_guard(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<LockGuard, WorktreeError> {
    claim_writer_guard_inner(context, path, cancel)
        .map_err(|error| name_writer_lease(context.project_state, path, error))
}

fn claim_writer_guard_inner(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<LockGuard, WorktreeError> {
    let directory = claim_writer_directory(context.project_state, path)?;
    let observation = match provenant_supervisor::custody::writer_observation_guard(&directory) {
        Ok(guard) => guard,
        Err(provenant_supervisor::custody::CustodyError::Busy) => directory
            .lock_existing(
                OsStr::new("writer-probe.lock"),
                LockMode::Exclusive,
                context.lock_budget,
                cancel,
                context.clock,
            )
            .map_err(|error| match error {
                FsError::LockTimeout { .. } => WorktreeError::LockBusy,
                FsError::Cancelled { .. } => WorktreeError::Cancelled,
                error => error.into(),
            })?,
        Err(error) => return Err(custody_refusal(observation_occupancy(path, error))),
    };
    cache_writer_guard_under_gate(&directory, path, observation).map_err(custody_refusal)
}

/// Hold writer exclusion for cache mutation. Membership comes from the
/// caller's Git inventory; the slot supplies only exclusion.
pub fn cache_writer_guard(project_state: &Directory, path: &Path) -> Result<LockGuard, Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let directory = writer_directory(project_state, path)
        .map_err(|error| unknown(format!("writer slot could not be opened: {error}")))?;
    let observation = provenant_supervisor::custody::writer_admission_guard(&directory)
        .map_err(|error| observation_occupancy(path, error))?;
    cache_writer_guard_under_gate(&directory, path, observation)
}

fn cache_writer_guard_under_gate(
    directory: &Directory,
    path: &Path,
    observation: LockGuard,
) -> Result<LockGuard, Occupancy> {
    let unknown = |detail: String| Occupancy::custody_unknown(path.to_path_buf(), detail);
    let guard = directory
        .try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => Occupancy::Held,
            error => unknown(format!("writer lock could not be acquired: {error}")),
        })?;
    drop(observation);
    retained_import(directory).map_err(unknown)?;
    outstanding_reservation(directory, path)?;
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
        let empty = dirty_paths_summary_bytes(&[]);
        assert!(empty.text.is_empty());
        assert_eq!((empty.listed, empty.omitted, empty.total), (0, 0, 0));
        let one = dirty_paths_summary_bytes(&[b"a.rs", b"b/c.rs"]);
        for path in ["a.rs", "b/c.rs"] {
            assert_eq!(one.text.matches(path).count(), 1, "{}", one.text);
        }
        let dedup = dirty_paths_summary_bytes(&[b"a.rs", b"a.rs", b"b.rs"]);
        for path in ["a.rs", "b.rs"] {
            assert_eq!(dedup.text.matches(path).count(), 1, "{}", dedup.text);
        }
        assert_eq!(dedup.total, 2);
        assert_eq!(dedup.listed + dedup.omitted, dedup.total);
        let many: Vec<Vec<u8>> = (0..15).map(|n| format!("f{n:02}").into_bytes()).collect();
        let many_refs: Vec<&[u8]> = many.iter().map(Vec::as_slice).collect();
        let summary = dirty_paths_summary_bytes(&many_refs);
        assert!(summary.text.len() <= DIRTY_SUMMARY_BYTES);
        assert_eq!(summary.total, many.len());
        assert_eq!(summary.listed + summary.omitted, summary.total);
        let wide: Vec<Vec<u8>> = (0..12)
            .map(|n| format!("{n:02}{}", "y".repeat(200)).into_bytes())
            .collect();
        let wide_refs: Vec<&[u8]> = wide.iter().map(Vec::as_slice).collect();
        let summary = dirty_paths_summary_bytes(&wide_refs);
        assert_eq!(summary.total, wide.len());
        assert_eq!(summary.listed + summary.omitted, summary.total);
    }
}
