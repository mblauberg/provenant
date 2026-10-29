//! The GC worktree selection, shared by merged removal and declared output.
use super::*;
use provenant_os::fs::{FileKind, PayloadTree, entry_names, temporary_name};
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
    pub foreign_merged_clean: bool,
    pub foreign_idle_before_ns: Option<i128>,
}

#[derive(Debug, Default, Serialize)]
pub struct WorktreeGcReport {
    pub dry_run: bool,
    pub worktrees: RemoveOutcome,
    pub output: Vec<OutputEntry>,
    /// Declared ignored output in worktrees without a terminal lane.
    pub idle_output: Vec<OutputEntry>,
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
    let managed = primary.join(WORKTREE_ROOT);
    let mut report = WorktreeGcReport {
        dry_run: policy.dry_run,
        ..Default::default()
    };
    for lane in lanes {
        if lane.path == primary {
            continue;
        }
        if !lane.path.starts_with(&managed) && !policy.foreign_merged_clean {
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
        // Queued and pending dispatches reserve a tree before a process or
        // terminal lane record exists. Never fall back around that evidence.
        if !(policy.no_live_lanes)(&lane.path) {
            retain(&mut report.worktrees, &branch, RetainReason::UnknownWriter);
            continue;
        }
        let foreign = policy.foreign_merged_clean && !(policy.terminal)(&lane.path);
        if foreign || (policy.foreign_merged_clean && lane.writer == WriterState::Unknown) {
            match collect_foreign(context, entry, lane, policy, cancel) {
                Ok(Some(info)) => report.worktrees.removed.push(info),
                Ok(None) => retain(&mut report.worktrees, &branch, RetainReason::NotMerged),
                Err(error) => retain(&mut report.worktrees, &branch, retain_reason(&error)),
            }
            continue;
        }
        if lane.writer != WriterState::Free {
            retain(
                &mut report.worktrees,
                &branch,
                match lane.writer {
                    WriterState::Busy => RetainReason::LockBusy,
                    WriterState::Live => RetainReason::LiveWriter,
                    _ => RetainReason::UnknownWriter,
                },
            );
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
        if !policy.regenerable.is_empty() {
            let mut prune = || -> Result<(), WorktreeError> {
                let locked = {
                    let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
                    lock_worktree(context, entry, cancel, policy.dry_run)?
                };
                if !(policy.no_live_lanes)(&lane.path) {
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
                protect_nonignored(context, locked.verified.repo(), &mut protected, cancel)?;
                let output = if !(policy.terminal)(&lane.path) {
                    &mut report.idle_output
                } else {
                    &mut report.output
                };
                let start = output.len();
                walk_output(
                    root,
                    &lane.path,
                    Path::new(""),
                    policy,
                    &protected,
                    output,
                    0,
                )?;
                if policy.dry_run {
                    preview_clean = clean_after_preview(
                        context,
                        locked.verified.repo(),
                        &lane.path,
                        &output[start..],
                        cancel,
                    )?;
                }
                Ok(())
            };
            if let Err(error) = prune()
                && !error.is_custody_unknown()
            {
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
        if policy.dry_run {
            let terminal = (policy.terminal)(&lane.path);
            let no_live_lanes = (policy.no_live_lanes)(&lane.path);
            if preview_clean && terminal && no_live_lanes {
                report.worktrees.removed.push(lane.clone());
                report
                    .worktrees
                    .pruned_refs
                    .push(branch.short_name().to_owned());
            } else if !terminal || !no_live_lanes {
                retain(&mut report.worktrees, &branch, RetainReason::UnknownWriter);
            } else {
                retain(&mut report.worktrees, &branch, RetainReason::Dirty);
            }
            continue;
        }
        let removed = {
            let _repository = match repository_lock(context, LockMode::Exclusive, cancel) {
                Ok(lock) => lock,
                Err(error) => {
                    retain(&mut report.worktrees, &branch, retain_reason(&error));
                    continue;
                }
            };
            match remove_tree_when(
                context,
                entry,
                lane.head,
                cancel,
                &|| (policy.terminal)(&lane.path) && (policy.no_live_lanes)(&lane.path),
                None,
            ) {
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
                    Some(removed)
                }
                Err(error) => {
                    retain(&mut report.worktrees, &branch, retain_reason(&error));
                    None
                }
            }
        };
        if let Some(removed) = removed
            && let Some(cache) = removed.lane_cache.as_deref()
            && let Err(error) = crate::cache::prune_worktree_cargo_cache(context.home, cache)
        {
            warn_cache_removal(&error);
        }
    }
    Ok(report)
}

/// Membership checks cost path depth, rather than a scan of the whole index
/// for every filesystem entry. Ancestors protect directories containing files.
struct ProtectedPaths {
    paths: HashSet<PathBuf>,
    ancestors: HashSet<PathBuf>,
    ignored: HashSet<PathBuf>,
}

impl ProtectedPaths {
    fn new(keep: &[String]) -> Self {
        let mut protected = Self {
            paths: HashSet::new(),
            ancestors: HashSet::new(),
            ignored: HashSet::new(),
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
        if !preview_directory_removed(&directory, &absolute, &planned, 0)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn preview_directory_removed(
    directory: &Directory,
    path: &Path,
    planned: &HashSet<PathBuf>,
    depth: usize,
) -> Result<bool, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Ok(false);
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
            &directory.open_verified_directory(&name)?,
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
    directory: &Directory,
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
        if name == ".git" || name == WORKTREE_ROOT {
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
            let child = directory.open_verified_directory(&name)?;
            if contains_repository(&child)? {
                return Ok(false);
            }
            if policy
                .regenerable
                .iter()
                .any(|configured| name == OsStr::new(configured))
                && !protected.ancestors.contains(&path)
                && path
                    .ancestors()
                    .any(|ancestor| protected.ignored.contains(ancestor))
            {
                prune_output(directory, &name, policy)?;
                Ok(true)
            } else {
                walk_output(&child, root, &path, policy, protected, report, depth + 1)?;
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

/// A .git file or directory marks a protected repository boundary.
fn contains_repository(directory: &Directory) -> Result<bool, FsError> {
    match directory.metadata_at(OsStr::new(".git"), false) {
        Ok(_) => Ok(true),
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error),
    }
}

// Real time bounds the idle proof; explicit removal needs only repository protection.
#[allow(clippy::disallowed_methods)]
fn check_output_before(directory: &Directory, before: i128) -> Result<(), FsError> {
    check_output_bounded(directory, before, &mut 1_000_000, std::time::Instant::now())
}

fn check_output_bounded(
    directory: &Directory,
    before: i128,
    remaining: &mut usize,
    started: std::time::Instant,
) -> Result<(), FsError> {
    if before == i128::MAX {
        return protect_repositories(directory, 0);
    }
    if newest_bounded(directory, 0, remaining, started)? >= before {
        return Err(FsError::RaceRefused {
            name: "recent output".into(),
        });
    }
    Ok(())
}

fn protect_repositories(directory: &Directory, depth: usize) -> Result<(), FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
    }
    for name in entry_names(directory, 100_000)? {
        if directory.metadata_at(&name, false)?.kind == FileKind::Directory {
            protect_repositories(&directory.open_verified_directory(&name)?, depth + 1)?;
        }
    }
    Ok(())
}

fn newest_bounded(
    directory: &Directory,
    depth: usize,
    remaining: &mut usize,
    started: std::time::Instant,
) -> Result<i128, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
    }
    let mut time = directory.metadata()?.modified_ns;
    for name in entry_names(directory, 100_000)? {
        if *remaining == 0 || started.elapsed() >= std::time::Duration::from_secs(5) {
            return Err(FsError::EntryLimit { limit: 1_000_000 });
        }
        *remaining -= 1;
        let meta = directory.metadata_at(&name, false)?;
        time = time.max(meta.modified_ns);
        if meta.kind == FileKind::Directory {
            time = time.max(newest_bounded(
                &directory.open_verified_directory(&name)?,
                depth + 1,
                remaining,
                started,
            )?);
        }
    }
    Ok(time)
}

fn prune_output(
    parent: &Directory,
    name: &OsStr,
    policy: &WorktreeGcPolicy<'_>,
) -> Result<(), FsError> {
    let directory = parent.open_verified_directory(name)?;
    let identity = directory.metadata()?;
    check_output_before(&directory, policy.written_before_ns)?;
    if policy.dry_run {
        return Ok(());
    }
    let staging_name = temporary_name()?;
    parent.mkdir(&staging_name, 0o700)?;
    let staging = parent.open_verified_directory(&staging_name)?;
    let payload = PayloadTree::claim(parent, &staging_name, &staging.metadata()?)?;
    // A private parent grants payload disposal without changing the original
    // directory's permissions or following links in the output tree.
    if let Err(error) = parent.rename_noreplace(name, &staging, OsStr::new("output")) {
        parent.rmdir(&staging_name)?;
        return Err(error);
    }
    let removed = || -> Result<(), FsError> {
        let detached = payload.child(OsStr::new("output"))?;
        if !identity.same_identity(&detached.metadata()?) {
            return Err(FsError::RaceRefused { name: name.into() });
        }
        check_output_before(detached.directory(), policy.written_before_ns)?;
        remove_output(&detached, 0)?;
        payload.rmdir(OsStr::new("output"))
    };
    if let Err(error) = removed() {
        staging.rename_noreplace(OsStr::new("output"), parent, name)?;
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
    if contains_repository(tree.directory())? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
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

/// Prove all ignored paths are disposable before removing any output.
/// The caller holds writer custody.
pub(super) fn prepare_removal(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    regenerable: &[String],
    keep: &[String],
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    prepare_removal_before(context, repo, regenerable, keep, false, cancel)
}

/// Preview exactly the same ignored-output eligibility as actual removal.
pub(super) fn precheck_removal(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    regenerable: &[String],
    keep: &[String],
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    prepare_removal_before(context, repo, regenerable, keep, true, cancel)
}

fn prepare_removal_before(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    regenerable: &[String],
    keep: &[String],
    preview_only: bool,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    let protected = output_protection(context, repo, keep, cancel)?;
    let mut policy = WorktreeGcPolicy {
        upstream: None,
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable,
        keep,
        written_before_ns: i128::MAX,
        dry_run: true,
        foreign_merged_clean: false,
        foreign_idle_before_ns: None,
    };
    let mut output = Vec::new();
    walk_output(
        repo.root_dir(),
        repo.root_path(),
        Path::new(""),
        &policy,
        &protected,
        &mut output,
        0,
    )?;
    if !clean_after_preview(context, repo, repo.root_path(), &output, cancel)? {
        let status = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
        let planned: Vec<_> = output
            .iter()
            .filter(|entry| entry.reason == "would_remove")
            .collect();
        let paths = parse_porcelain_v2_z(&status.stdout.bytes)
            .map_err(|_| WorktreeError::InvalidOutput)?
            .into_iter()
            .filter(|record| {
                !planned.iter().any(|entry| {
                    repo.root_path()
                        .join(OsStr::from_bytes(&record.path))
                        .starts_with(&entry.path)
                })
            })
            .take(5)
            .map(|record| String::from_utf8_lossy(&record.path).into_owned())
            .collect::<Vec<_>>();
        return Err(WorktreeError::IgnoredPaths(paths.join(", ")));
    }
    if preview_only {
        return Ok(());
    }
    policy.dry_run = false;
    output.clear();
    walk_output(
        repo.root_dir(),
        repo.root_path(),
        Path::new(""),
        &policy,
        &protected,
        &mut output,
        0,
    )?;
    Ok(())
}

fn output_protection(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    keep: &[String],
    cancel: &CancelToken,
) -> Result<ProtectedPaths, WorktreeError> {
    let mut protected = ProtectedPaths::new(keep);
    let index = run_git(
        context,
        repo,
        GitRead::IndexEntries { paths: Vec::new() },
        cancel,
    )?;
    if exit_code(&index)? != 0 {
        return Err(WorktreeError::UnknownWriter);
    }
    for entry in
        parse_index_entries(&index.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let entry = entry.map_err(|_| WorktreeError::InvalidOutput)?;
        protected.insert(Path::new(
            std::str::from_utf8(entry.path).map_err(|_| WorktreeError::InvalidOutput)?,
        ));
    }
    protect_nonignored(context, repo, &mut protected, cancel)?;
    Ok(protected)
}

/// An untracked directory named target is not a declaration of disposability.
fn protect_nonignored(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    protected: &mut ProtectedPaths,
    cancel: &CancelToken,
) -> Result<(), WorktreeError> {
    let status = run_git(context, repo, GitRead::StatusIncludingIgnored, cancel)?;
    if exit_code(&status)? != 0 {
        return Err(WorktreeError::UnknownWriter);
    }
    for record in
        parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?
    {
        let path =
            Path::new(std::str::from_utf8(&record.path).map_err(|_| WorktreeError::InvalidOutput)?);
        if !path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(WorktreeError::InvalidOutput);
        }
        if record.kind == StatusKind::Ignored {
            protected.ignored.insert(path.to_owned());
        } else {
            protected.insert(path);
        }
    }
    Ok(())
}

#[allow(clippy::disallowed_methods)]
fn newest_foreign(
    directory: &Directory,
    protected: &ProtectedPaths,
    policy: &WorktreeGcPolicy<'_>,
) -> Result<i128, FsError> {
    newest_foreign_bounded(
        directory,
        Path::new(""),
        protected,
        policy,
        &mut 1_000_000,
        std::time::Instant::now(),
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn newest_foreign_bounded(
    directory: &Directory,
    relative: &Path,
    protected: &ProtectedPaths,
    policy: &WorktreeGcPolicy<'_>,
    remaining: &mut usize,
    started: std::time::Instant,
    depth: usize,
) -> Result<i128, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if depth > 0 && contains_repository(directory)? {
        return Err(FsError::RaceRefused {
            name: ".git".into(),
        });
    }
    let mut time = directory.metadata()?.modified_ns;
    for name in entry_names(directory, 100_000)? {
        if *remaining == 0 || started.elapsed() >= std::time::Duration::from_secs(5) {
            return Err(FsError::EntryLimit { limit: 1_000_000 });
        }
        *remaining -= 1;
        let meta = directory.metadata_at(&name, false)?;
        time = time.max(meta.modified_ns);
        let path = relative.join(&name);
        let declared = policy
            .regenerable
            .iter()
            .any(|configured| name == OsStr::new(configured))
            && !protected.covers(&path)
            && !protected.ancestors.contains(&path)
            && path
                .ancestors()
                .any(|ancestor| protected.ignored.contains(ancestor));
        if meta.kind == FileKind::Directory && !declared {
            time = time.max(newest_foreign_bounded(
                &directory.open_verified_directory(&name)?,
                &path,
                protected,
                policy,
                remaining,
                started,
                depth + 1,
            )?);
        }
    }
    Ok(time)
}

/// Foreign worktrees need a complete free process census in place of a
/// Provenant custody proof. Preserve local refs and never create ownership records.
/// Applied removal creates writer exclusion even for trees with no state yet.
fn collect_foreign(
    context: &WorktreeContext<'_>,
    entry: &GitWorktree,
    lane: &WorktreeInfo,
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<Option<WorktreeInfo>, WorktreeError> {
    if !(policy.no_live_lanes)(&lane.path) {
        return Err(WorktreeError::UnknownWriter);
    }
    let Some(upstream) = policy.upstream else {
        return Err(WorktreeError::UnknownWriter);
    };
    if !is_ancestor(context, lane.head, upstream, cancel)? {
        return Ok(None);
    }
    let verified = verify_probed_worktree(context, &lane.path, cancel)?;
    // Serialise slot creation with worktree mutations. Slots remain stable.
    let repository = repository_lock(context, LockMode::Exclusive, cancel)?;
    let state = if policy.dry_run {
        match context
            .project_state
            .open_verified_directory(&state_dir_name(&lane.path))
        {
            Ok(state) => Some(state),
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        Some(writer_directory(context.project_state, &lane.path)?)
    };
    let _writer = match state {
        Some(state) => {
            let guard = if policy.dry_run {
                state.try_lock_existing(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
            } else {
                state.try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)
            };
            match guard {
                Ok(guard) => Some(guard),
                Err(error) if policy.dry_run && error.is_not_found() => None,
                Err(FsError::LockTimeout { .. }) => return Err(WorktreeError::LockBusy),
                Err(error) => return Err(error.into()),
            }
        }
        None => None,
    };
    drop(repository);
    (|| {
        let recheck = || -> Result<(), WorktreeError> {
            verified.repo().revalidate()?;
            let entries = read_worktrees(context, cancel)?;
            let fresh = entries
                .iter()
                .find(|fresh| fresh.path == entry.path)
                .ok_or(WorktreeError::IdentityChanged)?;
            if fresh.head != lane.head
                || fresh.branch != entry.branch
                || fresh.locked
                || fresh.prunable
            {
                return Err(WorktreeError::IdentityChanged);
            }
            if context
                .workers
                .observe_roots(std::slice::from_ref(&lane.path))
                .get(&lane.path)
                != Some(&WorkerState::Free)
                || !(policy.no_live_lanes)(&lane.path)
            {
                return Err(WorktreeError::UnknownWriter);
            }
            match worktree_dirty_at(context, verified.repo(), cancel)? {
                WorktreeCondition::Clean | WorktreeCondition::Ignored => Ok(()),
                _ => Err(WorktreeError::Dirty),
            }
        };
        recheck()?;
        let protected = output_protection(context, verified.repo(), policy.keep, cancel)?;
        if newest_foreign(verified.repo().root_dir(), &protected, policy)?
            >= policy
                .foreign_idle_before_ns
                .unwrap_or(policy.written_before_ns)
        {
            return Err(WorktreeError::UnknownWriter);
        }
        let mut output = Vec::new();
        let preview = WorktreeGcPolicy {
            written_before_ns: i128::MAX,
            dry_run: true,
            ..*policy
        };
        walk_output(
            verified.repo().root_dir(),
            &lane.path,
            Path::new(""),
            &preview,
            &protected,
            &mut output,
            0,
        )?;
        if !clean_after_preview(context, verified.repo(), &lane.path, &output, cancel)? {
            return Err(WorktreeError::Dirty);
        }
        recheck()?;
        if newest_foreign(verified.repo().root_dir(), &protected, policy)?
            >= policy
                .foreign_idle_before_ns
                .unwrap_or(policy.written_before_ns)
        {
            return Err(WorktreeError::UnknownWriter);
        }
        if !policy.dry_run {
            if !output.is_empty() {
                prepare_removal(
                    context,
                    verified.repo(),
                    policy.regenerable,
                    policy.keep,
                    cancel,
                )?;
            }
            // Bulk output disposal holds writer custody only. Repository custody
            // covers the final registration checks and Git metadata mutation.
            let _repository = repository_lock(context, LockMode::Exclusive, cancel)?;
            recheck()?;
            if worktree_dirty_at(context, verified.repo(), cancel)? != WorktreeCondition::Clean {
                return Err(WorktreeError::Dirty);
            }
            execute_mutation(
                context,
                GitMutation::WorktreeRemove {
                    path: lane.path.clone(),
                },
                cancel,
            )?;
        }
        Ok(Some(lane.clone()))
    })()
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn foreign_output_recency_uses_only_declared_directory_mtime()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let output = temp.path().join("target");
        std::fs::create_dir_all(output.join("nested"))?;
        let artifact = output.join("nested/artifact");
        std::fs::write(&artifact, "output")?;
        let set_time = |path: &Path, seconds| -> std::io::Result<()> {
            std::fs::File::open(path)?.set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            )
        };
        set_time(temp.path(), 100)?;
        set_time(&output, 200)?;
        set_time(&artifact, 1_000)?;
        let root = Directory::open_anchor(temp.path())?;
        let mut protected = ProtectedPaths::new(&[]);
        protected.ignored.insert("target".into());
        let policy = WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: true,
            foreign_idle_before_ns: None,
        };
        assert_eq!(newest_foreign(&root, &protected, &policy)?, 200_000_000_000);
        set_time(&artifact, 2_000)?;
        assert_eq!(newest_foreign(&root, &protected, &policy)?, 200_000_000_000);
        set_time(&output, 300)?;
        assert_eq!(newest_foreign(&root, &protected, &policy)?, 300_000_000_000);
        Ok(())
    }

    #[test]
    fn removal_skips_idle_cap_and_foreign_skips_declared_output_contents()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("target"))?;
        for i in 0..16 {
            std::fs::write(temp.path().join(format!("target/{i}")), "output")?;
        }
        std::fs::write(temp.path().join("source"), "source")?;
        let root = Directory::open_anchor(temp.path())?;
        let target = root.open_verified_directory(OsStr::new("target"))?;
        assert!(matches!(
            newest_bounded(&target, 0, &mut 2, std::time::Instant::now()),
            Err(FsError::EntryLimit { .. })
        ));
        check_output_bounded(&target, i128::MAX, &mut 0, std::time::Instant::now())?;
        let mut protected = ProtectedPaths::new(&[]);
        protected.ignored.insert("target".into());
        let policy = WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: true,
            foreign_idle_before_ns: None,
        };
        newest_foreign_bounded(
            &root,
            Path::new(""),
            &protected,
            &policy,
            &mut 2,
            std::time::Instant::now(),
            0,
        )?;
        protected.ignored.clear();
        assert!(matches!(
            newest_foreign_bounded(
                &root,
                Path::new(""),
                &protected,
                &policy,
                &mut 2,
                std::time::Instant::now(),
                0
            ),
            Err(FsError::EntryLimit { .. })
        ));
        std::fs::write(temp.path().join("target/.git"), "gitdir: native")?;
        assert!(
            check_output_bounded(&target, i128::MAX, &mut 0, std::time::Instant::now()).is_err()
        );
        Ok(())
    }
}
