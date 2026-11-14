//! The GC worktree selection, shared by merged removal and declared output.
use super::*;
use provenant_os::fs::{FileKind, PayloadTree, entry_names, temporary_name};
use provenant_records::custody::{
    RemovalOutcome, RemovalRecord, RemovalUncertainty, Subject, SubjectKind,
};
use std::collections::HashSet;

/// One budget covers every output candidate in a preview. Applied collection
/// continues to use the full proof before mutation.
struct PreviewWalk {
    started: std::time::Instant,
    remaining: std::cell::Cell<usize>,
    enabled: bool,
}

impl PreviewWalk {
    #[allow(clippy::disallowed_methods)]
    fn new(enabled: bool) -> Self {
        Self {
            started: std::time::Instant::now(),
            remaining: std::cell::Cell::new(20_000),
            enabled,
        }
    }

    fn step(&self) -> Result<(), FsError> {
        if self.enabled {
            let remaining = self.remaining.get();
            if remaining == 0 || self.started.elapsed() >= std::time::Duration::from_millis(500) {
                return Err(FsError::EntryLimit { limit: 20_000 });
            }
            self.remaining.set(remaining - 1);
        }
        Ok(())
    }

    fn newest(
        &self,
        directory: &Directory,
        depth: usize,
        repositories: bool,
    ) -> Result<i128, FsError> {
        self.step()?;
        if depth >= 64 {
            return Err(FsError::EntryLimit { limit: 64 });
        }
        if repositories && contains_repository(directory)? {
            return Err(FsError::RaceRefused {
                name: ".git".into(),
            });
        }
        let mut newest = directory.metadata()?.modified_ns;
        for name in entry_names(directory, if self.enabled { 20_000 } else { 100_000 })? {
            self.step()?;
            if name == ".git" {
                continue;
            }
            let metadata = directory.metadata_at(&name, false)?;
            newest = newest.max(metadata.modified_ns);
            if metadata.kind == FileKind::Directory {
                newest = newest.max(self.newest(
                    &directory.open_verified_directory(&name)?,
                    depth + 1,
                    repositories,
                )?);
            }
        }
        Ok(newest)
    }
}

#[derive(Clone, Copy, Clone, Copy)]
pub struct WorktreeGcPolicy<'a> {
    pub upstream: Option<GitOid>,
    pub upstream_reference: Option<&'a str>,
    pub terminal: &'a dyn Fn(&Path) -> bool,
    pub no_live_lanes: &'a dyn Fn(&Path) -> bool,
    pub regenerable: &'a [String],
    pub keep: &'a [String],
    pub written_before_ns: i128,
    pub dry_run: bool,
    pub include_foreign: bool,
    /// Application-owned custody, forge evidence and the one retirement writer.
    pub retirement: Option<&'a dyn RetirementOwner>,
    pub integration_patterns: &'a [String],
    pub excluded_paths: &'a [String],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeObservation {
    Absent,
    Open {
        base: String,
    },
    Closed {
        head: GitOid,
    },
    Merged {
        pr: String,
        head: GitOid,
        merge_commit: Option<GitOid>,
    },
    Unknown(String),
}
#[derive(Clone, Debug)]
pub enum RetirementAction {
    Subsumed(provenant_records::custody::LandedEvidence),
    Idle,
}
/// The application owns custody/forge feeds and persisted retirement. The
/// collector never creates a competing retirement record or guesses a feed.
pub trait RetirementOwner {
    fn custody(&self, lane: &WorktreeInfo) -> Result<(), String>;
    fn forge(&self, branch: &GitRefName) -> ForgeObservation;
    fn integration_bases(&self) -> Vec<String>;
    fn record(
        &self,
        context: &WorktreeContext<'_>,
        lane: &WorktreeInfo,
        action: &RetirementAction,
        completed: bool,
    ) -> Result<String, WorktreeError>;
}

#[derive(Debug, Serialize)]
pub struct CandidateReport {
    pub path: PathBuf,
    pub class: &'static str,
    pub action: &'static str,
    pub tip: GitOid,
    pub detail: Option<String>,
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
    /// One outcome per ref the ledger records as created: removed once
    /// landed with an unchanged recorded tip, otherwise retained with its reason.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<super::ArtefactOutcome>,
    /// Records are lifted to the enclosing GC output alongside ref outcomes.
    #[serde(skip)]
    pub removals: Vec<RemovalRecord>,
    pub candidates: Vec<CandidateReport>,
    pub first_foreign_preview: bool,
}

#[derive(Debug, Serialize)]
pub struct OutputEntry {
    pub path: PathBuf,
    pub removed: bool,
    pub reason: String,
}

fn kept_landing_record(
    project: provenant_core::ProjectId,
    lane: &WorktreeInfo,
    cause: RemovalUncertainty,
) -> RemovalRecord {
    RemovalRecord {
        project,
        subject: Subject::new(SubjectKind::Worktree, lane.path.to_string_lossy()),
        tip: lane.head,
        outcome: RemovalOutcome::Kept { cause },
    }
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
    _lazy_cleanliness: bool,
) -> Result<WorktreeGcReport, WorktreeError> {
    validate_binding(context)?;
    let entries = context.facts.fresh_inventory(context, cancel)?;
    let primary = canonicalize(context.repo.root_path())?;
    let marker = format!(
        "foreign-preview-{}-{}",
        context.repo.common_identity().device,
        context.repo.common_identity().inode
    );
    let foreign_seen = lanes
        .iter()
        .any(|lane| !lane.primary && matches!(lane.owner, Ok(None)));
    let first_foreign = foreign_seen
        && policy.include_foreign
        && match context
            .project_state
            .metadata_at(OsStr::new(&marker), false)
        {
            Ok(_) => false,
            Err(error) if error.is_not_found() => true,
            Err(error) => return Err(error.into()),
        };
    let dry = policy.dry_run || first_foreign;
    let mut report = WorktreeGcReport {
        dry_run: dry,
        first_foreign_preview: first_foreign,
        ..Default::default()
    };
    let settings = provenant_config::ProjectGcSettings {
        regenerable: policy.regenerable.to_vec(),
        keep: policy.keep.to_vec(),
        ..Default::default()
    };
    let preview_walk = PreviewWalk::new(dry);
    let integration =
        policy
            .upstream
            .zip(policy.upstream_reference)
            .and_then(|(tip, reference)| {
                GitRefName::new(reference)
                    .ok()
                    .map(|reference| LandingTarget {
                        reference: Some(reference),
                        tip,
                    })
            });
    let mut patterns = policy.integration_patterns.to_vec();
    if let Some(owner) = policy.retirement {
        patterns.extend(owner.integration_bases());
    }
    for lane in lanes {
        if lane.primary || lane.path == primary {
            continue;
        }
        let class = if matches!(lane.owner, Ok(Some(_))) {
            "managed"
        } else {
            "foreign"
        };
        let mut row = CandidateReport {
            path: lane.path.clone(),
            class,
            action: "kept",
            tip: lane.head,
            detail: None,
        };
        let result = (|| -> Result<(), WorktreeError> {
            if class == "foreign" && !policy.include_foreign {
                row.detail = Some("foreign cleanup disabled by repository configuration".into());
                return Ok(());
            }
            if policy
                .excluded_paths
                .iter()
                .any(|pattern| matches_pattern(&lane.path.to_string_lossy(), pattern))
            {
                row.detail = Some("excluded by gc.projects.keep_paths".into());
                return Ok(());
            }
            if lane.owner.is_err() {
                return Err(WorktreeError::UnknownWriter);
            }
            let entry = entries
                .iter()
                .find(|entry| entry.path == lane.path)
                .ok_or(WorktreeError::NotFound)?;
            if entry.locked || entry.prunable {
                return Err(WorktreeError::GitEntryHeld {
                    locked: entry.locked,
                });
            }
            // Never remove a symlink alias or a parent containing another checkout.
            let parent = lane.path.parent().ok_or(WorktreeError::InvalidRequest)?;
            let parent = Directory::open_anchor(parent)?;
            let name = lane.path.file_name().ok_or(WorktreeError::InvalidRequest)?;
            if parent.metadata_at(name, false)?.kind != FileKind::Directory {
                return Err(WorktreeError::InvalidRequest);
            }
            if entries
                .iter()
                .any(|other| other.path != lane.path && other.path.starts_with(&lane.path))
            {
                row.detail = Some("nested worktree retained".into());
                return Ok(());
            }
            if !(policy.no_live_lanes)(&lane.path) {
                return Err(WorktreeError::UnknownWriter);
            }
            if let Some(owner) = policy.retirement {
                owner
                    .custody(lane)
                    .map_err(|detail| WorktreeError::UnknownOccupancy {
                        path: lane.path.clone(),
                        detail,
                    })?;
            }
            let locked = lock_worktree(context, entry, cancel, dry)?;
            census_free(context, &lane.path)?;
            let effective = WorktreeGcPolicy {
                dry_run: dry,
                ..*policy
            };
            let ports = provenant_config::PortTable::read_project(&primary)
                .map_err(WorktreeError::PortConfiguration)?;
            let protected =
                output_protection(context, locked.verified.repo(), policy.keep, cancel)?;
            let output = if (policy.terminal)(&lane.path) {
                &mut report.output
            } else {
                &mut report.idle_output
            };
            walk_output(
                locked.verified.repo().root_dir(),
                &lane.path,
                Path::new(""),
                &effective,
                &ports.worktree_layout.roots,
                &protected,
                output,
                0,
                &preview_walk,
            )?;

            let branch = lane
                .branch
                .as_deref()
                .map(GitRefName::new)
                .transpose()
                .map_err(|_| WorktreeError::InvalidOutput)?;
            let refs = retained_refs(
                context,
                branch.as_ref(),
                integration.as_ref(),
                &patterns,
                cancel,
            )?;
            let mut proof = removal_evidence(context, lane.head, &refs, cancel)?;
            let forge = match (policy.retirement, branch.as_ref()) {
                (Some(owner), Some(branch)) => owner.forge(branch),
                _ => ForgeObservation::Unknown("forge feed unavailable".into()),
            };
            if let ForgeObservation::Merged {
                pr,
                head,
                merge_commit,
            } = &forge
            {
                if *head == lane.head {
                    proof = RemovalEvidence::Proven(provenant_records::custody::LandedEvidence {
                        predicate: provenant_records::custody::LandingPredicate::Landed,
                        source: provenant_records::custody::LandingSource::ForgeMerged {
                            pr: pr.clone(),
                            head: *head,
                            merge_commit: *merge_commit,
                        },
                    });
                }
            }
            // Source output is disposable only by a declared class. Undeclared
            // ignored files (.env, databases) keep the tree, as do submodules.
            let action = match proof {
                RemovalEvidence::Proven(evidence) => RetirementAction::Subsumed(evidence),
                RemovalEvidence::Unknown(cause) => {
                    report.removals.push(kept_landing_record(
                        context.binding.project_id(),
                        lane,
                        cause,
                    ));
                    row.detail = Some(format!("uncertain Git evidence: {cause:?}"));
                    return Ok(());
                }
                RemovalEvidence::NotSubsumed => {
                    if branch.is_none() {
                        row.detail = Some("detached HEAD is not subsumed".into());
                        return Ok(());
                    }
                    let eligible_pr = matches!(forge, ForgeObservation::Absent)
                        || matches!(forge, ForgeObservation::Closed { head } if head == lane.head);
                    if !eligible_pr {
                        row.detail = Some(format!("not subsumed; forge: {forge:?}"));
                        return Ok(());
                    }
                    let age = run_git(
                        context,
                        context.repo,
                        GitRead::CommitTime { tip: lane.head },
                        cancel,
                    )?;
                    if exit_code(&age)? != 0 {
                        return Err(WorktreeError::InvalidOutput);
                    }
                    let timestamp: i128 = std::str::from_utf8(trim_ascii(&age.stdout.bytes))
                        .map_err(|_| WorktreeError::InvalidOutput)?
                        .parse()
                        .map_err(|_| WorktreeError::InvalidOutput)?;
                    let newest =
                        preview_walk.newest(locked.verified.repo().root_dir(), 0, false)?;
                    if timestamp * 1_000_000_000 >= policy.written_before_ns
                        || newest >= policy.written_before_ns
                    {
                        row.detail = Some("not subsumed; recent work retained".into());
                        return Ok(());
                    }
                    if policy.retirement.is_none() {
                        row.detail = Some("retirement owner unavailable".into());
                        return Ok(());
                    }
                    RetirementAction::Idle
                }
            };
            if dry {
                precheck_removal(
                    context,
                    locked.verified.repo(),
                    policy.regenerable,
                    policy.keep,
                    cancel,
                )?;
            }
            drop(locked);
            let eligible = || -> Result<(), WorktreeError> {
                if !(policy.no_live_lanes)(&lane.path) {
                    return Err(WorktreeError::UnknownWriter);
                }
                if let Some(owner) = policy.retirement {
                    owner
                        .custody(lane)
                        .map_err(|detail| WorktreeError::UnknownOccupancy {
                            path: lane.path.clone(),
                            detail,
                        })?;
                }
                census_free(context, &lane.path)
            };
            eligible()?;
            if !dry {
                if let Some(owner) = policy.retirement {
                    owner.record(context, lane, &action, false)?;
                }
                let mut removed = remove_tree_when(
                    context,
                    entry,
                    lane.head,
                    cancel,
                    &eligible,
                    Some(&settings),
                    Repository::Remove,
                )?;
                report.worktrees.removed.push(removed.info.clone());
                if let (Some(branch), RetirementAction::Subsumed(_)) = (branch.as_ref(), &action) {
                    settle_branch(
                        context,
                        branch,
                        lane.head,
                        Disposition::Merged,
                        cancel,
                        &mut report.worktrees,
                    )?;
                }
                removed.repository = None;
                if let Some(cache) = removed.lane_cache.as_deref() {
                    if let Err(error) =
                        crate::cache::prune_worktree_cargo_cache(context.home, cache)
                    {
                        warn_cache_removal(&error);
                    }
                }
                if let Some(owner) = policy.retirement {
                    owner.record(context, lane, &action, true)?;
                }
            } else {
                report.worktrees.removed.push(lane.clone());
                if matches!(&action, RetirementAction::Subsumed(_)) {
                    if let Some(branch) = &branch {
                        report
                            .worktrees
                            .pruned_refs
                            .push(branch.short_name().into());
                    }
                }
            }
            row.action = if dry { "would_remove" } else { "removed" };
            match action {
                RetirementAction::Subsumed(evidence) => {
                    row.detail = Some(format!(
                        "{:?}; last fetched refs are used when forge is offline",
                        evidence.predicate
                    ));
                    if !dry {
                        report.removals.push(RemovalRecord {
                            project: context.binding.project_id(),
                            subject: Subject::new(
                                SubjectKind::Worktree,
                                lane.path.to_string_lossy(),
                            ),
                            tip: lane.head,
                            outcome: RemovalOutcome::Removed { evidence },
                        });
                    }
                }
                RetirementAction::Idle => {
                    row.action = if dry { "would_retire" } else { "retired" };
                    row.detail = Some("idle clean worktree retired; local branch retained".into());
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            row.detail = Some(error.to_string());
            if let Some(branch) = lane
                .branch
                .as_deref()
                .and_then(|name| GitRefName::new(name).ok())
            {
                retain_error(&mut report.worktrees, &branch, error);
            }
        }
        report.candidates.push(row);
    }
    if first_foreign && !policy.dry_run {
        context
            .project_state
            .atomic_write(
                OsStr::new(&marker),
                ".foreign-preview-",
                b"{\"schema_version\":2}\n",
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()
            .map_err(|_| WorktreeError::InvalidOutput)?;
    }
    Ok(report)
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
    preview_walk: &PreviewWalk,
) -> Result<bool, WorktreeError> {
    preview_walk.step()?;
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
        if !preview_directory_removed(&directory, &absolute, &planned, 0, preview_walk)? {
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
    preview_walk: &PreviewWalk,
) -> Result<bool, FsError> {
    preview_walk.step()?;
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    if contains_repository(directory)? {
        return Ok(false);
    }
    for name in entry_names(
        directory,
        if preview_walk.enabled {
            20_000
        } else {
            100_000
        },
    )? {
        preview_walk.step()?;
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
            preview_walk,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn walk_output(
    directory: &Directory,
    root: &Path,
    relative: &Path,
    policy: &WorktreeGcPolicy<'_>,
    layout_roots: &[PathBuf],
    protected: &ProtectedPaths,
    report: &mut Vec<OutputEntry>,
    depth: usize,
    preview_walk: &PreviewWalk,
) -> Result<(), FsError> {
    preview_walk.step()?;
    if depth >= 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    for name in entry_names(
        directory,
        if preview_walk.enabled {
            20_000
        } else {
            100_000
        },
    )? {
        preview_walk.step()?;
        if name == ".git" {
            continue;
        }
        let path = relative.join(&name);
        if layout_roots
            .iter()
            .any(|root| path.as_path() == root.as_path())
            || protected.covers(&path)
        {
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
            let contains_layout_root = layout_roots.iter().any(|root| root.starts_with(&path));
            if !contains_layout_root
                && (policy
                    .regenerable
                    .iter()
                    .any(|configured| name == OsStr::new(configured))
                    || is_default_regenerable(&path))
                && !protected.ancestors.contains(&path)
                && path
                    .ancestors()
                    .any(|ancestor| protected.ignored.contains(ancestor))
            {
                prune_output(directory, &name, policy, preview_walk)?;
                Ok(true)
            } else {
                walk_output(
                    &child,
                    root,
                    &path,
                    policy,
                    layout_roots,
                    protected,
                    report,
                    depth + 1,
                    preview_walk,
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

/// Provenant's own lane scratch is regenerable without a project declaration.
/// The walker still prunes it only when Git reports it ignored, and only with
/// the same writer custody and census proof as declared output.
fn is_default_regenerable(path: &Path) -> bool {
    path == Path::new(crate::scratch::PROJECT_SCRATCH)
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
    preview_walk: &PreviewWalk,
) -> Result<(), FsError> {
    let directory = parent.open_verified_directory(name)?;
    let identity = directory.metadata()?;
    if policy.dry_run {
        let newest = preview_walk.newest(&directory, 0, true)?;
        if newest >= policy.written_before_ns {
            return Err(FsError::RaceRefused {
                name: "recent output".into(),
            });
        }
        return Ok(());
    }
    check_output_before(&directory, policy.written_before_ns)?;
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

pub(super) fn dispose_archive_tree(tree: &PayloadTree) -> Result<(), FsError> {
    tree.make_writable()?;
    for name in tree.entries()? {
        let name = name?;
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            tree.directory().make_owner_accessible_at(&name)?;
            dispose_archive_tree(&tree.child(&name)?)?;
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(())
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
    let ports = provenant_config::PortTable::read_project(context.repo.root_path())
        .map_err(WorktreeError::PortConfiguration)?;
    let layout_roots = ports.worktree_layout.roots;
    let protected = output_protection(context, repo, keep, cancel)?;
    let mut policy = WorktreeGcPolicy {
        upstream: None,
        terminal: &|_| true,
        no_live_lanes: &|_| true,
        regenerable,
        keep,
        written_before_ns: i128::MAX,
        dry_run: true,
        include_foreign: false,
        retirement: None,
        upstream_reference: None,
        integration_patterns: &[],
        excluded_paths: &[],
    };
    let mut output = Vec::new();
    let preview_walk = PreviewWalk::new(preview_only);
    walk_output(
        repo.root_dir(),
        repo.root_path(),
        Path::new(""),
        &policy,
        &layout_roots,
        &protected,
        &mut output,
        0,
        &preview_walk,
    )?;
    if !clean_after_preview(
        context,
        repo,
        repo.root_path(),
        &output,
        cancel,
        &preview_walk,
    )? {
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
        &layout_roots,
        &protected,
        &mut output,
        0,
        &preview_walk,
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
