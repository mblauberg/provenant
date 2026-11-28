//! Worktree cleanliness and submodule evidence for custody decisions.
use super::*;

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

/// The one dirtiness observation a launch or retry makes: a fresh
/// `git status --porcelain` reading, with ignored paths excluded. A recorded
/// private attempt index is a snapshot of an earlier attempt, retained across
/// stop, not an observation of this worktree; it must not refuse a new writer
/// whose native status is clean. Removal and GC, which destroy that private
/// Git, use [`worktree_cleanliness_with_attempt_indexes_at`] instead.
pub(super) fn worktree_cleanliness_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCleanliness, WorktreeError> {
    native_worktree_cleanliness_at(context, repo, cancel)
}

/// The removal verdict also accounts for every durable private attempt index:
/// removing the worktree destroys those indexes, so staged work invisible to
/// the native status must still retain the tree.
pub(super) fn worktree_cleanliness_with_attempt_indexes_at(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    cancel: &CancelToken,
) -> Result<WorktreeCleanliness, WorktreeError> {
    let cleanliness = worktree_cleanliness_at(context, repo, cancel)?;
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

#[cfg(test)]
#[path = "cleanliness_tests.rs"]
mod dirty_tests;
