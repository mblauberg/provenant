//! Compose the worktree owner with verified project and fresh state roots.
use crate::{emit, fail, roots};
use provenant_cli::{Gc, Globals, Worktree};
use provenant_confinement::{
    UtilityBudget, UtilityRuntime,
    worktrees::{
        self, RetainReason, WorkerObserver, WorktreeContext, WorktreeCreate, WorktreeError,
        WorktreeRemove,
    },
};
use provenant_core::{CancelToken, Clock, GitOid, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    exec::SystemExecutor,
    fs::{Directory, FileKind, FsError},
};
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
    #[error("graph checkpoint collection failed: {0}")]
    GraphGc(String),
    #[error("{0}")]
    GcObservation(&'static str),
    #[error("cannot read disk space: {0}")]
    DiskSpace(std::io::Error),
    #[error("invalid worktree removal scope")]
    InvalidScope,
    #[error("target seed build: {0}")]
    SeedBuild(String),
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

pub(crate) fn local_worktree_message(error: &WorktreeError) -> String {
    use provenant_records::diagnostic::{Destination, safe_path, safe_text};
    match error {
        WorktreeError::CensusUnknown { path, detail }
        | WorktreeError::UnknownOccupancy { path, detail } => {
            let scope = safe_path(path, Destination::Local)
                .unwrap_or_else(|| "the requested worktree".into());
            format!("{error}: {scope}: {}", safe_text(detail))
        }
        _ => error.to_string(),
    }
}

impl Error {
    fn local_message(&self) -> String {
        match self {
            Self::Worktree(error) => local_worktree_message(error),
            _ => self.to_string(),
        }
    }

    fn repair(&self) -> Option<&'static str> {
        match self {
            Self::Worktree(error) => error.repair(),
            _ => None,
        }
    }

    fn outcome(&self) -> Outcome {
        match self {
            Self::Root(roots::RootError::Checkout | roots::RootError::State(_))
            | Self::InvalidScope
            | Self::Worktree(
                WorktreeError::InvalidRequest
                | WorktreeError::AmbiguousName { .. }
                | WorktreeError::NotFound
                | WorktreeError::CheckedOut
                | WorktreeError::BranchExistsWithBase { .. }
                | WorktreeError::BranchCollision { .. },
            ) => Outcome::Invalid,
            Self::Worktree(
                WorktreeError::Dirty(_)
                | WorktreeError::DetachedPrimary
                | WorktreeError::DetachedUnmerged { .. }
                | WorktreeError::IgnoredPaths(_)
                | WorktreeError::GitOperationInProgress
                | WorktreeError::LockBusy
                | WorktreeError::LiveWriter
                | WorktreeError::LiveProcess(_)
                | WorktreeError::UnknownWriter
                | WorktreeError::UnknownOccupancy { .. }
                | WorktreeError::GitEntryHeld { .. }
                | WorktreeError::IncompleteCensus { .. }
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
    SeedBuilt(String),
    Created(Box<worktrees::WorktreeInfo>),
    Listed(Vec<worktrees::WorktreeInfo>),
    Diagnosed(worktrees::WorktreeDiagnosis),
    Removed(worktrees::RemoveOutcome),
}

pub fn run(command: Worktree, globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    if matches!(command, Worktree::SessionList) {
        return session_list(globals, workers);
    }
    let name = match &command {
        Worktree::SeedBuild => "worktree seed-build",
        Worktree::Create { .. } => "worktree create",
        Worktree::List => "worktree list",
        Worktree::Doctor { .. } => "worktree doctor",
        Worktree::SessionList => unreachable!(),
        Worktree::Remove(_) => "worktree remove",
    };
    let read_only = matches!(command, Worktree::List | Worktree::Doctor { .. });
    match execute(command, globals, workers) {
        Ok(data) => match serde_json::to_value(&data) {
            Ok(value) => emit(
                &CommandResult::new(name, Outcome::Ok, data),
                globals,
                &human(value),
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
        Err(error) => crate::fail_with_repair(
            name,
            error.outcome(),
            if matches!(error, Error::Worktree(WorktreeError::LockBusy)) {
                "WORKTREE_LOCK_BUSY"
            } else {
                "WORKTREE"
            },
            &error.local_message(),
            error.repair(),
            globals,
        ),
    }
}

/// The terse human form. `--json` keeps the structured path envelope; here a
/// UTF-8 absolute path reads as a plain path, quoted only when it needs it.
fn human(mut value: serde_json::Value) -> String {
    plain_paths(&mut value);
    provenant_cli::terse(&value)
}

fn plain_paths(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            if let Some(path) = map.get("path").and_then(plain_path) {
                map.insert("path".into(), Value::String(path));
            }
            map.values_mut().for_each(plain_paths);
        }
        Value::Array(items) => items.iter_mut().for_each(plain_paths),
        _ => {}
    }
}

fn plain_path(wire: &serde_json::Value) -> Option<String> {
    match wire["kind"].as_str()? {
        "root" => Some("/".into()),
        "path" if wire["relative"]["encoding"] == "utf8" => Some(format!(
            "{}/{}",
            wire["root"].as_str()?.trim_end_matches('/'),
            wire["relative"]["path"].as_str()?
        )),
        _ => None,
    }
}

fn execute(
    command: Worktree,
    globals: &Globals,
    workers: &dyn WorkerObserver,
) -> Result<Data, Error> {
    let resolved = roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Required,
    )?;
    let (roots, home, project) = (&resolved.roots, &resolved.home, &resolved.project);
    if matches!(command, Worktree::SeedBuild) {
        let generation = crate::target_seed_build::build(roots.state.as_path(), project)
            .map_err(|error| Error::SeedBuild(error.to_string()))?;
        return Ok(Data::SeedBuilt(generation));
    }
    let facts = worktrees::RepoFacts::default();
    with_context_using_facts(
        roots,
        home,
        project,
        MUTATION_TIMEOUT,
        workers,
        &facts,
        |context, cancel| {
            Ok(match command {
                Worktree::SeedBuild => unreachable!(),
                Worktree::Create { branch, base } => Data::Created(Box::new(worktrees::create(
                    context,
                    WorktreeCreate { branch, base },
                    cancel,
                )?)),
                Worktree::List => Data::Listed(worktrees::list(context, cancel)?),
                Worktree::Doctor { path } => {
                    Data::Diagnosed(worktrees::doctor(context, &path, cancel)?)
                }
                Worktree::SessionList => unreachable!(),
                Worktree::Remove(mut args) => {
                    let upstream = if args.discard {
                        None
                    } else {
                        match crate::gc_support::fetch_default(context, cancel) {
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
                            expected_head: None,
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
                    let config = provenant_cli::config_reads::read_for(roots, "worktree")?;
                    let output = config
                        .gc
                        .projects
                        .get(&context.binding.project_id().to_string())
                        .cloned()
                        .unwrap_or_default();
                    let removed = worktrees::remove_with_output(context, request, cancel, &output)?;
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
        },
    )
}

/// The hook reads complete newline-delimited records even when its child
/// deadline stops this command before the inventory is complete.
fn session_list(globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    use std::io::Write;
    let result = (|| -> Result<(), Error> {
        let resolved = roots::resolve(
            globals,
            roots::Workspace::Checkout,
            roots::Product::Required,
        )?;
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        let mut failed = false;
        with_context(
            &resolved.roots,
            &resolved.home,
            &resolved.project,
            MUTATION_TIMEOUT,
            workers,
            |context, cancel| {
                worktrees::list_streaming(context, cancel, |total, row| {
                    if failed {
                        return;
                    }
                    let event = match row {
                        None => serde_json::json!({"total": total}),
                        Some(row) => serde_json::json!({"worktree": row}),
                    };
                    failed = serde_json::to_writer(&mut output, &event).is_err()
                        || output.write_all(b"\n").is_err()
                        || output.flush().is_err();
                })?;
                Ok(())
            },
        )?;
        if failed {
            return Err(Error::GcObservation("cannot write worktree inventory"));
        }
        Ok(())
    })();
    if result.is_ok() { 0 } else { 1 }
}

#[derive(Serialize, Default)]
struct GcData {
    #[serde(flatten)]
    report: provenant_confinement::cache::CacheGcReport,
    idle_days: u64,
    provider_state: provenant_run::ProviderStateGc,
    chair_logs: crate::chair_log_retention::Report,
    target_seeds: provenant_run::target_seed_gc::Report,
    build_dirs: provenant_run::build_dir_gc::Report,
    graph_checkpoints_removed: usize,
    staging_busy: Vec<PathBuf>,
    worktree_cleanup: Vec<worktrees::WorktreeGcReport>,
    alerts: Vec<String>,
    unobserved_projects: Vec<PathBuf>,
    skipped_transient: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

const GC_REPAIR: &str =
    "Run `provenant gc --apply --json` to inspect failed paths and retry incomplete collection.";

fn gc_failure<'a>(data: &'a GcData, message: &str, detailed: bool) -> CommandResult<GcOutput<'a>> {
    let mut result = CommandResult::new("gc", Outcome::Error, gc_result_data(data, detailed));
    result.error = Some(provenant_records::CommandError {
        diagnostic: None,
        code: "GC".into(),
        message: format!("{message}\n{}{GC_REPAIR}", gc_data_summary(data)),
        cause: None,
        repair: Some(GC_REPAIR.into()),
    });
    result
}

/// Daily tick task, using the same collection policy and custody as explicit GC.
pub(crate) fn scheduled_gc(
    globals: &Globals,
    idle_days: u64,
    workers: &dyn WorkerObserver,
) -> Result<(), String> {
    let command = Gc {
        scheduled: true,
        pressure: false,
        free_floor: None,
        dry_run: false,
        apply: true,
        idle_days,
    };
    let data = gc(&command, globals, workers).map_err(|error| error.local_message())?;
    if !data.provider_state.errors.is_empty() || !data.chair_logs.errors.is_empty() {
        return Err("retention incomplete; next: provenant gc --apply".into());
    }
    let upstream_failures = data
        .worktree_cleanup
        .iter()
        .filter(|project| project.upstream_error.is_some())
        .count();
    if !data.unobserved_projects.is_empty()
        || upstream_failures > 0
        || !data.build_dirs.errors.is_empty()
    {
        return Err(format!(
            "retention incomplete: {} unobserved project(s), {upstream_failures} upstream observation failure(s), {} build-cache error(s); next: provenant gc --apply",
            data.unobserved_projects.len(),
            data.build_dirs.errors.len()
        ));
    }
    Ok(())
}

/// `provenant gc`: remove orphaned and idle lane caches, sparing any whose
/// worktree has a live or unobservable writer.
pub fn run_gc(command: Gc, globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    match gc(&command, globals, workers) {
        Ok(data) => {
            let human = gc_data_summary(&data);
            let failed =
                !data.provider_state.errors.is_empty() || !data.chair_logs.errors.is_empty();
            let result = if failed {
                gc_failure(&data, "retention incomplete", globals.json)
            } else {
                CommandResult::new("gc", Outcome::Ok, gc_result_data(&data, globals.json))
            };
            emit(&result, globals, &human, command.dry_run)
        }
        Err(Error::CacheGc(
            error,
            unobserved_projects,
            skipped_transient,
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
                skipped_transient,
                provider_state: *provider_state,
                chair_logs: Default::default(),
                target_seeds: Default::default(),
                build_dirs: Default::default(),
                graph_checkpoints_removed: 0,
                staging_busy: Vec::new(),
                worktree_cleanup,
                alerts,
                notice: None,
            };
            let result = gc_failure(&data, &failure, globals.json);
            emit(&result, globals, "", command.dry_run)
        }
        Err(error) => crate::fail_with_repair(
            "gc",
            error.outcome(),
            "GC",
            &error.local_message(),
            Some(GC_REPAIR),
            globals,
        ),
    }
}

fn gc_data_summary(data: &GcData) -> String {
    let mut human = format!(
        "{} cache(s), {} bytes {}; {} kept\n{} provider-state removal(s), {} kept, {} error(s)\n",
        data.report.removed.len(),
        data.report.bytes_freed,
        if data.report.dry_run {
            "reclaimable"
        } else {
            "freed"
        },
        data.report.kept.len(),
        data.provider_state.removed.len(),
        data.provider_state.kept.len(),
        data.provider_state.errors.len(),
    );
    let trees: usize = data
        .worktree_cleanup
        .iter()
        .map(|p| p.worktrees.removed.len())
        .sum();
    let outputs: usize = data
        .worktree_cleanup
        .iter()
        .map(|p| {
            p.output
                .iter()
                .chain(&p.idle_output)
                .filter(|entry| {
                    entry.reason != "no eligible ignored output"
                        && entry.reason != "no regenerable directories configured"
                })
                .count()
        })
        .sum();
    human.push_str(&format!("{trees} merged worktree(s), {outputs} output directory(s), {} attempt tmp directory(s), {} capture(s)\n",
        data.provider_state.attempt_tmp.len(), data.provider_state.attempt_logs.len()));
    human.push_str(&format!(
        "{} target seed generation(s), {} apparent bytes {}\n",
        data.target_seeds.removed.len(),
        data.target_seeds.bytes,
        if data.report.dry_run {
            "reclaimable"
        } else {
            "removed"
        }
    ));
    human.push_str(&format!(
        "{} lane build director(ies), {} bytes {}; {} bytes of lane build output remain, {} busy, {} recent\n",
        data.build_dirs.removed.len(),
        data.build_dirs.bytes,
        if data.report.dry_run {
            "reclaimable"
        } else {
            "reclaimed"
        },
        data.build_dirs.cache_bytes,
        data.build_dirs.busy.len(),
        data.build_dirs.recent.len(),
    ));
    for error in &data.build_dirs.errors {
        human.push_str(&format!("lane build output: {error}\n"));
    }
    for temporary in &data.provider_state.attempt_tmp {
        human.push_str(&format!(
            "{} attempt tmp {} ({} bytes)\n",
            if data.report.dry_run {
                "would remove"
            } else {
                "removed"
            },
            temporary.path.display(),
            temporary.bytes,
        ));
    }
    let payload = &data.provider_state.payload;
    if payload.cap_bytes > 0 {
        human.push_str(&format!(
            "payload {} bytes accounted of {} cap, {} pinned, {} {} ({} attempt(s)), {} reclaimed in total\n",
            payload.accounted_bytes,
            payload.cap_bytes,
            payload.pinned_bytes,
            payload.reclaimed_bytes,
            if data.report.dry_run {
                "reclaimable"
            } else {
                "reclaimed"
            },
            payload.evicted.len(),
            payload.total_reclaimed_bytes,
        ));
    }
    if data.provider_state.expired_events > 0 {
        human.push_str(&format!(
            "{} {} event rows\n",
            if data.report.dry_run {
                "would expire"
            } else {
                "expired"
            },
            data.provider_state.expired_events
        ));
    }
    if data.chair_logs.expired > 0 {
        human.push_str(&format!(
            "{} {} chair log entries\n",
            if data.report.dry_run {
                "would expire"
            } else {
                "expired"
            },
            data.chair_logs.expired
        ));
    }
    for error in &data.chair_logs.errors {
        human.push_str(&format!("{}: {}\n", error.path.display(), error.reason));
    }
    for error in &data.provider_state.errors {
        human.push_str(&format!("{}: {}\n", error.path.display(), error.reason));
    }
    for entry in &data.report.kept {
        if entry.reason == provenant_confinement::cache::CacheGcReason::Unobservable {
            human.push_str(&format!("kept {} (unobservable)\n", entry.path.display()));
        }
    }
    for project in &data.worktree_cleanup {
        if let Some(error) = &project.upstream_error {
            human.push_str(&format!("{error}; merged worktrees kept\n"));
        }
        for kept in &project.worktrees.retained {
            if let Some(detail) = &kept.detail {
                human.push_str(&format!(
                    "kept worktree {} ({:?}): {detail}\n",
                    kept.branch, kept.reason
                ));
            } else if retained_exception(kept.reason) {
                human.push_str(&format!(
                    "kept worktree {} ({:?})\n",
                    kept.branch, kept.reason
                ));
            }
        }
        for outcome in &project.refs {
            use provenant_confinement::worktrees::ArtefactState;
            let verb = match outcome.outcome {
                ArtefactState::Removed => "removed",
                ArtefactState::WouldRemove => "would remove",
                ArtefactState::Failed => "could not remove",
                _ => continue,
            };
            let kind = serde_json::to_value(outcome.artefact)
                .ok()
                .and_then(|value| value.as_str().map(|kind| kind.replace('_', " ")))
                .unwrap_or_default();
            human.push_str(&format!("{verb} {kind} {}", outcome.name));
            if let Some(remote) = &outcome.remote {
                human.push_str(&format!(" on {remote}"));
            }
            if let Some(reason) = &outcome.reason {
                human.push_str(&format!(" ({reason})"));
            }
            human.push('\n');
        }
        for output in project
            .output
            .iter()
            .chain(&project.idle_output)
            .filter(|entry| !entry.removed && entry.reason != "would_remove")
        {
            human.push_str(&format!("{} {}\n", output.reason, output.path.display()));
        }
        for output in project
            .idle_output
            .iter()
            .filter(|entry| entry.reason == "would_remove")
        {
            human.push_str(&format!(
                "would reclaim idle output {} (idle output and custody checks passed)\n",
                output.path.display()
            ));
        }
    }
    for path in &data.unobserved_projects {
        human.push_str(&format!("unobserved project {}\n", path.display()));
    }
    for path in &data.skipped_transient {
        human.push_str(&format!("skipped transient project {}\n", path.display()));
    }
    for alert in &data.alerts {
        human.push_str(&format!("{alert}\n"));
    }
    if let Some(notice) = &data.notice {
        human.push_str(&format!("{notice}\n"));
    }
    human
}

fn retained_exception(reason: RetainReason) -> bool {
    matches!(
        reason,
        RetainReason::Dirty
            | RetainReason::GitOperationInProgress
            | RetainReason::UnknownWriter
            | RetainReason::LockBusy
            | RetainReason::Changed
            | RetainReason::GitFailure
            | RetainReason::LockedOrPrunable
    )
}

#[derive(Serialize)]
#[serde(untagged)]
enum GcOutput<'a> {
    Detailed(&'a GcData),
    Summary {
        dry_run: bool,
        removed: usize,
        kept: usize,
        bytes_freed: u64,
        provider_state_removed: usize,
        build_dirs_removed: usize,
        build_dir_bytes_reclaimed: u64,
        build_dir_bytes: u64,
        graph_checkpoints_removed: usize,
        errors: &'a [provenant_run::ProviderStateIssue],
        unobserved_projects: &'a [PathBuf],
        alerts: &'a [String],
        notice: &'a Option<String>,
    },
}
fn gc_result_data(data: &GcData, detailed: bool) -> GcOutput<'_> {
    if detailed {
        return GcOutput::Detailed(data);
    }
    GcOutput::Summary {
        dry_run: data.report.dry_run,
        removed: data.report.removed.len(),
        kept: data.report.kept.len(),
        bytes_freed: data.report.bytes_freed,
        provider_state_removed: data.provider_state.removed.len(),
        build_dirs_removed: data.build_dirs.removed.len(),
        build_dir_bytes_reclaimed: data.build_dirs.bytes,
        build_dir_bytes: data.build_dirs.cache_bytes,
        graph_checkpoints_removed: data.graph_checkpoints_removed,
        errors: &data.provider_state.errors,
        unobserved_projects: &data.unobserved_projects,
        alerts: &data.alerts,
        notice: &data.notice,
    }
}

struct GcProject {
    binding: provenant_config::ProjectBinding,
    state: Directory,
    lanes: Vec<worktrees::WorktreeInfo>,
    path: PathBuf,
}

fn gc_project(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    read_only: bool,
) -> Result<GcProject, Error> {
    with_context_mode(
        roots,
        home,
        project,
        if read_only {
            Duration::from_secs(2)
        } else {
            MUTATION_TIMEOUT
        },
        workers,
        facts,
        read_only,
        |context, cancel| {
            if gc_primary_checkout(
                context.repo,
                context.executor,
                context.utility,
                facts,
                cancel,
            )? != context.repo.root_path()
            {
                return Err(Error::GcObservation("GC requires the primary checkout"));
            }
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
                lanes: worktrees::list_for_gc(context, cancel)?,
            })
        },
    )
}

fn gc_primary_checkout<'a>(
    repo: &'a provenant_confinement::repo::VerifiedRepo,
    executor: &dyn provenant_os::exec::Executor,
    runtime: &UtilityRuntime,
    facts: &worktrees::RepoFacts,
    cancel: &CancelToken,
) -> Result<&'a Path, Error> {
    if facts.bare_setting(
        repo,
        executor,
        runtime,
        UtilityBudget::bounded(Duration::from_secs(5))?,
        cancel,
    )? != Some(false)
    {
        return Err(Error::GcObservation(
            "GC cannot select a primary checkout for a bare repository or unknown repository configuration",
        ));
    }
    let common = repo.common_git_dir();
    if common.file_name() == Some(OsStr::new(".git")) {
        common.parent().ok_or(Error::InvalidScope)
    } else if repo.private_git_dir() == common {
        Ok(repo.root_path())
    } else {
        Err(Error::GcObservation(
            "GC cannot use a linked worktree as the primary checkout",
        ))
    }
}

/// What `land` did to a merged branch's registered worktree.
#[derive(Debug, Default)]
pub(crate) struct LandWorktree {
    /// A worktree was registered for the branch.
    pub registered: bool,
    /// Declared build output deleted from it first, such as `target`.
    pub output: Vec<PathBuf>,
    pub removed: bool,
    pub branch_deleted: bool,
    /// Why the worktree or branch was kept, when it was.
    pub kept: Option<String>,
}

/// Remove the registered worktree of a merged branch through the same removal
/// as `worktree remove`, after deleting the build output GC already treats as
/// regenerable (`gc.projects.<id>.regenerable` plus `target`). Output pruning
/// keeps GC's checks: the lane must be idle with terminal writers, tracked
/// and kept paths stay, and symlinks are never followed. Removal refuses a
/// dirty or live worktree and keeps a branch not contained in `integration`.
/// The worktree is only removed while its HEAD is still `tip`, the commit the
/// caller proved merged, so commits made since keep the checkout too.
pub(crate) fn land_cleanup(
    globals: &Globals,
    branch: &str,
    integration: &str,
    tip: GitOid,
    workers: &dyn WorkerObserver,
) -> Result<LandWorktree, Error> {
    let roots::Resolved {
        assets,
        home,
        project,
        roots,
        ..
    } = roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Required,
    )?;
    let config = provenant_cli::config_reads::read_for(&roots, "worktree")?;
    with_context(
        &roots,
        &home,
        &project,
        MUTATION_TIMEOUT,
        workers,
        |context, cancel| {
            let mut landed = LandWorktree::default();
            let Some(lane) = worktrees::list(context, cancel)?.into_iter().find(|lane| {
                lane.branch
                    .as_deref()
                    .is_some_and(|name| name.strip_prefix("refs/heads/") == Some(branch))
            }) else {
                return Ok(landed);
            };
            landed.registered = true;
            if std::env::current_dir()
                .map_err(roots::RootError::from)?
                .starts_with(&lane.path)
            {
                landed.kept = Some(
                    "this command runs inside that worktree; run it from another checkout".into(),
                );
                return Ok(landed);
            }
            if lane.head != tip {
                landed.kept = Some(format!(
                    "the worktree is at {}, not the merged {tip}",
                    lane.head
                ));
                return Ok(landed);
            }
            let (observations, details) = context
                .workers
                .observe_roots_with_details(std::slice::from_ref(&lane.path));
            match observations.get(&lane.path) {
                Some(worktrees::Occupancy::Available) => {}
                Some(worktrees::Occupancy::Observed { .. }) => {
                    landed.kept =
                        Some(worktrees::live_refusal(context.workers, &lane.path).to_string());
                    return Ok(landed);
                }
                _ => {
                    landed.kept = Some(
                        WorktreeError::IncompleteCensus {
                            detail: details
                                .get(&lane.path)
                                .cloned()
                                .unwrap_or_else(|| workers.incomplete_census_detail()),
                        }
                        .to_string(),
                    );
                    return Ok(landed);
                }
            }
            let project_id = context.binding.project_id();
            let settings = config
                .gc
                .projects
                .get(&project_id.to_string())
                .cloned()
                .unwrap_or_default();
            let mut regenerable = settings.regenerable.clone();
            if !regenerable.iter().any(|name| name == "target") {
                regenerable.push("target".into());
            }
            let snapshot = crate::gc_support::lane_states(&roots, &home, &assets, project_id).ok();
            // A branch landed by hand may never have had a lane, so unlike a
            // sweep this accepts a worktree no lane used. A live or unobservable
            // lane, or a stale scan, still refuses.
            let idle = |path: &Path| {
                snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.no_live_lanes(path))
            };
            let (terminal, no_live_lanes) = (idle, idle);
            // The merge is the proof of completion, so build output written in
            // the last day does not hold the worktree back as it does in a sweep.
            let now_ns = SystemClock
                .wall_now()
                .map(|now| i128::from(now.as_micros()) * 1_000)
                .map_err(|_| Error::Budget)?;
            let collected = worktrees::collect(
                context,
                std::slice::from_ref(&lane),
                &worktrees::WorktreeGcPolicy {
                    upstream: None,
                    terminal: &terminal,
                    no_live_lanes: &no_live_lanes,
                    regenerable: &regenerable,
                    keep: &settings.keep,
                    written_before_ns: now_ns,
                    foreign_merged_clean: false,
                    dry_run: false,
                },
                cancel,
            )?;
            landed.output = collected
                .output
                .into_iter()
                .filter(|entry| entry.removed)
                .map(|entry| entry.path)
                .collect();
            match worktrees::remove_with_output(
                context,
                WorktreeRemove::Name {
                    name: branch.to_owned(),
                    discard: false,
                    integration: Some(integration.to_owned()),
                    expected_head: Some(tip),
                },
                cancel,
                &provenant_config::ProjectGcSettings {
                    regenerable,
                    ..settings
                },
            ) {
                Ok(outcome) => {
                    landed.removed = !outcome.removed.is_empty();
                    landed.branch_deleted = outcome.pruned_refs.iter().any(|name| name == branch);
                    landed.kept = outcome.retained.first().map(|kept| {
                        let detail = kept
                            .detail
                            .as_deref()
                            .map_or(String::new(), |detail| format!("; {detail}"));
                        format!("branch {} kept: {:?}{detail}", kept.branch, kept.reason)
                    });
                }
                Err(error) => {
                    let mut message = local_worktree_message(&error);
                    if let Some(repair) = error.repair() {
                        message.push_str("; ");
                        message.push_str(repair);
                    }
                    landed.kept = Some(message);
                }
            }
            Ok(landed)
        },
    )
}

fn gc(command: &Gc, globals: &Globals, workers: &dyn WorkerObserver) -> Result<GcData, Error> {
    use provenant_confinement::cache::{CacheGcPolicy, gc_worktree_cargo_caches_with_writer_guard};

    let workspace = if command.scheduled {
        roots::Workspace::Home
    } else {
        roots::Workspace::Checkout
    };
    let roots::Resolved {
        assets,
        home,
        project,
        roots,
        ..
    } = roots::resolve(globals, workspace, roots::Product::Required)?;
    let facts = worktrees::RepoFacts::default();
    let mut projects = Vec::new();
    let mut project_ids = Vec::new();
    let mut unobserved_projects = Vec::new();
    let mut skipped_transient = Vec::new();
    let config = provenant_cli::config_reads::read_for(&roots, "gc")?;
    if (command.scheduled || command.pressure) && !config.gc.schedule {
        return Ok(GcData {
            report: provenant_confinement::cache::CacheGcReport {
                dry_run: command.dry_run,
                ..Default::default()
            },
            idle_days: command.idle_days,
            provider_state: Default::default(),
            unobserved_projects,
            skipped_transient,
            notice: command
                .pressure
                .then(|| "pressure collection skipped: gc.schedule=false".into()),
            ..Default::default()
        });
    }
    if command.scheduled {
        let selection = provenant_config::registered_gc_project_selection(&roots)?;
        skipped_transient = selection.skipped_transient;
        for binding in selection.projects {
            let common = binding.common_dir();
            let path = if common.file_name() == Some(OsStr::new(".git")) {
                common.parent().ok_or(Error::InvalidScope)?
            } else {
                common
            };
            match gc_project(&roots, &home, path, workers, &facts, command.dry_run) {
                Ok(project) => {
                    project_ids.push(project.binding.project_id());
                    projects.push(project);
                }
                Err(_) => unobserved_projects.push(path.to_owned()),
            }
        }
    } else {
        let git = provenant_os::git::git_executable().ok_or(Error::GitUnavailable)?;
        let runtime = UtilityRuntime::read_only(&git)?;
        let executor = SystemExecutor::new(1)?;
        let repo = runtime.open_repo(
            &executor,
            &project,
            UtilityBudget::bounded(Duration::from_secs(5))?,
            &CancelToken::new(),
        )?;
        let common = repo.common_git_dir().to_owned();
        if provenant_config::transient_location(&roots, &common) {
            skipped_transient.push(common);
            return Ok(GcData {
                report: provenant_confinement::cache::CacheGcReport {
                    dry_run: command.dry_run,
                    ..Default::default()
                },
                idle_days: command.idle_days,
                skipped_transient,
                notice: command
                    .pressure
                    .then(|| "pressure collection skipped: transient project".into()),
                ..Default::default()
            });
        }
        let primary = gc_primary_checkout(&repo, &executor, &runtime, &facts, &CancelToken::new())?;
        let selected = gc_project(&roots, &home, primary, workers, &facts, command.dry_run)?;
        project_ids.push(selected.binding.project_id());
        projects.push(selected);
    }
    let graph_checkpoints_removed = if !command.scheduled && !command.pressure {
        crate::tick::collect_graph_checkpoints(
            &roots.state,
            project_ids.iter().copied(),
            config.gc.graph_checkpoint_days,
            command.dry_run,
            &SystemClock,
        )
        .map_err(Error::GraphGc)?
    } else {
        0
    };
    let free_floor = command
        .free_floor
        .map(Ok)
        .unwrap_or_else(|| config.gc.free_floor_bytes(roots.state.as_path()))
        .map_err(Error::DiskSpace)?;
    let output_idle_days = if Directory::open_anchor(roots.state.as_path())?
        .available_bytes()
        .map_err(Error::DiskSpace)?
        < free_floor
    {
        1
    } else {
        command.idle_days
    };
    let mut worktree_cleanup = Vec::new();
    for project in &mut projects {
        let settings = config
            .gc
            .project_settings(&project.binding.project_id().to_string(), &project.path);
        let cleanup = with_context_mode(
            &roots,
            &home,
            &project.path,
            if command.dry_run {
                Duration::from_secs(2)
            } else {
                MUTATION_TIMEOUT
            },
            workers,
            &facts,
            command.dry_run,
            |context, cancel| {
                // When the fetch fails, there is no origin, or the sweep is a
                // pressure sweep, upstream is None: the path-based pass below
                // cannot grant removal authority, and settled lane collection
                // falls back to the owner's base (the same rule settlement
                // uses, granting no new authority). Output pruning still has
                // its independent terminal-state proof.
                let (upstream, upstream_error) = if command.pressure || command.dry_run {
                    (None, None)
                } else {
                    match crate::gc_support::fetch_default(context, cancel) {
                        Ok(upstream) => (upstream, None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                };
                // Settled lanes whose settlement-time collection missed keep a
                // receipt that is not final. Collect them through the same
                // owner-bound proof before the path-based pass below, landing
                // against the fetched default branch or falling back to the
                // owner's base when upstream is None.
                let collected = if command.apply && !command.dry_run {
                    crate::fabric::gc_cleanup::collect_unsettled(
                        &roots,
                        &home,
                        &project.path,
                        project.binding.project_id(),
                        upstream.as_ref().map(|(_, oid)| *oid),
                        workers,
                    )
                    .unwrap_or_else(|error| {
                        use std::io::Write;
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "warning: settled lane collection failed for {}: {error}",
                            project.path.display()
                        );
                        Vec::new()
                    })
                } else {
                    Vec::new()
                };
                let lanes: Vec<_> = project
                    .lanes
                    .iter()
                    .filter(|lane| !collected.contains(&lane.path))
                    .cloned()
                    .collect();
                // Fetch can take a while, so begin the single sweep snapshot
                // only after it completes. A stale snapshot is refreshed by
                // the callbacks below, which collect() invokes under the
                // worktree writer lock before each mutation.
                let states = RefCell::new(
                    crate::gc_support::lane_states(
                        &roots,
                        &home,
                        &assets,
                        project.binding.project_id(),
                    )
                    .ok(),
                );
                let rescan_failed = std::cell::Cell::new(false);
                let refresh = || {
                    crate::gc_support::refresh_if_stale(
                        &states,
                        &rescan_failed,
                        |snapshot| snapshot.is_current(),
                        || {
                            crate::gc_support::lane_states(
                                &roots,
                                &home,
                                &assets,
                                project.binding.project_id(),
                            )
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
                let mut report = worktrees::collect_lazy(
                    context,
                    &lanes,
                    &worktrees::WorktreeGcPolicy {
                        upstream: upstream.as_ref().map(|(_, oid)| *oid),
                        terminal: &terminal,
                        no_live_lanes: &no_live_lanes,
                        regenerable: &settings.regenerable,
                        keep: &settings.keep,
                        written_before_ns: now_ns
                            - i128::from(output_idle_days) * 86_400 * 1_000_000_000,
                        foreign_merged_clean: !command.pressure
                            && settings.foreign_worktrees
                                == provenant_config::ForeignWorktrees::MergedClean,
                        dry_run: command.dry_run || !command.apply,
                    },
                    cancel,
                )?;
                report.upstream_error = upstream_error;
                // Only proven landed refs with unchanged ledger tips may go.
                if !command.pressure {
                    let sweep = crate::lane_cleanup::Sweep::new(command.dry_run || !command.apply);
                    report.refs = crate::lane_cleanup::collect(
                        context,
                        upstream
                            .as_ref()
                            .and_then(crate::lane_cleanup::fetched_target),
                        &sweep,
                        cancel,
                    );
                }
                Ok(report)
            },
        );
        match cleanup {
            Ok(cleanup) => worktree_cleanup.push(cleanup),
            Err(_) => unobserved_projects.push(project.path.clone()),
        }
    }
    let lanes: Vec<_> = projects.iter().flat_map(|project| &project.lanes).collect();
    let now_ns = SystemClock
        .wall_now()
        .map(|now| i128::from(now.as_micros()) * 1_000)
        .map_err(|_| Error::Budget)?;
    let policy = CacheGcPolicy {
        idle_before_ns: Some(now_ns - i128::from(command.idle_days) * 86_400 * 1_000_000_000),
        free_floor_bytes: Some(free_floor),
        max_total_bytes: Some(
            config
                .gc
                .lane_cargo_cache_max_bytes()
                .map_err(Error::DiskSpace)?,
        ),
        dry_run: command.dry_run,
        protected_owners: lanes
            .iter()
            .filter(|info| !info.occupancy.is_available())
            .map(|info| (info.path.clone(), info.occupancy.clone()))
            .collect(),
        project_owners: lanes.iter().map(|info| info.path.clone()).collect(),
        selected_only: command.pressure && !command.scheduled,
    };
    let alert_floor = config
        .gc
        .alert_below
        .as_deref()
        .map(provenant_config::parse_cache_size)
        .transpose()
        .map_err(|_| Error::GcObservation("invalid alert threshold"))?
        .unwrap_or(0)
        .max(free_floor);
    use provenant_config::{RetentionKind, RetentionTrigger, retention_schedule};
    let mut report = Ok(Default::default());
    let mut staging_busy = Vec::new();
    let mut provider_state = Default::default();
    let mut chair_logs = Default::default();
    let mut seed_collection: Result<
        provenant_run::target_seed_gc::Report,
        Box<dyn std::error::Error>,
    > = Ok(Default::default());
    let mut build_collection: Result<
        provenant_run::build_dir_gc::Report,
        Box<dyn std::error::Error>,
    > = Ok(Default::default());
    let seed_policy = &config.domains.fabric.lanes.cargo_seed;
    let build_settings = config.domains.fabric.lanes.build_settings();
    let trigger = if command.pressure {
        RetentionTrigger::Gc
    } else {
        RetentionTrigger::FullGc
    };
    for class in retention_schedule(trigger) {
        match class.class {
            RetentionKind::LaneCargoCaches => {
                report = gc_worktree_cargo_caches_with_writer_guard(&home, &policy, |path| {
                    for project in &projects {
                        if project.lanes.iter().any(|lane| lane.path == path) {
                            return if policy.dry_run {
                                worktrees::cache_writer_probe(&project.state, path)
                            } else {
                                worktrees::cache_writer_guard(&project.state, path).map(Some)
                            };
                        }
                    }
                    Err(worktrees::Occupancy::custody_unknown(
                        path.to_path_buf(),
                        "writer registration was not observed",
                    ))
                });
            }
            RetentionKind::Staging => {
                staging_busy = crate::staging_gc::collect(
                    roots.state.as_path(),
                    now_ns - i128::from(command.idle_days) * 86_400 * 1_000_000_000,
                    command.dry_run,
                )?;
            }
            RetentionKind::ProviderState => {
                provider_state = if command.pressure {
                    crate::provider_retention::collect_pressure(
                        (!command.scheduled).then_some(project_ids.as_slice()),
                        &roots,
                        &home,
                        &assets,
                        config.retention.provider_state_days,
                        config.retention.logs_days,
                        config.retention.payload_cap_bytes,
                        command.dry_run,
                    )
                } else {
                    crate::provider_retention::collect_with_logs(
                        &roots,
                        &home,
                        &assets,
                        config.retention.provider_state_days,
                        config.retention.logs_days,
                        config.retention.payload_cap_bytes,
                        command.dry_run,
                    )
                };
            }
            RetentionKind::ChairLogs => {
                chair_logs = crate::chair_log_retention::collect(
                    &roots.state,
                    provenant_workflow::log::Retention {
                        max_age_seconds: config.retention.chair_log_days * 86_400,
                        replay_horizon_seconds: config.retention.chair_log_replay_seconds(),
                        max_entries: config.retention.chair_log_entries,
                        max_bytes: config.retention.chair_log_bytes,
                    },
                    i64::try_from(now_ns / 1_000_000_000).map_err(|_| Error::Budget)?,
                    command.dry_run,
                );
            }
            RetentionKind::TargetSeeds => {
                seed_collection = if command.dry_run {
                    Ok(Default::default())
                } else {
                    provenant_run::target_seed_gc::collect(
                        roots.state.as_path(),
                        seed_policy.max_generations,
                        seed_policy.max_bytes,
                        false,
                    )
                };
            }
            RetentionKind::BuildDirectories => {
                build_collection = if command.dry_run {
                    Ok(Default::default())
                } else {
                    provenant_run::build_dir_gc::collect(
                        roots.state.as_path(),
                        &home,
                        provenant_run::build_dir_gc::Limits {
                            max_bytes: build_settings.build_dir_cap(),
                            free_floor,
                            grace_seconds: build_settings.build_dir_grace_seconds(),
                        },
                        false,
                    )
                };
            }
            // Custody-only and append-triggered classes never enter GC.
            RetentionKind::GitWorktrees
            | RetentionKind::ObservationLedgers
            | RetentionKind::QuotaHistory
            | RetentionKind::Usage => {}
        }
    }
    // Observe after every collection class, including a partially failed sweep.
    let mut alerts =
        crate::gc_support::disk_alert(&roots, &project_ids, alert_floor, command.dry_run)
            .unwrap_or_else(|error| vec![format!("disk alert failed: {error}")]);
    let target_seeds = match seed_collection {
        Ok(report) => report,
        Err(error) => {
            alerts.push(format!("target seed collection skipped: {error}"));
            Default::default()
        }
    };
    let build_dirs = match build_collection {
        Ok(report) => report,
        Err(error) => {
            alerts.push(format!("lane build output collection skipped: {error}"));
            Default::default()
        }
    };
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            return Err(Error::CacheGc(
                Box::new(error),
                unobserved_projects,
                skipped_transient,
                Box::new(provider_state),
                worktree_cleanup,
                alerts,
            ));
        }
    };
    let notice = if command.dry_run {
        Some("bounded preview: upstream refresh, worktree removal, target seed and build-profile surveys deferred to applied GC".into())
    } else {
        (command.pressure
            && report.removed.is_empty()
            && build_dirs.removed.is_empty()
            && provider_state.removed.is_empty()
            && provider_state.attempt_tmp.is_empty()
            && provider_state.attempt_logs.is_empty()
            && provider_state.expired_events == 0
            && provider_state.errors.is_empty()
            && worktree_cleanup
                .iter()
                .all(|project| project.output.is_empty() && project.idle_output.is_empty()))
        .then(|| "pressure collection: no eligible data in scope".into())
    };
    Ok(GcData {
        report,
        idle_days: command.idle_days,
        provider_state,
        chair_logs,
        target_seeds,
        build_dirs,
        graph_checkpoints_removed,
        staging_busy,
        unobserved_projects,
        skipped_transient,
        worktree_cleanup,
        alerts,
        notice,
    })
}

/// Run one worktree-owner action for `project` under fresh state `roots`,
/// with Git mutations bounded by `mutation` and writers observed by
/// `workers`. Peer ingress uses it to create a peer writer's worktree within
/// its peer-call budget.
pub(crate) fn with_context<T>(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    mutation: Duration,
    workers: &dyn WorkerObserver,
    action: impl FnOnce(&WorktreeContext<'_>, &CancelToken) -> Result<T, Error>,
) -> Result<T, Error> {
    let facts = worktrees::RepoFacts::default();
    with_context_using_facts(roots, home, project, mutation, workers, &facts, action)
}

/// Build successive contexts around the same command-owned facts and caps.
#[allow(clippy::too_many_arguments)]
pub(crate) fn with_context_using_facts<T>(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    mutation: Duration,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    action: impl FnOnce(&WorktreeContext<'_>, &CancelToken) -> Result<T, Error>,
) -> Result<T, Error> {
    with_context_mode(
        roots, home, project, mutation, workers, facts, false, action,
    )
}

#[allow(clippy::too_many_arguments)]
fn with_context_mode<T>(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    mutation: Duration,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    read_only: bool,
    action: impl FnOnce(&WorktreeContext<'_>, &CancelToken) -> Result<T, Error>,
) -> Result<T, Error> {
    // Preview only existing custody; an unbound project cannot grant mutation
    // authority and preview must not allocate its registration or lock files.
    let mut state = Directory::root(true)?;
    for component in roots.state.as_path().components() {
        if let Component::Normal(name) = component {
            if !read_only {
                match state.mkdir(name, 0o700) {
                    Ok(()) => state.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
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
    let utility_home = if read_only {
        state.open_verified_directory(OsStr::new("utility-home"))?
    } else {
        child(&state, "utility-home")?
    };
    let git = provenant_os::git::git_executable().ok_or(Error::GitUnavailable)?;
    // Lane scratch and provider state live below the state directory's runs.
    let runtime = UtilityRuntime::new(git, roots.state.as_path().join("utility-home"))?
        .with_lane_roots(provenant_confinement::utility::lane_state_roots(
            roots.state.as_path(),
        ));
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
    let repo = runtime.open_repo(&executor, project, budget, &cancel)?;
    let location = provenant_config::ProjectLocation::new(repo.common_git_dir())?;
    let binding = if read_only {
        provenant_config::lookup_project(roots, &location)?.ok_or(Error::GcObservation(
            "GC preview requires an existing project binding",
        ))?
    } else {
        provenant_config::bind_project(roots, location)?
    };
    let worktrees_state = if read_only {
        state.open_verified_directory(OsStr::new("worktrees"))?
    } else {
        child(&state, "worktrees")?
    };
    let name = binding.project_id().to_string();
    let project_state = if read_only {
        let project_state = worktrees_state.open_verified_directory(OsStr::new(&name))?;
        // Repositories predating this lock have no file until their first
        // mutation. Preview must accept that state without allocating a lock.
        match repo
            .common_dir()
            .metadata_at(OsStr::new(worktrees::REPOSITORY_LOCK), false)
        {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        project_state
    } else {
        child(&worktrees_state, &name)?
    };
    let clock = SystemClock;
    let policy_workers = worktrees::CensusPolicyWorkers::from_roots(workers, roots)?;
    let workers = worktrees::CensusSession::new(&policy_workers, &clock);
    let context = WorktreeContext {
        home,
        repo: &repo,
        binding: &binding,
        project_state: &project_state,
        executor: &executor,
        utility: &runtime,
        facts,
        workers: &workers,
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

#[cfg(test)]
mod human_tests {
    #[allow(unused_imports)] // Also compiled by the harness=false worktree driver.
    use super::*;

    #[test]
    fn local_unknown_occupancy_refusal_preserves_scope_and_evidence() {
        for error in [
            WorktreeError::UnknownOccupancy {
                path: PathBuf::from("/project/lane"),
                detail: "owner unreadable\nretry".into(),
            },
            WorktreeError::CensusUnknown {
                path: PathBuf::from("/project/lane"),
                detail: "census incomplete\nretry".into(),
            },
        ] {
            let message = local_worktree_message(&error);
            assert!(message.contains("/project/lane"));
            assert!(message.contains("\\nretry"));
            assert!(!message.contains('\n'));
            assert!(!error.to_string().contains("/project/lane"));
            if matches!(error, WorktreeError::UnknownOccupancy { .. }) {
                assert!(matches!(
                    Error::Worktree(WorktreeError::UnknownOccupancy {
                        path: "/lane".into(),
                        detail: "fence".into()
                    })
                    .outcome(),
                    Outcome::Refused
                ));
            }
            assert_eq!(Error::Worktree(error).local_message(), message);
        }
    }

    #[allow(dead_code)] // The harness=false driver compiles only the helpers.
    fn info(path: &str) -> Result<worktrees::WorktreeInfo, Box<dyn std::error::Error>> {
        Ok(worktrees::WorktreeInfo {
            warnings: Vec::new(),
            path: PathBuf::from(path),
            branch: Some("refs/heads/master".into()),
            head: GitOid::parse(&"4a".repeat(20))?,
            dirty: worktrees::WorktreeCondition::Dirty,
            occupancy: worktrees::Occupancy::Observed { process: None },
            owner: Err("worktree registration was not observed".into()),
            primary: false,
        })
    }

    #[allow(dead_code)]
    fn rendered(path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let data = Data::Listed(vec![info(path)?]);
        Ok(human(serde_json::to_value(&data)?))
    }

    #[allow(dead_code)]
    type Check = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn worktree_list_line_renders_the_path_as_a_plain_path() -> Check {
        let head = "4a".repeat(20);
        assert_eq!(
            rendered("/Users/alice/Repos/example-app")?,
            format!(
                "branch=refs/heads/master dirty=dirty head={head} owner=unknown \
                 path=/Users/alice/Repos/example-app writer=live\n"
            )
        );
        Ok(())
    }

    #[test]
    fn worktree_list_line_quotes_a_path_only_when_it_needs_it() -> Check {
        assert!(
            rendered("/Users/alice/My Repos/app")?.contains(" path=\"/Users/alice/My Repos/app\" ")
        );
        assert!(rendered("/")?.contains(" path=/ "));
        Ok(())
    }

    #[test]
    fn worktree_list_json_keeps_the_structured_path() -> Check {
        let value = serde_json::to_value(info("/Users/alice/app")?)?;
        assert_eq!(value["path"]["kind"], "path");
        assert_eq!(value["path"]["relative"]["path"], "Users/alice/app");
        Ok(())
    }

    #[test]
    fn worktree_list_distinguishes_unowned_from_uncertain_custody() -> Check {
        let mut tree = info("/repo/native checkout")?;
        tree.owner = Ok(None);
        tree.occupancy = worktrees::Occupancy::custody_unknown(tree.path.clone(), "fixture");
        let value = serde_json::to_value(&tree)?;
        assert_eq!(value["owner"], "unowned");
        assert_eq!(value["writer"], "unknown");
        assert!(value["next"].as_str().unwrap().contains("--worktree"));
        assert!(
            human(serde_json::to_value(Data::Listed(vec![tree.clone()]))?).contains("--worktree")
        );
        tree.owner = Err("worktree registration was not observed".into());
        assert!(serde_json::to_value(&tree)?.get("next").is_none());
        assert!(matches!(
            tree.occupancy,
            worktrees::Occupancy::Unknown { .. }
        ));
        Ok(())
    }
}

#[cfg(test)]
mod gc_preview_tests {
    #[allow(unused_imports)] // Also compiled by the harness=false worktree driver.
    use super::*;
    #[test]
    fn gc_summary_reports_build_output_reclaimed_and_remaining() {
        let mut data = GcData::default();
        data.build_dirs.removed.push("/cache/a/target/debug".into());
        data.build_dirs.bytes = 4096;
        data.build_dirs.cache_bytes = 8192;
        data.build_dirs.busy.push("/cache/b/target/debug".into());
        data.build_dirs.recent.push("/cache/c/target/debug".into());
        for (dry_run, expected) in [
            (
                true,
                "1 lane build director(ies), 4096 bytes reclaimable; 8192 bytes of lane build output remain, 1 busy, 1 recent\n",
            ),
            (
                false,
                "1 lane build director(ies), 4096 bytes reclaimed; 8192 bytes of lane build output remain, 1 busy, 1 recent\n",
            ),
        ] {
            data.report.dry_run = dry_run;
            assert!(gc_data_summary(&data).contains(expected));
        }
        let json = serde_json::to_value(gc_result_data(&data, false)).unwrap_or_default();
        assert_eq!(json["build_dirs_removed"], 1);
        assert_eq!(json["build_dir_bytes_reclaimed"], 4096);
        assert_eq!(json["build_dir_bytes"], 8192);
        let json = serde_json::to_value(gc_result_data(&data, true)).unwrap_or_default();
        assert_eq!(json["build_dirs"]["cache_bytes"], 8192);
        assert_eq!(json["build_dirs"]["recent"][0], "/cache/c/target/debug");
    }
    #[test]
    fn gc_summary_reports_event_expiry_in_preview_and_applied_mode() {
        let mut data = GcData::default();
        data.provider_state.expired_events = 3;
        for (dry_run, expected) in [
            (true, "would expire 3 event rows\n"),
            (false, "expired 3 event rows\n"),
        ] {
            data.report.dry_run = dry_run;
            assert!(gc_data_summary(&data).contains(expected));
            let json = serde_json::to_value(&data).unwrap_or_default();
            assert_eq!(json["provider_state"]["expired_events"], 3);
        }
    }
    #[test]
    fn gc_preview_names_idle_output_and_why_it_qualifies() {
        let mut data = GcData::default();
        let mut project = worktrees::WorktreeGcReport::default();
        project.idle_output.push(worktrees::OutputEntry {
            path: "/checkout/target".into(),
            removed: false,
            reason: "would_remove".into(),
        });
        data.worktree_cleanup.push(project);
        let summary = gc_data_summary(&data);
        assert!(summary.contains("would reclaim idle output /checkout/target"));
        assert!(summary.contains("idle output and custody checks passed"));
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(
            json["worktree_cleanup"][0]["idle_output"][0]["reason"],
            "would_remove"
        );
    }
    #[test]
    fn gc_default_output_aggregates_routine_rows_and_keeps_exceptions() {
        use provenant_confinement::cache::{CacheGcEntry, CacheGcReason};
        let mut data = GcData::default();
        for index in 0..10 {
            data.report.removed.push(CacheGcEntry {
                path: format!("/home/Library/Caches/provenant-lane-cargo/{index:064}").into(),
                bytes: 1024,
                reason: CacheGcReason::Orphaned,
            });
            data.report.kept.push(CacheGcEntry {
                path: format!(
                    "/home/Library/Caches/provenant-lane-cargo/{:064}",
                    index + 10
                )
                .into(),
                bytes: 2048,
                reason: CacheGcReason::Live,
            });
        }
        data.report.bytes_freed = 10240;
        data.provider_state
            .errors
            .push(provenant_run::ProviderStateIssue {
                path: "/state/broken".into(),
                reason: "permission denied".into(),
            });
        let mut project = worktrees::WorktreeGcReport::default();
        project.worktrees.retained.push(worktrees::RetainedBranch {
            branch: "broken".into(),
            reason: RetainReason::GitFailure,
            detail: None,
        });
        project.worktrees.retained.push(worktrees::RetainedBranch {
            branch: "census-blocked".into(),
            reason: RetainReason::UnknownWriter,
            detail: Some("process 968: cwd unreadable; configure confinement.census_ignore_unreadable; path=/Library/Developer/simdiskimaged, team_id=TEAM123".into()),
        });
        data.worktree_cleanup.push(project);
        let human = gc_data_summary(&data);
        let after = human.len();
        let detailed_json = serde_json::to_vec(&data).unwrap_or_default().len();
        let after_json = serde_json::to_vec(&gc_result_data(&data, false))
            .unwrap_or_default()
            .len();
        std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!(
                "gc fixture bytes: human {after}, detailed data {detailed_json}, default data {after_json}\n"
            ),
        )
        .unwrap_or_default();
        assert!(after_json < detailed_json);
        assert!(human.contains("10 cache(s), 10240 bytes freed"));
        assert!(human.contains("/state/broken: permission denied"));
        assert!(human.contains("kept worktree broken (GitFailure)"));
        assert!(human.contains("kept worktree census-blocked (UnknownWriter): process 968"));
        assert!(human.contains("confinement.census_ignore_unreadable"));
        assert!(human.contains("/Library/Developer/simdiskimaged"));
        assert!(human.contains("team_id=TEAM123"));
        assert!(!human.contains("/home/Library/Caches"));
        assert!(serde_json::to_value(&data).unwrap_or_default()["removed"].is_array());
    }
    #[test]
    fn gc_failure_human_output_names_failed_paths_and_next_step()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut data = GcData::default();
        data.provider_state
            .errors
            .push(provenant_run::ProviderStateIssue {
                path: "/state/failed-cache".into(),
                reason: "permission denied".into(),
            });
        let result = gc_failure(&data, "provider-state retention incomplete", false);
        let mut output = Vec::new();
        provenant_cli::render(
            &result,
            provenant_cli::RenderMode::Human {
                text: "",
                read_only: false,
                summary: None,
            },
            &mut output,
        )?;
        let human = String::from_utf8(output)?;
        assert!(
            human.contains("/state/failed-cache: permission denied"),
            "{human}"
        );
        assert!(human.contains("provenant gc --apply --json"), "{human}");
        Ok(())
    }
    #[test]
    fn read_only_context_accepts_absent_repository_lock_without_creating_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let facts = worktrees::RepoFacts::default();
        let workers = worktrees::SystemWorkerObserver;
        // Establish the pre-existing project binding and state directories,
        // as they would exist before upgrading to repository locking.
        with_context_using_facts(
            &roots,
            &rig.home,
            &rig.project,
            MUTATION_TIMEOUT,
            &workers,
            &facts,
            |_, _| Ok(()),
        )?;
        let lock = rig.project.join(".git").join(worktrees::REPOSITORY_LOCK);
        assert!(!lock.exists());
        let observed = with_context_mode(
            &roots,
            &rig.home,
            &rig.project,
            MUTATION_TIMEOUT,
            &workers,
            &facts,
            true,
            |_, _| Ok(true),
        )?;
        assert!(observed);
        assert!(!lock.exists());
        Ok(())
    }

    #[test]
    fn gc_preview_does_not_initialise_absent_state() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let assets = base.join("assets");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&assets)?;
        let state = base.join("absent");
        let roots = crate::roots::child(&home, &assets, state.clone())?;
        let result = with_context_mode(
            &roots,
            &home,
            &base,
            MUTATION_TIMEOUT,
            &worktrees::SystemWorkerObserver,
            &worktrees::RepoFacts::default(),
            true,
            |_, _| Ok(()),
        );
        assert!(result.is_err());
        assert!(!state.exists());
        Ok(())
    }
}
