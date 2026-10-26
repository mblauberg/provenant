//! The GC worktree selection, shared by merged removal and declared output.
use super::*;
use provenant_supervisor::fs::{FileKind, PayloadTree, entry_names, temporary_name};
use std::collections::HashSet;

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
    pub dry_run: bool,
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
    let entries = {
        let _repository = repository_lock(context, LockMode::Shared, cancel)?;
        read_worktrees(context, cancel)?
    };
    let primary = canonicalize(context.repo.root_path())?;
    let mut report = WorktreeGcReport {
        dry_run: policy.dry_run,
        ..Default::default()
    };
    for lane in lanes {
        if lane.path == primary {
            continue;
        }
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
        if entry.locked || entry.prunable {
            retain(
                &mut report.worktrees,
                &branch,
                RetainReason::LockedOrPrunable,
            );
            continue;
        }
        if lane.writer != WriterState::Free || !(policy.no_live_lanes)(&lane.path) {
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
        let mut preview_clean = lane.dirty == WorktreeCondition::Clean;
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
                let mut protected = ProtectedPaths::new(policy.keep);
                for entry in parse_index_entries(&index.stdout.bytes)
                    .map_err(|_| WorktreeError::InvalidOutput)?
                {
                    let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
                    protected.insert(Path::new(
                        std::str::from_utf8(entry.path)
                            .map_err(|_| WorktreeError::InvalidOutput)?,
                    ));
                }
                let start = report.output.len();
                walk_output(
                    root,
                    &lane.path,
                    Path::new(""),
                    policy,
                    &protected,
                    &mut report.output,
                    0,
                )?;
                if policy.dry_run {
                    preview_clean = clean_after_preview(
                        context,
                        locked.verified.repo(),
                        &lane.path,
                        &report.output[start..],
                        cancel,
                    )?;
                }
                Ok(())
            };
            if let Err(error) = prune() {
                retain(&mut report.worktrees, &branch, retain_reason(&error));
                continue;
            }
        }
        let Some(upstream) = policy.upstream else {
            continue;
        };
        if lane.head == upstream {
            retain(
                &mut report.worktrees,
                &branch,
                RetainReason::AtIntegrationTip,
            );
            continue;
        }
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
        match is_first_parent_commit(context, upstream, lane.head, cancel) {
            Ok(false) => {}
            Ok(true) => {
                retain(&mut report.worktrees, &branch, RetainReason::NoUniqueCommit);
                continue;
            }
            Err(error) => {
                retain(&mut report.worktrees, &branch, retain_reason(&error));
                continue;
            }
        }
        if !(policy.terminal)(&lane.path) {
            retain(&mut report.worktrees, &branch, RetainReason::UnknownWriter);
            continue;
        }
        if policy.dry_run {
            if preview_clean {
                report.worktrees.removed.push(lane.clone());
                report
                    .worktrees
                    .pruned_refs
                    .push(branch.short_name().to_owned());
            }
            continue;
        }
        let _repository = match repository_lock(context, LockMode::Exclusive, cancel) {
            Ok(lock) => lock,
            Err(error) => {
                retain(&mut report.worktrees, &branch, retain_reason(&error));
                continue;
            }
        };
        match remove_tree_when(context, entry, lane.head, cancel, &|| {
            (policy.terminal)(&lane.path) && (policy.no_live_lanes)(&lane.path)
        }) {
            Ok(removed) => {
                report.worktrees.removed.push(removed.info.clone());
                if let Err(error) = settle_branch(
                    context,
                    &branch,
                    lane.head,
                    Disposition::Merged,
                    cancel,
                    &mut report.worktrees,
                ) {
                    retain(&mut report.worktrees, &branch, retain_reason(&error));
                }
            }
            Err(error) => retain(&mut report.worktrees, &branch, retain_reason(&error)),
        }
    }
    Ok(report)
}

/// Membership checks cost path depth, rather than a scan of the whole index
/// for every filesystem entry. Ancestors protect directories containing files.
struct ProtectedPaths {
    paths: HashSet<PathBuf>,
    ancestors: HashSet<PathBuf>,
}

impl ProtectedPaths {
    fn new(keep: &[String]) -> Self {
        let mut protected = Self {
            paths: HashSet::new(),
            ancestors: HashSet::new(),
        };
        for path in keep {
            protected.insert(Path::new(path));
        }
        protected
    }

    fn insert(&mut self, path: &Path) {
        self.paths.insert(path.to_owned());
        self.ancestors.extend(path.ancestors().map(Path::to_owned));
    }

    fn covers(&self, path: &Path) -> bool {
        path.ancestors()
            .any(|ancestor| self.paths.contains(ancestor))
    }
}

fn clean_after_preview(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    root: &Path,
    output: &[OutputEntry],
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let status = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
    if exit_code(&status)? != 0 {
        return Err(WorktreeError::UnknownWriter);
    }
    let records =
        parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let planned: HashSet<_> = output
        .iter()
        .filter(|entry| entry.reason == "would_remove")
        .map(|entry| entry.path.clone())
        .collect();
    for record in records {
        if record.kind != StatusKind::Ignored {
            return Ok(false);
        }
        let path =
            Path::new(std::str::from_utf8(&record.path).map_err(|_| WorktreeError::InvalidOutput)?);
        if !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(WorktreeError::InvalidOutput);
        }
        let absolute = root.join(path);
        if absolute
            .ancestors()
            .any(|ancestor| planned.contains(ancestor))
        {
            continue;
        }
        // Git may collapse an ignored parent containing several outputs.
        // Every remaining leaf must belong to an eligible output directory.
        let mut components = path.components();
        let first = components.next().ok_or(WorktreeError::InvalidOutput)?;
        if repo.root_dir().metadata_at(first.as_os_str(), false)?.kind != FileKind::Directory {
            return Ok(false);
        }
        let mut directory = repo.root_dir().open_verified_directory(first.as_os_str())?;
        for component in components {
            if directory.metadata_at(component.as_os_str(), false)?.kind != FileKind::Directory {
                return Ok(false);
            }
            directory = directory.open_verified_directory(component.as_os_str())?;
        }
        if !preview_directory_removed(directory.as_ref(), &absolute, &planned, 0)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn preview_directory_removed(
    directory: &dyn SafeDir,
    path: &Path,
    planned: &HashSet<PathBuf>,
    depth: usize,
) -> Result<bool, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    for name in entry_names(directory, 100_000)? {
        let child = path.join(&name);
        if planned.contains(&child) {
            continue;
        }
        if directory.metadata_at(&name, false)?.kind != FileKind::Directory {
            return Ok(false);
        }
        if !preview_directory_removed(
            directory.open_verified_directory(&name)?.as_ref(),
            &child,
            planned,
            depth + 1,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn walk_output(
    directory: &dyn SafeDir,
    root: &Path,
    relative: &Path,
    policy: &WorktreeGcPolicy<'_>,
    protected: &ProtectedPaths,
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
        if protected.covers(&path) {
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
                && !protected.ancestors.contains(&path)
            {
                prune_output(directory, &name, policy)?;
                Ok(true)
            } else {
                let child = directory.open_verified_directory(&name)?;
                walk_output(
                    child.as_ref(),
                    root,
                    &path,
                    policy,
                    protected,
                    report,
                    depth + 1,
                )?;
                Ok(false)
            }
        };
        match prune() {
            Ok(true) => report.push(OutputEntry {
                path: root.join(path),
                removed: !policy.dry_run,
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
