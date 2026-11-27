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
use provenant_core::{
    CancelToken, Clock, DurationMicros, GitOid, NameRefusal, ProjectId, WaitBudget, WallTime,
};
use provenant_os::{
    clock::SystemClock,
    exec::SystemExecutor,
    fs::{Directory, FileKind, FsError, ledger::Ledger},
};
use provenant_records::{
    CommandResult, Layer, Outcome, StructuredPathWire,
    command_result::{NextStep, SettingRef},
    custody::{
        LandedEvidence, LandingPredicate, LandingSource, RemovalRecord, Subject, SubjectKind,
    },
    warning::{Warning, WarningCode, detected_source, warn},
};
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
    Fabric(#[from] provenant_cli::fabric::EdgeError),
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
    #[error(transparent)]
    GitConfigRead(Box<provenant_os::git_config::ConfigError<Error>>),
    #[error("cannot resolve forge context: {0}")]
    ForgeContextUnresolved(provenant_config::ForgeContextError),
    #[error("Git executable unavailable")]
    GitUnavailable,
    #[error("cannot establish worktree lock budget")]
    Budget,
    #[error("retention collection failed: {0}")]
    RetentionCollection(String),
    #[error("graph checkpoint collection failed: {0}")]
    GraphGc(String),
    #[error("{0}")]
    GcObservation(&'static str),
    #[error("cannot read disk space: {0}")]
    DiskSpace(std::io::Error),
    #[error("invalid worktree removal scope")]
    InvalidScope,
    #[error("worktree locate refused: {0}")]
    Locate(NameRefusal),
    #[error("{0}")]
    Settlement(String),
}

#[allow(clippy::print_stderr)]
fn note_kept_unmerged() {
    eprintln!(
        "note: the branch is not merged into the integration branch and was kept; pass --discard to delete it"
    );
}

fn warn_fetch(error: &Error) {
    use provenant_records::warning::{Warning, warn};

    // A forge refusal keeps its typed cause (the remotes seen and close
    // matches) and carries a declaration the operator can make; other fetch
    // failures keep the generic code. The warning is the public boundary, so
    // the repair travels with the cause instead of being dropped at the caller.
    // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    let warning = match error {
        Error::ForgeContextUnresolved(_) => Warning::new(
            provenant_records::warning::FORGE_CONTEXT_UNRESOLVED,
            format!("{error}; using the current integration resolution"),
            provenant_records::warning::detected_source(
                provenant_records::warning::FORGE_CONTEXT_UNRESOLVED,
            ),
        )
        .repair(error.repair()),
        _ => Warning::new(
            provenant_records::warning::REMOTE_FETCH_FAILED,
            format!("{error}; using the current integration resolution"),
            provenant_records::warning::detected_source(
                provenant_records::warning::REMOTE_FETCH_FAILED,
            ),
        ),
    };
    warn(warning.once_per_change());
}

pub(crate) fn local_worktree_message(error: &WorktreeError) -> String {
    use provenant_records::diagnostic::safe_text;
    match error {
        WorktreeError::CensusUnknown { path, detail }
        | WorktreeError::UnknownOccupancy { path, detail } => {
            let scope = path.display();
            format!("{error}: {scope}: {}", safe_text(detail))
        }
        // The config error's `Display` carries its context and bounded leaf
        // cause, so a timeout or truncation is not dropped at the CLI edge.
        WorktreeError::GitConfigRead(error) => error.to_string(),
        _ => error.to_string(),
    }
}

impl Error {
    pub(crate) fn local_message(&self) -> String {
        match self {
            Self::Worktree(error) => local_worktree_message(error),
            Self::GitConfigRead(error) => error.to_string(),
            _ => self.to_string(),
        }
    }

    /// The concrete next step for every worktree refusal. Inner worktree
    /// errors with no declared step fall back to a real inspection command, so
    /// no refusal reaches `command_error` without an actionable repair.
    pub(crate) fn repair(&self) -> &'static str {
        match self {
            Self::Worktree(error) => error.repair().unwrap_or(
                "run `provenant worktree list` to inspect the condition, then retry the worktree command",
            ),
            Self::Root(_) => crate::REPAIR_ROOT,
            Self::Fabric(_) => {
                "resolve the reported fabric error, then retry the worktree command"
            }
            Self::Permissions(_) => {
                "correct the reported permission and rerun the worktree command"
            }
            Self::CacheGc(..) | Self::RetentionCollection(_) | Self::GcObservation(_) => {
                "resolve the reported collection error, then rerun `provenant gc`"
            }
            Self::ForgeContextUnresolved(_) => {
                "declare `git.remote` in .provenant/ports.json (or the host ports), then rerun `provenant gc`"
            }
            Self::Utility(_) => "correct the reported utility error and retry the worktree command",
            Self::Config(_) | Self::GitConfigRead(_) => crate::REPAIR_CONFIG,
            Self::Fs(_) | Self::Exec(_) | Self::DiskSpace(_) => crate::REPAIR_IO,
            Self::GitUnavailable => "install Git on PATH, then retry the worktree command",
            Self::Budget => "wait for the running worktree operation to finish, then retry",
            Self::GraphGc(_) => {
                "resolve the reported graph collection error, then rerun `provenant gc`"
            }
            Self::InvalidScope => {
                "name a worktree or pass --all to `provenant worktree remove`"
            }
            Self::Locate(_) => {
                "run `provenant worktree list` to see the branches, lanes and attempts to locate"
            }
            Self::Settlement(_) => "wait for the writer to settle, then retry the worktree command",
        }
    }

    pub(crate) fn outcome(&self) -> Outcome {
        match self {
            Self::Root(roots::RootError::Checkout | roots::RootError::State(_))
            | Self::InvalidScope
            | Self::Locate(_)
            | Self::Worktree(
                WorktreeError::InvalidRequest
                | WorktreeError::AmbiguousName { .. }
                | WorktreeError::NotFound
                | WorktreeError::CheckedOut
                | WorktreeError::BranchCollision { .. },
            ) => Outcome::Invalid,
            Self::Worktree(
                WorktreeError::WorktreeExists { .. }
                | WorktreeError::UnrelatedBase(..)
                | WorktreeError::Dirty(_)
                | WorktreeError::DetachedPrimary
                | WorktreeError::DetachedUnmerged { .. }
                | WorktreeError::UnprovenLanding { .. }
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
                | WorktreeError::ProjectLayoutUnresolved
                | WorktreeError::PrimaryMoved,
            ) => Outcome::Refused,
            Self::Worktree(WorktreeError::CustodyCommand(error))
                if matches!(
                    &error.class,
                    provenant_records::command_result::ErrorClass::Configured
                ) =>
            {
                Outcome::Refused
            }
            Self::Worktree(WorktreeError::Cancelled) => Outcome::Cancelled,
            _ => Outcome::Error,
        }
    }

    /// The one mapping from a worktree refusal to its transport facts. Used by
    /// the production emit path and by the render tests, so the two cannot drift.
    pub(crate) fn command_error(&self, code: &str) -> provenant_records::CommandError {
        if let Self::Worktree(WorktreeError::CustodyCommand(error)) = self {
            return error.as_ref().clone();
        }
        // The conflict reason is evidence beside the short refusal: the
        // message names the path and branch, the cause names what clashed.
        let cause = match self {
            Self::Worktree(WorktreeError::WorktreeExists { reason, .. }) => Some(reason.as_str()),
            _ => None,
        };
        crate::command_error(code, &self.local_message(), cause, self.repair())
    }
}

/// One durable row of `<state>/worktrees/removed.jsonl`. Field order is a
/// persisted-format contract (`provenant.worktree-removed.v1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RemovedRecord {
    pub schema: &'static str,
    pub branch: String,
    pub tip: GitOid,
    pub path: StructuredPathWire,
    pub at: WallTime,
}

const REMOVED_SCHEMA: &str = "provenant.worktree-removed.v1";

/// Append one bounded record per removed checkout and return the rows so the
/// command can print exactly what it recorded. A record is only written for a
/// worktree with a known branch and path; the removal itself already succeeded.
fn record_removals(
    state: &Path,
    outcome: &worktrees::RemoveOutcome,
    clock: &dyn Clock,
) -> Result<Vec<RemovedRecord>, Error> {
    let Some(at) = clock.wall_now().ok() else {
        return Ok(Vec::new());
    };
    let records: Vec<_> = outcome
        .removed
        .iter()
        .filter_map(|info| {
            let branch = info.branch.clone()?;
            let path = StructuredPathWire::from_path(&info.path).ok()?;
            Some(RemovedRecord {
                schema: REMOVED_SCHEMA,
                branch,
                tip: info.head,
                path,
                at,
            })
        })
        .collect();
    if records.is_empty() {
        return Ok(records);
    }
    let directory = state.join("worktrees");
    std::fs::create_dir_all(&directory).map_err(Error::DiskSpace)?;
    let directory = Directory::open_anchor(&directory).map_err(Error::Fs)?;
    let name = OsStr::new("removed.jsonl");
    let ledger = Ledger::new(&directory, name);
    for record in &records {
        let mut line = serde_json::to_vec(record).map_err(|_| Error::InvalidScope)?;
        line.push(b'\n');
        ledger.append(&line).map_err(Error::Fs)?;
    }
    Ok(records)
}

#[derive(Serialize)]
struct RemovedData {
    #[serde(flatten)]
    outcome: worktrees::RemoveOutcome,
    records: Vec<RemovedRecord>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Data {
    Created(Box<worktrees::WorktreeInfo>),
    Listed(Vec<worktrees::WorktreeInfo>),
    Located(Located),
    Diagnosed(Box<worktrees::WorktreeDiagnosis>),
    Removed(RemovedData),
}

/// The envelope for `worktree locate`. The path stays inside the structured
/// path envelope in JSON; human mode prints it alone for `cd "$(...)"`.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Located {
    pub path: StructuredPathWire,
    pub branch: Option<String>,
    pub lane: Option<String>,
    pub base: Option<String>,
}

pub fn run(command: Worktree, globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    if matches!(command, Worktree::SessionList) {
        return session_list(globals, workers);
    }
    let name = match &command {
        Worktree::Create { .. } => "worktree create",
        Worktree::List => "worktree list",
        Worktree::Locate { .. } => "worktree locate",
        Worktree::Doctor { .. } => "worktree doctor",
        Worktree::SessionList => unreachable!(),
        Worktree::Remove(_) => "worktree remove",
    };
    let read_only = matches!(
        command,
        Worktree::List | Worktree::Locate { .. } | Worktree::Doctor { .. }
    );
    match execute(command, globals, workers) {
        Ok(data) => match serde_json::to_value(&data) {
            Ok(value) => {
                let human = match &data {
                    Data::Listed(_) => {
                        let mut text = human(value);
                        text.push_str(&worktree_list_stranded_human(globals, workers));
                        text
                    }
                    Data::Located(located) => located
                        .path
                        .to_path_buf()
                        .map(|path| format!("{}\n", path.display()))
                        .unwrap_or_else(|_| human(value)),
                    _ => human(value),
                };
                emit(
                    &CommandResult::new(name, Outcome::Ok, data),
                    globals,
                    &human,
                    read_only,
                )
            }
            Err(_) => fail(
                name,
                Outcome::Error,
                "ENCODE",
                "cannot encode worktree result",
                "rerun the worktree command with `--json`, or report the encoding failure if it persists",
                globals,
            ),
        },
        Err(error) => crate::fail_with_command_error(
            name,
            error.outcome(),
            error.command_error(
                if matches!(error, Error::Worktree(WorktreeError::LockBusy)) {
                    "WORKTREE_LOCK_BUSY"
                } else {
                    "WORKTREE"
                },
            ),
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

/// The human-only stranded-work section appended to `worktree list`. Its
/// JSON stays the worktree array so consumers of that shape are unchanged.
fn worktree_list_stranded_human(globals: &Globals, workers: &dyn WorkerObserver) -> String {
    let Ok(resolved) = roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Required,
    ) else {
        return String::new();
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let rows = crate::stranded_work::survey(
        &resolved.roots,
        &resolved.home,
        &resolved.project,
        workers,
        &clock,
        &cancel,
        crate::stranded_work::now_micros(&clock),
    );
    let mut text = String::new();
    for line in crate::stranded_work::human_lines(&rows) {
        text.push_str(&line);
        text.push('\n');
    }
    text
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
                Worktree::Create { branch, base } => {
                    let info = worktrees::create(context, WorktreeCreate { branch, base }, cancel)?;
                    let verified = provenant_confinement::verify_worktree(context.repo, &info.path)
                        .map_err(WorktreeError::from)?;
                    match provenant_config::PortTable::read_for(
                        Some(project),
                        roots.state.as_path(),
                    ) {
                        Ok(ports) => {
                            let env = provenant_run::build::prepare::lane_environment(home);
                            if let Err(error) = provenant_run::build::prepare::run_if_changed(
                                verified.repo().private_dir(),
                                &ports.build.prepare,
                                verified.path(),
                                &env,
                                WaitBudget::up_to(
                                    context.clock.mono_now(),
                                    provenant_core::DurationMicros::from_micros(30_000_000),
                                ),
                                cancel,
                                context.clock,
                            ) {
                                provenant_fabric_runtime::custody::warning(format_args!(
                                    "lane worktree preparation warning (build.prepare): {error}; worktree creation continues"
                                ));
                            }
                        }
                        Err(error) => provenant_fabric_runtime::custody::warning(format_args!(
                            "lane worktree preparation warning (build.prepare): cannot read project declarations: {error}; worktree creation continues"
                        )),
                    }
                    let mut subjects = vec![worktrees::acquire_worktree_subject(verified.repo())?];
                    if let Some(branch) = info.branch.as_deref().and_then(|reference| {
                        provenant_confinement::repo::GitRefName::new(reference).ok()
                    }) && let Some(owner) =
                        worktrees::branch_ownership(context, branch.short_name(), info.head)?
                        && let Some(subject) = owner.logical_branch_subject()
                    {
                        subjects.push(subject);
                    }
                    if let Err(error) = crate::custody::acquire(&resolved, &subjects) {
                        provenant_fabric_runtime::custody::warning(format_args!(
                            "worktree custody pending: {error}"
                        ));
                    }
                    Data::Created(Box::new(info))
                }
                Worktree::List => Data::Listed(worktrees::list(context, cancel)?),
                Worktree::Locate { query } => {
                    let rows = worktrees::list(context, cancel)?;
                    let found = worktrees::locate(&query, &rows).map_err(Error::Locate)?;
                    let path = StructuredPathWire::from_path(&found.path)
                        .map_err(|_| Error::InvalidScope)?;
                    Data::Located(Located {
                        path,
                        branch: found.branch.clone(),
                        lane: found.lane.clone(),
                        base: found.base.clone(),
                    })
                }
                Worktree::Doctor { path } => {
                    let snapshot =
                        crate::gc_support::lane_states(roots, context.binding.project_id(), cancel)
                            .ok();
                    let terminal = |candidate: &Path| {
                        snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.get(candidate).copied())
                            .unwrap_or(false)
                    };
                    let diagnosis = worktrees::doctor(context, &path, &terminal, cancel)?;
                    Data::Diagnosed(Box::new(diagnosis))
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
                    let authority =
                        provenant_cli::fabric::Context::open(roots.clone(), home, project)
                            .map_err(|_| {
                                Error::GcObservation("cannot establish removal authority")
                            })?;
                    if authority.identity.project != context.binding.project_id() {
                        return Err(Error::InvalidScope);
                    }
                    let chair = provenant_cli::fabric::current_chair(&authority)?;
                    let retirement = crate::custody::RetirementWriter::open(
                        roots,
                        context.binding.project_id(),
                        chair.clone(),
                        context.clock,
                        cancel,
                    )?;
                    let removed = worktrees::remove_with_retirement(
                        context,
                        request,
                        cancel,
                        &output,
                        &retirement,
                        chair.actor,
                    )?;
                    if named
                        && !args.discard
                        && removed
                            .retained
                            .iter()
                            .any(|kept| kept.reason == RetainReason::NotMerged)
                    {
                        note_kept_unmerged();
                    }
                    let records = record_removals(roots.state.as_path(), &removed, context.clock)?;
                    Data::Removed(RemovedData {
                        outcome: removed,
                        records,
                    })
                }
            })
        },
    )
}

/// The hook reads complete newline-delimited records even when its child
/// deadline stops this command before the inventory is complete.
// The hook reads newline-delimited records from stdout as its wire protocol.
#[allow(clippy::disallowed_methods)]
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

/// What one bounded pass decided about settled-scratch candidates. A candidate
/// with undecided custody is deferred with its bytes intact, never collected.
#[derive(Clone, Default, Serialize)]
struct GcCandidateProgress {
    decided: usize,
    collected: usize,
    deferred: usize,
    /// Best-effort bytes reclaimed from collected candidates.
    bytes: u64,
    /// Retained bytes are unknown when any deferred tree was not fully measured.
    deferred_bytes: Option<u64>,
}

fn candidate_progress(state: &provenant_run::ProviderStateGc) -> GcCandidateProgress {
    let bytes = state.temporary_bytes();
    let collected = state.attempt_tmp.len();
    let deferred = state.scratch_deferred.len();
    GcCandidateProgress {
        decided: collected.saturating_add(deferred),
        collected,
        deferred,
        bytes,
        deferred_bytes: state
            .scratch_deferred
            .iter()
            .try_fold(0_u64, |total, entry| {
                entry.bytes.map(|bytes| total.saturating_add(bytes))
            }),
    }
}

fn candidate_progress_line(data: &GcData) -> String {
    let Some(progress) = &data.candidate_progress else {
        return "settled candidate progress: not measured (attempt scratch class did not run)\n"
            .into();
    };
    let retained = progress
        .deferred_bytes
        .map_or_else(|| "unknown".into(), |bytes| format!("{bytes} bytes"));
    format!(
        "{} settled candidate(s) decided: {} {} ({} bytes {}), {} deferred ({} retained)\n",
        progress.decided,
        progress.collected,
        if data.report.dry_run {
            "would collect"
        } else {
            "collected"
        },
        progress.bytes,
        if data.report.dry_run {
            "reclaimable"
        } else {
            "reclaimed"
        },
        progress.deferred,
        retained
    )
}

#[derive(Serialize, Default)]
struct GcData {
    #[serde(flatten)]
    report: provenant_confinement::cache::CacheGcReport,
    idle_seconds: u64,
    provider_state: provenant_run::ProviderStateGc,
    sweep_budget_seconds: u64,
    sweep_budget_source: String,
    /// Resolved `gc.free_floor` and the layer or config file that set it.
    free_floor_bytes: u64,
    free_floor_source: String,
    /// Resolved `gc.attempt_scratch_max` and where it came from.
    attempt_scratch_max_bytes: u64,
    attempt_scratch_max_source: String,
    /// Resolved `gc.scratch_grace` and where it came from.
    scratch_grace_seconds: u64,
    scratch_grace_source: String,
    /// Resolved preview budget and whether a preview stopped at it.
    preview_budget_seconds: u64,
    preview_budget_source: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    preview_budget_stopped: bool,
    /// Resolved share of free space that warns about attempt scratch.
    scratch_warning_share: f64,
    scratch_warning_share_source: String,
    sweep_cursor: Option<crate::gc_sweep::Cursor>,
    sweep_warning: Option<String>,
    sweep_stopped: bool,
    chair_logs: crate::chair_log_retention::Report,
    build_dirs: provenant_run::build_dir_gc::Report,
    graph_checkpoints_removed: usize,
    /// Import-extra pins removed (apply) or that would be removed (preview).
    import_pins: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stale_graphs: Vec<crate::gc_support::StaleGraph>,
    #[serde(skip)]
    graph_bytes: Option<provenant_config::ByteObservation>,
    port_receipts: crate::port_receipt_retention::Report,
    retention_errors: Vec<provenant_config::RetentionIssue>,
    #[serde(skip)]
    failed_classes: Vec<provenant_config::RetentionKind>,
    /// Classes this pass started, so classes it never reached are reported
    /// as not run rather than as a measured zero.
    #[serde(skip)]
    classes_run: Vec<provenant_config::RetentionKind>,
    /// Scheduled classes this pass did not run, because an earlier pass ran
    /// them before the saved cursor or the sweep stopped first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    classes_not_run: Vec<provenant_config::RetentionKind>,
    /// Settled-scratch candidates decided, collected and deferred this pass.
    #[serde(default)]
    candidate_progress: Option<GcCandidateProgress>,
    #[serde(skip)]
    retention_observations: Vec<provenant_config::RetentionBytes>,
    discard_archives: worktrees::ArchiveReport,
    discard_archive_retention_seconds: u64,
    discard_archive_retention_setting: String,
    discard_archive_retention_source: String,
    staging_busy: Vec<PathBuf>,
    worktree_cleanup: Vec<worktrees::WorktreeGcReport>,
    /// Advisory processes found in settled worktrees and attempts by the one
    /// bounded census this pass. Never a signal target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    held_processes: Vec<worktrees::HeldProcess>,
    /// Uncommitted work in worktrees with no live lane. Kept, never removed,
    /// and named here so cleanup cannot lose it silently.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stranded_work: Vec<crate::stranded_work::StrandedWork>,
    /// The bounded dweller census did not finish, so held rows may be missing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    held_incomplete: bool,
    alerts: Vec<String>,
    unobserved_projects: Vec<PathBuf>,
    skipped_transient: Vec<PathBuf>,
    /// Bindings retired (apply) or that would be retired (preview) because
    /// their common directory has been gone past the idle lifetime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired_projects: Vec<RetiredProject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct RetiredProject {
    project: ProjectId,
    common_dir: PathBuf,
}

fn gc_cache_bytes(data: &GcData) -> u64 {
    let mut observed = data
        .report
        .removed
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut bytes = data.report.bytes_freed;
    for entry in data
        .worktree_cleanup
        .iter()
        .flat_map(|report| &report.caches)
    {
        if observed.insert(entry.path.clone()) {
            bytes = bytes.saturating_add(entry.bytes);
        }
    }
    bytes
}

// These disjoint collectors report allocated bytes. Other classes may be
// unknown or use apparent bytes, so this is a lower bound rather than a census.
fn gc_known_reclaimed_bytes(data: &GcData) -> u64 {
    let worktrees = data
        .worktree_cleanup
        .iter()
        .map(|r| r.bytes)
        .fold(0_u64, u64::saturating_add);
    [
        gc_cache_bytes(data),
        data.provider_state.temporary_bytes(),
        data.discard_archives.bytes,
        data.build_dirs.bytes,
        worktrees,
    ]
    .into_iter()
    .fold(0_u64, u64::saturating_add)
}

fn gc_retention_bytes(data: &GcData) -> Vec<provenant_config::RetentionBytes> {
    provenant_config::RETENTION_CLASSES
        .iter()
        .map(|class| provenant_config::RetentionBytes {
            class: class.class,
            observation: data
                .retention_observations
                .iter()
                .find(|row| row.class == class.class)
                .map_or(provenant_config::ByteObservation::Unknown, |row| {
                    row.observation
                }),
        })
        .collect()
}

impl GcData {
    /// Measure only adapters selected for this pass. Keep results, so rendering
    /// cannot turn an unexecuted adapter's empty report into a measured zero.
    fn observe_retention(&mut self, trigger: provenant_config::RetentionTrigger) {
        self.classes_not_run = provenant_config::retention_schedule(trigger)
            .map(|class| class.class)
            .filter(|class| !self.classes_run.contains(class))
            .collect();
        self.retention_observations = provenant_config::retention_schedule(trigger)
            .map(|class| provenant_config::RetentionBytes {
                class: class.class,
                observation: if self.failed_classes.contains(&class.class)
                    || self.classes_not_run.contains(&class.class)
                {
                    provenant_config::ByteObservation::Unknown
                } else {
                    class.adapter.measure(self)
                },
            })
            .collect();
    }
}

impl provenant_config::RetentionMeasurements for GcData {
    fn lane_cargo_caches(&self) -> provenant_config::ByteObservation {
        let bytes = gc_cache_bytes(self);
        if !self.report.errors.is_empty()
            || self.report.bytes_estimated
            || self
                .worktree_cleanup
                .iter()
                .any(|r| !r.cache_errors.is_empty())
        {
            provenant_config::ByteObservation::Partial { bytes }
        } else {
            provenant_config::ByteObservation::Measured { bytes }
        }
    }
    fn git_worktrees(&self) -> provenant_config::ByteObservation {
        let bytes = self
            .worktree_cleanup
            .iter()
            .map(|r| r.bytes)
            .fold(0_u64, u64::saturating_add);
        if self
            .worktree_cleanup
            .iter()
            .any(|r| !r.byte_errors.is_empty() || (!r.dry_run && !r.worktrees.removed.is_empty()))
        {
            provenant_config::ByteObservation::Partial { bytes }
        } else {
            provenant_config::ByteObservation::Measured { bytes }
        }
    }
    fn build_directories(&self) -> provenant_config::ByteObservation {
        if self.build_dirs.errors.is_empty() && !self.build_dirs.measurement_skipped {
            provenant_config::ByteObservation::Measured {
                bytes: self.build_dirs.cache_bytes,
            }
        } else {
            provenant_config::ByteObservation::Partial {
                bytes: self.build_dirs.cache_bytes,
            }
        }
    }
    fn discard_archives(&self) -> provenant_config::ByteObservation {
        if self.discard_archives.errors.is_empty() {
            provenant_config::ByteObservation::Measured {
                bytes: self.discard_archives.bytes,
            }
        } else {
            provenant_config::ByteObservation::Partial {
                bytes: self.discard_archives.bytes,
            }
        }
    }
    fn payload_cap(&self) -> provenant_config::ByteObservation {
        if self.provider_state.payload.errors.is_empty() {
            provenant_config::ByteObservation::Measured {
                bytes: self.provider_state.payload.reclaimed_bytes,
            }
        } else {
            provenant_config::ByteObservation::Partial {
                bytes: self.provider_state.payload.reclaimed_bytes,
            }
        }
    }
    fn provider_state(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Partial {
            bytes: self
                .provider_state
                .attempt_logs
                .iter()
                .map(|r| r.bytes)
                .fold(0_u64, u64::saturating_add),
        }
    }
    fn attempt_scratch(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Partial {
            bytes: self.provider_state.temporary_bytes(),
        }
    }
    fn graph_checkpoints(&self) -> provenant_config::ByteObservation {
        self.graph_bytes
            .unwrap_or(provenant_config::ByteObservation::Unknown)
    }
    fn port_receipts(&self) -> provenant_config::ByteObservation {
        self.port_receipts.observation
    }
    // These owners expose no byte census to GC. Their adapters explicitly report
    // that limitation; adding a class cannot inherit it through a wildcard.
    fn staging(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn chair_logs(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn observation_ledgers(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn retirement_records(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn ref_generation_sequences(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn quota_history(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn usage(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
    fn managed_releases(&self) -> provenant_config::ByteObservation {
        provenant_config::ByteObservation::Unknown
    }
}

fn gc_removal_records(data: &GcData) -> Vec<RemovalRecord> {
    data.worktree_cleanup
        .iter()
        .flat_map(|report| report.removals.iter().cloned())
        .collect()
}

/// Every class that deliberately kept a candidate, with bytes and reason.
/// Observations that carry no byte census say so instead of claiming zero.
fn gc_uncollected(data: &GcData) -> Vec<crate::gc_support::UncollectedItem> {
    use crate::gc_support::UncollectedItem;
    let mut rows = Vec::new();
    for entry in &data.report.kept {
        rows.push(UncollectedItem {
            path: entry.path.clone(),
            bytes: Some(entry.bytes),
            class: "cargo_cache",
            reason: entry.reason.as_str().to_owned(),
        });
    }
    for path in &data.build_dirs.busy {
        rows.push(UncollectedItem {
            path: path.clone(),
            bytes: None,
            class: "cargo_target",
            reason: "live holder or held Cargo lock".to_owned(),
        });
    }
    for path in &data.build_dirs.recent {
        rows.push(UncollectedItem {
            path: path.clone(),
            bytes: None,
            class: "cargo_target",
            reason: "recent write within the grace period".to_owned(),
        });
    }
    // The target census measures bytes under managed roots but does not split
    // them per kept target. Emit that total once instead of claiming each kept
    // target has no measurable size.
    if (!data.build_dirs.busy.is_empty() || !data.build_dirs.recent.is_empty())
        && data.build_dirs.cache_bytes > 0
    {
        rows.push(UncollectedItem {
            path: PathBuf::from("<managed cargo targets>"),
            bytes: Some(data.build_dirs.cache_bytes),
            class: "cargo_target",
            reason: "class total under managed target roots".to_owned(),
        });
    }
    for issue in &data.provider_state.kept {
        // Only a separate retained-tree observation can describe bytes still here.
        let bytes = data
            .provider_state
            .scratch_deferred
            .iter()
            .find(|entry| entry.path == issue.path)
            .and_then(|entry| entry.bytes);
        rows.push(UncollectedItem {
            path: issue.path.clone(),
            bytes,
            class: "provider_state",
            reason: issue.reason.clone(),
        });
    }
    for path in &data.discard_archives.busy {
        rows.push(UncollectedItem {
            path: path.clone(),
            bytes: None,
            class: "discard_archive",
            reason: "live holder".to_owned(),
        });
    }
    for issue in &data.retention_errors {
        rows.push(UncollectedItem {
            path: issue.path.clone(),
            bytes: None,
            class: "retention",
            reason: format!("{}: {}", issue.operation, issue.cause),
        });
    }
    for process in &data.held_processes {
        rows.push(UncollectedItem {
            path: process.root.to_path_buf().unwrap_or_default(),
            bytes: None,
            class: "held_process",
            reason: format!("live holder: {}", process.advisory()),
        });
    }
    for work in &data.stranded_work {
        rows.push(UncollectedItem {
            path: work.path.clone(),
            bytes: None,
            class: "stranded_work",
            reason: format!(
                "unmerged work: {} ({} file(s), newest edit {})",
                work.branch.as_deref().unwrap_or("no branch"),
                work.files,
                work.age
            ),
        });
    }
    rows
}

const GC_REPAIR: &str =
    "Run `provenant gc --apply --json` to inspect failed paths and retry incomplete collection.";

/// A bound stopped the pass after its cursor was saved. This is progress, not
/// a failure: the exit table reserves `Error` for an operation that failed.
const GC_BUDGET_STOP: WarningCode = WarningCode("GC_BUDGET_STOP");

fn gc_failure<'a>(data: &'a GcData, message: &str, detailed: bool) -> CommandResult<GcOutput<'a>> {
    let mut result = CommandResult::new("gc", Outcome::Error, gc_result_data(data, detailed));
    result.error = Some(provenant_records::CommandError::new(
        "GC",
        format!("{message}\n{}{GC_REPAIR}", gc_data_summary(data)),
        None,
        GC_REPAIR,
    ));
    result
}

/// The budget that stopped a pass, with its configured value and source. A
/// preview and an applied sweep have distinct, separately configured bounds.
fn gc_budget_stop_key(data: &GcData) -> (&'static str, u64, &str) {
    if data.preview_budget_stopped {
        (
            "gc.preview_budget",
            data.preview_budget_seconds,
            &data.preview_budget_source,
        )
    } else {
        (
            "gc.sweep_budget",
            data.sweep_budget_seconds,
            &data.sweep_budget_source,
        )
    }
}

fn gc_budget_stop<'a>(data: &'a GcData, detailed: bool) -> CommandResult<GcOutput<'a>> {
    let (key, seconds, source) = gc_budget_stop_key(data);
    let mut result = CommandResult::new("gc", Outcome::Ok, gc_result_data(data, detailed));
    let step = NextStep::from_args(
        &["provenant", "gc", "--apply"],
        "resume the saved cursor on the next sweep",
    );
    let mut warning = Warning::new(
        GC_BUDGET_STOP,
        format!("{key}={seconds}s stopped the sweep; the cursor was saved and the next pass resumes from it"),
        detected_source(GC_BUDGET_STOP),
    )
    .setting(SettingRef {
        key: key.to_owned(),
        value: seconds.to_string(),
        layer: Layer::Host,
        origin: source.to_owned(),
    });
    warning.next.push(step.clone());
    // The binary edge drains the sink into the envelope, so raise it there as
    // well as carrying it on the result for library callers.
    warn(warning.clone());
    result.warnings.push(warning);
    result.next.push(step);
    result
}

fn gc_result(data: &GcData, detailed: bool) -> CommandResult<GcOutput<'_>> {
    let failed = !data.provider_state.errors.is_empty()
        || !data.chair_logs.errors.is_empty()
        || !data.discard_archives.errors.is_empty()
        || !data.port_receipts.errors.is_empty()
        || !data.retention_errors.is_empty()
        || !data.provider_state.payload.errors.is_empty();
    if failed {
        gc_failure(data, "retention incomplete", detailed)
    } else if data.preview_budget_stopped || data.sweep_stopped {
        gc_budget_stop(data, detailed)
    } else {
        CommandResult::new("gc", Outcome::Ok, gc_result_data(data, detailed))
    }
}

/// How a scheduled collection that raised no error ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GcCompletion {
    Complete,
    /// The sweep stopped at its budget and saved a cursor, so the next sweep
    /// resumes it. This is progress, not a failure.
    Partial(String),
}

/// Daily tick task, using the same collection policy and custody as explicit GC.
pub(crate) fn scheduled_gc(
    globals: &Globals,
    idle_seconds: u64,
    workers: &dyn WorkerObserver,
    home: Option<&Path>,
) -> Result<GcCompletion, String> {
    let command = Gc {
        common: Default::default(),
        scheduled: true,
        pressure: false,
        free_floor: None,
        dry_run: false,
        apply: true,
        idle: Some(idle_seconds),
        finished_branch: None,
    };
    let data = gc(&command, globals, workers, home).map_err(|error| error.local_message())?;
    if !data.provider_state.errors.is_empty()
        || !data.chair_logs.errors.is_empty()
        || !data.discard_archives.errors.is_empty()
        || !data.port_receipts.errors.is_empty()
        || !data.retention_errors.is_empty()
        || !data.provider_state.payload.errors.is_empty()
    {
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
    Ok(if data.sweep_stopped {
        GcCompletion::Partial(
            "retention sweep reached gc.sweep_budget and saved its cursor; it resumes on the next sweep"
                .into(),
        )
    } else {
        GcCompletion::Complete
    })
}

/// `provenant gc`: remove orphaned and idle legacy lane caches, sparing any
/// whose worktree has a live or unobservable writer.
pub fn run_gc(command: Gc, globals: &Globals, workers: &dyn WorkerObserver) -> u8 {
    // Keep per-PID EPERM census detail behind the global --full flag; the
    // default prints one aggregate warning with a count.
    worktrees::set_unreadable_census_detail(globals.full);
    if let Some(branch) = &command.finished_branch {
        if command.dry_run || !command.apply {
            return crate::fail_with_repair(
                "gc",
                Outcome::Refused,
                "GC",
                "targeted scratch collection requires --apply and excludes --dry-run",
                "pass --apply and drop --dry-run",
                globals,
            );
        }
        return crate::provider_retention::run_branch_scratch(globals, branch);
    }
    match gc(&command, globals, workers, None) {
        Ok(data) => {
            let human = gc_compact_summary(&data, globals.full);
            let result = gc_result(&data, globals.json);
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
                report: *error.report,
                idle_seconds: command.idle.unwrap_or(6 * 3600),
                sweep_budget_seconds: 0,
                sweep_budget_source: "unavailable".into(),
                sweep_cursor: None,
                sweep_warning: None,
                sweep_stopped: false,
                unobserved_projects,
                skipped_transient,
                provider_state: *provider_state,
                chair_logs: Default::default(),
                build_dirs: Default::default(),
                graph_checkpoints_removed: 0,
                stale_graphs: Vec::new(),
                graph_bytes: None,
                port_receipts: Default::default(),
                retention_errors: Vec::new(),
                failed_classes: Vec::new(),
                retention_observations: Vec::new(),
                discard_archives: Default::default(),
                staging_busy: Vec::new(),
                worktree_cleanup,
                alerts,
                notice: None,
                ..Default::default()
            };
            let result = gc_failure(&data, &failure, globals.json);
            emit(&result, globals, "", command.dry_run)
        }
        Err(error) => crate::fail_with_repair(
            "gc",
            error.outcome(),
            "GC",
            &error.local_message(),
            GC_REPAIR,
            globals,
        ),
    }
}

/// Run one bounded sweep for a peer `gc` request and return its aggregates.
/// Unlike `run_gc` this writes no process envelope: the peer wraps the value
/// in a `PeerResult::Gc` and the calling host renders the fan-out.
pub(crate) fn gc_for_peer(
    command: &Gc,
    globals: &provenant_cli::Globals,
    workers: &dyn WorkerObserver,
) -> Result<serde_json::Value, String> {
    worktrees::set_unreadable_census_detail(globals.full);
    let data = gc(command, globals, workers, None).map_err(|error| error.local_message())?;
    serde_json::to_value(&data).map_err(|error| error.to_string())
}

/// The compact view stays under 32 KiB; `--full` carries the per-candidate
/// rows. Aggregate lines are grouped by class and reason before they are cut.
const GC_AGGREGATE_LIMIT: usize = 30 * 1024;

/// One line per kept class and reason. The default compact view shows this
/// aggregate first so it survives the renderer's line clip; JSON and `--full`
/// carry one row per candidate instead.
fn gc_uncollected_aggregate_lines(data: &GcData) -> Vec<String> {
    let mut groups: std::collections::BTreeMap<(&str, String), (usize, Option<u64>)> =
        std::collections::BTreeMap::new();
    for item in gc_uncollected(data) {
        let entry = groups
            .entry((item.class, item.reason))
            .or_insert((0, Some(0)));
        entry.0 += 1;
        entry.1 = entry.1.zip(item.bytes).map(|(sum, bytes)| sum + bytes);
    }
    let mut used = 0;
    let mut lines = Vec::new();
    let mut omitted = 0;
    for ((class, reason), (count, bytes)) in groups {
        let bytes = bytes.map_or_else(
            || "bytes unknown".to_owned(),
            |bytes| format!("{bytes} bytes"),
        );
        let line = format!("uncollected {class}: {count} item(s), {bytes}; kept: {reason}");
        if used + line.len() + 1 > GC_AGGREGATE_LIMIT {
            omitted += 1;
            continue;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    if omitted > 0 {
        lines.push(format!(
            "uncollected aggregate: {omitted} class/reason group(s) omitted; rerun with --full"
        ));
    }
    lines
}

fn gc_compact_summary(data: &GcData, full: bool) -> String {
    let trees: usize = data
        .worktree_cleanup
        .iter()
        .map(|entry| entry.worktrees.removed.len())
        .sum();
    let mut summary = format!(
        "{}: {trees} worktrees, {} attempt scratch directories, {} known bytes; {} retention errors\nGC sweep budget: {}s (source: {}); preview budget: {}s (source: {}); free-space floor: {} bytes (source: {}); attempt scratch cap: {} bytes (source: {}); scratch grace: {}s (source: {}); scratch warning share: {}\n",
        if data.report.dry_run {
            "would collect"
        } else {
            "collected"
        },
        data.provider_state.attempt_tmp.len(),
        gc_known_reclaimed_bytes(data),
        data.retention_errors.len() + data.provider_state.errors.len(),
        data.sweep_budget_seconds,
        data.sweep_budget_source,
        data.preview_budget_seconds,
        data.preview_budget_source,
        data.free_floor_bytes,
        data.free_floor_source,
        data.attempt_scratch_max_bytes,
        data.attempt_scratch_max_source,
        data.scratch_grace_seconds,
        data.scratch_grace_source,
        data.scratch_warning_share
    );
    summary.push_str(&candidate_progress_line(data));
    if data.preview_budget_stopped {
        summary.push_str(&format!(
            "preview stopped at gc.preview_budget={}s (source: {}); raise it or rerun with --apply\n",
            data.preview_budget_seconds, data.preview_budget_source
        ));
    }
    for line in gc_uncollected_aggregate_lines(data) {
        summary.push_str(&line);
        summary.push('\n');
    }
    if let Some(notice) = &data.notice {
        summary.push_str(notice);
        summary.push('\n');
    }
    if data.sweep_stopped {
        summary.push_str(&format!(
            "sweep stopped at gc.sweep_budget={}s (source: {}); cursor saved for the next sweep\n",
            data.sweep_budget_seconds, data.sweep_budget_source
        ));
    }
    if let Some(warning) = &data.sweep_warning {
        summary.push_str("warning: ");
        summary.push_str(warning);
        summary.push('\n');
    }
    for alert in &data.alerts {
        summary.push_str(alert);
        summary.push('\n');
    }
    for process in &data.held_processes {
        summary.push_str(&process.advisory());
        summary.push('\n');
    }
    for line in crate::stranded_work::summary_lines(&data.stranded_work) {
        summary.push_str(&line);
        summary.push('\n');
    }
    if data.held_incomplete {
        summary.push_str(worktrees::HELD_CENSUS_INCOMPLETE_WARNING);
        summary.push('\n');
    }
    if full {
        for item in gc_uncollected(data) {
            summary.push_str(&item.human_line());
            summary.push('\n');
        }
    }
    summary
}

fn gc_data_summary(data: &GcData) -> String {
    let mut human = format!(
        "{} cache(s), {} bytes {}; {} kept\n{} provider-state removal(s), {} kept, {} error(s)\n",
        data.report.removed.len(),
        gc_cache_bytes(data),
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
    human.push_str(&format!(
        "{} known allocated bytes {} (other classes may be unmeasured)\n",
        gc_known_reclaimed_bytes(data),
        if data.report.dry_run {
            "reclaimable"
        } else {
            "reclaimed"
        }
    ));
    let mut by_class = gc_retention_bytes(data)
        .into_iter()
        .filter_map(|row| {
            let (suffix, bytes) = match row.observation {
                provenant_config::ByteObservation::Measured { bytes } => ("", bytes),
                provenant_config::ByteObservation::Partial { bytes } => (" (partial)", bytes),
                provenant_config::ByteObservation::Unknown => return None,
            };
            (bytes > 0).then(|| format!("{:?}={bytes}{suffix}", row.class))
        })
        .collect::<Vec<_>>();
    by_class.sort();
    if !by_class.is_empty() {
        human.push_str(&format!(
            "reclaimed bytes by class: {}\n",
            by_class.join(", ")
        ));
    }
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
    human.push_str(&candidate_progress_line(data));
    human.push_str(&format!(
        "{} import pin(s) {}\n",
        data.import_pins,
        if data.report.dry_run {
            "would be removed"
        } else {
            "removed"
        }
    ));
    let not_run = |class| data.classes_not_run.contains(&class);
    let archives_count = if data.report.dry_run {
        data.discard_archives.would_remove.len()
    } else {
        data.discard_archives.removed.len()
    };
    human.push_str(&format!(
        "{} discard archive(s), {} bytes {} (retention {}; source: {})\n",
        archives_count,
        data.discard_archives.bytes,
        if data.report.dry_run {
            "reclaimable"
        } else {
            "reclaimed"
        },
        data.discard_archive_retention_setting,
        data.discard_archive_retention_source,
    ));
    for archive in &data.discard_archives.busy {
        human.push_str(&format!("busy discard archive {}\n", archive.display()));
    }
    let idle = if data.build_dirs.target_idle_setting.is_empty() {
        String::new()
    } else {
        format!(
            "; idle after {} (source: {})",
            data.build_dirs.target_idle_setting, data.build_dirs.target_idle_source
        )
    };
    let cargo_bytes_label = data.build_dirs.byte_method.as_str();
    if not_run(provenant_config::RetentionKind::BuildDirectories) {
        human.push_str("Cargo targets: not run in this pass\n");
    } else if data.build_dirs.measurement_skipped {
        human.push_str(&format!(
            "Cargo target census partial: {} known {cargo_bytes_label} / {} byte budget (setting {}; source: {}{idle}); resumes at {}\n",
            data.build_dirs.cache_bytes,
            data.build_dirs.budget_bytes,
            data.build_dirs.budget_setting,
            data.build_dirs.budget_source,
            data.build_dirs
                .interrupted_at
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new("next sweep"))
                .display(),
        ));
    } else {
        human.push_str(&format!(
        "{} Cargo target(s), {} {cargo_bytes_label} {}; {} / {} {cargo_bytes_label} remain (budget setting {}; source: {}{idle}), {} busy, {} recent\n",
        data.build_dirs.removed.len(),
        data.build_dirs.bytes,
        if data.report.dry_run {
            "reclaimable"
        } else {
            "reclaimed"
        },
        data.build_dirs.cache_bytes,
        data.build_dirs.budget_bytes,
        data.build_dirs.budget_setting,
        data.build_dirs.budget_source,
        data.build_dirs.busy.len(),
        data.build_dirs.recent.len(),
    ));
    }
    if !not_run(provenant_config::RetentionKind::BuildDirectories)
        && data.build_dirs.byte_method == provenant_run::build_dir_gc::ByteMethod::AllocatedFallback
    {
        human
            .push_str("Cargo target sizes: private bytes unavailable; reporting allocated bytes\n");
    }
    for removal in &data.build_dirs.removed {
        human.push_str(&format!(
            "{} Cargo target {} ({}{}; {} {cargo_bytes_label}{})\n",
            if data.report.dry_run {
                "would remove"
            } else {
                "removed"
            },
            removal.path.display(),
            removal.reason.as_str(),
            removal
                .last_use
                .map(|last_use| format!("; last use from {}", last_use.as_str()))
                .unwrap_or_default(),
            removal.bytes,
            removal
                .worktree
                .as_deref()
                .map(|worktree| format!("; worktree {}", worktree.display()))
                .unwrap_or_default(),
        ));
    }
    for warning in &data.build_dirs.warnings {
        human.push_str(&format!("Cargo cache warning: {warning}\n"));
    }
    for error in &data.build_dirs.errors {
        human.push_str(&format!("Cargo target: {error}\n"));
    }
    for target in &data.build_dirs.busy {
        human.push_str(&format!("busy Cargo target {}\n", target.display()));
    }
    for kept in &data.provider_state.kept {
        human.push_str(&format!("kept {}: {}\n", kept.path.display(), kept.reason));
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
    for temporary in &data.provider_state.partial_tmp {
        human.push_str(&format!(
            "partially collected attempt tmp {} ({} bytes); retry remains\n",
            temporary.path.display(),
            temporary.bytes
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
    for error in &data.retention_errors {
        human.push_str(&format!(
            "{}: {}: {}\n",
            error.path.display(),
            error.operation,
            error.cause
        ));
    }
    for error in &data.port_receipts.errors {
        human.push_str(&format!(
            "{}: {}: {}\n",
            error.path.display(),
            error.operation,
            error.cause
        ));
    }
    for error in &data.provider_state.payload.errors {
        human.push_str(&format!("{}: {}\n", error.path.display(), error.reason));
    }
    for error in &data.chair_logs.errors {
        human.push_str(&format!("{}: {}\n", error.path.display(), error.reason));
    }
    for error in &data.discard_archives.errors {
        human.push_str(&format!(
            "{}: {}: {}\n",
            error.path.display(),
            error.operation,
            error.cause
        ));
    }
    for error in &data.report.errors {
        human.push_str(&format!(
            "{}: {}: {}\n",
            error.path.display(),
            error.operation,
            error.cause
        ));
    }
    for error in &data.provider_state.errors {
        human.push_str(&format!("{}: {}\n", error.path.display(), error.reason));
    }
    for entry in &data.report.kept {
        if entry.reason == provenant_confinement::cache::CacheGcReason::Unobservable {
            human.push_str(&format!("kept {} (unobservable)\n", entry.path.display()));
        }
    }
    human.push_str("Allocated tree bytes include shared clone blocks; actual volume space reclaimed may differ.\n");
    for project in &data.worktree_cleanup {
        human.push_str(&format!(
            "git worktrees: {} allocated bytes{}\n",
            project.bytes,
            if project.byte_errors.is_empty() {
                ""
            } else {
                " (partial; limits below)"
            }
        ));
        let mut classes = std::collections::BTreeMap::new();
        for candidate in &project.candidates {
            *classes
                .entry((candidate.class, candidate.action))
                .or_insert(0usize) += 1;
        }
        for ((class, action), count) in classes {
            human.push_str(&format!("{class}: {count} {action}\n"));
        }
        for error in project.byte_errors.iter().chain(&project.cache_errors) {
            human.push_str(&format!(
                "{}: {}: {}\n",
                error.path.display(),
                error.operation,
                error.cause
            ));
        }

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
    let retired_verb = if data.report.dry_run {
        "would retire"
    } else {
        "retired"
    };
    for project in &data.retired_projects {
        human.push_str(&format!(
            "{retired_verb} project binding {} at {}\n",
            project.project,
            project.common_dir.display()
        ));
    }
    for process in &data.held_processes {
        human.push_str(&format!("{}\n", process.advisory()));
    }
    for line in crate::stranded_work::human_lines(&data.stranded_work) {
        human.push_str(&line);
        human.push('\n');
    }
    if data.held_incomplete {
        human.push_str(&format!("{}\n", worktrees::HELD_CENSUS_INCOMPLETE_WARNING));
    }
    for alert in &data.alerts {
        human.push_str(&format!("{alert}\n"));
    }
    if let Some(notice) = &data.notice {
        human.push_str(&format!("{notice}\n"));
    }
    // The failure view also aggregates kept candidates: one line per class
    // and reason, never a long list of per-path rows.
    for line in gc_uncollected_aggregate_lines(data) {
        human.push_str(&line);
        human.push('\n');
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
#[allow(clippy::large_enum_variant)] // Detailed output borrows the full report; summary stays compact.
enum GcOutput<'a> {
    Detailed {
        #[serde(flatten)]
        data: &'a GcData,
        /// The sweep stopped at a budget and saved a cursor; exit stays zero.
        partial: bool,
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        uncollected: Vec<crate::gc_support::UncollectedItem>,
        retention_bytes: Vec<provenant_config::RetentionBytes>,
        removal_records: Vec<RemovalRecord>,
    },
    Summary {
        dry_run: bool,
        /// The sweep stopped at a budget and saved a cursor; exit stays zero.
        partial: bool,
        candidate_progress: &'a Option<GcCandidateProgress>,
        retention_bytes: Vec<provenant_config::RetentionBytes>,
        removal_records: Vec<RemovalRecord>,
        removed: usize,
        kept: usize,
        bytes_freed: u64,
        known_reclaimed_bytes: u64,
        provider_state_removed: usize,
        build_dirs_removed: usize,
        build_dir_bytes_reclaimed: u64,
        build_dir_byte_method: &'a provenant_run::build_dir_gc::ByteMethod,
        build_dir_bytes: u64,
        build_dir_budget_bytes: u64,
        build_dir_budget_setting: &'a str,
        build_dir_budget_source: &'a str,
        build_dir_measurement_skipped: bool,
        build_dir_interrupted_at: &'a Option<PathBuf>,
        build_dir_target_idle_setting: &'a str,
        build_dir_target_idle_source: &'a str,
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        classes_not_run: &'a [provenant_config::RetentionKind],
        graph_checkpoints_removed: usize,
        import_pins: usize,
        discard_archives_reclaimed: usize,
        discard_archive_bytes: u64,
        discard_archives_busy: &'a [PathBuf],
        discard_archive_retention_setting: &'a str,
        discard_archive_retention_source: &'a str,
        sweep_budget_seconds: u64,
        sweep_budget_source: &'a str,
        free_floor_bytes: u64,
        free_floor_source: &'a str,
        attempt_scratch_max_bytes: u64,
        attempt_scratch_max_source: &'a str,
        scratch_grace_seconds: u64,
        scratch_grace_source: &'a str,
        preview_budget_seconds: u64,
        preview_budget_source: &'a str,
        preview_budget_stopped: bool,
        scratch_warning_share: f64,
        scratch_warning_share_source: &'a str,
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        uncollected: Vec<crate::gc_support::UncollectedItem>,
        sweep_cursor: &'a Option<crate::gc_sweep::Cursor>,
        sweep_warning: &'a Option<String>,
        sweep_stopped: bool,
        port_receipts: &'a crate::port_receipt_retention::Report,
        retention_errors: &'a [provenant_config::RetentionIssue],
        payload_errors: &'a [provenant_run::ProviderStateIssue],
        errors: &'a [provenant_run::ProviderStateIssue],
        unobserved_projects: &'a [PathBuf],
        alerts: &'a [String],
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        held_processes: &'a [worktrees::HeldProcess],
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        stranded_work: &'a [crate::stranded_work::StrandedWork],
        held_incomplete: bool,
        notice: &'a Option<String>,
    },
}
fn append_gc_subpass_removals(
    report: &mut worktrees::WorktreeGcReport,
    removals: Vec<RemovalRecord>,
) {
    if !report.dry_run {
        for removal in removals {
            if !report.removals.contains(&removal) {
                report.removals.push(removal);
            }
        }
    }
}

fn append_gc_removal_records(report: &mut worktrees::WorktreeGcReport) {
    if !report.dry_run {
        // The collection owners attach only projections of their durable
        // retirement records. A raw removed row does not authorise a receipt.
        let removals = report
            .refs
            .iter()
            .filter_map(|outcome| outcome.removal.clone())
            .collect();
        append_gc_subpass_removals(report, removals);
    }
}

fn gc_result_data(data: &GcData, detailed: bool) -> GcOutput<'_> {
    if detailed {
        return GcOutput::Detailed {
            data,
            partial: data.preview_budget_stopped || data.sweep_stopped,
            uncollected: gc_uncollected(data),
            retention_bytes: gc_retention_bytes(data),
            removal_records: gc_removal_records(data),
        };
    }
    GcOutput::Summary {
        dry_run: data.report.dry_run,
        partial: data.preview_budget_stopped || data.sweep_stopped,
        candidate_progress: &data.candidate_progress,
        retention_bytes: gc_retention_bytes(data),
        removal_records: gc_removal_records(data),
        removed: data.report.removed.len(),
        kept: data.report.kept.len(),
        bytes_freed: data.report.bytes_freed,
        known_reclaimed_bytes: gc_known_reclaimed_bytes(data),
        provider_state_removed: data.provider_state.removed.len(),
        build_dirs_removed: data.build_dirs.removed.len(),
        build_dir_bytes_reclaimed: data.build_dirs.bytes,
        build_dir_byte_method: &data.build_dirs.byte_method,
        build_dir_bytes: data.build_dirs.cache_bytes,
        build_dir_budget_bytes: data.build_dirs.budget_bytes,
        build_dir_budget_setting: &data.build_dirs.budget_setting,
        build_dir_budget_source: &data.build_dirs.budget_source,
        build_dir_measurement_skipped: data.build_dirs.measurement_skipped,
        build_dir_interrupted_at: &data.build_dirs.interrupted_at,
        build_dir_target_idle_setting: &data.build_dirs.target_idle_setting,
        build_dir_target_idle_source: &data.build_dirs.target_idle_source,
        classes_not_run: &data.classes_not_run,
        graph_checkpoints_removed: data.graph_checkpoints_removed,
        import_pins: data.import_pins,
        discard_archives_reclaimed: if data.report.dry_run {
            data.discard_archives.would_remove.len()
        } else {
            data.discard_archives.removed.len()
        },
        discard_archive_bytes: data.discard_archives.bytes,
        discard_archives_busy: &data.discard_archives.busy,
        discard_archive_retention_setting: &data.discard_archive_retention_setting,
        discard_archive_retention_source: &data.discard_archive_retention_source,
        sweep_budget_seconds: data.sweep_budget_seconds,
        sweep_budget_source: &data.sweep_budget_source,
        free_floor_bytes: data.free_floor_bytes,
        free_floor_source: &data.free_floor_source,
        attempt_scratch_max_bytes: data.attempt_scratch_max_bytes,
        attempt_scratch_max_source: &data.attempt_scratch_max_source,
        scratch_grace_seconds: data.scratch_grace_seconds,
        scratch_grace_source: &data.scratch_grace_source,
        preview_budget_seconds: data.preview_budget_seconds,
        preview_budget_source: &data.preview_budget_source,
        preview_budget_stopped: data.preview_budget_stopped,
        scratch_warning_share: data.scratch_warning_share,
        scratch_warning_share_source: &data.scratch_warning_share_source,
        uncollected: gc_uncollected(data),
        sweep_cursor: &data.sweep_cursor,
        sweep_warning: &data.sweep_warning,
        sweep_stopped: data.sweep_stopped,
        port_receipts: &data.port_receipts,
        retention_errors: &data.retention_errors,
        payload_errors: &data.provider_state.payload.errors,
        errors: &data.provider_state.errors,
        unobserved_projects: &data.unobserved_projects,
        alerts: &data.alerts,
        held_processes: &data.held_processes,
        stranded_work: &data.stranded_work,
        held_incomplete: data.held_incomplete,
        notice: &data.notice,
    }
}

fn gc_origin_source(origin: &provenant_config::Origin) -> String {
    match origin {
        provenant_config::Origin::Compiled { profile } => {
            format!("compiled {} profile", profile.as_str())
        }
        provenant_config::Origin::User { path } => {
            path.as_path_text().unwrap_or("config.toml").to_owned()
        }
    }
}

struct GcProject {
    binding: provenant_config::ProjectBinding,
    state: Directory,
    lanes: Vec<worktrees::WorktreeInfo>,
    path: PathBuf,
    /// Import-extra pins the sweep removed (apply) or would remove (preview).
    import_pins: usize,
    /// Uncommitted work in worktrees no lane holds. Kept and named only.
    stranded_work: Vec<crate::stranded_work::StrandedWork>,
}

/// Open a checkout normally, or a bare common directory only through an
/// existing binding whose trusted layout is verified by the utility.
fn open_gc_repo(
    roots: &provenant_core::Roots,
    runtime: &UtilityRuntime,
    executor: &SystemExecutor,
    path: &Path,
    budget: UtilityBudget,
    cancel: &CancelToken,
) -> Result<provenant_confinement::repo::VerifiedRepo, Error> {
    match runtime.open_repo(executor, path, budget, cancel) {
        Ok(repo) => Ok(repo),
        Err(error) => {
            if let Ok(common) = provenant_os::fs::canonicalize(path)
                && let Ok(location) = provenant_config::ProjectLocation::new(common)
                && let Some(binding) = provenant_config::lookup_project(roots, &location)?
            {
                return runtime
                    .open_bound_bare_repo(executor, &binding, budget, cancel)
                    .map_err(Into::into);
            }
            Err(error.into())
        }
    }
}

/// One advisory dweller observation for a whole GC pass. It runs after the
/// collection classes, so a slow census cannot spend the sweep budget before
/// any candidate is collected. The session caches process observations for the
/// pass: no later project or candidate walks the process table again.
///
/// The report is advisory and never selects a PID to signal. A cancelled or
/// cut-short observation is `incomplete`, so an undecided candidate is never
/// read as free.
struct DwellerCensus;

impl DwellerCensus {
    fn observe(
        workers: &dyn WorkerObserver,
        roots: &[PathBuf],
        cancel: &CancelToken,
    ) -> worktrees::HeldReport {
        if cancel.is_cancelled() {
            return worktrees::HeldReport {
                incomplete: true,
                ..worktrees::HeldReport::default()
            };
        }
        if roots.is_empty() {
            return worktrees::HeldReport::default();
        }
        let clock = SystemClock::new();
        let started = clock.mono_now().as_micros();
        let within_budget = || {
            !cancel.is_cancelled()
                && clock.mono_now().as_micros().saturating_sub(started)
                    < worktrees::HELD_CENSUS_BUDGET_MICROS
        };
        let session = worktrees::CensusSession::new(workers, &clock);
        worktrees::held_report(&session, roots, &within_budget)
    }
}

/// Settled attempt directories and terminal worktrees across every project.
/// One host-wide census covers them all instead of one census per project.
fn dweller_roots(roots: &provenant_core::Roots, cancel: &CancelToken) -> Vec<PathBuf> {
    crate::gc_support::settled_roots(roots, cancel)
}

#[allow(clippy::too_many_arguments)] // Project context and sweep deadline stay explicit at this boundary.
fn gc_project(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    owner: (provenant_config::ProjectBinding, worktrees::ProjectLayout),
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    read_only: bool,
    remaining: Duration,
    cancel: &CancelToken,
) -> Result<GcProject, Error> {
    let (binding, layout) = owner;
    let mutation_budget = if read_only {
        remaining.min(Duration::from_secs(2))
    } else {
        remaining.min(MUTATION_TIMEOUT)
    }
    .max(Duration::from_millis(1));
    with_context_mode_with_owner_cancel(
        roots,
        home,
        project,
        mutation_budget,
        workers,
        facts,
        read_only,
        true,
        Some((binding, layout)),
        cancel,
        |context, cancel| {
            let bare = facts.bare_setting(
                context.repo,
                context.executor,
                context.utility,
                context.utility_budget,
                cancel,
            )?;
            let valid_owner = match (bare, context.layout.project()) {
                (Some(false), Some(primary)) => primary == context.repo.root_path(),
                (Some(true), None) => context.layout.root() == context.repo.common_git_dir(),
                _ => false,
            };
            if !valid_owner {
                return Err(Error::GcObservation(
                    "GC requires a verified primary checkout or bound bare owner; run it from the primary checkout (provenant worktree list shows it)",
                ));
            }
            let lanes = if read_only {
                worktrees::list_for_gc_preview(context, cancel)?
            } else {
                worktrees::list_for_gc(context, cancel)?
            };
            let now_micros = crate::stranded_work::now_micros(context.clock);
            let stranded_work =
                crate::stranded_work::survey_in_context(context, cancel, now_micros);
            // Settlement clears these pins on every outcome. GC is the
            // backstop for pins left by an earlier version or an attempt
            // whose record has since gone. The preview counts them without
            // touching the ref store; apply removes them.
            let call = provenant_run::Request {
                clock: context.clock,
                cancel,
            };
            let store_context = provenant_run::ExecutionContext {
                state_root: roots.state.clone(),
                project_id: context.binding.project_id(),
                actor_id: provenant_core::ActorId::from_bytes([0; 16]),
                read_prompts: false,
            };
            let import_pins =
                match provenant_run::RunStore::open_disk_read_only(&store_context, &call) {
                    Ok(Some(store)) => match provenant_run::gc_import_pins(
                        context.utility.git_program_path(),
                        context.repo,
                        &store,
                        &call,
                        read_only,
                    ) {
                        Ok(count) => count,
                        Err(error) => {
                            warn_import_pin_collection(project, &error);
                            0
                        }
                    },
                    Ok(None) => 0,
                    Err(error) => {
                        warn_import_pin_collection(project, &error);
                        0
                    }
                };
            Ok(GcProject {
                binding: context.binding.clone(),
                path: project.to_owned(),
                import_pins,
                state: Directory::open_anchor(
                    &roots
                        .state
                        .as_path()
                        .join("worktrees")
                        .join(context.binding.project_id().to_string()),
                )?,
                lanes,
                stranded_work,
            })
        },
    )
}

/// An unreadable run store cannot prove which pins are settled, so the
/// backstop keeps them all. Warn and let the rest of the sweep run: failing the
/// project here would skip settled-lane collection and the path-based pass.
fn warn_import_pin_collection(project: &Path, error: &(impl std::fmt::Display + ?Sized)) {
    provenant_records::warning::warn(
        provenant_records::warning::Warning::new(
            provenant_records::warning::IMPORT_PIN_COLLECTION_FAILED,
            format!(
                "import pin collection failed for {}: {error}",
                project.display()
            ),
            provenant_records::warning::detected_source(
                provenant_records::warning::IMPORT_PIN_COLLECTION_FAILED,
            ),
        )
        .subject(project.display().to_string())
        .once_per_change(),
    );
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

/// The remote reference the branch-scratch spawn should record. Self-describing
/// landing evidence names its own reference, so the forge is asked only for the
/// fallback; an unresolved context yields no source and the spawn is skipped.
fn retention_scratch_source(
    evidence: &LandedEvidence,
    forge: impl FnOnce() -> Option<String>,
) -> Option<String> {
    match &evidence.source {
        LandingSource::IntegrationTarget { reference, .. }
        | LandingSource::TreeEquivalent { reference, .. } => Some(
            reference
                .strip_prefix("refs/remotes/")
                .unwrap_or(reference)
                .to_owned(),
        ),
        _ => forge(),
    }
}

/// Remove the registered worktree of a merged branch through the same removal
/// as `worktree remove`, after deleting the build output GC already treats as
/// regenerable (`gc.projects.<id>.regenerable` plus `target`). Output pruning
/// keeps GC's checks: the lane must be idle with terminal writers, tracked
/// and kept paths stay, and symlinks are never followed. Removal refuses a
/// dirty or live worktree and keeps a branch not contained in `integration`.
/// The worktree is only removed while its HEAD is still `tip`, the commit the
/// caller proved merged, so commits made since keep the checkout too.
/// An observed merge cleans only its linked subjects. A later low-space sweep
/// retries busy/unknown items; it is never the primary event mechanism.
pub(crate) fn retention_event(
    resolved: &roots::Resolved,
    subjects: &[Subject],
    evidence: &LandedEvidence,
    workers: &dyn WorkerObserver,
) -> Result<(), Error> {
    let facts = worktrees::RepoFacts::default();
    let config = provenant_cli::config_reads::read_for(&resolved.roots, "gc")?;
    with_context_using_facts(
        &resolved.roots,
        &resolved.home,
        &resolved.project,
        MUTATION_TIMEOUT,
        workers,
        &facts,
        |context, cancel| {
            let lanes = worktrees::list_for_gc_matching(context, cancel, |path, branch| {
                subjects.iter().any(|subject| match subject.kind {
                    SubjectKind::Worktree => {
                        subject.worktree_path().as_deref() == Some(path)
                            && provenant_confinement::verify_worktree(context.repo, path)
                                .ok()
                                .and_then(|tree| worktrees::worktree_subject(tree.repo()).ok())
                                .as_ref()
                                == Some(subject)
                    }
                    SubjectKind::Path => path == Path::new(&subject.id),
                    SubjectKind::Branch => branch == Some(subject.id.as_str()),
                    SubjectKind::Project => false,
                    _ => false,
                })
            })?;
            let settings = config
                .gc
                .project_settings(&context.binding.project_id().to_string(), &resolved.project);
            let attempts: std::collections::BTreeMap<_, _> = lanes
                .iter()
                .map(|lane| {
                    let ids = lane
                        .branch
                        .as_deref()
                        .and_then(|branch| branch.strip_prefix("refs/heads/"))
                        .and_then(|branch| {
                            crate::provider_retention::branch_attempts(
                                &resolved.roots,
                                context.binding.project_id(),
                                branch,
                            )
                            .ok()
                        });
                    (lane.path.clone(), ids)
                })
                .collect();
            let free = |path: &Path| {
                attempts
                    .get(path)
                    .and_then(Option::as_ref)
                    .is_some_and(|ids| {
                        crate::provider_retention::attempts_released(
                            &resolved.roots,
                            context.binding.project_id(),
                            ids,
                            path,
                        )
                        .unwrap_or(false)
                    })
            };
            let terminal = free;
            let owner = crate::retention_owner::Owner::new(
                &resolved.roots,
                &resolved.home,
                &resolved.project,
            )
            .with_merge_observation(evidence);
            let authority = provenant_cli::fabric::Context::open(
                resolved.roots.clone(),
                &resolved.home,
                &resolved.project,
            )
            .map_err(|_| Error::GcObservation("cannot establish retirement authority"))?;
            if authority.identity.project != context.binding.project_id() {
                return Err(Error::InvalidScope);
            }
            let retirement = crate::custody::RetirementWriter::open(
                &resolved.roots,
                context.binding.project_id(),
                provenant_cli::fabric::current_chair(&authority)?,
                context.clock,
                cancel,
            )?;
            // Self-describing evidence names its own reference; an unresolved
            // fallback context simply skips the scratch spawn.
            let scratch_source = retention_scratch_source(evidence, || {
                worktrees::forge_context(context)
                    .ok()
                    .map(|forge| format!("{}/{}", forge.remote.value, forge.default_branch.value))
            });
            let policy = worktrees::WorktreeGcPolicy {
                mode: worktrees::CollectionMode::LandingCleanup,
                proof: Some(evidence),
                upstream: None,
                upstream_reference: None,
                terminal: &terminal,
                no_live_lanes: &free,
                regenerable: &settings.regenerable,
                keep: &settings.keep,
                written_before_ns: i128::from(
                    context
                        .clock
                        .wall_now()
                        .map_err(|_| Error::Budget)?
                        .as_micros(),
                ) * 1_000,
                dry_run: false,
                include_foreign: settings.foreign_worktrees
                    == provenant_config::ForeignWorktrees::Collect,
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&owner),
                integration_patterns: &settings.integration_branches,
                excluded_paths: &settings.keep_paths,
            };
            for class in provenant_config::retention_schedule(
                provenant_config::RetentionTrigger::MergeObservation,
            ) {
                if class.class == provenant_config::RetentionKind::GitWorktrees {
                    let report = worktrees::collect(context, &lanes, &policy, cancel)?;
                    for lane in &report.worktrees.removed {
                        if let Some(branch) = lane
                            .branch
                            .as_deref()
                            .and_then(|branch| branch.strip_prefix("refs/heads/"))
                            && let Some(Some(ids)) = attempts.get(&lane.path)
                            && let Some(source) = scratch_source.as_deref()
                        {
                            let _ = crate::provider_retention::spawn_branch_scratch(
                                &resolved.roots,
                                &resolved.home,
                                &resolved.project,
                                branch,
                                lane.head,
                                source,
                                ids,
                            );
                        }
                    }
                }
            }
            Ok(())
        },
    )
}

pub(crate) fn land_cleanup(
    globals: &Globals,
    branch: &str,
    integration: &str,
    tip: GitOid,
    workers: &dyn WorkerObserver,
) -> Result<LandWorktree, Error> {
    let roots::Resolved {
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
            let project_id = context.binding.project_id();
            let attempts =
                crate::provider_retention::branch_attempts(&roots, project_id, branch).ok();
            let Some(lane) = worktrees::list(context, cancel)?.into_iter().find(|lane| {
                lane.branch
                    .as_deref()
                    .is_some_and(|name| name.strip_prefix("refs/heads/") == Some(branch))
            }) else {
                if let Some(ids) = &attempts {
                    let _ = crate::provider_retention::spawn_branch_scratch(
                        &roots,
                        &home,
                        &project,
                        branch,
                        tip,
                        integration,
                        ids,
                    );
                }
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
            // A branch landed by hand may never have had a lane, so unlike a
            // sweep this accepts a worktree no lane used. A live or unobservable
            // lane, or a stale scan, still refuses.
            let idle = |path: &Path| {
                attempts.as_ref().is_some_and(|ids| {
                    crate::provider_retention::attempts_released(&roots, project_id, ids, path)
                        .unwrap_or(false)
                })
            };
            let (terminal, no_live_lanes) = (idle, idle);
            // The merge is the proof of completion, so build output written in
            // the last day does not hold the worktree back as it does in a sweep.
            let now_ns = SystemClock
                .wall_now()
                .map(|now| i128::from(now.as_micros()) * 1_000)
                .map_err(|_| Error::Budget)?;
            let integration_ref =
                provenant_confinement::repo::GitRefName::remote_tracking(integration)
                    .map_err(|_| Error::InvalidScope)?;
            let integration_tip =
                context
                    .facts
                    .resolve_commit(context, integration_ref.clone(), cancel)?;
            let evidence = LandedEvidence {
                predicate: LandingPredicate::Landed,
                source: LandingSource::IntegrationTarget {
                    reference: integration_ref.as_str().to_owned(),
                    commit: integration_tip,
                },
            };
            let identity = provenant_cli::fabric::Context::open(roots.clone(), &home, &project)
                .map_err(|_| Error::GcObservation("cannot establish retirement authority"))?;
            if identity.identity.project != project_id {
                return Err(Error::InvalidScope);
            }
            let chair = provenant_cli::fabric::current_chair(&identity)?;
            let retirement = crate::custody::RetirementWriter::open(
                &roots,
                project_id,
                chair.clone(),
                context.clock,
                cancel,
            )?;
            let retention_owner = crate::retention_owner::Owner::new(&roots, &home, &project)
                .with_merge_observation(&evidence);
            let policy = worktrees::WorktreeGcPolicy {
                upstream: Some(integration_tip),
                mode: provenant_confinement::worktrees::CollectionMode::LandingCleanup,
                proof: Some(&evidence),
                upstream_reference: Some(integration_ref.as_str()),
                terminal: &terminal,
                no_live_lanes: &no_live_lanes,
                regenerable: &regenerable,
                keep: &settings.keep,
                written_before_ns: now_ns,
                include_foreign: true,
                retirement_port: Some(&retirement),
                retirement_feeds: Some(&retention_owner),
                integration_patterns: &[],
                excluded_paths: &[],
                dry_run: false,
            };
            match worktrees::collect_selected(context, std::slice::from_ref(&lane), &policy, cancel)
            {
                Ok(outcome) => {
                    landed.output = outcome
                        .output
                        .into_iter()
                        .filter(|entry| entry.removed)
                        .map(|entry| entry.path)
                        .collect();
                    landed.removed = outcome
                        .worktrees
                        .removed
                        .iter()
                        .any(|removed| removed.path == lane.path);
                    landed.branch_deleted = outcome
                        .worktrees
                        .pruned_refs
                        .iter()
                        .any(|name| name == branch);
                    landed.kept = outcome.worktrees.retained.first().map(|kept| {
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
            if landed.removed {
                let _ = crate::provider_retention::spawn_branch_scratch(
                    &roots,
                    &home,
                    &project,
                    branch,
                    tip,
                    integration,
                    attempts.as_deref().unwrap_or(&[]),
                );
            }
            Ok(landed)
        },
    )
}

fn stale_age_text(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn gc(
    command: &Gc,
    globals: &Globals,
    workers: &dyn WorkerObserver,
    home_override: Option<&Path>,
) -> Result<GcData, Error> {
    use provenant_confinement::cache::CacheGcPolicy;

    let workspace = if command.scheduled {
        roots::Workspace::Home
    } else {
        roots::Workspace::Checkout
    };
    let resolved = match roots::resolve(globals, workspace, roots::Product::Required) {
        Err(roots::RootError::Checkout) if !command.scheduled => {
            roots::resolve(globals, roots::Workspace::Project, roots::Product::Required)?
        }
        result => result?,
    };
    let roots::Resolved {
        assets,
        home: resolved_home,
        project,
        roots,
        ..
    } = resolved;
    // A checkpoint carries the settlement's home explicitly so an in-process
    // sweep never scans the host's real home cache.
    let home = home_override.map_or(resolved_home, Path::to_path_buf);
    let facts = worktrees::RepoFacts::default();
    let mut projects = Vec::new();
    let mut scheduled_project_ids = Vec::new();
    let mut unobserved_projects = Vec::new();
    let mut skipped_transient = Vec::new();
    let config = provenant_cli::config_reads::read_for(&roots, "gc")?;
    let sweep_budget_seconds = config
        .gc
        .sweep_budget_seconds()
        .map_err(|_| Error::GcObservation("invalid gc.sweep_budget"))?;
    let sweep_budget = Duration::from_secs(sweep_budget_seconds);
    let sweep_budget_source = config
        .origins
        .get("gc.sweep_budget")
        .map(gc_origin_source)
        .unwrap_or_else(|| "compiled default".into());
    // One resolution of the floor and its source for admission, pressure and
    // reporting. An explicit `--free-floor` override names itself as the source.
    let (free_floor, free_floor_source) = match command.free_floor {
        Some(bytes) => (bytes, "command --free-floor".to_owned()),
        None => {
            let limits = config
                .gc
                .disk_limits(roots.state.as_path())
                .map_err(Error::DiskSpace)?;
            let source = config
                .origins
                .get("gc.free_floor")
                .map(gc_origin_source)
                .unwrap_or_else(|| limits.free_floor_source.description().to_owned());
            (limits.free_floor, source)
        }
    };
    // One resolution of the per-attempt scratch cap and its source for the
    // scratch adapter and for reporting. Zero disables the cap.
    let attempt_scratch_max_bytes = config
        .gc
        .attempt_scratch_max_bytes()
        .map_err(|_| Error::GcObservation("invalid gc.attempt_scratch_max"))?;
    let attempt_scratch_max_source = config
        .origins
        .get("gc.attempt_scratch_max")
        .map(gc_origin_source)
        .unwrap_or_else(|| "compiled default".into());
    let scratch_grace_seconds = config
        .gc
        .scratch_grace_seconds()
        .map_err(|_| Error::GcObservation("invalid gc.scratch_grace"))?;
    let scratch_grace_source = config
        .origins
        .get("gc.scratch_grace")
        .map(gc_origin_source)
        .unwrap_or_else(|| "compiled default".into());
    let preview_budget_seconds = config
        .gc
        .preview_budget_seconds()
        .map_err(|_| Error::GcObservation("invalid gc.preview_budget"))?;
    let preview_budget_source = config
        .origins
        .get("gc.preview_budget")
        .map(gc_origin_source)
        .unwrap_or_else(|| "compiled default".into());
    let scratch_warning_share = config.gc.scratch_warning_share_value();
    let scratch_warning_share_source = config
        .origins
        .get("gc.scratch_warning_share")
        .map(gc_origin_source)
        .unwrap_or_else(|| "compiled default".into());
    if (command.scheduled || command.pressure) && !config.gc.schedule {
        return Ok(GcData {
            report: provenant_confinement::cache::CacheGcReport {
                dry_run: command.dry_run,
                ..Default::default()
            },
            idle_seconds: command.idle.unwrap_or(6 * 3600),
            sweep_budget_seconds,
            sweep_budget_source,
            free_floor_bytes: free_floor,
            free_floor_source: free_floor_source.clone(),
            attempt_scratch_max_bytes,
            attempt_scratch_max_source: attempt_scratch_max_source.clone(),
            scratch_grace_seconds,
            scratch_grace_source: scratch_grace_source.clone(),
            preview_budget_seconds,
            preview_budget_source: preview_budget_source.clone(),
            scratch_warning_share,
            scratch_warning_share_source: scratch_warning_share_source.clone(),
            provider_state: Default::default(),
            unobserved_projects,
            skipped_transient,
            notice: command
                .pressure
                .then(|| "pressure collection skipped: gc.schedule=false".into()),
            ..Default::default()
        });
    }
    let clock = SystemClock::new();
    // An explicit floor request joins an in-flight sweep instead of reporting
    // an empty skip. The join is bounded by the configured sweep budget; on
    // expiry it is an error, never a successful collection of zero bytes.
    let joins_held_sweep = command.free_floor.is_some() && !command.dry_run;
    let wait_started = clock.mono_now();
    let mut lease = if command.dry_run {
        None
    } else {
        let acquired = if joins_held_sweep {
            crate::gc_sweep::acquire_waiting(
                roots.state.as_path(),
                sweep_budget,
                sweep_budget,
                &clock,
            )
        } else {
            crate::gc_sweep::acquire(roots.state.as_path(), sweep_budget, &clock)
        }
        .map_err(|error| Error::RetentionCollection(error.to_string()))?;
        match acquired {
            crate::gc_sweep::Acquire::Held(lease) => Some(lease),
            crate::gc_sweep::Acquire::Busy(warning) if joins_held_sweep => {
                return Err(Error::RetentionCollection(format!(
                    "GC sweep still running after a {sweep_budget:?} bounded wait; {}; this invocation collected nothing",
                    warning.unwrap_or_else(|| "sweep already running".into())
                )));
            }
            crate::gc_sweep::Acquire::Busy(warning) => {
                return Ok(GcData {
                    sweep_budget_seconds,
                    sweep_budget_source,
                    free_floor_bytes: free_floor,
                    free_floor_source: free_floor_source.clone(),
                    attempt_scratch_max_bytes,
                    attempt_scratch_max_source: attempt_scratch_max_source.clone(),
                    scratch_grace_seconds,
                    scratch_grace_source: scratch_grace_source.clone(),
                    preview_budget_seconds,
                    preview_budget_source: preview_budget_source.clone(),
                    scratch_warning_share,
                    scratch_warning_share_source: scratch_warning_share_source.clone(),
                    sweep_warning: warning.clone(),
                    notice: Some(
                        warning.unwrap_or_else(|| "sweep already running; skipped".into()),
                    ),
                    ..Default::default()
                });
            }
        }
    };
    let waited = Duration::from_micros(
        clock
            .mono_now()
            .as_micros()
            .saturating_sub(wait_started.as_micros()),
    );
    // This invocation's deadline starts only once it holds the lease, with the
    // budget the bounded join left over.
    let deadline = crate::gc_sweep::Deadline::start(sweep_budget.saturating_sub(waited));
    if let Some(lease) = &mut lease {
        lease
            .watch_fence(deadline.cancel().clone())
            .map_err(|error| Error::RetentionCollection(error.to_string()))?;
    }
    let trigger_identity = if command.pressure {
        "pressure"
    } else {
        "full_gc"
    };
    let scope_identity = if command.scheduled {
        "scheduled".to_owned()
    } else {
        format!(
            "manual:{}",
            provenant_core::hex(project.as_os_str().as_encoded_bytes())
        )
    };
    let identity_mismatch = lease
        .as_ref()
        .is_some_and(|lease| !lease.matches_identity(trigger_identity, &scope_identity));
    let mut sweep_warning = None;
    if identity_mismatch {
        let warning = lease
            .as_ref()
            .and_then(|lease| lease.warning.clone())
            .map(|takeover| {
                format!(
                    "{takeover}; saved GC sweep belongs to a different trigger or project scope; started fresh and preserved its cursor"
                )
            })
            .unwrap_or_else(|| {
                "saved GC sweep belongs to a different trigger or project scope; started fresh and preserved its cursor".into()
            });
        append_sweep_warning(&mut sweep_warning, warning);
    }
    if let Some(lease) = &mut lease {
        lease
            .set_identity(trigger_identity, &scope_identity)
            .map_err(|error| Error::RetentionCollection(error.to_string()))?;
    }
    let initial_cursor = lease.as_ref().and_then(|lease| lease.cursor().cloned());
    let takeover_warning = lease.as_ref().and_then(|lease| lease.warning.clone());
    if let Some(takeover) = takeover_warning.clone() {
        append_sweep_warning(&mut sweep_warning, takeover);
    }
    if sweep_budget_seconds == 0 {
        append_sweep_warning(
            &mut sweep_warning,
            "gc.sweep_budget=0 checkpoints immediately without collecting".into(),
        );
    }
    let idle_seconds = command.idle.unwrap_or(
        config
            .gc
            .idle_seconds()
            .map_err(|_| Error::GcObservation("invalid idle duration"))?,
    );
    // A binding whose common directory is gone stops being a project once its
    // last record has been idle for the same lifetime. Pressure sweeps leave
    // the registry alone; previews name what a later apply retires.
    let retirement = if command.pressure {
        provenant_config::Retirement::default()
    } else {
        provenant_config::retire_vanished_projects(&roots, idle_seconds, command.dry_run)?
    };
    for warning in retirement.warnings {
        append_sweep_warning(&mut sweep_warning, warning);
    }
    let retired_projects: Vec<RetiredProject> = retirement
        .retired
        .into_iter()
        .map(|binding| RetiredProject {
            project: binding.project_id(),
            common_dir: binding.common_dir().to_owned(),
        })
        .collect();
    if initial_cursor
        .as_ref()
        .is_some_and(|cursor| cursor.class == "complete")
    {
        if let Some(lease) = lease.take() {
            lease
                .complete()
                .map_err(|error| Error::RetentionCollection(error.to_string()))?;
        }
        return Ok(GcData {
            sweep_budget_seconds,
            sweep_budget_source,
            sweep_warning,
            retired_projects,
            notice: Some(
                "all retention classes had completed before the prior sweep reached its budget"
                    .into(),
            ),
            ..Default::default()
        });
    }
    if command.scheduled {
        let floor = free_floor;
        if provenant_confinement::disk::lane_available_bytes(roots.state.as_path(), &home, &project)
            .map_err(Error::DiskSpace)?
            >= floor
        {
            if initial_cursor.is_none() {
                if let Some(lease) = lease.take() {
                    lease
                        .complete()
                        .map_err(|error| Error::RetentionCollection(error.to_string()))?;
                }
            } else {
                // A paused sweep stays resumable until its recorded classes
                // finish; low pressure only defers the next collection pass.
                drop(lease.take());
            }
            return Ok(GcData {
                report: provenant_confinement::cache::CacheGcReport {
                    dry_run: command.dry_run,
                    ..Default::default()
                },
                sweep_budget_seconds,
                sweep_budget_source,
                free_floor_bytes: free_floor,
                free_floor_source: free_floor_source.clone(),
                attempt_scratch_max_bytes,
                attempt_scratch_max_source: attempt_scratch_max_source.clone(),
                scratch_grace_seconds,
                scratch_grace_source: scratch_grace_source.clone(),
                preview_budget_seconds,
                preview_budget_source: preview_budget_source.clone(),
                scratch_warning_share,
                scratch_warning_share_source: scratch_warning_share_source.clone(),
                sweep_cursor: initial_cursor,
                sweep_warning,
                retired_projects: retired_projects.clone(),
                notice: Some("background sweep skipped: free space is above gc.free_floor".into()),
                ..Default::default()
            });
        }
        let selection_detail = initial_cursor
            .as_ref()
            .filter(|cursor| cursor.class == "project_registry")
            .and_then(|cursor| cursor.detail.as_deref());
        let selection_epoch = if let Some(lease) = lease.as_ref() {
            lease.epoch()
        } else {
            SystemClock
                .wall_now()
                .map_err(|_| Error::Budget)?
                .as_micros() as u64
        };
        let selection_epoch = format!("{selection_epoch:016x}");
        let mut selection = {
            let active_lease = lease.as_ref();
            let mut keep_going = || sweep_should_continue(&deadline, active_lease);
            provenant_config::registered_gc_project_selection_with_progress(
                &roots,
                selection_detail,
                &selection_epoch,
                active_lease.is_some(),
                &mut keep_going,
            )?
        };
        for warning in selection.warnings.drain(..) {
            append_sweep_warning(&mut sweep_warning, warning);
        }
        if let Some(candidate) = selection.interrupted_at.take() {
            let cursor =
                preserved_discovery_cursor(initial_cursor.as_ref(), || crate::gc_sweep::Cursor {
                    class: "project_registry".into(),
                    project: None,
                    candidate: Some(candidate),
                    detail: selection.continuation.take(),
                });
            if let Some(lease) = &mut lease {
                lease
                    .checkpoint(cursor.clone())
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            return Ok(GcData {
                sweep_budget_seconds,
                sweep_budget_source,
                sweep_cursor: Some(cursor),
                sweep_warning,
                sweep_stopped: true,
                skipped_transient,
                retired_projects: retired_projects.clone(),
                notice: Some(
                    "sweep stopped at gc.sweep_budget during project registry discovery; next run resumes at the saved registry cursor".into(),
                ),
                ..Default::default()
            });
        }
        scheduled_project_ids = selection.project_ids.clone();
        let git = provenant_os::git::git_executable().ok_or(Error::GitUnavailable)?;
        let runtime = UtilityRuntime::read_only(&git, roots.state.as_path())?.with_lane_roots(
            provenant_confinement::utility::lane_state_roots(roots.state.as_path()),
        );
        let executor = SystemExecutor::new(1)?;
        let cancel = deadline.cancel().clone();
        let project_resume = initial_cursor
            .as_ref()
            .filter(|cursor| cursor.class == "project_selection")
            .and_then(|cursor| cursor.project.as_ref())
            .and_then(|project| {
                let project = project.to_string_lossy();
                provenant_core::ProjectId::parse(project.as_ref())
                    .ok()
                    .map(|project| project.to_string())
            });
        let project_start = project_resume.as_ref().map_or(0, |resume| {
            selection
                .project_ids
                .iter()
                .position(|project| project.to_string().as_str() >= resume.as_str())
                .unwrap_or(selection.project_ids.len())
        });
        for project_id in selection.project_ids.into_iter().skip(project_start) {
            let project_key = project_id.to_string();
            if !sweep_should_continue(&deadline, lease.as_ref()) {
                let cursor = preserved_discovery_cursor(initial_cursor.as_ref(), || {
                    crate::gc_sweep::Cursor {
                        class: "project_selection".into(),
                        project: Some(PathBuf::from(project_key.as_str())),
                        candidate: None,
                        detail: None,
                    }
                });
                if let Some(lease) = &mut lease {
                    lease
                        .checkpoint(cursor.clone())
                        .map_err(|error| Error::RetentionCollection(error.to_string()))?;
                }
                return Ok(GcData {
                    sweep_budget_seconds,
                    sweep_budget_source,
                    sweep_cursor: Some(cursor),
                    sweep_warning: sweep_warning.clone(),
                    sweep_stopped: true,
                    unobserved_projects,
                    skipped_transient,
                    retired_projects: retired_projects.clone(),
                    notice: Some("sweep stopped at gc.sweep_budget during project discovery; next run resumes from the saved cursor".into()),
                    ..Default::default()
                });
            }
            let binding = match provenant_config::lookup_gc_project(&roots, project_id) {
                Ok(provenant_config::GcProjectStatus::Git(binding)) => binding,
                Ok(provenant_config::GcProjectStatus::Transient(path)) => {
                    skipped_transient.push(path);
                    continue;
                }
                Ok(provenant_config::GcProjectStatus::Vanished { binding, .. }) => {
                    skipped_transient.push(binding.common_dir().to_owned());
                    continue;
                }
                Ok(provenant_config::GcProjectStatus::Excluded) => continue,
                Err(error) => {
                    unobserved_projects.push(provenant_config::project_binding_dir(
                        roots.state.as_path(),
                        project_id,
                    ));
                    append_sweep_warning(
                        &mut sweep_warning,
                        format!("project {project_key} lookup failed: {error}"),
                    );
                    continue;
                }
            };
            let common = binding.common_dir().to_owned();
            // The utility owns bound layout resolution, including trusted
            // local bare provenance for a checkout-less common directory.
            let budget = sweep_utility_budget(&deadline)?;
            let layout = match runtime
                .project_layout_for_binding(&executor, &binding, budget, &cancel)
            {
                Ok(layout) => layout,
                Err(_) => {
                    if !sweep_should_continue(&deadline, lease.as_ref()) {
                        let cursor = preserved_discovery_cursor(initial_cursor.as_ref(), || {
                            crate::gc_sweep::Cursor {
                                class: "project_selection".into(),
                                project: Some(PathBuf::from(project_key.as_str())),
                                candidate: None,
                                detail: None,
                            }
                        });
                        if let Some(lease) = &mut lease {
                            lease
                                .checkpoint(cursor.clone())
                                .map_err(|error| Error::RetentionCollection(error.to_string()))?;
                        }
                        return Ok(GcData {
                                sweep_budget_seconds,
                                sweep_budget_source,
                                sweep_cursor: Some(cursor),
                                sweep_warning: sweep_warning.clone(),
                                sweep_stopped: true,
                                unobserved_projects,
                                skipped_transient,
                                retired_projects: retired_projects.clone(),
                                notice: Some("sweep stopped at gc.sweep_budget during project discovery; next run resumes from the saved cursor".into()),
                                ..Default::default()
                            });
                    }
                    unobserved_projects.push(common.clone());
                    continue;
                }
            };
            let path = layout.project().unwrap_or(layout.root()).to_path_buf();
            match gc_project(
                &roots,
                &home,
                &path,
                (binding, layout),
                workers,
                &facts,
                command.dry_run,
                deadline.remaining(),
                deadline.cancel(),
            ) {
                Ok(project) => {
                    projects.push(project);
                }
                Err(_) if !sweep_should_continue(&deadline, lease.as_ref()) => {
                    let cursor = preserved_discovery_cursor(initial_cursor.as_ref(), || {
                        crate::gc_sweep::Cursor {
                            class: "project_selection".into(),
                            project: Some(PathBuf::from(project_key.as_str())),
                            candidate: None,
                            detail: None,
                        }
                    });
                    if let Some(lease) = &mut lease {
                        lease
                            .checkpoint(cursor.clone())
                            .map_err(|error| Error::RetentionCollection(error.to_string()))?;
                    }
                    return Ok(GcData {
                        sweep_budget_seconds,
                        sweep_budget_source,
                        sweep_cursor: Some(cursor),
                        sweep_warning: sweep_warning.clone(),
                        sweep_stopped: true,
                        unobserved_projects,
                        skipped_transient,
                        retired_projects: retired_projects.clone(),
                        notice: Some("sweep stopped at gc.sweep_budget during project discovery; next run resumes from the saved cursor".into()),
                        ..Default::default()
                    });
                }
                Err(_) => unobserved_projects.push(path.to_owned()),
            }
        }
    } else {
        let git = provenant_os::git::git_executable().ok_or(Error::GitUnavailable)?;
        let runtime = UtilityRuntime::read_only(&git, roots.state.as_path())?;
        let executor = SystemExecutor::new(1)?;
        let repo = open_gc_repo(
            &roots,
            &runtime,
            &executor,
            &project,
            sweep_utility_budget(&deadline)?,
            deadline.cancel(),
        )?;
        let common = repo.common_git_dir().to_owned();
        if provenant_config::transient_location(&roots, &common) {
            skipped_transient.push(common);
            if let Some(lease) = lease.take() {
                lease
                    .complete()
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            return Ok(GcData {
                report: provenant_confinement::cache::CacheGcReport {
                    dry_run: command.dry_run,
                    ..Default::default()
                },
                idle_seconds: command.idle.unwrap_or(6 * 3600),
                sweep_budget_seconds,
                sweep_budget_source,
                skipped_transient,
                retired_projects: retired_projects.clone(),
                notice: command
                    .pressure
                    .then(|| "pressure collection skipped: transient project".into()),
                ..Default::default()
            });
        }
        let bare = facts.bare_setting(
            &repo,
            &executor,
            &runtime,
            sweep_utility_budget(&deadline)?,
            deadline.cancel(),
        )?;
        if bare.is_none() {
            return Err(Error::GcObservation(
                "GC cannot verify whether the repository is bare",
            ));
        }
        let location = if bare == Some(false) && repo.private_git_dir() == repo.common_git_dir() {
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                repo.common_git_dir(),
                repo.root_path(),
            )?
        } else {
            provenant_config::ProjectLocation::new(repo.common_git_dir())?
        };
        let binding = if command.dry_run || bare == Some(true) {
            provenant_config::lookup_project(&roots, &location)?.ok_or(Error::GcObservation(
                "GC requires an existing project binding for a preview or bare repository; run `provenant status` from the project checkout once to bind it",
            ))?
        } else {
            provenant_config::bind_project(&roots, location)?
        };
        let layout = runtime.project_layout_for_binding(
            &executor,
            &binding,
            sweep_utility_budget(&deadline)?,
            deadline.cancel(),
        )?;
        let primary = layout.project().unwrap_or(layout.root()).to_path_buf();

        let selected = gc_project(
            &roots,
            &home,
            &primary,
            (binding, layout),
            workers,
            &facts,
            command.dry_run,
            deadline.remaining(),
            deadline.cancel(),
        )?;

        projects.push(selected);
    }
    projects.sort_by(|left, right| left.path.cmp(&right.path));
    let project_ids = if command.scheduled {
        scheduled_project_ids
    } else {
        projects
            .iter()
            .map(|project| project.binding.project_id())
            .collect()
    };
    let output_idle_seconds = idle_seconds;
    let lanes: Vec<_> = projects.iter().flat_map(|project| &project.lanes).collect();
    let now_ns = SystemClock
        .wall_now()
        .map(|now| i128::from(now.as_micros()) * 1_000)
        .map_err(|_| Error::Budget)?;
    let target_idle_seconds = config
        .gc
        .cargo_target_idle_seconds()
        .map_err(|_| Error::GcObservation("invalid Cargo target idle duration"))?;
    let policy = CacheGcPolicy {
        idle_before_ns: Some(now_ns - i128::from(idle_seconds) * 1_000_000_000),
        // Chair-made, hand-made and other projects' worktrees keep their
        // caches until `gc.cargo_target_idle`.
        foreign_idle_before_ns: Some(now_ns - i128::from(target_idle_seconds) * 1_000_000_000),
        // build_directories owns the shared lane/target budget and disk floor.
        // This pass reclaims only idle or orphaned whole private caches.
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
    let mut stranded_work: Vec<_> = projects
        .iter()
        .flat_map(|project| project.stranded_work.iter().cloned())
        .collect();
    stranded_work.sort_by(|left, right| left.path.cmp(&right.path));
    let import_pins = projects.iter().map(|project| project.import_pins).sum();
    let mut adapters = GcAdapters {
        command,
        roots: &roots,
        home: &home,
        assets: &assets,
        config: &config,
        workers,
        facts: &facts,
        projects,
        project_ids: &project_ids,
        policy,
        now_ns,
        output_idle_seconds,
        free_floor,
        cache_error: None,
        lease: lease.take(),
        deadline: &deadline,
        resume_cursor: initial_cursor.filter(|cursor| {
            cursor.class != "project_selection" && cursor.class != "project_registry"
        }),
        active_class: String::new(),
        project_start: None,
        candidate_start: None,
        stopped_cursor: None,
        data: GcData {
            idle_seconds,
            import_pins,
            sweep_budget_seconds,
            sweep_budget_source: sweep_budget_source.clone(),
            free_floor_bytes: free_floor,
            free_floor_source: free_floor_source.clone(),
            attempt_scratch_max_bytes,
            attempt_scratch_max_source: attempt_scratch_max_source.clone(),
            scratch_grace_seconds,
            scratch_grace_source: scratch_grace_source.clone(),
            preview_budget_seconds,
            preview_budget_source: preview_budget_source.clone(),
            scratch_warning_share,
            scratch_warning_share_source: scratch_warning_share_source.clone(),
            sweep_warning,
            stranded_work,
            unobserved_projects,
            skipped_transient,
            retired_projects,
            discard_archive_retention_seconds: config
                .gc
                .discard_archive_retention_seconds()
                .unwrap_or(14 * 86_400),
            discard_archive_retention_setting: config.gc.discard_archive_retention.clone(),
            discard_archive_retention_source: config
                .origins
                .get("gc.discard_archive_retention")
                .map(gc_origin_source)
                .unwrap_or_else(|| "compiled default".into()),
            ..Default::default()
        },
    };
    let trigger = if command.pressure {
        provenant_config::RetentionTrigger::Pressure
    } else {
        provenant_config::RetentionTrigger::FullGc
    };
    let mut classes = provenant_config::retention_schedule(trigger).collect::<Vec<_>>();
    // Attempt scratch is the cheapest, highest-churn class: it moves first so
    // a bounded pass always spends its budget on the work that frees the most
    // space before it reaches the slower classes.
    if let Some(index) = classes
        .iter()
        .position(|class| class.class == provenant_config::RetentionKind::AttemptScratch)
    {
        let first = classes.remove(index);
        classes.insert(0, first);
    }
    let resume_index = adapters.resume_cursor.as_ref().and_then(|cursor| {
        if cursor.class == "complete" {
            Some(classes.len())
        } else {
            classes
                .iter()
                .position(|class| format!("{:?}", class.class) == cursor.class)
        }
    });
    let first_index = resume_index.unwrap_or(0);
    let mut stopped = false;
    for (class_index, class) in classes.iter().enumerate().skip(first_index) {
        let class_name = format!("{:?}", class.class);
        adapters.active_class = class_name.clone();
        adapters.project_start = adapters
            .resume_cursor
            .as_ref()
            .filter(|cursor| cursor.class == class_name)
            .and_then(|cursor| cursor.project.clone());
        adapters.candidate_start = adapters
            .resume_cursor
            .as_ref()
            .filter(|cursor| cursor.class == class_name)
            .and_then(|cursor| cursor.candidate.clone());
        if adapters.stop_requested() {
            let cursor = crate::gc_sweep::Cursor {
                class: class_name.clone(),
                project: adapters.project_start,
                candidate: adapters.candidate_start,
                detail: adapters
                    .resume_cursor
                    .as_ref()
                    .filter(|cursor| cursor.class == class_name)
                    .and_then(|cursor| cursor.detail.clone()),
            };
            if let Some(lease) = &mut adapters.lease {
                lease
                    .checkpoint(cursor.clone())
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            adapters.data.sweep_cursor = Some(cursor);
            stopped = true;
            break;
        }
        if let Some(lease) = &mut adapters.lease {
            lease
                .checkpoint(crate::gc_sweep::Cursor {
                    class: class_name.clone(),
                    project: adapters.project_start.clone(),
                    candidate: adapters.candidate_start.clone(),
                    detail: adapters
                        .resume_cursor
                        .as_ref()
                        .filter(|cursor| cursor.class == class_name)
                        .and_then(|cursor| cursor.detail.clone()),
                })
                .map_err(|error| Error::RetentionCollection(error.to_string()))?;
        }
        adapters.data.classes_run.push(class.class);
        if let Err(error) = class.adapter.collect(&mut adapters) {
            if adapters.stop_requested() {
                let cursor =
                    adapters
                        .stopped_cursor
                        .take()
                        .unwrap_or_else(|| crate::gc_sweep::Cursor {
                            class: class_name.clone(),
                            project: adapters.project_start.clone(),
                            candidate: adapters.candidate_start.clone(),
                            detail: adapters
                                .resume_cursor
                                .as_ref()
                                .filter(|cursor| cursor.class == class_name)
                                .and_then(|cursor| cursor.detail.clone()),
                        });
                if let Some(lease) = &mut adapters.lease {
                    lease
                        .checkpoint(cursor.clone())
                        .map_err(|error| Error::RetentionCollection(error.to_string()))?;
                }
                adapters.data.sweep_cursor = Some(cursor);
                stopped = true;
                break;
            }
            adapters.data.failed_classes.push(class.class);
            adapters
                .data
                .retention_errors
                .push(provenant_config::RetentionIssue {
                    path: roots.state.as_path().to_owned(),
                    operation: format!("collect {:?}", class.class),
                    cause: error.to_string(),
                });
        }
        if let Some(cursor) = adapters.stopped_cursor.take() {
            if let Some(lease) = &mut adapters.lease {
                lease
                    .checkpoint(cursor.clone())
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            adapters.data.sweep_cursor = Some(cursor);
            stopped = true;
            break;
        }
        if adapters.stop_requested() {
            let cursor = classes.get(class_index + 1).map_or_else(
                || crate::gc_sweep::Cursor {
                    class: "complete".into(),
                    project: None,
                    candidate: None,
                    detail: None,
                },
                |next| crate::gc_sweep::Cursor {
                    class: format!("{:?}", next.class),
                    project: None,
                    candidate: None,
                    detail: None,
                },
            );
            if let Some(lease) = &mut adapters.lease {
                lease
                    .checkpoint(cursor.clone())
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            adapters.data.sweep_cursor = Some(cursor);
            stopped = true;
            break;
        }
    }
    adapters.data.sweep_stopped = stopped;
    // Advisory dweller census, deliberately after the collection classes: a
    // large settled store can no longer spend the sweep budget before any
    // candidate is decided. One bounded observation covers every root in the
    // pass and is reused by every project; an undecided root stays incomplete
    // rather than being reported as free.
    let dwellers = DwellerCensus::observe(
        workers,
        &dweller_roots(&roots, deadline.cancel()),
        deadline.cancel(),
    );
    adapters.data.held_processes = dwellers.processes;
    adapters.data.held_incomplete = dwellers.incomplete;
    if !stopped && let Some(lease) = adapters.lease.take() {
        lease
            .complete()
            .map_err(|error| Error::RetentionCollection(error.to_string()))?;
    }
    adapters.data.observe_retention(trigger);
    adapters.data.alerts.extend(
        crate::gc_support::disk_alert(&roots, &project_ids, alert_floor, command.dry_run)
            .unwrap_or_else(|error| vec![format!("disk alert failed: {error}")]),
    );
    if let Some(error) = adapters.cache_error {
        return Err(Error::CacheGc(
            Box::new(error),
            adapters.data.unobserved_projects,
            adapters.data.skipped_transient,
            Box::new(adapters.data.provider_state),
            adapters.data.worktree_cleanup,
            adapters.data.alerts,
        ));
    }
    let mut data = adapters.data;

    // Stale open graphs are advisory: gc always warns and only stops them when
    // the caller explicitly asked to apply changes.
    use provenant_fabric_runtime::graph::{CancellationTarget, FileJournal};
    use provenant_records::diagnostic::Layer;
    let stale_age_seconds = config.gc.stale_graph_age_seconds().unwrap_or(86_400);
    let stale_source = if matches!(
        config.origins.get("gc.stale_graph_age"),
        Some(provenant_config::Origin::User { .. })
    ) {
        Layer::Host
    } else {
        Layer::Default
    };
    let now_micros = i64::try_from(now_ns / 1_000).unwrap_or(i64::MAX);
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let (stale, scan_warnings) = crate::gc_support::stale_graphs(
        &roots,
        &project_ids,
        stale_age_seconds,
        now_micros,
        &clock,
        &cancel,
    );
    data.alerts.extend(scan_warnings);
    for graph in &stale {
        data.alerts.push(format!(
            "graph {} is {} and {} old with {}: run `provenant stop {}` or `provenant gc --apply` to stop it (gc.stale_graph_age={stale_age_seconds}s, source: {})",
            graph.name,
            graph.state,
            stale_age_text(graph.age_seconds),
            graph.evidence,
            graph.name,
            stale_source.description(),
        ));
        if command.apply {
            let budget = WaitBudget::cli(clock.mono_now(), 5).map_err(|_| Error::Budget)?;
            match FileJournal::request_cancellation(
                &roots.state,
                graph.project,
                CancellationTarget {
                    graph_id: &graph.name,
                    node: None,
                    reason: Some("gc: stale graph with landed work"),
                },
                budget,
                &cancel,
                &clock,
            ) {
                Ok(_) => data.alerts.push(format!(
                    "requested stop for stale graph {} ({})",
                    graph.name, graph.project
                )),
                Err(error) => data.alerts.push(format!(
                    "could not stop stale graph {}: {error}",
                    graph.name
                )),
            }
        }
    }
    data.stale_graphs = stale;

    let notice = if data.sweep_stopped {
        Some(
            "sweep reached gc.sweep_budget; the saved retention cursor resumes on the next pass"
                .into(),
        )
    } else if command.dry_run {
        Some("Allocated bytes are measured without following links; APFS clones may share physical blocks. Upstream evidence uses last fetched refs in previews.".into())
    } else {
        (command.pressure
            && data.report.removed.is_empty()
            && data.build_dirs.removed.is_empty()
            && data.provider_state.removed.is_empty()
            && data.provider_state.attempt_tmp.is_empty()
            && data.provider_state.partial_tmp.is_empty()
            && data.provider_state.attempt_logs.is_empty()
            && data.provider_state.expired_events == 0
            && data.provider_state.errors.is_empty()
            && data
                .worktree_cleanup
                .iter()
                .all(|project| project.output.is_empty() && project.idle_output.is_empty()))
        .then(|| "pressure collection: no eligible data in scope".into())
    };
    data.notice = notice;
    Ok(data)
}

struct GcAdapters<'a> {
    command: &'a Gc,
    roots: &'a provenant_core::Roots,
    home: &'a Path,
    assets: &'a Path,
    config: &'a provenant_config::ResolvedConfig<provenant_cli::domains::Domains>,
    workers: &'a dyn WorkerObserver,
    facts: &'a worktrees::RepoFacts,
    projects: Vec<GcProject>,
    project_ids: &'a [provenant_core::ProjectId],
    policy: provenant_confinement::cache::CacheGcPolicy,
    now_ns: i128,
    output_idle_seconds: u64,
    free_floor: u64,
    cache_error: Option<provenant_confinement::cache::CacheGcError>,
    lease: Option<crate::gc_sweep::Lease>,
    deadline: &'a crate::gc_sweep::Deadline,
    resume_cursor: Option<crate::gc_sweep::Cursor>,
    active_class: String,
    project_start: Option<PathBuf>,
    candidate_start: Option<PathBuf>,
    stopped_cursor: Option<crate::gc_sweep::Cursor>,
    data: GcData,
}

impl GcAdapters<'_> {
    fn resume_candidate(&self) -> Option<&Path> {
        self.resume_cursor
            .as_ref()
            .filter(|cursor| cursor.class == self.active_class)
            .and_then(|cursor| cursor.candidate.as_deref())
    }

    fn resume_detail(&self) -> Option<&str> {
        self.resume_cursor
            .as_ref()
            .filter(|cursor| cursor.class == self.active_class)
            .and_then(|cursor| cursor.detail.as_deref())
    }

    fn sweep_control<'a>(
        &'a self,
        start_at: Option<&'a Path>,
        detail: Option<&'a str>,
        keep_going: &'a dyn Fn() -> bool,
    ) -> provenant_core::SweepControl<'a> {
        provenant_core::SweepControl {
            start_at,
            detail,
            cancel: self.deadline.cancel().clone(),
            keep_going: Some(keep_going),
        }
    }

    fn stop_at(
        &mut self,
        project: Option<PathBuf>,
        candidate: Option<PathBuf>,
        detail: Option<String>,
    ) -> Result<(), Error> {
        let cursor = crate::gc_sweep::Cursor {
            class: self.active_class.clone(),
            project,
            candidate,
            detail,
        };
        if let Some(lease) = &mut self.lease {
            lease
                .checkpoint(cursor.clone())
                .map_err(|error| Error::RetentionCollection(error.to_string()))?;
        }
        self.stopped_cursor = Some(cursor);
        Ok(())
    }

    fn stop_requested(&self) -> bool {
        !sweep_should_continue(self.deadline, self.lease.as_ref())
    }

    /// Stop before an adapter does any work; the class then counts as not run.
    fn stop_at_resume(&mut self) -> Result<bool, Error> {
        if !self.stop_requested() {
            return Ok(false);
        }
        self.data.classes_run.pop();
        let cursor = crate::gc_sweep::Cursor {
            class: self.active_class.clone(),
            project: self.project_start.clone(),
            candidate: self.candidate_start.clone(),
            detail: self.resume_detail().map(str::to_owned),
        };
        if let Some(lease) = &mut self.lease {
            lease
                .checkpoint(cursor.clone())
                .map_err(|error| Error::RetentionCollection(error.to_string()))?;
        }
        self.stopped_cursor = Some(cursor);
        Ok(true)
    }
}

fn sweep_should_continue(
    deadline: &crate::gc_sweep::Deadline,
    lease: Option<&crate::gc_sweep::Lease>,
) -> bool {
    let cancel = deadline.cancel();
    let okay = !deadline.expired()
        && !cancel.is_cancelled()
        && lease.is_none_or(|lease| lease.check_fence(cancel));
    if !okay {
        cancel.cancel();
    }
    okay
}

fn preserved_discovery_cursor(
    initial: Option<&crate::gc_sweep::Cursor>,
    fallback: impl FnOnce() -> crate::gc_sweep::Cursor,
) -> crate::gc_sweep::Cursor {
    match initial {
        Some(saved) if saved.class != "project_selection" && saved.class != "project_registry" => {
            saved.clone()
        }
        _ => fallback(),
    }
}

fn sweep_utility_budget(deadline: &crate::gc_sweep::Deadline) -> Result<UtilityBudget, Error> {
    UtilityBudget::bounded(
        deadline
            .remaining()
            .min(Duration::from_secs(5))
            .max(Duration::from_millis(1)),
    )
    .map_err(Into::into)
}

fn append_sweep_warning(target: &mut Option<String>, warning: String) {
    if warning.is_empty() {
        return;
    }
    match target {
        Some(current) => {
            current.push_str("; ");
            current.push_str(&warning);
        }
        None => *target = Some(warning),
    }
}

impl provenant_config::RetentionCollector for GcAdapters<'_> {
    type Error = Error;
    fn lane_cargo_caches(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let home = self.home;

        let start_at = self.resume_candidate().map(Path::to_path_buf);
        let detail = self.resume_detail().map(str::to_owned);
        let result = {
            let policy = &self.policy;
            let projects = &self.projects;
            let deadline = self.deadline;
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let scan = provenant_confinement::cache::CacheGcScan {
                start_at: start_at.as_deref(),
                detail: detail.as_deref(),
                keep_going: Some(&keep_going),
            };

            provenant_confinement::cache::gc_worktree_cargo_caches_with_writer_guard(
                home,
                policy,
                |path| {
                    for project in projects {
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
                },
                &scan,
            )
        };

        let report = match result {
            Ok(report) => report,
            Err(error) => {
                let report = (*error.report).clone();
                self.cache_error = Some(error);
                report
            }
        };
        let interrupted_at = report.interrupted_at.clone();
        let continuation = report.continuation.clone();
        self.data.report = report;
        if let Some(candidate) = interrupted_at {
            self.stop_at(None, Some(candidate), continuation)?;
        }

        Ok(())
    }
    fn git_worktrees(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let command = self.command;
        let roots = self.roots;
        let home = self.home;
        let config = self.config;
        let workers = self.workers;
        let facts = self.facts;
        let output_idle_seconds = self.output_idle_seconds;
        let cancel = self.deadline.cancel().clone();
        let sweep_deadline = self.deadline;
        let project_start = self.project_start.clone();
        let candidate_start = self.resume_candidate().map(Path::to_path_buf);
        let resume_detail = self.resume_detail().map(str::to_owned);
        let project_start_index = project_start.as_ref().map_or(0, |path| {
            self.projects
                .iter()
                .position(|project| project.path.as_path() >= path.as_path())
                .unwrap_or(self.projects.len())
        });

        for project_index in project_start_index..self.projects.len() {
            let project_path = self.projects[project_index].path.clone();
            let candidate_path = (project_start.as_ref() == Some(&project_path))
                .then(|| candidate_start.clone())
                .flatten();
            let project_resume_detail = (project_start.as_ref() == Some(&project_path))
                .then(|| resume_detail.clone())
                .flatten();
            let stopped_detail = RefCell::new(None::<String>);
            if self.stop_requested() || cancel.is_cancelled() {
                self.stop_at(
                    Some(project_path.clone()),
                    candidate_path.clone(),
                    project_resume_detail,
                )?;
                break;
            }
            if let Some(lease) = &mut self.lease {
                lease
                    .checkpoint(crate::gc_sweep::Cursor {
                        class: self.active_class.clone(),
                        project: Some(project_path.clone()),
                        candidate: candidate_path.clone(),
                        detail: project_resume_detail.clone(),
                    })
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            let project = &mut self.projects[project_index];
            let settings = config
                .gc
                .project_settings(&project.binding.project_id().to_string(), &project.path);
            let remaining = self.deadline.remaining();
            let stopped_candidate = RefCell::new(None::<PathBuf>);
            let cleanup = with_gc_context_mode_cancel(
                roots,
                home,
                &project.path,
                if command.dry_run {
                    Duration::from_secs(2)
                } else {
                    MUTATION_TIMEOUT
                }
                .min(remaining.max(Duration::from_millis(1))),
                workers,
                facts,
                command.dry_run,
                &cancel,
                |context, cancel| {
                    // When the fetch fails, there is no origin, or the sweep is a
                    // pressure sweep, upstream is None: the path-based pass below
                    // cannot grant removal authority. Settled lane collection
                    // requires an exact delivered or integration ref for its
                    // removal evidence. Output pruning still has its independent
                    // terminal-state proof.
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
                    // owner-bound proof before the path-based pass below. The
                    // subpass requires an exact integration ref for reportable removal.
                    let mut should_stop =
                        || !sweep_should_continue(sweep_deadline, self.lease.as_ref());
                    let unsettled = if command.apply && !command.dry_run {
                        crate::fabric::gc_cleanup::collect_unsettled(
                            roots,
                            home,
                            &project.path,
                            project.binding.project_id(),
                            upstream.clone(),
                            workers,
                            cancel,
                            project_resume_detail.as_deref(),
                            &mut should_stop,
                        )
                        .unwrap_or_else(|error| {
                            provenant_records::warning::warn(
                                provenant_records::warning::Warning::new(
                                    provenant_records::warning::SETTLED_LANE_COLLECTION_FAILED,
                                    format!(
                                        "settled lane collection failed for {}: {error}",
                                        project.path.display()
                                    ),
                                    provenant_records::warning::detected_source(
                                        provenant_records::warning::SETTLED_LANE_COLLECTION_FAILED,
                                    ),
                                )
                                .subject(project.path.display().to_string()),
                            );
                            crate::fabric::gc_cleanup::UnsettledCollection::default()
                        })
                    } else {
                        crate::fabric::gc_cleanup::UnsettledCollection::default()
                    };
                    if unsettled.interrupted_at.is_some() {
                        *stopped_candidate.borrow_mut() = unsettled.interrupted_at.clone();
                    }
                    if unsettled.continuation.is_some() {
                        *stopped_detail.borrow_mut() = unsettled.continuation.clone();
                    }
                    let mut lanes: Vec<_> = project
                        .lanes
                        .iter()
                        .filter(|lane| !unsettled.removed.contains(&lane.path))
                        .cloned()
                        .collect();
                    lanes.sort_by(|left, right| left.path.cmp(&right.path));
                    let start_candidate = candidate_path
                        .as_ref()
                        .map(|path| {
                            lanes
                                .iter()
                                .position(|lane| lane.path.as_path() >= path.as_path())
                                .unwrap_or(lanes.len())
                        })
                        .unwrap_or(0);
                    // Fetch can take a while, so begin the single sweep snapshot
                    // only after it completes. A stale snapshot is refreshed by
                    // the callbacks below, which collect() invokes under the
                    // worktree writer lock before each mutation.
                    let states = RefCell::new(
                        crate::gc_support::lane_states_with_observation(
                            roots,
                            project.binding.project_id(),
                            command.dry_run,
                            cancel,
                        )
                        .ok(),
                    );
                    let rescan_failed = std::cell::Cell::new(false);
                    let refresh = || {
                        if command.dry_run && states.borrow().is_some() {
                            return;
                        }
                        crate::gc_support::refresh_if_stale(
                            &states,
                            &rescan_failed,
                            |snapshot| snapshot.is_current(),
                            || {
                                crate::gc_support::lane_states_with_observation(
                                    roots,
                                    project.binding.project_id(),
                                    false,
                                    cancel,
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
                    let retention_owner =
                        crate::retention_owner::Owner::new(roots, home, &project.path)
                            .with_dry_run(command.dry_run);
                    let retirement_writer = if command.apply && !command.dry_run {
                        let authority = provenant_cli::fabric::Context::open(
                            roots.clone(),
                            home,
                            &project.path,
                        )
                        .map_err(|_| {
                            Error::GcObservation("cannot establish retirement authority")
                        })?;
                        if authority.identity.project != context.binding.project_id() {
                            return Err(Error::InvalidScope);
                        }
                        Some(crate::custody::RetirementWriter::open(
                            roots,
                            context.binding.project_id(),
                            provenant_cli::fabric::current_chair(&authority)?,
                            context.clock,
                            cancel,
                        )?)
                    } else {
                        None
                    };
                    let retirement_reader = if retirement_writer.is_none() {
                        Some(crate::custody::RetirementReader::open(
                            roots,
                            context.binding.project_id(),
                            context.clock,
                            cancel,
                        )?)
                    } else {
                        None
                    };
                    let retirement_port = retirement_writer.as_ref().map(|writer| {
                        writer as &dyn provenant_confinement::worktrees::RetirementPort
                    });
                    let retirement_lookup = retirement_writer
                        .as_ref()
                        .map(|writer| writer as &dyn crate::lane_cleanup::RetirementLookup)
                        .or_else(|| {
                            retirement_reader
                                .as_ref()
                                .map(|reader| reader as &dyn crate::lane_cleanup::RetirementLookup)
                        })
                        .ok_or(Error::GcObservation("retirement reader unavailable"))?;

                    let mut report = worktrees::collect_from(
                        context,
                        &lanes,
                        &worktrees::WorktreeGcPolicy {
                            upstream: upstream.as_ref().map(|(_, oid)| *oid),
                            mode: provenant_confinement::worktrees::CollectionMode::Worktrees,
                            proof: None,
                            upstream_reference: upstream.as_ref().map(|(name, _)| name.as_str()),
                            terminal: &terminal,
                            no_live_lanes: &no_live_lanes,
                            regenerable: &settings.regenerable,
                            keep: &settings.keep,
                            written_before_ns: now_ns
                                - i128::from(output_idle_seconds) * 1_000_000_000,
                            include_foreign: !command.pressure
                                && settings.foreign_worktrees
                                    == provenant_config::ForeignWorktrees::Collect,
                            retirement_port,
                            retirement_feeds: Some(&retention_owner),
                            integration_patterns: &settings.integration_branches,
                            excluded_paths: &settings.keep_paths,
                            dry_run: command.dry_run || !command.apply,
                        },
                        cancel,
                        start_candidate,
                        &mut should_stop,
                    )?;

                    if let Some(candidate) = report.interrupted_at {
                        *stopped_candidate.borrow_mut() =
                            lanes.get(candidate).map(|lane| lane.path.clone());
                    }

                    if !report.dry_run {
                        let writer = retirement_writer.as_ref().ok_or(Error::GcObservation(
                            "retirement writer unavailable during apply",
                        ))?;
                        let recovered = retention_owner.recover(context, writer, cancel)?;
                        append_gc_subpass_removals(&mut report, recovered);
                    }
                    report.upstream_error = upstream_error;
                    if unsettled.removals.iter().any(|removal| {
                        matches!(
                            removal.outcome,
                            provenant_records::custody::RemovalOutcome::Removed { .. }
                        )
                    }) {
                        report.byte_errors.push(provenant_config::RetentionIssue {
                            path: project.path.clone(),
                            operation: "account recovered settlement".into(),
                            cause: "already-removed settlement bytes are unavailable".into(),
                        });
                    }
                    append_gc_subpass_removals(&mut report, unsettled.removals);
                    // Only proven landed refs with unchanged ledger tips may go.
                    if !command.pressure {
                        let sweep =
                            crate::lane_cleanup::Sweep::new(report.dry_run || !command.apply)
                                .with_retention(&retention_owner, &settings);

                        report.refs = crate::lane_cleanup::collect(
                            context,
                            upstream
                                .as_ref()
                                .and_then(crate::lane_cleanup::fetched_target),
                            &sweep,
                            retirement_lookup,
                            retirement_port,
                            cancel,
                        );
                    }
                    append_gc_removal_records(&mut report);
                    Ok(report)
                },
            );
            match cleanup {
                Ok(cleanup) => self.data.worktree_cleanup.push(cleanup),
                Err(_) => {
                    self.data.unobserved_projects.push(project.path.clone());
                    if self.stop_requested() || cancel.is_cancelled() {
                        self.stop_at(
                            Some(project_path.clone()),
                            candidate_path.clone(),
                            stopped_detail
                                .into_inner()
                                .or(project_resume_detail.clone()),
                        )?;
                        break;
                    }
                }
            }
            let candidate = stopped_candidate.into_inner();
            let detail = stopped_detail.into_inner();
            if candidate.is_some() || detail.is_some() {
                self.stop_at(Some(project_path), candidate, detail)?;
            }
            if self.stopped_cursor.is_some() {
                break;
            }
        }

        Ok(())
    }
    fn discard_archives(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let command = self.command;
        let roots = self.roots;
        let home = self.home;
        let _config = self.config;
        let workers = self.workers;
        let facts = self.facts;
        let now_ns = self.now_ns;
        let project_start = self.project_start.clone();
        let candidate_start = self.resume_candidate().map(Path::to_path_buf);
        let resume_detail = self.resume_detail().map(str::to_owned);
        let cancel = self.deadline.cancel().clone();
        let sweep_deadline = self.deadline;
        let project_start_index = project_start.as_ref().map_or(0, |path| {
            self.projects
                .iter()
                .position(|project| project.path.as_path() >= path.as_path())
                .unwrap_or(self.projects.len())
        });

        for project_index in project_start_index..self.projects.len() {
            let project_path = self.projects[project_index].path.clone();
            let candidate_path = (project_start.as_ref() == Some(&project_path))
                .then(|| candidate_start.clone())
                .flatten();
            let candidate_detail = (project_start.as_ref() == Some(&project_path))
                .then(|| resume_detail.clone())
                .flatten();
            if self.stop_requested() || cancel.is_cancelled() {
                self.stop_at(Some(project_path), candidate_path, None)?;
                break;
            }
            if let Some(lease) = &mut self.lease {
                lease
                    .checkpoint(crate::gc_sweep::Cursor {
                        class: self.active_class.clone(),
                        project: Some(project_path.clone()),
                        candidate: candidate_path.clone(),
                        detail: candidate_detail.clone(),
                    })
                    .map_err(|error| Error::RetentionCollection(error.to_string()))?;
            }
            let project = &self.projects[project_index];
            let remaining = self.deadline.remaining();
            let mut archived = with_gc_context_mode_cancel(
                roots,
                home,
                &project.path,
                MUTATION_TIMEOUT.min(remaining.max(Duration::from_millis(1))),
                workers,
                facts,
                command.dry_run,
                &cancel,
                |context, cancel| {
                    let mut should_stop =
                        || !sweep_should_continue(sweep_deadline, self.lease.as_ref());
                    Ok(worktrees::collect_archives(
                        context,
                        now_ns
                            - i128::from(self.data.discard_archive_retention_seconds)
                                * 1_000_000_000,
                        command.dry_run,
                        cancel,
                        candidate_path.as_deref(),
                        candidate_detail.as_deref(),
                        &mut should_stop,
                    ))
                },
            )
            .unwrap_or_else(|error| worktrees::ArchiveReport {
                errors: vec![provenant_config::RetentionIssue {
                    path: project.path.clone(),
                    operation: "observe archives".into(),
                    cause: error.to_string(),
                }],
                ..Default::default()
            });
            let stopped_candidate = archived.interrupted_at.take();
            let stopped_detail = archived.continuation.take();
            self.data.discard_archives.bytes = self
                .data
                .discard_archives
                .bytes
                .saturating_add(archived.bytes);
            self.data
                .discard_archives
                .removed
                .append(&mut archived.removed);
            self.data
                .discard_archives
                .would_remove
                .append(&mut archived.would_remove);
            self.data.discard_archives.busy.append(&mut archived.busy);
            self.data
                .discard_archives
                .errors
                .append(&mut archived.errors);
            if let Some(candidate) = stopped_candidate {
                self.stop_at(Some(project_path), Some(candidate), stopped_detail)?;
                break;
            }
            if self.stop_requested() || cancel.is_cancelled() {
                self.stop_at(Some(project_path), candidate_path, candidate_detail)?;
                break;
            }
        }

        Ok(())
    }
    fn staging(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let command = self.command;
        let roots = self.roots;
        let now_ns = self.now_ns;
        let cancel = self.deadline.cancel().clone();
        let deadline = self.deadline;
        let lease = self.lease.as_ref();
        let mut should_stop = || !sweep_should_continue(deadline, lease);
        let report = crate::staging_gc::collect(
            roots.state.as_path(),
            now_ns - i128::from(self.data.idle_seconds) * 1_000_000_000,
            command.dry_run,
            &cancel,
            self.resume_candidate(),
            self.resume_detail(),
            &mut should_stop,
        )?;
        self.data.staging_busy = report.busy;
        if let Some(candidate) = report.interrupted_at {
            self.stop_at(None, Some(candidate), report.continuation)?;
        }

        Ok(())
    }
    fn chair_logs(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let command = self.command;
        let roots = self.roots;
        let config = self.config;
        let now_ns = self.now_ns;

        let report = {
            let deadline = self.deadline;
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let control =
                self.sweep_control(self.resume_candidate(), self.resume_detail(), &keep_going);
            crate::chair_log_retention::collect(
                &roots.state,
                provenant_workflow::log::Retention {
                    max_age_seconds: config.retention.chair_log_days * 86_400,
                    replay_horizon_seconds: config.retention.chair_log_replay_seconds(),
                    max_entries: config.retention.chair_log_entries,
                    max_bytes: config.retention.chair_log_bytes,
                },
                i64::try_from(now_ns / 1_000_000_000).map_err(|_| Error::Budget)?,
                command.dry_run,
                &control,
            )
        };
        if let Some(candidate) = report.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), report.continuation.clone())?;
        }
        self.data.chair_logs = report;

        Ok(())
    }
    fn build_directories(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let command = self.command;
        let roots = self.roots;
        let home = self.home;
        let config = self.config;
        let free_floor = self.free_floor;
        let sweep_deadline = self.deadline;
        let resume_detail = self.resume_detail().map(str::to_owned);
        let profile = config.dispatch_profile(&[])?;
        let cache_root = provenant_run::build_dir_gc::shared_cache_root(home);
        let budget = match provenant_config::cargo_cache_budget(&profile, &cache_root) {
            Ok(budget) => budget,
            Err(error) => {
                let message = format!(
                    "Cargo cache budget could not be resolved ({error}); skipping target cache collection"
                );
                self.data.build_dirs.errors.push(message.clone());
                append_sweep_warning(&mut self.data.sweep_warning, message);
                return Ok(());
            }
        };
        let target_idle_seconds = config
            .gc
            .cargo_target_idle_seconds()
            .map_err(|_| Error::GcObservation("invalid Cargo target idle duration"))?;
        let mut protection = provenant_run::build_dir_gc::TargetProtection::default();
        for project in &self.projects {
            let live = project
                .lanes
                .iter()
                .filter(|lane| !lane.occupancy.is_available())
                .collect::<Vec<_>>();
            for lane in &live {
                protection.worktrees.insert(lane.path.clone());
            }
        }
        protection.unobserved = !self.data.unobserved_projects.is_empty();
        let epoch_number = if let Some(lease) = self.lease.as_ref() {
            lease.epoch()
        } else {
            let timestamp = SystemClock
                .wall_now()
                .map_err(|_| Error::Budget)?
                .as_micros() as u64;
            timestamp ^ u64::from(std::process::id()).rotate_left(32)
        };
        let epoch = format!("{epoch_number:016x}");
        let mut warnings = if self
            .lease
            .as_ref()
            .is_some_and(|lease| sweep_should_continue(sweep_deadline, Some(lease)))
        {
            provenant_run::build_dir_gc::prune_stale_survey_snapshots(
                roots.state.as_path(),
                resume_detail.as_deref(),
            )
        } else {
            Vec::new()
        };
        let mut report = {
            let lease = self.lease.as_ref();
            let mut should_stop = || !sweep_should_continue(sweep_deadline, lease);
            let unobserved = protection.unobserved;
            let mut refresh_protection = || {
                let mut should_stop = || !sweep_should_continue(sweep_deadline, lease);
                let mut refreshed = provenant_run::build_dir_gc::target_protection_for_launch_while(
                    roots,
                    None,
                    None,
                    self.workers,
                    &mut should_stop,
                );
                refreshed.unobserved |= unobserved;
                refreshed
            };
            provenant_run::build_dir_gc::collect_with_progress(
                roots.state.as_path(),
                home,
                provenant_run::build_dir_gc::Limits {
                    max_bytes: budget.bytes,
                    free_floor,
                    grace_seconds: config
                        .domains
                        .fabric
                        .lanes
                        .build_settings()
                        .build_dir_grace_seconds(),
                    idle_seconds: self.data.idle_seconds,
                    target_idle_seconds: Some(target_idle_seconds),
                },
                command.dry_run,
                &budget,
                &protection,
                resume_detail.as_deref(),
                &epoch,
                &mut should_stop,
                &mut refresh_protection,
            )
        }
        .map_err(|error| Error::RetentionCollection(error.to_string()))?;
        report.warnings.append(&mut warnings);
        report.target_idle_setting = config.gc.cargo_target_idle.clone();
        report.target_idle_source = config
            .origins
            .get("gc.cargo_target_idle")
            .map(gc_origin_source)
            .unwrap_or_else(|| "compiled default".into());
        if let Some(candidate) = report.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), report.cursor_detail.clone())?;
        } else if self
            .lease
            .as_ref()
            .is_some_and(|lease| sweep_should_continue(sweep_deadline, Some(lease)))
        {
            report
                .warnings
                .extend(provenant_run::build_dir_gc::prune_stale_survey_snapshots(
                    roots.state.as_path(),
                    None,
                ));
        }
        self.data.build_dirs = report;

        Ok(())
    }

    fn provider_state(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let mut report = {
            let deadline = self.deadline;
            let cancel = deadline.cancel();
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let scan = provenant_run::RetentionScan {
                start_at: self
                    .resume_candidate()
                    .and_then(|path| path.file_name()?.to_str()?.parse().ok()),
                detail: self.resume_detail(),
                keep_going: Some(&keep_going),
                preview_budget: Duration::from_secs(self.data.preview_budget_seconds),
            };
            crate::provider_retention::collect_provider_state(
                self.roots,
                self.home,
                self.assets,
                self.config.retention.provider_state_days,
                self.config.retention.logs_days,
                self.command.dry_run,
                cancel,
                &scan,
            )
        };
        if let Some(candidate) = report.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), report.continuation.clone())?;
        }
        // Scratch runs first and shares this report with provider-state GC.
        // Preserve its outcomes and diagnostics when the later pass completes.
        let scratch = &mut self.data.provider_state;
        report.attempt_tmp.append(&mut scratch.attempt_tmp);
        report.partial_tmp.append(&mut scratch.partial_tmp);
        report
            .scratch_deferred
            .append(&mut scratch.scratch_deferred);
        report.kept.append(&mut scratch.kept);
        report.errors.append(&mut scratch.errors);
        self.data.preview_budget_stopped |= report.preview_budget_stopped;
        self.data.provider_state = report;
        Ok(())
    }
    fn attempt_scratch(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let mut report = {
            let deadline = self.deadline;
            let cancel = deadline.cancel();
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let scan = provenant_run::RetentionScan {
                start_at: self
                    .resume_candidate()
                    .and_then(|path| path.file_name()?.to_str()?.parse().ok()),
                detail: self.resume_detail(),
                keep_going: Some(&keep_going),
                preview_budget: Duration::from_secs(self.data.preview_budget_seconds),
            };
            crate::provider_retention::collect_attempt_scratch(
                self.roots,
                self.home,
                self.assets,
                self.data.idle_seconds,
                self.data.attempt_scratch_max_bytes,
                self.data.scratch_grace_seconds,
                (self.command.pressure && !self.command.scheduled).then_some(self.project_ids),
                self.command.dry_run,
                cancel,
                &scan,
            )
        };
        if let Some(candidate) = report.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), report.continuation.clone())?;
        }
        self.data.candidate_progress = Some(candidate_progress(&report));
        self.data
            .provider_state
            .attempt_tmp
            .append(&mut report.attempt_tmp);
        self.data
            .provider_state
            .partial_tmp
            .append(&mut report.partial_tmp);
        self.data
            .provider_state
            .scratch_deferred
            .append(&mut report.scratch_deferred);
        self.data.provider_state.kept.append(&mut report.kept);
        self.data.provider_state.errors.append(&mut report.errors);
        self.data.preview_budget_stopped |= report.preview_budget_stopped;
        Ok(())
    }
    fn payload_cap(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let mut report = {
            let deadline = self.deadline;
            let cancel = deadline.cancel();
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let scan = provenant_run::RetentionScan {
                start_at: self
                    .resume_candidate()
                    .and_then(|path| path.file_name()?.to_str()?.parse().ok()),
                detail: self.resume_detail(),
                keep_going: Some(&keep_going),
                preview_budget: Duration::from_secs(self.data.preview_budget_seconds),
            };
            crate::provider_retention::collect_payload_cap(
                (self.command.pressure && !self.command.scheduled).then_some(self.project_ids),
                self.roots,
                self.home,
                self.assets,
                self.config.retention.payload_cap_bytes,
                !self.command.pressure,
                self.command.dry_run,
                cancel,
                &scan,
            )
        };
        if let Some(candidate) = report.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), report.continuation.clone())?;
        }
        self.data.provider_state.payload = report.payload;
        self.data.provider_state.errors.append(&mut report.errors);
        Ok(())
    }
    fn graph_checkpoints(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let projects = self.project_ids.to_vec();
        let resume_detail = self.resume_detail().map(str::to_owned);
        let resuming_collection = resume_detail.as_deref().is_some_and(|detail| {
            detail.starts_with("graph-collect-v1|") || detail.starts_with("graph-collection-v1|")
        });
        if !resuming_collection {
            let measured = {
                let deadline = self.deadline;
                let lease = self.lease.as_ref();
                let keep_going = || sweep_should_continue(deadline, lease);
                let control = self.sweep_control(
                    self.resume_candidate(),
                    resume_detail.as_deref(),
                    &keep_going,
                );
                crate::checkpoint_retention::measure(
                    &self.roots.state,
                    projects.iter().copied(),
                    &control,
                )
            }
            .map_err(Error::GraphGc)?;
            self.data.graph_bytes = Some(measured.observation);
            if let Some(candidate) = measured.interrupted_at {
                self.stop_at(None, Some(candidate), measured.continuation)?;
                return Ok(());
            }
        }

        let (start_at, detail) = if resuming_collection {
            (self.resume_candidate(), resume_detail.as_deref())
        } else {
            (None, None)
        };
        let collected = {
            let deadline = self.deadline;
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let control = self.sweep_control(start_at, detail, &keep_going);
            crate::checkpoint_retention::collect(
                &self.roots.state,
                projects.iter().copied(),
                self.config.gc.graph_checkpoint_days,
                self.command.dry_run,
                &SystemClock,
                &control,
            )
        }
        .map_err(Error::GraphGc)?;
        if let Some(candidate) = collected.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), collected.continuation.clone())?;
        }
        self.data.graph_checkpoints_removed = collected.removed;
        Ok(())
    }
    fn port_receipts(&mut self) -> Result<(), Error> {
        if self.stop_at_resume()? {
            return Ok(());
        }
        let report = {
            let deadline = self.deadline;
            let lease = self.lease.as_ref();
            let keep_going = || sweep_should_continue(deadline, lease);
            let control =
                self.sweep_control(self.resume_candidate(), self.resume_detail(), &keep_going);
            crate::port_receipt_retention::collect(
                self.roots,
                self.command.dry_run,
                &SystemClock,
                &control,
            )
        };
        if let Some(candidate) = report.interrupted_at.clone() {
            self.stop_at(None, Some(candidate), report.continuation.clone())?;
        }
        self.data.port_receipts = report;
        Ok(())
    }
    fn observation_ledgers(&mut self) -> Result<(), Error> {
        Err(Error::GcObservation(
            "retention class belongs to its declared append/install/permanent boundary",
        ))
    }
    fn retirement_records(&mut self) -> Result<(), Error> {
        Err(Error::GcObservation(
            "retention class belongs to its declared append/install/permanent boundary",
        ))
    }
    fn ref_generation_sequences(&mut self) -> Result<(), Error> {
        Err(Error::GcObservation(
            "retention class belongs to its declared append/install/permanent boundary",
        ))
    }
    fn quota_history(&mut self) -> Result<(), Error> {
        Err(Error::GcObservation(
            "retention class belongs to its declared append/install/permanent boundary",
        ))
    }
    fn usage(&mut self) -> Result<(), Error> {
        Err(Error::GcObservation(
            "retention class belongs to its declared append/install/permanent boundary",
        ))
    }
    fn managed_releases(&mut self) -> Result<(), Error> {
        Err(Error::GcObservation(
            "retention class belongs to its declared append/install/permanent boundary",
        ))
    }
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
    let cancel = CancelToken::new();
    with_context_mode_cancel(
        roots, home, project, mutation, workers, facts, read_only, &cancel, action,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn with_context_mode_cancel<T>(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    mutation: Duration,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    read_only: bool,
    cancel: &CancelToken,
    action: impl FnOnce(&WorktreeContext<'_>, &CancelToken) -> Result<T, Error>,
) -> Result<T, Error> {
    with_context_mode_with_owner_cancel(
        roots, home, project, mutation, workers, facts, read_only, false, None, cancel, action,
    )
}

#[allow(clippy::too_many_arguments)]
fn with_gc_context_mode_cancel<T>(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    mutation: Duration,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    read_only: bool,
    cancel: &CancelToken,
    action: impl FnOnce(&WorktreeContext<'_>, &CancelToken) -> Result<T, Error>,
) -> Result<T, Error> {
    with_context_mode_with_owner_cancel(
        roots, home, project, mutation, workers, facts, read_only, true, None, cancel, action,
    )
}

#[allow(clippy::too_many_arguments)]
fn with_context_mode_with_owner_cancel<T>(
    roots: &provenant_core::Roots,
    home: &Path,
    project: &Path,
    mutation: Duration,
    workers: &dyn WorkerObserver,
    facts: &worktrees::RepoFacts,
    read_only: bool,
    allow_bare: bool,
    owner: Option<(provenant_config::ProjectBinding, worktrees::ProjectLayout)>,
    cancel: &CancelToken,
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
    let runtime = UtilityRuntime::new(
        git,
        roots.state.as_path().join("utility-home"),
        roots.state.as_path(),
    )?
    .with_lane_roots(provenant_confinement::utility::lane_state_roots(
        roots.state.as_path(),
    ));
    let executor = SystemExecutor::new(1)?;
    let budget = UtilityBudget::bounded(
        mutation
            .min(Duration::from_secs(5))
            .max(Duration::from_millis(1)),
    )?;
    let repo = if allow_bare {
        open_gc_repo(roots, &runtime, &executor, project, budget, cancel)?
    } else {
        runtime.open_repo(&executor, project, budget, cancel)?
    };
    let (binding, layout) = if let Some((binding, layout)) = owner {
        let common = Directory::open_anchor(binding.common_dir())?;
        if binding.common_dir() != repo.common_git_dir()
            || !common.metadata()?.same_identity(&repo.common_identity())
        {
            return Err(worktrees::WorktreeError::IdentityChanged.into());
        }
        (binding, layout)
    } else {
        let bare = facts.bare_setting(&repo, &executor, &runtime, budget, cancel)?;
        let location = if bare != Some(true) && repo.private_git_dir() == repo.common_git_dir() {
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                repo.common_git_dir(),
                repo.root_path(),
            )?
        } else {
            provenant_config::ProjectLocation::new(repo.common_git_dir())?
        };
        let binding = if read_only || bare == Some(true) {
            provenant_config::lookup_project(roots, &location)?.ok_or(Error::GcObservation(
                "GC preview requires an existing project binding; run `provenant status` from the project checkout once to bind it",
            ))?
        } else {
            provenant_config::bind_project(roots, location)?
        };
        let layout = runtime.project_layout_for_binding(&executor, &binding, budget, cancel)?;
        (binding, layout)
    };
    let worktrees_state = if read_only {
        state.open_verified_directory(OsStr::new("worktrees"))?
    } else {
        child(&state, "worktrees")?
    };
    let name = binding.project_id().to_string();
    let project_state = if read_only {
        worktrees_state.open_verified_directory(OsStr::new(&name))?
    } else {
        child(&worktrees_state, &name)?
    };
    let clock = SystemClock;
    let policy_workers = worktrees::CensusPolicyWorkers::from_roots(workers, roots)?;
    let workers = worktrees::CensusSession::new(&policy_workers, &clock);
    let context = WorktreeContext {
        home,
        repo: &repo,
        layout: &layout,
        binding: &binding,
        project_state: &project_state,
        executor: &executor,
        utility: &runtime,
        facts,
        workers: &workers,
        clock: &clock,
        utility_budget: budget,
        mutation_budget: UtilityBudget::bounded(mutation)?,
        lock_budget: WaitBudget::up_to(
            clock.mono_now(),
            DurationMicros::from_micros(
                mutation
                    .as_micros()
                    .min(5_000_000)
                    .try_into()
                    .unwrap_or(5_000_000),
            ),
        ),
    };
    let result = action(&context, cancel);
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

    #[test]
    fn local_config_read_refusal_keeps_context_and_leaf_cause() {
        let error = WorktreeError::GitConfigRead(Box::new(provenant_os::git_config::ConfigError {
            context: provenant_os::git_config::ConfigContext::new(
                "read local core.bare",
                "/project/.git/config",
                Some("core.bare"),
            ),
            source: provenant_confinement::utility::UtilityError::TimedOut,
        }));
        let message = local_worktree_message(&error);
        assert!(
            message.contains("read local core.bare on /project/.git/config for key core.bare"),
            "{message}"
        );
        assert!(message.contains("utility command timed out"), "{message}");
        assert_eq!(
            Error::Worktree(error).local_message(),
            message,
            "the CLI and worktree wrappers must render the same bounded cause"
        );
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
            lane: None,
            attempt: None,
            base: None,
            reused: false,
        })
    }

    // Spec: docs/specs/supervisor.md#shared-occupancy-removal-never-forces
    #[test]
    fn removal_record_persists_schema_branch_tip_path_and_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("lane");
        let outcome = worktrees::RemoveOutcome {
            removed: vec![info(path.to_str().expect("utf8 path")).expect("worktree info")],
            ..Default::default()
        };
        let records = record_removals(temp.path(), &outcome, &SystemClock::new()).expect("record");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schema, "provenant.worktree-removed.v1");
        let stored = std::fs::read_to_string(temp.path().join("worktrees/removed.jsonl"))
            .expect("removed.jsonl");
        let row: serde_json::Value = serde_json::from_str(stored.trim()).expect("record row");
        assert_eq!(row["schema"], "provenant.worktree-removed.v1");
        assert_eq!(row["branch"], "refs/heads/master");
        assert_eq!(
            row["tip"],
            GitOid::parse(&"4a".repeat(20)).unwrap().to_string()
        );
        assert!(row["path"].is_object());
        assert!(row["at"].is_string());
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
mod gc_preview_tests;

#[cfg(test)]
mod gc_subpass_removal_tests {
    #[allow(unused_imports)] // Also compiled by the harness=false worktree driver.
    use super::*;
    #[allow(unused_imports)]
    use provenant_records::custody::{RefEndpoint, RefOrigin, RefOwnership};

    #[test]
    // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    fn a_forge_fetch_refusal_keeps_its_cause_and_repair_in_the_warning() {
        let error = Error::ForgeContextUnresolved(provenant_config::ForgeContextError::NoRemote {
            remotes: vec!["alpha".into(), "beta".into()],
            close: vec!["alpha".into(), "beta".into()],
        });
        let (_, warnings) = provenant_records::capture(|| warn_fetch(&error));
        let warning = warnings
            .iter()
            .find(|warning| warning.code.as_ref() == "FORGE_CONTEXT_UNRESOLVED")
            .expect("a typed forge refusal warning");
        assert!(warning.message.contains("alpha"), "{}", warning.message);
        assert!(warning.message.contains("beta"), "{}", warning.message);
        let next = warning.next.first().expect("next").display_text();
        assert!(next.contains("git.remote"), "{next}");
    }

    #[test]
    fn actual_gc_lifts_unsettled_branch_removal_and_preview_keeps_none() {
        let project = provenant_core::ProjectId::from_bytes([8; 16]);
        let tip = GitOid::parse(&"a1".repeat(20)).unwrap();
        let evidence = LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::IntegrationTarget {
                reference: "refs/remotes/origin/main".into(),
                commit: GitOid::parse(&"b2".repeat(20)).unwrap(),
            },
        };
        let reference = RefOwnership {
            repository: project,
            endpoint: RefEndpoint::Local,
            reference: "refs/heads/topic".into(),
            generation: 1,
            origin: RefOrigin::Created,
        };
        let record = RemovalRecord {
            project,
            subject: reference.custody_subject().unwrap(),
            tip,
            outcome: provenant_records::custody::RemovalOutcome::Removed {
                evidence: provenant_records::custody::RemovalEvidence::Landing { evidence },
                retirement_id: "retirement-1".into(),
            },
        };

        let mut applied = worktrees::WorktreeGcReport::default();
        append_gc_subpass_removals(&mut applied, vec![record.clone()]);
        assert_eq!(applied.removals, vec![record.clone()]);
        let mut data = GcData::default();
        data.worktree_cleanup.push(applied);
        let output = serde_json::to_value(gc_result_data(&data, false)).unwrap();
        assert_eq!(
            output["removal_records"][0],
            serde_json::to_value(&record).unwrap()
        );

        let mut preview = worktrees::WorktreeGcReport {
            dry_run: true,
            ..Default::default()
        };
        append_gc_subpass_removals(&mut preview, vec![record]);
        assert!(preview.removals.is_empty());
    }

    #[test]
    fn retention_scratch_source_is_lazy_and_skips_an_unresolved_forge() {
        let self_describing = LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::IntegrationTarget {
                reference: "refs/remotes/origin/main".into(),
                commit: GitOid::parse(&"b2".repeat(20)).unwrap(),
            },
        };
        let source = retention_scratch_source(&self_describing, || {
            panic!("self-describing evidence must not resolve the forge")
        });
        assert_eq!(source.as_deref(), Some("origin/main"));

        let fallback = LandedEvidence {
            predicate: LandingPredicate::Landed,
            source: LandingSource::ForgeMerged {
                pr: "42".into(),
                head: GitOid::parse(&"a1".repeat(20)).unwrap(),
                merge_commit: None,
            },
        };
        let calls = std::cell::Cell::new(0);
        let source = retention_scratch_source(&fallback, || {
            calls.set(calls.get() + 1);
            Some("origin/trunk".into())
        });
        assert_eq!(source.as_deref(), Some("origin/trunk"));
        assert_eq!(calls.get(), 1);

        let unresolved = retention_scratch_source(&fallback, || None);
        assert_eq!(unresolved, None);
    }
}
