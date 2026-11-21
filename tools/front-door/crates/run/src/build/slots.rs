//! Persistent slot records and isolated build execution.

use super::process::Command;
use super::{
    BuildContext, BuildError, Captured, Exception, ExecPlace, Job, JobOutcome, JobResult,
    RunContext, SlotChanges,
};
use provenant_core::{Clock, MonoTime};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode};
use provenant_supervisor::census::{
    MAX_CENSUS_ENTRIES, ProcessIdentity, ProcessObservation, ProcessObserver, SystemProcessObserver,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Persisted slot record, schema 1.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRecord {
    pub schema: u32,
    pub project_key: String,
    pub index: u64,
    pub created_unix_ms: u64,
    pub last_job: Option<SlotJobMark>,
    pub prepare_digest: Option<String>,
    pub health: SlotHealth,
    pub last_canary_unix_ms: Option<u64>,
}

/// Last completed job associated with a slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotJobMark {
    pub snapshot: String,
    pub base: Option<String>,
    pub owner: String,
    pub finished_unix_ms: u64,
}

/// Slot health and quarantine history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotHealth {
    pub infra_failures_unix_ms: Vec<u64>,
    pub state: SlotState,
    pub quarantine_reason: Option<String>,
}

/// Slot health state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    #[default]
    Ok,
    Quarantined,
}

/// Paths inside one project's slot cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotPaths {
    pub root: PathBuf,
    pub repo: PathBuf,
    pub target: PathBuf,
    pub tmp: PathBuf,
    pub record: PathBuf,
    pub job: PathBuf,
    pub lock: PathBuf,
}

impl SlotPaths {
    /// Resolve all files belonging to one cache slot.
    pub fn new(home: &std::path::Path, project_key: &str, index: u64) -> Self {
        let root = super::paths::slot_root(home, project_key, index);
        Self {
            repo: root.join("repo"),
            target: root.join("target"),
            tmp: root.join("tmp"),
            record: root.join("slot.json"),
            job: root.join("job.json"),
            lock: root.join("slot.lock"),
            root,
        }
    }
}

impl SlotRecord {
    /// Read a slot record if one exists.
    pub fn read(path: &std::path::Path) -> std::io::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(std::io::Error::other),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Replace the record atomically in its own directory.
    pub fn write(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("slot record has no parent"))?;
        let tmp = parent.join(format!(
            ".slot-{}-{}-{}.tmp",
            std::process::id(),
            super::ledger::now_unix_ms(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp)?;
            serde_json::to_writer(&mut file, self).map_err(std::io::Error::other)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

/// Intent is published before spawn; Running replaces it with the child's
/// process identity. The default keeps older markers decodable as Running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JobMarkerState {
    #[default]
    Running,
    Intent,
}

#[derive(Debug, Serialize, Deserialize)]
struct JobMarker {
    pid: u32,
    pgid: i32,
    start_time: u64,
    #[serde(default)]
    boot: String,
    #[serde(default)]
    session_id: i64,
    owner: String,
    #[serde(default)]
    state: JobMarkerState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotExecutionFailure {
    pub kind: Option<super::InfraKind>,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlotExecution {
    pub snapshot: Option<String>,
    pub prev_snapshot: Option<String>,
    pub slot_changes: Option<SlotChanges>,
    pub exit: Option<super::ExitStatus>,
    pub step_exits: Vec<Option<super::ExitStatus>>,
    pub captured: Option<Captured>,
    pub outcome: Option<JobOutcome>,
    pub failure: Option<SlotExecutionFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageFailure {
    pub exception: Exception,
    pub infra: Option<super::InfraKind>,
    pub reset: super::Reset,
    pub detail: String,
}

impl From<Exception> for StageFailure {
    fn from(exception: Exception) -> Self {
        Self {
            exception,
            infra: None,
            reset: super::Reset::None,
            detail: format!("{exception:?}"),
        }
    }
}

/// Staged materialisation held across the job prelude and execution.
pub struct PreparedSlot {
    pub site: super::ExecSite,
    paths: SlotPaths,
    /// Slot lock plus the lease inherited by the build's children, held from
    /// staging through the post-job reset and record write. Every destructive
    /// slot operation borrows this guard.
    custody: SlotCustody,
    _port_lease: Option<provenant_os::lane_ports::Lease>,
    record: SlotRecord,
    snapshot: super::snapshot::SnapshotRef,
    source_root: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    started_ms: u64,
    started_at: SystemTime,
    warnings: Vec<String>,
    clock_reset: bool,
}
impl PreparedSlot {
    pub fn add_env(&mut self, values: Vec<(std::ffi::OsString, std::ffi::OsString)>) {
        for (key, value) in values {
            super::set_env(&mut self.env, key, value);
        }
    }
}

/// Prepare slot contents and retain both slot and port leases through execution.
pub fn stage(job: &Job, context: &RunContext, index: u64) -> Result<PreparedSlot, StageFailure> {
    if let super::rust::RustClass::Local { exception } = super::rust::classify(job) {
        return Err(exception.into());
    }
    let source_root = super::job_root(job).to_path_buf();
    let store = super::store::ensure(&context.state, &job.project_key).map_err(map_error)?;
    let store_lock = super::store::lock(
        &super::paths::store_lock(&context.state, &job.project_key),
        false,
    )
    .map_err(map_error)?;
    let paths = SlotPaths::new(&context.home, &job.project_key, index);
    fs::create_dir_all(&paths.root).map_err(|_| Exception::PoolUnavailable)?;
    fs::create_dir_all(&paths.tmp).map_err(|_| Exception::PoolUnavailable)?;
    let slot_lock = slot_lock(&paths.lock).map_err(|_| Exception::PoolUnavailable)?;
    let intent_lease = acquire_intent_lease(&paths.root).map_err(|detail| StageFailure {
        exception: Exception::PoolUnavailable,
        infra: None,
        reset: super::Reset::None,
        detail,
    })?;
    // Staging adopts the guards it just took. Unlike collection it must first
    // reap an orphaned build group, so the live-child probe is not repeated
    // here; the guards are held through every reset below.
    let custody = SlotCustody::from_guards(slot_lock, intent_lease);
    let orphan_killed =
        reap_orphan(&paths.job, &custody).map_err(|_| Exception::PoolUnavailable)?;
    let mut record = SlotRecord::read(&paths.record)
        .map_err(|_| Exception::PoolUnavailable)?
        .unwrap_or_else(|| SlotRecord {
            schema: 1,
            project_key: job.project_key.clone(),
            index,
            created_unix_ms: super::ledger::now_unix_ms(),
            ..SlotRecord::default()
        });
    ensure_slot_repo(&paths, &store)?;
    let mut clock_reset = false;
    match super::hygiene::before_job(&mut record, super::ledger::now_unix_ms()) {
        super::hygiene::PreJob::Proceed => {}
        super::hygiene::PreJob::LightReset => {
            super::hygiene::light_reset(&paths, &custody).map_err(|_| Exception::PoolUnavailable)?
        }
        super::hygiene::PreJob::FullReset => {
            clock_reset = true;
            super::hygiene::full_reset(&paths, &custody).map_err(|_| Exception::PoolUnavailable)?;
            record.prepare_digest = None;
            ensure_slot_repo(&paths, &store)?;
        }
    }
    let binding = match &job.source {
        super::SnapshotSource::WorkingTree { git, .. } => Some(git),
        super::SnapshotSource::Commit { .. } => None,
    };
    let needs_base_pin = matches!(&job.source, super::SnapshotSource::WorkingTree { .. })
        || job.verify_base.is_some();
    let snapshot = if needs_base_pin {
        super::snapshot::create_in_slot(
            &job.source,
            binding,
            &store,
            &paths.tmp,
            &job.owner,
            job.verify_base.as_ref().map(|base| base.commit.as_str()),
        )
        .map_err(|_| Exception::NoSnapshot)?
    } else {
        super::snapshot::create(&job.source, None, &store, &job.owner)
            .map_err(|_| Exception::NoSnapshot)?
    };
    let clean = context.strategy.port.clean_policy();
    let mut warnings = clean.warnings;
    if orphan_killed {
        warnings.push("terminated an orphaned build process group from job.json".into());
    }
    if let Err(detail) = sync_snapshot(
        &paths,
        &store,
        &snapshot,
        job.verify_base.as_ref(),
        clean.value,
    ) {
        let now = super::ledger::now_unix_ms();
        let mut failed_sync = super::outcome_seed(job, context, now);
        failed_sync.infra = Some(super::InfraKind::Git);
        failed_sync.warnings.push(detail.clone());
        let reset = match super::hygiene::after_job(&mut record, &failed_sync, now) {
            super::Reset::None => super::Reset::Light,
            reset => reset,
        };
        failed_sync.reset = reset;
        match reset {
            super::Reset::Quarantine => {
                record.health.state = SlotState::Quarantined;
                record.health.quarantine_reason = Some(detail);
                if let Err(error) = record.write(&paths.record) {
                    return Err(StageFailure {
                        exception: Exception::PoolUnavailable,
                        infra: Some(super::InfraKind::Git),
                        reset,
                        detail: format!(
                            "slot sync failed and slot record could not be saved: {error}"
                        ),
                    });
                }
                return Err(StageFailure {
                    exception: Exception::PoolUnavailable,
                    infra: Some(super::InfraKind::Git),
                    reset,
                    detail: failed_sync
                        .warnings
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "slot Git sync failed".into()),
                });
            }
            super::Reset::Light | super::Reset::None => {
                super::hygiene::light_reset(&paths, &custody)
                    .map_err(|error| stage_reset_failure(error, reset))?;
            }
            super::Reset::Full => {
                super::hygiene::full_reset(&paths, &custody)
                    .map_err(|error| stage_reset_failure(error, reset))?;
                record.prepare_digest = None;
                ensure_slot_repo(&paths, &store)?;
            }
        }
        if let Err(error) = record.write(&paths.record) {
            return Err(StageFailure {
                exception: Exception::PoolUnavailable,
                infra: Some(super::InfraKind::Git),
                reset,
                detail: format!("{detail}; slot record could not be saved before retry: {error}"),
            });
        }
        warnings.push(format!(
            "slot Git sync failed ({detail}); retrying once after {reset:?} reset"
        ));
        if let Err(retry_detail) = sync_snapshot(
            &paths,
            &store,
            &snapshot,
            job.verify_base.as_ref(),
            clean.value,
        ) {
            let record_error = record.write(&paths.record).err();
            return Err(StageFailure {
                exception: Exception::PoolUnavailable,
                infra: Some(super::InfraKind::Git),
                reset,
                detail: match record_error {
                    Some(error) => format!(
                        "{detail}; retry failed: {retry_detail}; slot record save failed: {error}"
                    ),
                    None => format!("{detail}; retry failed: {retry_detail}"),
                },
            });
        }
    }
    drop(store_lock);
    let prepare = context.strategy.port.prepare.clone();
    let digest = digest_prepare(&prepare);
    if record.prepare_digest.as_deref() != Some(&digest) {
        let build_context = build_context(job, context);
        let env = super::rust::job_env(&job.env, &build_context, &paths);
        for argv in &prepare {
            if argv.is_empty() {
                continue;
            }
            let status = Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(&paths.repo)
                .env_clear()
                .envs(env.iter().cloned())
                .output();
            if !status.is_ok_and(|output| output.status.success()) {
                return Err(Exception::PoolUnavailable.into());
            }
        }
        record.prepare_digest = Some(digest);
    }
    fs::create_dir_all(&paths.target).map_err(|_| Exception::PoolUnavailable)?;
    let clock = provenant_os::clock::SystemClock::new();
    let started_at = clock
        .wall_now()
        .ok()
        .and_then(|now| u64::try_from(now.as_micros()).ok())
        .map(|micros| UNIX_EPOCH + Duration::from_micros(micros))
        .unwrap_or(UNIX_EPOCH);
    let started_ms = started_at
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let _before = fingerprint_scan(&paths.target, started_at);
    let build_context = build_context(job, context);
    let mut env = super::rust::job_env(&job.env, &build_context, &paths);
    let port_lease = match lane_port_plan(&context.state).and_then(|plan| match plan {
        Some(plan) => plan.try_acquire().map_err(std::io::Error::other),
        None => Ok(None),
    }) {
        Ok(Some(lease)) => {
            for (name, value) in lease.environment() {
                super::set_env(&mut env, name, value);
            }
            Some(lease)
        }
        Ok(None) => {
            warnings.push("no lane-port block was free for the build job".into());
            None
        }
        Err(error) => {
            warnings.push(format!("lane-port block unavailable: {error}"));
            None
        }
    };
    let _ = record.write(&paths.record);
    for warning in &warnings {
        let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
    }
    Ok(PreparedSlot {
        site: super::ExecSite {
            root: paths.repo.clone(),
            head: Some(snapshot.commit.clone()),
            slot: Some(index),
        },
        paths,
        custody,
        _port_lease: port_lease,
        record,
        snapshot,
        source_root,
        env,
        started_ms,
        started_at,
        warnings,
        clock_reset,
    })
}

/// The only slot-side effects a maintenance canary may apply. A canary that
/// ran locally has no `Slot` placement, so it has no slot to reset or stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanarySlotEffect {
    None,
    Stamp(u64),
    Reset,
}

fn canary_slot_effect(
    canary: bool,
    placement: super::ExecPlace,
    outcome: &JobOutcome,
    now_ms: u64,
) -> CanarySlotEffect {
    if !canary || placement != super::ExecPlace::Slot {
        return CanarySlotEffect::None;
    }
    if super::freshness_failed(outcome) {
        CanarySlotEffect::Reset
    } else {
        CanarySlotEffect::Stamp(now_ms)
    }
}

/// Execute an already staged job after the caller has run its optional prelude.
pub fn execute_staged(
    job: &Job,
    context: &RunContext,
    index: u64,
    mut staged: PreparedSlot,
) -> Result<SlotExecution, Exception> {
    let mut owned_job = job_without_prelude(job);
    let mut outcome = super::outcome_seed(&owned_job, context, super::ledger::now_unix_ms());
    outcome.warnings.append(&mut staged.warnings);
    if staged.clock_reset {
        outcome.infra = Some(super::InfraKind::Clock);
        outcome.reset = super::Reset::Full;
        outcome
            .warnings
            .push("clock moved backwards; slot fully reset".into());
    }
    outcome.slot = Some(index);
    outcome.snapshot = Some(staged.snapshot.commit.clone());
    outcome.prev_snapshot = staged
        .record
        .last_job
        .as_ref()
        .map(|job| job.snapshot.clone());
    outcome.base = staged.snapshot.base.clone().or(outcome.base);
    outcome.exec = ExecPlace::Slot;
    outcome.started_unix_ms = staged.started_ms;
    let marker_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let callback_error = std::sync::Arc::clone(&marker_error);
    let process_identity = std::sync::Arc::new(std::sync::Mutex::new(
        None::<provenant_supervisor::census::ProcessIdentity>,
    ));
    let callback_identity = std::sync::Arc::clone(&process_identity);
    let path = staged.paths.job.clone();
    let owner = owned_job.owner.clone();
    let mut on_spawn = move |pid| match mark_job(&path, pid, &owner) {
        Ok(identity) => {
            if let Ok(mut slot) = callback_identity.lock() {
                *slot = Some(identity);
            }
        }
        Err(error) => {
            let termination_error = terminate_untracked_group(pid).err();
            if let Ok(mut slot) = callback_error.lock() {
                *slot = Some(match termination_error {
                    Some(termination) => {
                        format!("{error}; spawned group cleanup failed: {termination}")
                    }
                    None => error.to_string(),
                });
            }
        }
    };
    // Custody is published before the child exists. The inherited lease below
    // keeps the slot occupied even if this process dies before `on_spawn`.
    mark_intent(&staged.paths.job, &owned_job.owner).map_err(|error| {
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: cannot publish slot custody before spawn: {error}"
        );
        Exception::PoolUnavailable
    })?;
    let peak_rss = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sampler = start_rss_sampler(
        std::sync::Arc::clone(&process_identity),
        std::sync::Arc::clone(&peak_rss),
        &owned_job.owner,
    );
    let result = super::local::execute_in_slot(
        &mut owned_job,
        context,
        outcome,
        super::local::SlotLaunch {
            index,
            root: &staged.paths.repo,
            head: &staged.snapshot.commit,
            env: staged.env.clone(),
            worktree: &staged.source_root,
            on_spawn: &mut on_spawn,
            intent_lease: Some(staged.custody.intent_lease()),
        },
    );
    drop(on_spawn);
    if let Some((stop, worker)) = sampler {
        let _ = stop.send(());
        let _ = worker.join();
    }
    let marker_error = marker_error.lock().ok().and_then(|mut error| error.take());
    let (mut outcome, step_exits, captured, mut failure) = match result {
        Ok(result) => (result.outcome, result.step_exits, result.captured, None),
        Err((error, outcome)) => {
            let kind = match &error {
                BuildError::Infra { kind, .. } => Some(*kind),
                BuildError::Cancelled => None,
                _ => Some(super::InfraKind::SlotLayout),
            };
            (
                *outcome,
                Vec::new(),
                None,
                Some(SlotExecutionFailure {
                    kind,
                    detail: error.to_string(),
                }),
            )
        }
    };
    if let Some(error) = marker_error {
        outcome.infra = Some(super::InfraKind::SlotLayout);
        outcome.warnings.push(format!(
            "child was terminated because job.json could not be persisted: {error}"
        ));
        failure.get_or_insert_with(|| SlotExecutionFailure {
            kind: Some(super::InfraKind::SlotLayout),
            detail: format!("job.json persistence failed after spawn: {error}"),
        });
    }
    match reap_orphan(&staged.paths.job, &staged.custody) {
        Ok(true) => {
            outcome.infra = Some(super::InfraKind::SlotLayout);
            outcome
                .warnings
                .push("terminated surviving processes from the completed build group".into());
            failure.get_or_insert_with(|| SlotExecutionFailure {
                kind: Some(super::InfraKind::SlotLayout),
                detail: "build process group survived command completion".into(),
            });
        }
        Ok(false) => {}
        Err(error) => {
            outcome.infra = Some(super::InfraKind::SlotLayout);
            outcome.warnings.push(format!(
                "could not confirm build process group cleanup; retained job.json: {error}"
            ));
            failure.get_or_insert_with(|| SlotExecutionFailure {
                kind: Some(super::InfraKind::SlotLayout),
                detail: format!("could not confirm build process group cleanup: {error}"),
            });
        }
    }
    let after = fingerprint_scan(&staged.paths.target, staged.started_at);
    let rebuilt_units = after.iter().filter(|(_, modified)| *modified).count();
    let mut rebuilt_packages: Vec<_> = after
        .iter()
        .filter(|(_, modified)| *modified)
        .map(|(package, _)| package.clone())
        .collect();
    rebuilt_packages.sort();
    rebuilt_packages.dedup();
    outcome.units_rebuilt = Some(rebuilt_units as u64);
    outcome.units_fresh = Some(after.len().saturating_sub(rebuilt_units) as u64);
    outcome.rebuilt_packages = rebuilt_packages.into_iter().take(256).collect();
    outcome.peak_rss_bytes = (peak_rss.load(std::sync::atomic::Ordering::Relaxed) > 0)
        .then(|| peak_rss.load(std::sync::atomic::Ordering::Relaxed));
    let changes = slot_changes(&staged.paths.repo);
    if changes.as_ref().is_some_and(|changes| changes.count > 0) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "build slot changes were not copied back"
        );
    }
    outcome.slot_changes = changes.clone();
    outcome.finished_unix_ms = super::ledger::now_unix_ms();
    staged.record.last_job = Some(SlotJobMark {
        snapshot: staged.snapshot.commit.clone(),
        base: staged.snapshot.base.clone(),
        owner: owned_job.owner.clone(),
        finished_unix_ms: outcome.finished_unix_ms,
    });
    let finished = outcome.finished_unix_ms;
    let mut post_reset = super::hygiene::after_job(&mut staged.record, &outcome, finished);
    // Judge the canary here, while this function still holds the exact slot's
    // lock and custody lease. A local fallback never reaches this point, so no
    // slot the canary did not exercise is ever reset or stamped fresh.
    let canary = matches!(owned_job.kind, super::JobKind::Maintenance { canary: true });
    match canary_slot_effect(canary, super::ExecPlace::Slot, &outcome, finished) {
        CanarySlotEffect::None => {}
        CanarySlotEffect::Reset => post_reset = super::Reset::Full,
        CanarySlotEffect::Stamp(at) => {
            super::hygiene::mark_fresh(&mut staged.record, at, &staged.custody)
        }
    }
    if post_reset != super::Reset::None {
        outcome.reset = post_reset;
    }
    let reset_result = match post_reset {
        super::Reset::Light => super::hygiene::light_reset(&staged.paths, &staged.custody),
        super::Reset::Full => {
            staged.record.prepare_digest = None;
            super::hygiene::full_reset(&staged.paths, &staged.custody)
        }
        _ => Ok(()),
    };
    if let Err(error) = reset_result {
        staged.record.health.state = SlotState::Quarantined;
        staged.record.health.quarantine_reason = Some(format!("reset failed: {error}"));
        outcome.reset = super::Reset::Quarantine;
        outcome.warnings.push(format!("slot reset failed: {error}"));
    }
    if let Err(error) = staged.record.write(&staged.paths.record) {
        outcome
            .warnings
            .push(format!("could not save slot record: {error}"));
    }
    Ok(SlotExecution {
        snapshot: outcome.snapshot.clone(),
        prev_snapshot: outcome.prev_snapshot.clone(),
        slot_changes: changes,
        exit: outcome.exit,
        step_exits,
        captured,
        outcome: Some(outcome),
        failure,
    })
}

/// Primary S0-compatible entry for jobs whose prelude was absent or already staged.
pub fn execute(job: &Job, context: &RunContext, index: u64) -> Result<SlotExecution, Exception> {
    if job.prelude.is_some() {
        return Err(Exception::PoolUnavailable);
    }
    match stage(job, context, index) {
        Ok(staged) => execute_staged(job, context, index, staged),
        Err(stage_failure) => {
            let mut local_job = job_without_prelude(job);
            let mut outcome = super::outcome_seed(job, context, super::ledger::now_unix_ms());
            outcome.exception = Some(stage_failure.exception);
            outcome.infra = stage_failure.infra;
            outcome.reset = stage_failure.reset;
            outcome.slot = None;
            outcome.exec = ExecPlace::Local;
            let warning = format!(
                "build slot unavailable; building locally: {}",
                stage_failure.detail
            );
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
            outcome.warnings.push(warning);
            let result =
                super::local::execute(&mut local_job, context, outcome, ExecPlace::Local, None, 0);
            Ok(slot_execution_from_local(result))
        }
    }
}

fn slot_execution_from_local(result: Result<JobResult, super::DispatchFailure>) -> SlotExecution {
    match result {
        Ok(result) => SlotExecution {
            slot_changes: result.outcome.slot_changes.clone(),
            exit: result.outcome.exit,
            step_exits: result.step_exits,
            captured: result.captured,
            outcome: Some(result.outcome),
            ..SlotExecution::default()
        },
        Err((error, outcome)) => {
            let kind = match &error {
                BuildError::Infra { kind, .. } => Some(*kind),
                BuildError::Cancelled => None,
                _ => Some(super::InfraKind::SlotLayout),
            };
            SlotExecution {
                exit: outcome.exit,
                outcome: Some(*outcome),
                failure: Some(SlotExecutionFailure {
                    kind,
                    detail: error.to_string(),
                }),
                ..SlotExecution::default()
            }
        }
    }
}

fn job_without_prelude(job: &Job) -> Job {
    Job {
        id: job.id.clone(),
        project_key: job.project_key.clone(),
        project: job.project.clone(),
        origin: job.origin,
        owner: job.owner.clone(),
        kind: job.kind.clone(),
        priority: job.priority,
        source: job.source.clone(),
        verify_base: job.verify_base.clone(),
        steps: job.steps.clone(),
        stop_on_failure: job.stop_on_failure,
        env: job.env.clone(),
        output: job.output,
        timeout: job.timeout,
        sandboxed: job.sandboxed,
        prelude: None,
    }
}

fn map_error(_error: BuildError) -> Exception {
    Exception::PoolUnavailable
}

fn stage_reset_failure(error: BuildError, reset: super::Reset) -> StageFailure {
    let infra = match &error {
        BuildError::Infra { kind, .. } => *kind,
        _ => super::InfraKind::SlotLayout,
    };
    StageFailure {
        exception: Exception::PoolUnavailable,
        infra: Some(infra),
        reset,
        detail: error.to_string(),
    }
}

fn ensure_slot_repo(paths: &SlotPaths, store: &Path) -> Result<(), Exception> {
    if !paths.repo.join(".git").exists() {
        let git = provenant_os::git::git_executable().ok_or(Exception::PoolUnavailable)?;
        let mut clone = Command::new(git);
        clear_git(&mut clone);
        let output = clone
            .args(["clone", "--no-checkout"])
            .arg(store)
            .arg(&paths.repo)
            .output()
            .map_err(|_| Exception::PoolUnavailable)?;
        if !output.status.success() {
            return Err(Exception::PoolUnavailable);
        }
    }
    for (key, value) in [("core.hooksPath", "/dev/null"), ("gc.auto", "0")] {
        if !git_config_status(&paths.repo, &[key, value]) {
            return Err(Exception::PoolUnavailable);
        }
    }
    Ok(())
}

fn sync_snapshot(
    paths: &SlotPaths,
    store: &Path,
    snapshot: &super::snapshot::SnapshotRef,
    verify_base: Option<&super::BaseRef>,
    clean: provenant_config::BuildClean,
) -> Result<(), String> {
    let store = store.to_string_lossy();
    if !git_status(&paths.repo, &["fetch", &store, &snapshot.ref_name]) {
        return Err("could not fetch the pinned snapshot into the slot".into());
    }
    let base_ref = snapshot.ref_name.replace("refs/snapshots/", "refs/bases/");
    if snapshot.base.is_some() && !git_status(&paths.repo, &["fetch", &store, &base_ref]) {
        return Err("could not fetch the pinned verify base into the slot".into());
    }
    if !checkout_commit(&paths.repo, &snapshot.commit) {
        return Err("could not check out the pinned snapshot in the slot".into());
    }
    if let (Some(base), Some(reference)) = (snapshot.base.as_deref(), verify_base) {
        let remote = reference.remote_ref.trim_start_matches("refs/remotes/");
        if !git_status(
            &paths.repo,
            &["update-ref", &format!("refs/remotes/{remote}"), base],
        ) {
            return Err("could not update the slot's verify base ref".into());
        }
    }
    match clean {
        provenant_config::BuildClean::None => {}
        provenant_config::BuildClean::Untracked if !git_status(&paths.repo, &["clean", "-ffd"]) => {
            return Err("could not clean untracked files from the slot".into());
        }
        provenant_config::BuildClean::Ignored if !git_status(&paths.repo, &["clean", "-ffdx"]) => {
            return Err("could not clean ignored files from the slot".into());
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn finish_slot_execution(
    mut execution: SlotExecution,
    mut outcome: JobOutcome,
) -> Result<JobResult, super::DispatchFailure> {
    let mut slot_outcome = execution.outcome.take().unwrap_or_else(|| outcome.clone());
    let prior_warnings = std::mem::take(&mut outcome.warnings);
    slot_outcome.warnings.splice(0..0, prior_warnings);
    slot_outcome.waited_ms = outcome.waited_ms;
    slot_outcome.enqueued_unix_ms = outcome.enqueued_unix_ms;
    slot_outcome.exception = slot_outcome.exception.or(outcome.exception);
    slot_outcome.infra = slot_outcome.infra.or(outcome.infra);
    if slot_outcome.reset == super::Reset::None {
        slot_outcome.reset = outcome.reset;
    }
    slot_outcome.strategy = outcome.strategy;
    slot_outcome.priority = outcome.priority;
    slot_outcome.slot = if slot_outcome.exec == ExecPlace::Local {
        None
    } else {
        slot_outcome.slot.or(outcome.slot)
    };
    if let Some(failure) = execution.failure.take() {
        let error = if slot_outcome.cancelled {
            BuildError::Cancelled
        } else {
            BuildError::Infra {
                kind: failure.kind.unwrap_or(super::InfraKind::SlotLayout),
                detail: failure.detail,
            }
        };
        return Err((error, Box::new(slot_outcome)));
    }
    Ok(JobResult {
        outcome: slot_outcome,
        step_exits: execution.step_exits,
        captured: execution.captured,
    })
}
fn clear_git(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
}
fn checkout_commit(repo: &Path, commit: &str) -> bool {
    // A stale stat cache makes `checkout --force` rewrite files whose content
    // is unchanged, which would rebuild units that did not change. Refresh it
    // first so only genuinely different files get a new mtime. A failure here
    // only means there is real local work to discard, which checkout does.
    git_status(
        repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "update-index",
            "--refresh",
        ],
    );
    git_status(
        repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "checkout",
            "--force",
            "--detach",
            commit,
        ],
    )
}

fn git_config_status(repo: &Path, tail: &[&str]) -> bool {
    let tail = tail.iter().map(OsString::from).collect::<Vec<_>>();
    provenant_os::git_config::write(&tail, |args| git_status_os(repo, args))
        .is_ok_and(|success| success)
}

fn git_status(repo: &Path, args: &[&str]) -> bool {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    git_status_os(repo, &args).is_ok_and(|success| success)
}

fn git_status_os(repo: &Path, args: &[OsString]) -> Result<bool, std::io::Error> {
    let Some(git) = provenant_os::git::git_executable() else {
        return Ok(false);
    };
    let mut command = Command::new(git);
    clear_git(&mut command);
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map(|output| output.status.success())
}
/// Take the slot's custody lease. A held lease means a previous job's child
/// process still exists, even when its running marker was never published.
fn acquire_intent_lease(root: &Path) -> Result<LockGuard, String> {
    let directory = Directory::open_anchor(root).map_err(|error| error.to_string())?;
    let name = OsStr::new("intent.lock");
    match directory.try_lock_existing(name, LockMode::Exclusive) {
        Err(FsError::LockTimeout { .. }) => {
            return Err("slot still holds an in-flight build custody lease".into());
        }
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    directory
        .try_lock(name, LockMode::Exclusive)
        .map_err(|error| error.to_string())
}

/// Whether a running job marker still names a live build leader. Called while
/// holding the slot's custody lease, so a leader that died but left descendants
/// still fails the lease check rather than reaching this probe.
fn live_build_child(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_slice::<JobMarker>(&bytes) else {
        return false;
    };
    if marker.state != JobMarkerState::Running || marker.owner.is_empty() {
        return false;
    }
    matches!(
        SystemProcessObserver.observe(i64::from(marker.pid)),
        ProcessObservation::Present(fact)
            if !fact.zombie && fact.identity.native_start == marker.start_time
    )
}

/// Exclusive custody of one slot's payload for a destructive operation.
///
/// Holding this means the slot lock is ours and no live build child holds the
/// inherited custody lease or a running marker. Staging, reset and collection
/// all use the same lock pair, so a slot cannot be reset or reused underneath a
/// process that can still write to it. Every destructive slot operation borrows
/// this guard, so an unlocked reset, delete, record unlink or freshness stamp
/// cannot compile.
pub struct SlotCustody {
    _slot_lock: LockGuard,
    _lease: LockGuard,
}

impl SlotCustody {
    /// Acquire custody for collection or an explicit reset: both guards plus a
    /// live-child probe, so a slot with a surviving build is left alone.
    pub fn acquire(paths: &SlotPaths) -> Result<Self, String> {
        let slot_lock = slot_lock(&paths.lock).map_err(|error| error.to_string())?;
        let lease = acquire_intent_lease(&paths.root)?;
        if live_build_child(&paths.job) {
            return Err("slot still has a live build child".into());
        }
        Ok(Self::from_guards(slot_lock, lease))
    }

    /// Adopt the guards taken by staging. Staging reaps an orphaned group
    /// before publishing its own marker, so it does not re-probe liveness.
    fn from_guards(slot_lock: LockGuard, lease: LockGuard) -> Self {
        Self {
            _slot_lock: slot_lock,
            _lease: lease,
        }
    }

    /// The lease inherited by the build's process tree.
    fn intent_lease(&self) -> &LockGuard {
        &self._lease
    }
}

fn slot_lock(path: &Path) -> std::io::Result<provenant_os::fs::LockGuard> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("slot lock has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("slot lock has no filename"))?;
    let directory =
        provenant_os::fs::Directory::open_anchor(parent).map_err(std::io::Error::other)?;
    directory
        .try_lock(name, provenant_os::fs::LockMode::Exclusive)
        .map_err(std::io::Error::other)
}
fn digest_prepare(prepare: &[Vec<String>]) -> String {
    let bytes = serde_json::to_vec(prepare).unwrap_or_default();
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn build_context(job: &Job, context: &RunContext) -> BuildContext {
    if let Some(value) = job
        .env
        .iter()
        .find(|(key, _)| key == provenant_core::consumer_env::BUILD_CONTEXT)
        .and_then(|(_, value)| value.to_str())
        && let Ok(context) = BuildContext::from_json(value)
    {
        return context;
    }
    let get = |name: &str| {
        job.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| PathBuf::from(value))
    };
    BuildContext {
        schema: 1,
        project: job.project.clone(),
        project_key: job.project_key.clone(),
        worktree: super::job_root(job).to_path_buf(),
        attempt: None,
        sandboxed: job.sandboxed,
        git: None,
        port: context.strategy.port.clone(),
        strategy_source: context.strategy.source,
        host_home: context.home.clone(),
        rustup_home: get("RUSTUP_HOME"),
        cargo_home: get("CARGO_HOME").unwrap_or_else(|| context.home.join(".cargo")),
        host_path: job
            .env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .unwrap_or_default(),
        verify_base: job.verify_base.clone(),
    }
}
fn lane_port_plan(state: &Path) -> std::io::Result<Option<provenant_os::lane_ports::Plan>> {
    let settings = provenant_config::LaneBuildSettings::read(state)?;
    let blocks = settings.lane_port_blocks();
    if blocks == 0 {
        return Ok(None);
    }
    let convert = |value: u64| u32::try_from(value).map_err(std::io::Error::other);
    Ok(Some(provenant_os::lane_ports::Plan {
        state: state.to_path_buf(),
        base: convert(settings.lane_port_base_value())?,
        size: convert(settings.lane_port_block_size())?,
        blocks: convert(blocks)?,
        blocks_origin: None,
    }))
}
fn start_rss_sampler(
    identity: std::sync::Arc<
        std::sync::Mutex<Option<provenant_supervisor::census::ProcessIdentity>>,
    >,
    peak: std::sync::Arc<std::sync::atomic::AtomicU64>,
    owner: &str,
) -> Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)> {
    let attempt = provenant_core::AttemptId::parse(owner)
        .unwrap_or_else(|_| provenant_core::AttemptId::from_bytes([0; 16]));
    let (stop, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("build-rss-census".into())
        .spawn(move || {
            loop {
                let stopping = matches!(
                    receiver.recv_timeout(Duration::from_secs(5)),
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                );
                if let Some(identity) = identity.lock().ok().and_then(|value| value.clone())
                    && let Ok(snapshot) =
                        provenant_supervisor::process_tree::sample(&identity, attempt)
                    && let Ok(group) = u32::try_from(identity.group_id)
                {
                    let rss = snapshot.group_rss_bytes(group);
                    peak.fetch_max(rss, std::sync::atomic::Ordering::Relaxed);
                }
                if stopping {
                    break;
                }
            }
        })
        .ok()?;
    Some((stop, worker))
}

fn write_marker(path: &Path, marker: &JobMarker) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "json.{}.tmp",
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        serde_json::to_writer(&mut file, marker).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Publish custody before spawn. The caller holds the slot's intent lease, so
/// a holder that dies before spawning leaves only an intent with no live lease.
fn mark_intent(path: &Path, owner: &str) -> std::io::Result<()> {
    let marker = JobMarker {
        pid: std::process::id(),
        pgid: 0,
        start_time: 0,
        boot: String::new(),
        session_id: 0,
        owner: owner.to_owned(),
        state: JobMarkerState::Intent,
    };
    write_marker(path, &marker)
}

fn mark_job(
    path: &Path,
    pid: u32,
    owner: &str,
) -> std::io::Result<provenant_supervisor::census::ProcessIdentity> {
    let identity = match SystemProcessObserver.observe(i64::from(pid)) {
        ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
        _ => return Err(std::io::Error::other("child start time unavailable")),
    };
    let marker = JobMarker {
        pid,
        pgid: i32::try_from(pid).map_err(std::io::Error::other)?,
        start_time: identity.native_start,
        boot: identity.boot.clone(),
        session_id: identity.session_id,
        owner: owner.to_owned(),
        state: JobMarkerState::Running,
    };
    write_marker(path, &marker)?;
    Ok(identity)
}
/// Delete a stale job marker. Only a holder of the slot's custody may clear a
/// running marker (and signal its recorded group), so a live build cannot lose
/// its marker underneath it.
fn reap_orphan(path: &Path, _custody: &SlotCustody) -> std::io::Result<bool> {
    let marker: JobMarker = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if marker.state == JobMarkerState::Intent {
        // The caller owns the intent lease, which proves no previous holder
        // or spawned child is still alive. An intent without a running marker
        // is safe to clear only under that proof.
        fs::remove_file(path)?;
        return Ok(false);
    }
    if marker.pid != u32::try_from(marker.pgid).unwrap_or_default() || marker.owner.is_empty() {
        return Err(std::io::Error::other("invalid stale job marker identity"));
    }
    let killed = terminate_recorded_group(&marker)?;
    fs::remove_file(path)?;
    Ok(killed)
}

fn terminate_untracked_group(pid: u32) -> std::io::Result<()> {
    let identity = match SystemProcessObserver.observe(i64::from(pid)) {
        ProcessObservation::Present(fact) if !fact.zombie => fact.identity,
        _ => {
            return Err(std::io::Error::other(
                "spawned process identity unavailable",
            ));
        }
    };
    let marker = JobMarker {
        pid,
        pgid: i32::try_from(pid).map_err(std::io::Error::other)?,
        start_time: identity.native_start,
        boot: identity.boot,
        session_id: identity.session_id,
        owner: "untracked".into(),
        state: JobMarkerState::Running,
    };
    terminate_recorded_group(&marker).map(|_| ())
}

fn group_members(marker: &JobMarker) -> std::io::Result<Vec<ProcessIdentity>> {
    let observer = SystemProcessObserver;
    let current_boot = observer.boot_identity().map_err(std::io::Error::other)?;
    if !marker.boot.is_empty() && marker.boot != current_boot {
        return Ok(Vec::new());
    }
    let leader = observer.observe(i64::from(marker.pid));
    let (boot, session_id) = match leader {
        ProcessObservation::Present(fact)
            if fact.identity.native_start == marker.start_time
                && fact.identity.group_id == i64::from(marker.pgid)
                && (marker.boot.is_empty() || fact.identity.boot == marker.boot)
                && (marker.session_id <= 0 || fact.identity.session_id == marker.session_id) =>
        {
            (fact.identity.boot, fact.identity.session_id)
        }
        ProcessObservation::Present(fact) if fact.identity.native_start != marker.start_time => {
            // The leader PID has been reused. Its group cannot be the recorded
            // group, so do not signal the newly allocated identity.
            return Ok(Vec::new());
        }
        ProcessObservation::Present(_) => {
            return Err(std::io::Error::other(
                "job marker leader no longer matches its recorded group",
            ));
        }
        ProcessObservation::Absent => {
            if marker.boot.is_empty() || marker.session_id <= 0 {
                return Err(std::io::Error::other(
                    "job marker lacks group identity after leader exit",
                ));
            }
            if marker.boot != current_boot {
                return Ok(Vec::new());
            }
            let census = observer
                .census(MAX_CENSUS_ENTRIES)
                .map_err(std::io::Error::other)?;
            if !census.is_complete() {
                return Err(std::io::Error::other(
                    "process census is incomplete after build leader exit",
                ));
            }
            let group_remains = census.facts().any(|fact| {
                !fact.zombie
                    && fact.identity.boot == marker.boot
                    && fact.identity.group_id == i64::from(marker.pgid)
                    && fact.identity.session_id == marker.session_id
            });
            if group_remains {
                return Err(std::io::Error::other(
                    "build leader exited without a recorded matching descendant identity",
                ));
            }
            return Ok(Vec::new());
        }
        ProcessObservation::Unknown(_) => {
            return Err(std::io::Error::other(
                "could not verify the recorded job process identity",
            ));
        }
    };
    if boot != current_boot || session_id <= 0 {
        return Ok(Vec::new());
    }
    let census = observer
        .census(MAX_CENSUS_ENTRIES)
        .map_err(std::io::Error::other)?;
    if !census.is_complete() {
        return Err(std::io::Error::other(
            "process census is incomplete while checking a build group",
        ));
    }
    let mut members = Vec::new();
    for fact in census.facts().filter(|fact| {
        !fact.zombie
            && fact.identity.boot == boot
            && fact.identity.group_id == i64::from(marker.pgid)
            && fact.identity.session_id == session_id
    }) {
        if fact.identity.pid == i64::from(marker.pid)
            && fact.identity.native_start != marker.start_time
        {
            return Ok(Vec::new());
        }
        members.push(fact.identity.clone());
    }
    Ok(members)
}

fn same_identity(left: &ProcessIdentity, right: &ProcessIdentity) -> bool {
    left.boot == right.boot
        && left.pid == right.pid
        && left.native_start == right.native_start
        && left.group_id == right.group_id
        && left.session_id == right.session_id
}

/// Reobserve a group only after its original leader and members were verified
/// before TERM. The caller compares these rows with that saved identity set.
fn group_members_after_signal(marker: &JobMarker) -> std::io::Result<Vec<ProcessIdentity>> {
    let observer = SystemProcessObserver;
    let current_boot = observer.boot_identity().map_err(std::io::Error::other)?;
    if marker.boot != current_boot || marker.session_id <= 0 {
        return Ok(Vec::new());
    }
    match observer.observe(i64::from(marker.pid)) {
        ProcessObservation::Present(fact) if fact.identity.native_start != marker.start_time => {
            // Census the recorded group: verified descendants may outlive the leader.
        }
        ProcessObservation::Present(fact)
            if fact.identity.boot != marker.boot
                || fact.identity.group_id != i64::from(marker.pgid)
                || fact.identity.session_id != marker.session_id =>
        {
            return Err(std::io::Error::other(
                "job marker leader changed group identity during cleanup",
            ));
        }
        ProcessObservation::Present(_) | ProcessObservation::Absent => {}
        ProcessObservation::Unknown(_) => {
            // A complete census below can still verify previously observed
            // identities when a direct PID observation is inconclusive.
        }
    }
    let census = observer
        .census(MAX_CENSUS_ENTRIES)
        .map_err(std::io::Error::other)?;
    if !census.is_complete() {
        return Err(std::io::Error::other(
            "process census is incomplete while confirming build group cleanup",
        ));
    }
    Ok(census
        .facts()
        .filter(|fact| {
            !fact.zombie
                && fact.identity.boot == marker.boot
                && fact.identity.group_id == i64::from(marker.pgid)
                && fact.identity.session_id == marker.session_id
        })
        .map(|fact| fact.identity.clone())
        .collect())
}

fn remaining_recorded_members(
    marker: &JobMarker,
    observed: &[ProcessIdentity],
) -> std::io::Result<(bool, bool)> {
    let current = group_members_after_signal(marker)?;
    let known = current
        .iter()
        .any(|member| observed.iter().any(|before| same_identity(before, member)));
    let unknown = current
        .iter()
        .any(|member| !observed.iter().any(|before| same_identity(before, member)));
    Ok((known, unknown))
}

fn terminate_recorded_group(marker: &JobMarker) -> std::io::Result<bool> {
    let clock = provenant_os::clock::SystemClock::new();
    let observed = group_members(marker)?;
    if observed.is_empty() {
        return Ok(false);
    }
    let _ = provenant_os::pid::signal_process_group(
        i64::from(marker.pgid),
        provenant_os::pid::ProcessSignal::Terminate,
    )
    .map_err(std::io::Error::other)?;
    let deadline = MonoTime::from_micros(clock.mono_now().as_micros().saturating_add(5_000_000));
    loop {
        let (known, unknown) = remaining_recorded_members(marker, &observed)?;
        if unknown {
            return Err(std::io::Error::other(
                "unobserved process identity appeared in the build group",
            ));
        }
        if !known {
            return Ok(true);
        }
        if clock.mono_now() >= deadline {
            break;
        }
        let _ = clock.wait_until(MonoTime::from_micros(
            clock.mono_now().as_micros().saturating_add(50_000),
        ));
    }
    let (known, unknown) = remaining_recorded_members(marker, &observed)?;
    if unknown {
        return Err(std::io::Error::other(
            "unobserved process identity appeared in the build group",
        ));
    }
    if known {
        let _ = provenant_os::pid::signal_process_group(
            i64::from(marker.pgid),
            provenant_os::pid::ProcessSignal::Kill,
        )
        .map_err(std::io::Error::other)?;
        let kill_deadline =
            MonoTime::from_micros(clock.mono_now().as_micros().saturating_add(1_000_000));
        while clock.mono_now() < kill_deadline {
            let (known, unknown) = remaining_recorded_members(marker, &observed)?;
            if unknown {
                return Err(std::io::Error::other(
                    "unobserved process identity appeared in the build group",
                ));
            }
            if !known {
                return Ok(true);
            }
            let _ = clock.wait_until(MonoTime::from_micros(
                clock.mono_now().as_micros().saturating_add(50_000),
            ));
        }
        return Err(std::io::Error::other(
            "build process group survived SIGKILL",
        ));
    }
    Ok(true)
}
fn fingerprint_scan(target: &Path, started_at: SystemTime) -> Vec<(String, bool)> {
    let mut units = Vec::new();
    let Ok(profiles) = fs::read_dir(target) else {
        return units;
    };
    for profile in profiles.flatten() {
        let fingerprints = profile.path().join(".fingerprint");
        let Ok(packages) = fs::read_dir(fingerprints) else {
            continue;
        };
        for package in packages.flatten() {
            let name = package.file_name().to_string_lossy().into_owned();
            let package_name = name
                .rsplit_once('-')
                .filter(|(_, hash)| hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .map_or_else(|| name.clone(), |(name, _)| name.to_owned());
            let rebuilt = package
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified > started_at);
            units.push((package_name, rebuilt));
        }
    }
    units.sort();
    units
}
fn slot_changes(repo: &Path) -> Option<SlotChanges> {
    let mut git = Command::new(provenant_os::git::git_executable()?);
    clear_git(&mut git);
    let output = git
        .arg("-C")
        .arg(repo)
        .args([
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut rows = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty());
    let mut count = 0_u64;
    let mut sample = Vec::new();
    while let Some(row) = rows.next() {
        let fields = match row.first() {
            Some(b'1') => 9,
            Some(b'2') => 10,
            Some(b'u') => 11,
            Some(b'?') | Some(b'!') => 2,
            _ => continue,
        };
        let path = row.splitn(fields, |byte| *byte == b' ').nth(fields - 1)?;
        if row.first() == Some(&b'2') {
            let _ = rows.next();
        }
        count += 1;
        if sample.len() < 20 {
            sample.push(String::from_utf8_lossy(path).into_owned());
        }
    }
    Some(SlotChanges { count, sample })
}

/// Slot/local placement convenience for the ledger.
pub fn local_outcome(outcome: &mut JobOutcome) {
    outcome.exec = ExecPlace::Local;
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "scratch Git and process fixtures"
)]
mod tests {
    use super::*;
    #[test]
    fn record_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("slot.json");
        let record = SlotRecord {
            schema: 1,
            project_key: "project".into(),
            index: 3,
            created_unix_ms: 42,
            ..SlotRecord::default()
        };
        record.write(&path).unwrap();
        assert_eq!(SlotRecord::read(&path).unwrap(), Some(record));
    }

    #[test]
    fn canary_effect_only_touches_the_slot_it_ran_in() {
        use super::super::{ExitStatus, report};
        let mut clean = report::test_row();
        clean.exit = Some(ExitStatus::code(0));
        let mut failed = report::test_row();
        failed.exit = Some(ExitStatus::code(1));

        // The only slot-side effects a canary may apply are decided here,
        // while the staged slot's lock and custody lease are held. A canary
        // that ran locally has no slot placement and must leave every slot alone.
        assert_eq!(
            canary_slot_effect(true, ExecPlace::Slot, &failed, 42),
            CanarySlotEffect::Reset
        );
        assert_eq!(
            canary_slot_effect(true, ExecPlace::Slot, &clean, 42),
            CanarySlotEffect::Stamp(42)
        );
        assert_eq!(
            canary_slot_effect(true, ExecPlace::Local, &clean, 42),
            CanarySlotEffect::None,
            "a local fallback must not stamp a slot the canary never exercised"
        );
        assert_eq!(
            canary_slot_effect(false, ExecPlace::Slot, &clean, 42),
            CanarySlotEffect::None
        );
    }

    fn real_git() -> Command {
        Command::new(provenant_os::git::git_executable().expect("git executable"))
    }

    fn git_at(path: &Path, args: &[&str]) -> std::process::Output {
        let output = real_git()
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[test]
    fn a_to_b_to_a_checkout_preserves_untouched_file_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        let init = real_git()
            .arg("-C")
            .arg(&source)
            .args(["init"])
            .output()
            .unwrap();
        assert!(init.status.success());
        git_at(&source, &["config", "user.name", "Test"]);
        git_at(&source, &["config", "user.email", "test@example.invalid"]);
        fs::write(source.join("changed.txt"), "A").unwrap();
        fs::write(source.join("untouched.txt"), "constant").unwrap();
        git_at(&source, &["add", "."]);
        git_at(&source, &["commit", "-m", "A"]);
        let a = String::from_utf8(git_at(&source, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        fs::write(source.join("changed.txt"), "B").unwrap();
        git_at(&source, &["add", "."]);
        git_at(&source, &["commit", "-m", "B"]);
        let b = String::from_utf8(git_at(&source, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned();
        let store = tmp.path().join("store.git");
        let cloned = real_git()
            .args(["clone", "--bare"])
            .arg(&source)
            .arg(&store)
            .output()
            .unwrap();
        assert!(cloned.status.success());
        let slot = tmp.path().join("slot");
        let cloned = real_git()
            .args(["clone", "--no-checkout"])
            .arg(&store)
            .arg(&slot)
            .output()
            .unwrap();
        assert!(cloned.status.success());
        assert!(checkout_commit(&slot, &a));
        let untouched = slot.join("untouched.txt");
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        std::fs::File::open(&untouched)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let before = fs::metadata(&untouched).unwrap().modified().unwrap();
        assert!(checkout_commit(&slot, &b));
        assert_eq!(
            fs::metadata(&untouched).unwrap().modified().unwrap(),
            before
        );
        assert!(checkout_commit(&slot, &a));
        assert_eq!(
            fs::metadata(&untouched).unwrap().modified().unwrap(),
            before
        );
    }

    #[test]
    fn stale_job_marker_terminates_its_recorded_process_group() {
        use std::os::unix::process::CommandExt;
        let tmp = tempfile::tempdir().unwrap();
        let paths = SlotPaths::new(&tmp.path().join("home"), "project", 0);
        std::fs::create_dir_all(&paths.root).unwrap();
        // Staging adopts the guards before it reaps a recorded group, so the
        // live-child probe must not stand in the way of this cleanup.
        let custody = SlotCustody::from_guards(
            slot_lock(&paths.lock).unwrap(),
            acquire_intent_lease(&paths.root).unwrap(),
        );
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        mark_job(&paths.job, child.id(), "test-owner").unwrap();
        reap_orphan(&paths.job, &custody).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(!paths.job.exists());
    }
}
