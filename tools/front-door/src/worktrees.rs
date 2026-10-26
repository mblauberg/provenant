//! Compose the worktree owner with verified project and fresh state roots.
use crate::{emit, fail, roots};
use provenant_cli::{Gc, Globals, Worktree};
use provenant_confinement::{
    UtilityBudget, UtilityRuntime,
    worktrees::{
        self, RetainReason, WorktreeContext, WorktreeCreate, WorktreeError, WorktreeRemove,
    },
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::clock::SystemClock;
use provenant_os::exec::SystemExecutor;
use provenant_os::fs::Directory;
use provenant_os::fs::FileKind;
use provenant_os::fs::FsError;
use provenant_os::fs::canonicalize;
use provenant_records::{CommandResult, Outcome};
use serde::Serialize;
use std::{
    cell::RefCell,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

/// A checkout writes the whole tree and a 1.8 GB repository took well over the
/// 5 second read budget. Ten minutes covers very large checkouts yet still ends
/// a hung `git worktree add`; `create` rolls back what a timeout leaves.
const MUTATION_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Root(#[from] roots::RootError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Permissions(#[from] provenant_confinement::permissions::PermissionError),
    #[error("{0}")]
    CacheGc(
        Box<provenant_confinement::cache::CacheGcError>,
        Vec<PathBuf>,
        Box<provenant_run::ProviderStateGc>,
        Vec<worktrees::WorktreeGcReport>,
        Vec<String>,
    ),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Fs(#[from] provenant_os::fs::FsError),
    #[error(transparent)]
    Exec(#[from] provenant_os::exec::ExecError),
    #[error("Git executable unavailable")]
    GitUnavailable,
    #[error("cannot establish worktree lock budget")]
    Budget,
    #[error("{0}")]
    GcObservation(&'static str),
    #[error("invalid worktree removal scope")]
    InvalidScope,
}

#[allow(clippy::print_stderr)]
fn note_kept_unmerged() {
    eprintln!(
        "note: the branch is not merged into the integration branch and was kept; pass --discard to delete it"
    );
}

#[allow(clippy::print_stderr)]
fn warn_fetch(error: &Error) {
    eprintln!("warning: {error}; using the current integration resolution");
}

impl Error {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Root(roots::RootError::Checkout | roots::RootError::State(_))
            | Self::InvalidScope
            | Self::Worktree(
                WorktreeError::InvalidRequest | WorktreeError::NotFound | WorktreeError::CheckedOut,
            ) => Outcome::Invalid,
            Self::Worktree(
                WorktreeError::Dirty
                | WorktreeError::GitOperationInProgress
                | WorktreeError::LiveWriter
                | WorktreeError::UnknownWriter
                | WorktreeError::Primary
                | WorktreeError::IdentityChanged
                | WorktreeError::PrimaryMoved,
            ) => Outcome::Refused,
            Self::Worktree(WorktreeError::Cancelled) => Outcome::Cancelled,
            _ => Outcome::Error,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Data {
    Created(worktrees::WorktreeInfo),
    Listed(Vec<worktrees::WorktreeInfo>),
    Removed(worktrees::RemoveOutcome),
}

pub fn run(command: Worktree, globals: &Globals) -> u8 {
    let name = match &command {
        Worktree::Create { .. } => "worktree create",
        Worktree::List => "worktree list",
        Worktree::Remove(_) => "worktree remove",
    };
    let read_only = matches!(command, Worktree::List);
    match execute(command, globals) {
        Ok(data) => match serde_json::to_value(&data) {
            Ok(value) => emit(
                &CommandResult::new(name, Outcome::Ok, data),
                globals,
                &provenant_cli::terse(&value),
                read_only,
            ),
            Err(_) => fail(
                name,
                Outcome::Error,
                "ENCODE",
                "cannot encode worktree result",
                globals,
            ),
        },
        Err(error) => fail(
            name,
            error.outcome(),
            "WORKTREE",
            &error.to_string(),
            globals,
        ),
    }
}

fn execute(command: Worktree, globals: &Globals) -> Result<Data, Error> {
    let assets = roots::product_root()?;
    let project = roots::selected_project(globals.project.as_deref(), true)?;
    let roots = roots::roots(globals, &assets, &project)?;
    with_context(&roots, &project, MUTATION_TIMEOUT, |context, cancel| {
        Ok(match command {
            Worktree::Create { branch, base } => Data::Created(worktrees::create(
                context,
                WorktreeCreate { branch, base },
                cancel,
            )?),
            Worktree::List => Data::Listed(worktrees::list(context, cancel)?),
            Worktree::Remove(mut args) => {
                let upstream = if args.discard {
                    None
                } else {
                    match crate::gc_support::fetch_default(context) {
                        Ok(upstream) => upstream,
                        Err(error) => {
                            warn_fetch(&error);
                            None
                        }
                    }
                };
                if let Some((reference, _)) = upstream
                    && args.integration.is_none()
                {
                    args.integration = Some(
                        reference
                            .strip_prefix("refs/remotes/")
                            .ok_or(Error::InvalidScope)?
                            .to_owned(),
                    );
                }
                let named = args.name.is_some();
                let request = if let Some(name) = args.name {
                    WorktreeRemove::Name {
                        name,
                        discard: args.discard,
                        integration: args.integration,
                    }
                } else {
                    WorktreeRemove::Merged {
                        integration: args.integration.ok_or(Error::InvalidScope)?,
                        scope: match args.since {
                            Some(oid) => worktrees::PruneScope::Since(oid),
                            None => worktrees::PruneScope::Branches(args.branch),
                        },
                    }
                };
                let removed = worktrees::remove(context, request, cancel)?;
                if named
                    && !args.discard
                    && removed
                        .retained
                        .iter()
                        .any(|kept| kept.reason == RetainReason::NotMerged)
                {
                    note_kept_unmerged();
                }
                Data::Removed(removed)
            }
        })
    })
}

#[derive(Serialize, Default)]
struct GcData {
    #[serde(flatten)]
    report: provenant_confinement::cache::CacheGcReport,
    idle_days: u64,
    provider_state: provenant_run::ProviderStateGc,
    worktree_cleanup: Vec<worktrees::WorktreeGcReport>,
    alerts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unobserved_projects: Vec<PathBuf>,
}

/// `provenant gc`: remove orphaned and idle lane caches, sparing any whose
/// worktree has a live or unobservable writer.
pub fn run_gc(command: Gc, globals: &Globals) -> u8 {
    match gc(&command, globals) {
        Ok(data) => {
            let human = gc_data_summary(&data);
            let failed = !data.provider_state.errors.is_empty();
            let mut result = CommandResult::new(
                "gc",
                if failed { Outcome::Error } else { Outcome::Ok },
                data,
            );
            if failed {
                result.error = Some(provenant_records::CommandError {
                    code: "GC".into(),
                    message:
                        "provider-state retention incomplete; inspect data.provider_state.errors"
                            .into(),
                    cause: None,
                    repair: None,
                });
            }
            emit(&result, globals, &human, command.dry_run)
        }
        Err(Error::CacheGc(
            error,
            unobserved_projects,
            provider_state,
            worktree_cleanup,
            alerts,
        )) => {
            let error = *error;
            let failure = error.to_string();
            let data = GcData {
                report: error.report,
                idle_days: command.idle_days,
                unobserved_projects,
                provider_state: *provider_state,
                worktree_cleanup,
                alerts,
            };
            let message = format!("{failure}\n{}", gc_data_summary(&data));
            let mut result = CommandResult::new("gc", Outcome::Error, data);
            result.error = Some(provenant_records::CommandError {
                code: "GC".into(),
                message,
                cause: None,
                repair: None,
            });
            emit(&result, globals, "", command.dry_run)
        }
        Err(error) => fail("gc", error.outcome(), "GC", &error.to_string(), globals),
    }
}

fn gc_data_summary(data: &GcData) -> String {
    let mut human = gc_summary(&data.report);
    human.push_str(&format!(
        "{} provider-state removal(s), {} kept, {} error(s)\n",
        data.provider_state.removed.len(),
        data.provider_state.kept.len(),
        data.provider_state.errors.len()
    ));
    for project in &data.worktree_cleanup {
        if let Some(error) = &project.upstream_error {
            human.push_str(&format!("{error}; merged worktrees kept\n"));
        }
        for tree in &project.worktrees.removed {
            human.push_str(&format!(
                "{} merged worktree {}\n",
                if project.dry_run {
                    "would remove"
                } else {
                    "removed"
                },
                tree.path.display()
            ));
        }
        for output in &project.output {
            human.push_str(&format!("{} {}\n", output.reason, output.path.display()));
        }
    }
    for alert in &data.alerts {
        human.push_str(&format!("{alert}\n"));
    }
    human
}

fn gc_summary(report: &provenant_confinement::cache::CacheGcReport) -> String {
    let mut human = String::new();
    let verb = if report.dry_run {
        "would remove"
    } else {
        "removed"
    };
    for entry in &report.removed {
        human.push_str(&format!(
            "{verb} {} ({} bytes, {})\n",
            entry.path.display(),
            entry.bytes,
            gc_reason(entry.reason)
        ));
    }
    for entry in &report.kept {
        human.push_str(&format!(
            "kept {} ({})\n",
            entry.path.display(),
            gc_reason(entry.reason)
        ));
    }
    human.push_str(&format!(
        "{} cache(s), {} bytes {}\n",
        report.removed.len(),
        report.bytes_freed,
        if report.dry_run {
            "reclaimable"
        } else {
            "freed"
        }
    ));
    human
}

fn gc_reason(reason: provenant_confinement::cache::CacheGcReason) -> &'static str {
    use provenant_confinement::cache::CacheGcReason;
    match reason {
        CacheGcReason::Orphaned => "orphaned",
        CacheGcReason::Idle => "idle",
        CacheGcReason::FreeFloor => "free_floor",
        CacheGcReason::Tombstone => "tombstone",
        CacheGcReason::Foreign => "foreign",
        CacheGcReason::Live => "live",
        CacheGcReason::Unobservable => "unobservable",
    }
}

struct GcProject {
    binding: provenant_config::ProjectBinding,
    state: Directory,
    lanes: Vec<worktrees::WorktreeInfo>,
    path: PathBuf,
}

fn gc_project(roots: &provenant_core::Roots, project: &Path) -> Result<GcProject, Error> {
    with_context(roots, project, MUTATION_TIMEOUT, |context, cancel| {
        Ok(GcProject {
            binding: context.binding.clone(),
            path: project.to_owned(),
            state: Directory::open_anchor(
                &roots
                    .state
                    .as_path()
                    .join("worktrees")
                    .join(context.binding.project_id().to_string()),
            )?,
            lanes: worktrees::list(context, cancel)?,
        })
    })
}

fn gc(command: &Gc, globals: &Globals) -> Result<GcData, Error> {
    use provenant_confinement::cache::{CacheGcPolicy, gc_worktree_cargo_caches_with_writer_guard};
    use provenant_confinement::worktrees::{WorkerState, WriterState};

    let assets = roots::product_root()?;
    let home = roots::home()?;
    let project = if command.scheduled {
        home.clone()
    } else {
        roots::selected_project(globals.project.as_deref(), true)?
    };
    let roots = roots::roots(globals, &assets, &project)?;
    let mut projects = Vec::new();
    let mut project_ids = Vec::new();
    let mut unobserved_projects = Vec::new();
    let config = provenant_config::read_with_domains(&roots, &provenant_cli::domains::SECTIONS)?;
    if command.scheduled {
        if !config.gc.schedule {
            return Ok(GcData {
                report: provenant_confinement::cache::CacheGcReport {
                    dry_run: command.dry_run,
                    ..Default::default()
                },
                idle_days: command.idle_days,
                provider_state: Default::default(),
                unobserved_projects,
                ..Default::default()
            });
        }
        for binding in provenant_config::registered_projects(&roots)? {
            project_ids.push(binding.project_id());
            let common = binding.common_dir();
            let path = if common.file_name() == Some(OsStr::new(".git")) {
                common.parent().ok_or(Error::InvalidScope)?
            } else {
                common
            };
            match gc_project(&roots, path) {
                Ok(project) => projects.push(project),
                Err(_) => unobserved_projects.push(path.to_owned()),
            }
        }
    } else {
        let selected = gc_project(&roots, &project)?;
        project_ids.push(selected.binding.project_id());
        projects.push(selected);
    }
    let alerts = crate::gc_support::disk_alert(
        &roots,
        &project_ids,
        config.gc.alert_below.as_deref(),
        command.dry_run,
    )
    .unwrap_or_else(|error| vec![format!("disk alert failed: {error}")]);
    let mut worktree_cleanup = Vec::new();
    for project in &mut projects {
        let settings = config
            .gc
            .projects
            .get(&project.binding.project_id().to_string())
            .cloned()
            .unwrap_or_default();
        let cleanup = with_context(
            &roots,
            &project.path,
            MUTATION_TIMEOUT,
            |context, cancel| {
                // A failed fetch cannot grant removal authority. Output pruning
                // still has its independent terminal-state proof.
                let (upstream, upstream_error) = match crate::gc_support::fetch_default(context) {
                    Ok(upstream) => (upstream, None),
                    Err(error) => (None, Some(error.to_string())),
                };
                // Fetch can take a while, so begin the single sweep snapshot
                // only after it completes. A stale snapshot is refreshed by
                // the callbacks below, which collect() invokes under the
                // worktree writer lock before each mutation.
                let states = RefCell::new(
                    crate::gc_support::lane_states(&roots, project.binding.project_id()).ok(),
                );
                let rescan_failed = std::cell::Cell::new(false);
                let refresh = || {
                    crate::gc_support::refresh_if_stale(
                        &states,
                        &rescan_failed,
                        |snapshot| snapshot.is_current(),
                        || {
                            crate::gc_support::lane_states(&roots, project.binding.project_id())
                                .ok()
                        },
                    );
                };
                let terminal = |path: &Path| {
                    refresh();
                    states
                        .borrow()
                        .as_ref()
                        .and_then(|snapshot| snapshot.get(path))
                        == Some(&true)
                };
                let no_live_lanes = |path: &Path| {
                    refresh();
                    states
                        .borrow()
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.no_live_lanes(path))
                };
                let now_ns = SystemClock
                    .wall_now()
                    .map(|now| i128::from(now.as_micros()) * 1_000)
                    .map_err(|_| Error::Budget)?;
                let mut report = worktrees::collect(
                    context,
                    &project.lanes,
                    &worktrees::WorktreeGcPolicy {
                        upstream: upstream.map(|(_, oid)| oid),
                        terminal: &terminal,
                        no_live_lanes: &no_live_lanes,
                        regenerable: &settings.regenerable,
                        keep: &settings.keep,
                        written_before_ns: now_ns - 86_400 * 1_000_000_000,
                        dry_run: command.dry_run || !command.apply,
                    },
                    cancel,
                )?;
                report.upstream_error = upstream_error;
                Ok(report)
            },
        );
        match cleanup {
            Ok(cleanup) => worktree_cleanup.push(cleanup),
            Err(_) => unobserved_projects.push(project.path.clone()),
        }
    }
    let provider_state = crate::provider_retention::collect(
        &roots,
        &home,
        &assets,
        config.retention.provider_state_days,
        command.dry_run,
    );
    let lanes: Vec<_> = projects.iter().flat_map(|project| &project.lanes).collect();
    let now_ns = SystemClock
        .wall_now()
        .map(|now| i128::from(now.as_micros()) * 1_000)
        .map_err(|_| Error::Budget)?;
    let policy = CacheGcPolicy {
        idle_before_ns: Some(now_ns - i128::from(command.idle_days) * 86_400 * 1_000_000_000),
        free_floor_bytes: command.free_floor,
        dry_run: command.dry_run,
        protected_owners: lanes
            .iter()
            .filter(|info| info.writer != WriterState::Free)
            .map(|info| (info.path.clone(), info.writer))
            .collect(),
        project_owners: lanes.iter().map(|info| info.path.clone()).collect(),
    };
    let report = match gc_worktree_cargo_caches_with_writer_guard(&home, &policy, |path| {
        for project in &projects {
            if let Some(lane) = project.lanes.iter().find(|lane| lane.path == path) {
                return worktrees::cache_writer_guard(
                    &project.binding,
                    &project.state,
                    path,
                    lane.branch.as_deref(),
                );
            }
        }
        Err(WorkerState::Unknown)
    }) {
        Ok(report) => report,
        Err(error) => {
            return Err(Error::CacheGc(
                Box::new(error),
                unobserved_projects,
                Box::new(provider_state),
                worktree_cleanup,
                alerts,
            ));
        }
    };
    Ok(GcData {
        report,
        idle_days: command.idle_days,
        provider_state,
        unobserved_projects,
        worktree_cleanup,
        alerts,
    })
}

/// Run one worktree-owner action for `project` under fresh state `roots`,
/// with Git mutations bounded by `mutation`. Peer ingress uses it to create a
/// peer writer's worktree within its peer-call budget.
pub(crate) fn with_context<T>(
    roots: &provenant_core::Roots,
    project: &Path,
    mutation: Duration,
    action: impl FnOnce(&WorktreeContext<'_>, &CancelToken) -> Result<T, Error>,
) -> Result<T, Error> {
    // Create the fresh state hierarchy through held, no-follow directories.
    let mut state = Directory::root(true)?;
    for component in roots.state.as_path().components() {
        if let Component::Normal(name) = component {
            match state.mkdir(name, 0o700) {
                Ok(()) => state.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            state = match state.open_verified_directory(name) {
                Ok(child) => child,
                // Only the configured absolute state path may cross mounts.
                // Re-anchor the held no-follow child after checking identity.
                Err(error) if error.is_cross_device() => {
                    let before = state.metadata_at(name, false)?;
                    let child = state.open_directory(name)?;
                    let opened = child.metadata()?;
                    let after = state.metadata_at(name, false)?;
                    if before.kind != FileKind::Directory
                        || !before.same_identity(&opened)
                        || !opened.same_identity(&after)
                    {
                        return Err(FsError::RaceRefused { name: name.into() }.into());
                    }
                    child.into_anchor()?
                }
                Err(error) => return Err(error.into()),
            };
        }
    }
    let utility_home = child(&state, "utility-home")?;
    let git = super::roots::git_binary().ok_or(Error::GitUnavailable)?;
    let runtime = UtilityRuntime::new(
        canonicalize(&git)?,
        roots.state.as_path().join("utility-home"),
    )?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = runtime.open_repo(&executor, project, budget, &cancel)?;
    let binding = provenant_config::bind_project(
        roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let worktrees_state = child(&state, "worktrees")?;
    let project_state = child(&worktrees_state, &binding.project_id().to_string())?;
    let clock = SystemClock;
    let home = roots::home()?;
    let context = WorktreeContext {
        home: &home,
        repo: &repo,
        binding: &binding,
        project_state: &project_state,
        executor: &executor,
        utility: &runtime,
        #[cfg(not(test))]
        workers: &worktrees::SystemWorkerObserver,
        #[cfg(test)]
        workers: &crate::worktree_tests::FreeWorkers,
        clock: &clock,
        utility_budget: budget,
        mutation_budget: UtilityBudget::bounded(mutation)?,
        lock_budget: WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Budget)?,
    };
    let result = action(&context, &cancel);
    drop(utility_home);
    result
}

fn child(
    parent: &provenant_os::fs::Directory,
    name: &str,
) -> Result<provenant_os::fs::Directory, Error> {
    let name = OsStr::new(name);
    match parent.mkdir(name, 0o700) {
        Ok(()) => parent.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(parent.open_verified_directory(name)?)
}
