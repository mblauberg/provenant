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
    collect_impl(context, lanes, policy, cancel, false)
}

/// Collect against [`list_for_gc`], checking candidate cleanliness under
/// writer exclusion before using it to authorise cleanup.
pub fn collect_lazy(
    context: &WorktreeContext<'_>,
    lanes: &[WorktreeInfo],
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<WorktreeGcReport, WorktreeError> {
    collect_impl(context, lanes, policy, cancel, true)
}

fn collect_impl(
    context: &WorktreeContext<'_>,
    lanes: &[WorktreeInfo],
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
    lazy_cleanliness: bool,
) -> Result<WorktreeGcReport, WorktreeError> {
    validate_binding(context)?;
    // A preview describes one inventory snapshot. Applied cleanup refreshes
    // the inventory under custody before mutation.
    let entries = {
        let _repository = repository_lock(context, LockMode::Shared, cancel)?;
        context.repo.revalidate()?;
        let snapshot = lazy_cleanliness
            .then(|| context.facts.take_inventory(context))
            .flatten();
        if policy.dry_run {
            match snapshot {
                Some(entries) => entries,
                None => context.facts.read_inventory(context, cancel)?,
            }
        } else {
            context.facts.read_inventory(context, cancel)?
        }
    };
    // Preview all candidates against one census snapshot. Applying still
    // refreshes under custody immediately before each mutation.
    if policy.dry_run {
        let roots: Vec<_> = lanes.iter().map(|lane| lane.path.clone()).collect();
        context.workers.observe_roots(&roots);
    }
    let primary = canonicalize(context.repo.root_path())?;
    let managed = primary.join(WORKTREE_ROOT);
    let mut report = WorktreeGcReport {
        dry_run: policy.dry_run,
        ..Default::default()
    };
    let settings = provenant_config::ProjectGcSettings {
        regenerable: policy.regenerable.to_vec(),
        keep: policy.keep.to_vec(),
        ..Default::default()
    };
    for lane in lanes {
        if lane.path == primary {
            continue;
        }
        // An unknown writer does not make ignored output disposable. Require
        // a complete free census and writer exclusion before considering it.
        // This is independent of the policy for removing foreign worktrees.
        if (matches!(&lane.occupancy, Occupancy::Unknown { .. })
            || (!lane.path.starts_with(&managed)
                && lane.is_unowned()
                && lane.occupancy.is_available()))
            && !policy.regenerable.is_empty()
            && let Some(entry) = entries.iter().find(|entry| entry.path == lane.path)
        {
            let result = prune_unknown_output(
                context,
                &entries,
                entry,
                lane,
                policy,
                cancel,
                &mut report.idle_output,
            );
            if let Err(error) = result {
                report.idle_output.push(OutputEntry {
                    path: lane.path.clone(),
                    removed: false,
                    reason: error.to_string(),
                });
            }
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
        if foreign
            || (policy.foreign_merged_clean && matches!(&lane.occupancy, Occupancy::Unknown { .. }))
        {
            // The fallback proves ancestry and a free census, not that a managed
            // lane is finished: keep a fresh tree at the tip or on its first parent.
            if lane.path.starts_with(&managed)
                && let Some(upstream) = policy.upstream
            {
                match integration_protection(context, lane, upstream, cancel) {
                    Ok(None) => {}
                    Ok(Some(reason)) => {
                        retain(&mut report.worktrees, &branch, reason);
                        continue;
                    }
                    Err(error) => {
                        retain(&mut report.worktrees, &branch, retain_reason(&error));
                        continue;
                    }
                }
            }
            match collect_foreign(
                context,
                &entries,
                entry,
                lane,
                &branch,
                policy,
                &settings,
                cancel,
                &mut report.worktrees,
            ) {
                Ok(Some(info)) if policy.dry_run => {
                    report.worktrees.removed.push(info);
                    report
                        .worktrees
                        .pruned_refs
                        .push(branch.short_name().to_owned());
                }
                Ok(Some(_)) => {}
                Ok(None) => retain(&mut report.worktrees, &branch, RetainReason::NotMerged),
                Err(error) => retain_error(&mut report.worktrees, &branch, error),
            }
            continue;
        }
        if !lane.occupancy.is_available() {
            let reason = match &lane.occupancy {
                Occupancy::Held => RetainReason::LockBusy,
                Occupancy::Observed { .. } => RetainReason::LiveWriter,
                _ => RetainReason::UnknownWriter,
            };
            if reason == RetainReason::UnknownWriter
                && let Some(detail) = census_refusal_detail(context, &lane.path)
            {
                retain_with_detail(&mut report.worktrees, &branch, reason, detail);
            } else {
                retain(&mut report.worktrees, &branch, reason);
            }
            continue;
        }
        let checked;
        let lane = if lazy_cleanliness {
            let locked = match lock_worktree(context, entry, cancel, policy.dry_run) {
                Ok(locked) => locked,
                Err(error) => {
                    retain_error(&mut report.worktrees, &branch, error);
                    continue;
                }
            };
            let dirty = worktree_dirty_at(context, locked.verified.repo(), cancel)
                .unwrap_or(WorktreeCondition::Unknown);
            checked = WorktreeInfo {
                dirty,
                ..lane.clone()
            };
            &checked
        } else {
            lane
        };
        if !matches!(
            lane.dirty,
            WorktreeCondition::Clean | WorktreeCondition::Ignored
        ) {
            retain(&mut report.worktrees, &branch, RetainReason::Dirty);
            continue;
        }
        if !policy.regenerable.is_empty() {
            let mut prune = || -> Result<(), WorktreeError> {
                // Writer custody alone excludes other mutators of this tree.
                let locked = lock_worktree(context, entry, cancel, policy.dry_run)?;
                if !(policy.no_live_lanes)(&lane.path) {
                    return Err(WorktreeError::UnknownWriter);
                }
                if !matches!(
                    worktree_dirty_at(context, locked.verified.repo(), cancel)?,
                    WorktreeCondition::Clean | WorktreeCondition::Ignored
                ) {
                    return Err(WorktreeError::Dirty(String::new()));
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
                walk_output(
                    root,
                    &lane.path,
                    Path::new(""),
                    policy,
                    &protected,
                    output,
                    0,
                )?;
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
        match integration_protection(context, lane, upstream, cancel) {
            Ok(None) => {}
            Ok(Some(reason)) => {
                retain(&mut report.worktrees, &branch, reason);
                continue;
            }
            Err(error) => {
                retain(&mut report.worktrees, &branch, retain_reason(&error));
                continue;
            }
        }
        if policy.dry_run {
            {
                let precheck = || -> Result<(), WorktreeError> {
                    // The census and ignored-output walk run without the
                    // repository lock; it covers the branch ref check.
                    let locked = lock_worktree(context, entry, cancel, true)?;
                    {
                        let _repository = repository_lock(context, LockMode::Shared, cancel)?;
                        tree_unchanged(&entries, entry, lane.head)?;
                        branch_unchanged(
                            context, &entries, &branch, &lane.path, lane.head, cancel,
                        )?;
                    }
                    // Applying needs a fresh Free census; a preview must not
                    // report a removal that applying would refuse.
                    if let Some(detail) = census_refusal_detail(context, &lane.path) {
                        return Err(WorktreeError::IncompleteCensus { detail });
                    }
                    precheck_removal(
                        context,
                        locked.verified.repo(),
                        policy.regenerable,
                        policy.keep,
                        cancel,
                    )
                };
                match precheck() {
                    Ok(()) => {}
                    Err(error) => {
                        retain_error(&mut report.worktrees, &branch, error);
                        continue;
                    }
                }
            }
            let terminal = (policy.terminal)(&lane.path);
            let no_live_lanes = (policy.no_live_lanes)(&lane.path);
            if terminal && no_live_lanes {
                report.worktrees.removed.push(lane.clone());
                report
                    .worktrees
                    .pruned_refs
                    .push(branch.short_name().to_owned());
            } else {
                retain(&mut report.worktrees, &branch, RetainReason::UnknownWriter);
            }
            continue;
        }
        if let Err(error) = remove_and_finalise(
            context,
            &Removal {
                entry,
                lane,
                branch: &branch,
                settings: &settings,
                eligible: &|| {
                    if !(policy.terminal)(&lane.path) || !(policy.no_live_lanes)(&lane.path) {
                        return Err(WorktreeError::UnknownWriter);
                    }
                    census_free(context, &lane.path)
                },
                lanes: &|| (policy.terminal)(&lane.path) && (policy.no_live_lanes)(&lane.path),
            },
            cancel,
            &mut report.worktrees,
        ) {
            retain_error(&mut report.worktrees, &branch, error);
        }
    }
    Ok(report)
}

#[allow(clippy::disallowed_methods)] // Bound this filesystem walk in real time.
fn prune_unknown_output(
    context: &WorktreeContext<'_>,
    preview_entries: &[GitWorktree],
    entry: &GitWorktree,
    lane: &WorktreeInfo,
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
    output: &mut Vec<OutputEntry>,
) -> Result<(), WorktreeError> {
    if !(policy.no_live_lanes)(&lane.path) {
        return Err(WorktreeError::UnknownWriter);
    }
    let locked = lock_worktree(context, entry, cancel, policy.dry_run)?;
    {
        let _repository = repository_lock(context, LockMode::Shared, cancel)?;
        if policy.dry_run {
            tree_unchanged(preview_entries, entry, lane.head)?;
        } else {
            tree_unchanged(
                &context.facts.read_inventory(context, cancel)?,
                entry,
                lane.head,
            )?;
        }
    }
    if policy.dry_run {
        if let Some(detail) = census_refusal_detail(context, &lane.path) {
            return Err(WorktreeError::IncompleteCensus { detail });
        }
    } else {
        census_free(context, &lane.path)?;
    }
    if !(policy.no_live_lanes)(&lane.path) {
        return Err(WorktreeError::UnknownWriter);
    }
    let repo = locked.verified.repo();
    if policy.written_before_ns != i128::MAX {
        let mut newest = newest_worktree(
            repo.root_dir(),
            0,
            &mut 1_000_000,
            std::time::Instant::now(),
        )?;
        // Index-only operations also count as worktree activity.
        for name in ["index", "HEAD"] {
            match repo.private_dir().metadata_at(OsStr::new(name), false) {
                Ok(metadata) => newest = newest.max(metadata.modified_ns),
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        if newest >= policy.written_before_ns {
            return Err(FsError::RaceRefused {
                name: "recent worktree activity".into(),
            }
            .into());
        }
    }
    let protected = output_protection(context, repo, policy.keep, cancel)?;
    walk_output(
        repo.root_dir(),
        &lane.path,
        Path::new(""),
        policy,
        &protected,
        output,
        0,
    )?;
    Ok(())
}

// Count source and output writes alike; never follow symlinks or traverse the
// worktree's Git administrative files. An incomplete walk keeps the output.
#[allow(clippy::disallowed_methods)]
fn newest_worktree(
    directory: &Directory,
    depth: usize,
    remaining: &mut usize,
    started: std::time::Instant,
) -> Result<i128, FsError> {
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    let mut newest = directory.metadata()?.modified_ns;
    for name in entry_names(directory, 100_000)? {
        if name == ".git" {
            continue;
        }
        if *remaining == 0 || started.elapsed() >= std::time::Duration::from_secs(5) {
            return Err(FsError::EntryLimit { limit: 1_000_000 });
        }
        *remaining -= 1;
        let metadata = directory.metadata_at(&name, false)?;
        newest = newest.max(metadata.modified_ns);
        if metadata.kind == FileKind::Directory {
            newest = newest.max(newest_worktree(
                &directory.open_verified_directory(&name)?,
                depth + 1,
                remaining,
                started,
            )?);
        }
    }
    Ok(newest)
}

/// Managed trees are kept while they sit at the integration tip or are a
/// first-parent commit of it: a fresh lane has no unique work to prove merged.
fn integration_protection(
    context: &WorktreeContext<'_>,
    lane: &WorktreeInfo,
    upstream: GitOid,
    cancel: &CancelToken,
) -> Result<Option<RetainReason>, WorktreeError> {
    if lane.head == upstream {
        return Ok(Some(RetainReason::AtIntegrationTip));
    }
    Ok(
        is_first_parent_commit(context, upstream, lane.head, cancel)?
            .then_some(RetainReason::NoUniqueCommit),
    )
}

/// Removal needs a fresh positive census. Custody accepts an unobservable
/// process for ordinary removal; deleting a tree automatically does not.
fn census_free(context: &WorktreeContext<'_>, path: &Path) -> Result<(), WorktreeError> {
    context.workers.invalidate();
    match census_refusal_detail(context, path) {
        Some(detail) => Err(WorktreeError::IncompleteCensus { detail }),
        None => Ok(()),
    }
}

fn census_refusal_detail(context: &WorktreeContext<'_>, path: &Path) -> Option<String> {
    let (states, details) = context
        .workers
        .observe_roots_with_details(std::slice::from_ref(&path.to_path_buf()));
    match states.get(path) {
        Some(Occupancy::Available) => None,
        Some(Occupancy::Unknown { evidence, .. }) => Some(
            details
                .get(path)
                .cloned()
                .unwrap_or_else(|| evidence.clone()),
        ),
        Some(_) => Some(
            details
                .get(path)
                .cloned()
                .unwrap_or_else(|| "worker census did not prove the tree free".into()),
        ),
        None => Some("worker census did not return the requested tree".into()),
    }
}

fn retain_error(outcome: &mut RemoveOutcome, branch: &GitRefName, error: WorktreeError) {
    let detail = match &error {
        WorktreeError::IncompleteCensus { detail } => Some(detail.clone()),
        _ => None,
    };
    if let Some(detail) = detail {
        retain_with_detail(outcome, branch, retain_reason(&error), detail);
    } else {
        retain(outcome, branch, retain_reason(&error));
    }
}

fn retain_with_detail(
    outcome: &mut RemoveOutcome,
    branch: &GitRefName,
    reason: RetainReason,
    detail: String,
) {
    retain(outcome, branch, reason);
    if let Some(retained) = outcome.retained.last_mut() {
        retained.detail = Some(detail);
    }
}

/// One tree's removal: what it is, and the fences rechecked around it.
struct Removal<'a> {
    entry: &'a GitWorktree,
    lane: &'a WorktreeInfo,
    branch: &'a GitRefName,
    settings: &'a provenant_config::ProjectGcSettings,
    /// Lanes and a fresh free census, checked under writer custody alone.
    eligible: &'a dyn Fn() -> Result<(), WorktreeError>,
    /// The cheap lane fence rechecked under the repository lock.
    lanes: &'a dyn Fn() -> bool,
}

/// Delete output under writer custody alone; the repository lock covers only
/// the final registration recheck, the Git removal and branch settlement.
fn remove_and_finalise(
    context: &WorktreeContext<'_>,
    removal: &Removal<'_>,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
) -> Result<(), WorktreeError> {
    let mut removed = remove_tree_when(
        context,
        removal.entry,
        removal.lane.head,
        cancel,
        removal.eligible,
        Some(removal.settings),
        Repository::Acquire {
            lanes: removal.lanes,
        },
    )?;
    outcome.removed.push(removed.info.clone());
    if let Err(error) = settle_branch(
        context,
        removal.branch,
        removal.lane.head,
        Disposition::Merged,
        cancel,
        outcome,
    ) {
        retain(outcome, removal.branch, retain_reason(&error));
    }
    // Writer exclusion stays held through cache pruning.
    removed.repository = None;
    if let Some(cache) = removed.lane_cache.as_deref()
        && let Err(error) = crate::cache::prune_worktree_cargo_cache(context.home, cache)
    {
        warn_cache_removal(&error);
    }
    Ok(())
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

/// Foreign worktrees need a complete free process census in place of a
/// Provenant custody proof. Applied removal creates writer exclusion even for
/// trees with no state yet, deletes the local ref only when a merged
/// disposition settles it, and never creates ownership records.
#[allow(clippy::too_many_arguments)]
fn collect_foreign(
    context: &WorktreeContext<'_>,
    preview_entries: &[GitWorktree],
    entry: &GitWorktree,
    lane: &WorktreeInfo,
    branch: &GitRefName,
    policy: &WorktreeGcPolicy<'_>,
    settings: &provenant_config::ProjectGcSettings,
    cancel: &CancelToken,
    outcome: &mut RemoveOutcome,
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
    let mut info = lane.clone();
    if policy.dry_run {
        let observed = preview_foreign(
            context,
            preview_entries,
            entry,
            lane,
            branch,
            policy,
            cancel,
        )?;
        if info.dirty == WorktreeCondition::Unknown {
            info.dirty = observed;
        }
    } else {
        // remove_tree_when verifies once, under writer custody.
        remove_and_finalise(
            context,
            &Removal {
                entry,
                lane,
                branch,
                settings,
                eligible: &|| {
                    if !(policy.no_live_lanes)(&lane.path) {
                        return Err(WorktreeError::UnknownWriter);
                    }
                    census_free(context, &lane.path)
                },
                lanes: &|| (policy.no_live_lanes)(&lane.path),
            },
            cancel,
            outcome,
        )?;
    }
    Ok(Some(info))
}

/// Preview against the command's inventory snapshot without creating state.
/// Applied removal refreshes Git and custody facts before mutation.
fn preview_foreign(
    context: &WorktreeContext<'_>,
    entries: &[GitWorktree],
    entry: &GitWorktree,
    lane: &WorktreeInfo,
    branch: &GitRefName,
    policy: &WorktreeGcPolicy<'_>,
    cancel: &CancelToken,
) -> Result<WorktreeCondition, WorktreeError> {
    let verified = verify_probed_worktree(context, &lane.path, cancel)?;
    // A missing slot means no writer has ever held this tree.
    let writer = {
        let _repository = repository_lock(context, LockMode::Shared, cancel)?;
        cache_writer_probe(context.project_state, &lane.path).map_err(custody_refusal)?
    };
    let recheck = || -> Result<WorktreeCondition, WorktreeError> {
        verified.repo().revalidate()?;
        tree_unchanged(entries, entry, lane.head)?;
        if let Some(detail) = census_refusal_detail(context, &lane.path) {
            return Err(WorktreeError::IncompleteCensus { detail });
        }
        if !(policy.no_live_lanes)(&lane.path) {
            return Err(WorktreeError::UnknownWriter);
        }
        match worktree_dirty_at(context, verified.repo(), cancel)? {
            state @ (WorktreeCondition::Clean | WorktreeCondition::Ignored) => Ok(state),
            _ => Err(WorktreeError::Dirty(String::new())),
        }
    };
    let observed = recheck()?;
    let protected = output_protection(context, verified.repo(), policy.keep, cancel)?;
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
        return Err(WorktreeError::Dirty(String::new()));
    }
    recheck()?;
    branch_unchanged(context, entries, branch, &lane.path, lane.head, cancel)?;
    drop(writer);
    Ok(observed)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn removal_checks_nested_repositories_without_idle_cap()
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
        std::fs::write(temp.path().join("target/.git"), "gitdir: native")?;
        assert!(
            check_output_bounded(&target, i128::MAX, &mut 0, std::time::Instant::now()).is_err()
        );
        Ok(())
    }
}
