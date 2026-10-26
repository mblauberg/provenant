//! The GC worktree selection, shared by merged removal and declared output.
use super::*;
use provenant_supervisor::fs::{FileKind, PayloadTree, entry_names, temporary_name};

#[derive(Clone, Copy)]
pub struct WorktreeGcPolicy<'a> {
    pub upstream: Option<GitOid>,
    pub terminal: &'a dyn Fn(&Path) -> bool,
    pub no_live_lanes: &'a dyn Fn(&Path) -> bool,
    pub regenerable: &'a [String],
    pub keep: &'a [String],
    pub written_before_ns: i128,
    pub dry_run: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct WorktreeGcReport {
    pub worktrees: RemoveOutcome,
    pub output: Vec<OutputEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OutputEntry {
    pub path: PathBuf,
    pub removed: bool,
    pub reason: String,
}

/// Recheck each candidate under repository and writer custody. Failures keep
/// that entry, rather than aborting cleanup of unrelated worktrees.
pub fn collect(
    context: &WorktreeContext<'_>,
    lanes: &[WorktreeInfo],
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<WorktreeGcReport, WorktreeError> {
    validate_binding(context)?;
    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let entries = read_worktrees(context, cancel)?;
    let mut report = WorktreeGcReport::default();
    for lane in lanes {
        let Some(branch) = lane
            .branch
            .as_ref()
            .and_then(|name| GitRefName::new(name).ok())
        else {
            continue;
        };
        let Some(entry) = entries.iter().find(|entry| entry.path == lane.path) else {
            continue;
        };
        if lane.writer != WriterState::Free
            || entry.locked
            || entry.prunable
            || !(policy.no_live_lanes)(&lane.path)
        {
            retain(&mut report.worktrees, &branch, RetainReason::UnknownWriter);
            continue;
        }
        if !matches!(
            lane.dirty,
            WorktreeCondition::Clean | WorktreeCondition::Ignored
        ) {
            retain(&mut report.worktrees, &branch, RetainReason::Dirty);
            continue;
        }
        if !policy.regenerable.is_empty() && (policy.terminal)(&lane.path) {
            let mut prune = || -> Result<(), WorktreeError> {
                let locked = lock_worktree_for_mutation(context, entry, cancel)?;
                if !(policy.terminal)(&lane.path) {
                    return Err(WorktreeError::UnknownWriter);
                }
                if !matches!(
                    worktree_dirty_at(context, locked.verified.repo(), cancel)?,
                    WorktreeCondition::Clean | WorktreeCondition::Ignored
                ) {
                    return Err(WorktreeError::Dirty);
                }
                let root = locked.verified.repo().root_dir();
                let index = run_git(
                    context,
                    locked.verified.repo(),
                    GitRead::IndexEntries { paths: Vec::new() },
                    cancel,
                )?;
                if exit_code(&index)? != 0 {
                    return Err(WorktreeError::UnknownWriter);
                }
                let mut keep = policy.keep.to_vec();
                for entry in parse_index_entries(&index.stdout.bytes)
                    .map_err(|_| WorktreeError::InvalidOutput)?
                {
                    let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
                    keep.push(
                        std::str::from_utf8(entry.path)
                            .map_err(|_| WorktreeError::InvalidOutput)?
                            .to_owned(),
                    );
                }
                let output_policy = WorktreeGcPolicy {
                    keep: &keep,
                    ..*policy
                };
                walk_output(
                    root,
                    &lane.path,
                    Path::new(""),
                    &output_policy,
                    &mut report.output,
                    0,
                )?;
                Ok(())
            };
            if let Err(error) = prune() {
                retain(&mut report.worktrees, &branch, retain_reason(&error));
            }
        }
        let Some(upstream) = policy.upstream else {
            continue;
        };
        match is_ancestor(context, lane.head, upstream, cancel) {
            Ok(true) => {}
            Ok(false) => {
                retain(&mut report.worktrees, &branch, RetainReason::NotMerged);
                continue;
            }
            Err(error) => {
                retain(&mut report.worktrees, &branch, retain_reason(&error));
                continue;
            }
        }
        if policy.dry_run {
            // Ignored output is still protected by the strict removal owner.
            if lane.dirty == WorktreeCondition::Clean {
                report.worktrees.removed.push(lane.clone());
                report
                    .worktrees
                    .pruned_refs
                    .push(branch.short_name().to_owned());
            }
            continue;
        }
        match remove_tree_when(context, entry, lane.head, cancel, &|| {
            (policy.no_live_lanes)(&lane.path)
        }) {
            Ok(removed) => {
                report.worktrees.removed.push(removed.info.clone());
                settle_branch(
                    context,
                    &branch,
                    lane.head,
                    Disposition::Merged,
                    cancel,
                    &mut report.worktrees,
                )?;
            }
            Err(error) => retain(&mut report.worktrees, &branch, retain_reason(&error)),
        }
    }
    Ok(report)
}

fn walk_output(
    directory: &dyn SafeDir,
    root: &Path,
    relative: &Path,
    policy: &WorktreeGcPolicy<'_>,
    report: &mut Vec<OutputEntry>,
    depth: usize,
) -> Result<(), FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    for name in entry_names(directory, 100_000)? {
        if name == ".git" || name == ".worktrees" {
            continue;
        }
        let path = relative.join(&name);
        if policy.keep.iter().any(|keep| path.starts_with(keep)) {
            continue;
        }
        let mut prune = || -> Result<bool, FsError> {
            let meta = directory.metadata_at(&name, false)?;
            if meta.kind != FileKind::Directory {
                return Ok(false);
            }
            if policy
                .regenerable
                .iter()
                .any(|configured| name == OsStr::new(configured))
                && !policy
                    .keep
                    .iter()
                    .any(|keep| Path::new(keep).starts_with(&path))
            {
                prune_output(directory, &name, policy)?;
                Ok(true)
            } else {
                let child = directory.open_verified_directory(&name)?;
                walk_output(child.as_ref(), root, &path, policy, report, depth + 1)?;
                Ok(false)
            }
        };
        match prune() {
            Ok(true) => report.push(OutputEntry {
                path: root.join(path),
                removed: true,
                reason: if policy.dry_run {
                    "would_remove"
                } else {
                    "removed"
                }
                .into(),
            }),
            Ok(false) => {}
            Err(error) => report.push(OutputEntry {
                path: root.join(path),
                removed: false,
                reason: error.to_string(),
            }),
        }
    }
    Ok(())
}

fn newest(directory: &dyn SafeDir, depth: usize) -> Result<i128, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    let mut time = directory.metadata()?.modified_ns;
    for name in entry_names(directory, 100_000)? {
        let meta = directory.metadata_at(&name, false)?;
        time = time.max(meta.modified_ns);
        if meta.kind == FileKind::Directory {
            time = time.max(newest(
                directory.open_verified_directory(&name)?.as_ref(),
                depth + 1,
            )?);
        }
    }
    Ok(time)
}

fn prune_output(
    parent: &dyn SafeDir,
    name: &OsStr,
    policy: &WorktreeGcPolicy<'_>,
) -> Result<(), FsError> {
    let directory = parent.open_verified_directory(name)?;
    let identity = directory.metadata()?;
    if newest(directory.as_ref(), 0)? >= policy.written_before_ns {
        return Err(FsError::RaceRefused { name: name.into() });
    }
    if policy.dry_run {
        return Ok(());
    }
    let staging_name = temporary_name()?;
    parent.mkdir(&staging_name, 0o700)?;
    let staging = parent.open_verified_directory(&staging_name)?;
    let payload = PayloadTree::claim(parent, &staging_name, &staging.metadata()?)?;
    // A private parent grants payload disposal without changing the original
    // directory's permissions or following links in the output tree.
    if let Err(error) = parent.rename_noreplace_to(name, staging.as_ref(), OsStr::new("output")) {
        parent.rmdir(&staging_name)?;
        return Err(error);
    }
    let removed = || -> Result<(), FsError> {
        let detached = payload.child(OsStr::new("output"))?;
        if !identity.same_identity(&detached.metadata()?)
            || newest(detached.directory(), 0)? >= policy.written_before_ns
        {
            return Err(FsError::RaceRefused { name: name.into() });
        }
        remove_output(&detached, 0)?;
        payload.rmdir(OsStr::new("output"))
    };
    if let Err(error) = removed() {
        staging.rename_noreplace_to(OsStr::new("output"), parent, name)?;
        parent.rmdir(&staging_name)?;
        return Err(error);
    }
    parent.rmdir(&staging_name)?;
    parent.sync()
}

fn remove_output(tree: &PayloadTree, depth: usize) -> Result<(), FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    tree.make_writable()?;
    for name in entry_names(tree.directory(), 100_000)? {
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            remove_output(&tree.child(&name)?, depth + 1)?;
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(())
}
