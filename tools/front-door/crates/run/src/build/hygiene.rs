//! Slot reset, garbage collection and best-effort maintenance.

use super::{
    BuildError, InfraKind, JobOutcome, Reset,
    slots::{SlotCustody, SlotPaths, SlotRecord, SlotState},
};
use provenant_config::{PortTable, ResolvedAdmissionClass};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::{
    clock::SystemClock,
    exec::{ExecOptions, StreamPolicy, SystemExecutor, Termination, run_text},
    fs::{Directory, LockMode},
};
use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Pre-job action selected by the clock guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreJob {
    Proceed,
    LightReset,
    FullReset,
}

/// A backwards clock invalidates Cargo's timestamp freshness assumptions.
pub fn before_job(record: &mut SlotRecord, now_unix_ms: u64) -> PreJob {
    if record
        .last_job
        .as_ref()
        .is_some_and(|job| now_unix_ms < job.finished_unix_ms)
    {
        PreJob::FullReset
    } else {
        PreJob::Proceed
    }
}

/// Count infrastructure failures in the trailing 24-hour window, excluding cancellation.
pub fn after_job(record: &mut SlotRecord, outcome: &JobOutcome, now_unix_ms: u64) -> Reset {
    let cutoff = now_unix_ms.saturating_sub(86_400_000);
    record
        .health
        .infra_failures_unix_ms
        .retain(|time| *time >= cutoff && *time <= now_unix_ms);
    if outcome.cancelled || outcome.infra.is_none() || outcome.infra == Some(InfraKind::Clock) {
        return Reset::None;
    }
    record.health.infra_failures_unix_ms.push(now_unix_ms);
    match record.health.infra_failures_unix_ms.len() {
        1 => Reset::Light,
        2 => Reset::Full,
        _ => {
            record.health.state = SlotState::Quarantined;
            record.health.quarantine_reason = Some(format!(
                "repeated infrastructure failures: {:?}",
                outcome.infra
            ));
            Reset::Quarantine
        }
    }
}

/// Remove untracked and ignored changes from a slot checkout. The guard proves
/// the caller owns the slot; an unlocked call cannot compile.
pub fn light_reset(slot: &SlotPaths, _custody: &SlotCustody) -> Result<(), BuildError> {
    for args in [
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "checkout",
            "--force",
            "--detach",
            "HEAD",
        ][..],
        &["clean", "-ffdx"][..],
    ] {
        crate::git_context::seed_git(&slot.repo, args).map_err(|error| BuildError::Infra {
            kind: InfraKind::Git,
            detail: error.to_string(),
        })?;
    }
    Ok(())
}

/// Remove fixed children of a verified slot; the slot executor recreates repo.
/// The guard proves the caller holds both the slot lock and the inherited
/// custody lease for the duration.
pub fn full_reset(slot: &SlotPaths, _custody: &SlotCustody) -> Result<(), BuildError> {
    use provenant_os::fs::{Directory, FileKind, PayloadTree};
    use std::ffi::OsStr;
    let parent = slot
        .root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| BuildError::Config("invalid slot root".into()))?;
    if parent.file_name() != Some(OsStr::new("provenant-slots"))
        || slot
            .root
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|n| n.parse::<u64>().ok())
            .is_none()
        || slot.target != slot.root.join("target")
        || slot.repo != slot.root.join("repo")
    {
        return Err(BuildError::Config(
            "refusing reset outside slot cache".into(),
        ));
    }
    let cache = Directory::open_anchor(parent).map_err(|e| BuildError::Infra {
        kind: InfraKind::SlotLayout,
        detail: e.to_string(),
    })?;
    let project_name = slot
        .root
        .parent()
        .and_then(Path::file_name)
        .ok_or_else(|| BuildError::Config("invalid project slot root".into()))?;
    let slot_name = slot
        .root
        .file_name()
        .ok_or_else(|| BuildError::Config("invalid slot index".into()))?;
    let project = cache
        .open_directory(project_name)
        .map_err(|e| BuildError::Infra {
            kind: InfraKind::SlotLayout,
            detail: e.to_string(),
        })?;
    let root = project
        .open_directory(slot_name)
        .map_err(|e| BuildError::Infra {
            kind: InfraKind::SlotLayout,
            detail: e.to_string(),
        })?;
    for name in ["target", "repo", "tmp"] {
        let name = OsStr::new(name);
        let metadata = match root.metadata_at(name, false) {
            Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
            Err(e) if e.is_not_found() => continue,
            Ok(_) => return Err(BuildError::Config("slot child is not a directory".into())),
            Err(e) => {
                return Err(BuildError::Infra {
                    kind: InfraKind::Disk,
                    detail: e.to_string(),
                });
            }
        };
        let opened = root.open_directory(name).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        opened.make_private().map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        let private = opened.metadata().map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        if !metadata.same_identity(&private) {
            return Err(BuildError::Config("slot child changed during reset".into()));
        }
        drop(opened);
        let tree = PayloadTree::claim(&root, name, &private).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
        remove_contents(&tree, 0).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e,
        })?;
        drop(tree);
        root.rmdir(name).map_err(|e| BuildError::Infra {
            kind: InfraKind::Disk,
            detail: e.to_string(),
        })?;
    }
    Ok(())
}

/// Unlink a collected slot's record only under custody. Collection rereads the
/// record under the same guard it uses for the reset, so a build admitted in
/// between cannot observe or write a half-collected slot.
pub(super) fn unlink_record(
    slot: &SlotPaths,
    _custody: &SlotCustody,
) -> Result<(), provenant_os::fs::FsError> {
    Directory::open_anchor(&slot.root)?.unlink(OsStr::new("slot.json"))
}

/// Stamp a slot fresh after a canary found no freshness failure. The guard
/// proves the stamp is applied to the exact slot the canary exercised, while
/// its lock and custody lease are still held.
pub(super) fn mark_fresh(record: &mut SlotRecord, at: u64, _custody: &SlotCustody) {
    record.last_canary_unix_ms = Some(at);
}

fn remove_contents(tree: &provenant_os::fs::PayloadTree, depth: usize) -> Result<(), String> {
    use provenant_os::fs::FileKind;
    if depth >= 128 {
        return Err("slot tree nesting too deep".into());
    }
    tree.make_writable().map_err(|e| e.to_string())?;
    for entry in tree.entries().map_err(|e| e.to_string())? {
        let name = entry.map_err(|e| e.to_string())?;
        let meta = tree.metadata_at(&name).map_err(|e| e.to_string())?;
        if meta.kind == FileKind::Directory {
            let child = tree.child(&name).map_err(|e| e.to_string())?;
            remove_contents(&child, depth + 1)?;
            drop(child);
            tree.rmdir(&name).map_err(|e| e.to_string())?;
        } else {
            tree.unlink(&name).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Result of slot, store and ledger garbage collection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub removed_slots: u64,
    pub removed_bytes: u64,
    /// Ledger files rotated out of `jobs.jsonl`.
    pub rotated_ledgers: u64,
    /// Rotated ledger files older than the retention window that were removed.
    pub expired_ledgers: u64,
    pub warnings: Vec<String>,
}

/// Collect idle slot payloads while retaining the lock inode for other waiters.
pub fn gc(home: &Path, state: &Path, limit: u64, now: SystemTime) -> GcReport {
    gc_with_retention(
        home,
        state,
        limit,
        now,
        Duration::from_secs(7 * 86400),
        Duration::from_secs(7 * 86400),
    )
}

/// Collect with one idle/retention pair for every project.
pub fn gc_with_retention(
    home: &Path,
    state: &Path,
    limit: u64,
    now: SystemTime,
    idle: Duration,
    retention: Duration,
) -> GcReport {
    gc_projects(home, state, limit, now, |_| (idle, retention))
}

/// Collect using each project's resolved `build.slot_idle` and
/// `build.snapshot_retention`. The settings closure returns `(idle, retention)`
/// and is called once per project key found in the slot cache or store.
pub fn gc_projects<F>(
    home: &Path,
    state: &Path,
    limit: u64,
    now: SystemTime,
    settings: F,
) -> GcReport
where
    F: Fn(&str) -> (Duration, Duration),
{
    let mut report = GcReport::default();
    let now_ms = unix_millis(now);
    let mut projects: BTreeMap<String, Vec<SlotStatus>> = BTreeMap::new();
    for row in status(home, state) {
        projects
            .entry(row.project_key.clone())
            .or_default()
            .push(row);
    }
    for (project_key, rows) in &projects {
        let (idle, _) = settings(project_key);
        for row in rows {
            let paths = SlotPaths::new(home, project_key, row.index);
            // Collection is a destructive slot operation: hold the slot lock
            // and its custody lease together, and refuse a slot whose live
            // build child still holds that lease. Bind the guard so it stays
            // alive through the reread, the full reset and the record unlink.
            let custody = match SlotCustody::acquire(&paths) {
                Ok(custody) => custody,
                Err(_) => continue,
            };
            // Re-read under the custody guard: status is deliberately an
            // unlocked observation.
            let record = match SlotRecord::read(&paths.record) {
                Ok(Some(record)) => record,
                Ok(None) => continue,
                Err(error) => {
                    report
                        .warnings
                        .push(format!("cannot read slot record: {error}"));
                    continue;
                }
            };
            let last = record
                .last_job
                .as_ref()
                .map_or(record.created_unix_ms, |job| job.finished_unix_ms);
            if row.index < limit && (last > now_ms || now_ms - last <= millis(idle)) {
                continue;
            }
            match full_reset(&paths, &custody) {
                Ok(()) => {
                    // Retain the slot directory and lock inode; unlinking it while a waiter
                    // has the inode open would let a new holder acquire a different lock.
                    if let Err(error) = unlink_record(&paths, &custody) {
                        report
                            .warnings
                            .push(format!("cannot remove slot record: {error}"));
                    } else {
                        report.removed_slots += 1;
                        report.removed_bytes +=
                            row.allocated_bytes.saturating_sub(allocated(&paths.root));
                    }
                }
                Err(error) => report
                    .warnings
                    .push(format!("cannot collect slot: {error}")),
            }
        }
    }
    // Stores exist independently of slots: every store gets its expired pins
    // collected, including projects that also have slots.
    if let Ok(entries) = std::fs::read_dir(super::paths::store_root(state)) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".git")) else {
                continue;
            };
            let (_, retention) = settings(name);
            if let Err(error) = super::store::gc(state, name, retention, now) {
                report
                    .warnings
                    .push(format!("cannot collect build store {name}: {error}"));
            }
        }
    }
    match super::ledger::rotate(state, now) {
        Ok(rotation) => {
            report.rotated_ledgers = u64::from(rotation.rotated);
            report.expired_ledgers = rotation.expired;
        }
        Err(error) => report
            .warnings
            .push(format!("cannot rotate build ledger: {error}")),
    }
    report
}

fn unix_millis(now: SystemTime) -> u64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

/// One observed slot row for the status command.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotStatus {
    pub project_key: String,
    pub index: u64,
    pub allocated_bytes: u64,
    pub health: String,
    pub last_job: Option<super::slots::SlotJobMark>,
}

/// Read slot records and allocated sizes without following symlinks.
pub fn status(home: &Path, _state: &Path) -> Vec<SlotStatus> {
    let root = super::paths::slot_cache_root(home);
    let mut rows = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return rows;
    };
    for project in projects.flatten() {
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let project_key = project.file_name().to_string_lossy().into_owned();
        let Ok(slots) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for entry in slots.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(index) = entry.file_name().to_string_lossy().parse::<u64>() else {
                continue;
            };
            let record: Option<SlotRecord> = std::fs::read(entry.path().join("slot.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            rows.push(SlotStatus {
                project_key: project_key.clone(),
                index,
                allocated_bytes: allocated(&entry.path()),
                health: record
                    .as_ref()
                    .map_or("unknown", |r| match r.health.state {
                        SlotState::Ok => "ok",
                        SlotState::Quarantined => "quarantined",
                    })
                    .into(),
                last_job: record.and_then(|r| r.last_job),
            });
        }
    }
    rows.sort_by(|a, b| (&a.project_key, a.index).cmp(&(&b.project_key, b.index)));
    rows
}

/// Allocated bytes occupied by the build stores.
pub fn store_bytes(state: &Path) -> u64 {
    allocated(&super::paths::store_root(state))
}

fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = meta.blocks().saturating_mul(512);
    if meta.file_type().is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            total = total.saturating_add(allocated(&entry.path()));
        }
    }
    total
}

/// Hourly prewarm budget and the last time each project was warmed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MaintenanceState {
    pub schema: u32,
    #[serde(default)]
    pub last_prewarm: BTreeMap<String, u64>,
    #[serde(default)]
    pub hour_budget_used: HourBudget,
    /// In-flight budget claims. A crashed claimant's reservation is removed by
    /// the next claimant without a refund; only the claiming process returns a
    /// unit, through `cancel_maintenance_reservation`, when its job never
    /// launched.
    #[serde(default)]
    pub reservations: BTreeMap<String, MaintenanceReservation>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct HourBudget {
    pub hour: u64,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct MaintenanceReservation {
    pub pid: u32,
    pub start_time: u64,
    pub reserved_unix_ms: u64,
}

/// Result of a maintenance/prewarm request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainOutcome {
    pub ran: bool,
    pub canary: bool,
    pub exit_code: Option<i32>,
    pub warning: Option<String>,
}

fn with_maintenance_state<T>(
    state: &Path,
    change: impl FnOnce(&mut MaintenanceState) -> Result<T, String>,
) -> Result<T, String> {
    let root = super::paths::build_root(state);
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let directory = Directory::open_anchor(&root).map_err(|error| error.to_string())?;
    let clock = SystemClock::new();
    let lock = directory
        .lock(
            OsStr::new("maintenance.lock"),
            LockMode::Exclusive,
            WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(10_000_000)),
            &CancelToken::new(),
            &clock,
        )
        .map_err(|error| error.to_string())?;
    let path = super::paths::maintenance_path(state);
    let mut record: MaintenanceState = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| MaintenanceState {
            schema: 1,
            ..MaintenanceState::default()
        });
    let result = change(&mut record)?;
    if record.schema == 0 {
        record.schema = 1;
    }
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
    }
    std::fs::rename(&temporary, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        error.to_string()
    })?;
    drop(lock);
    Ok(result)
}

fn process_present(pid: u32, start_time: u64) -> bool {
    match SystemProcessObserver.observe(i64::from(pid)) {
        ProcessObservation::Present(fact) => {
            !fact.zombie && fact.identity.native_start == start_time
        }
        _ => false,
    }
}

fn current_process_reservation(now_ms: u64) -> MaintenanceReservation {
    let pid = std::process::id();
    let start_time = match SystemProcessObserver.observe(i64::from(pid)) {
        ProcessObservation::Present(fact) => fact.identity.native_start,
        _ => 0,
    };
    MaintenanceReservation {
        pid,
        start_time,
        reserved_unix_ms: now_ms,
    }
}

/// Claim one unit of the hour's prewarm budget under the maintenance lock.
/// Checking and consuming are one critical section, so parallel maintainers
/// cannot all observe the same remaining budget. A dead claim's reservation is
/// removed without refunding its unit: a unit is refunded only by the claiming
/// process itself, through `cancel_maintenance_reservation`, when the job never
/// launched. Losing a claimant to a crash therefore costs that hour one unit,
/// which is the safe direction.
fn claim_maintenance_budget(
    state: &Path,
    hour: u64,
    budget: u64,
    now_ms: u64,
) -> Result<Option<MaintenanceReservation>, String> {
    let reservation = current_process_reservation(now_ms);
    let owner = format!("{}:{}", reservation.pid, reservation.start_time);
    with_maintenance_state(state, |record| {
        if record.hour_budget_used.hour != hour {
            record.hour_budget_used = HourBudget { hour, count: 0 };
            record.reservations.clear();
        }
        let stale: Vec<_> = record
            .reservations
            .iter()
            .filter(|(_, claim)| {
                now_ms.saturating_sub(claim.reserved_unix_ms) > 2 * 3_600_000
                    || !process_present(claim.pid, claim.start_time)
            })
            .map(|(owner, _)| owner.clone())
            .collect();
        for owner in stale {
            // The owner is gone, so drop its reservation but keep the unit
            // spent. Its job may already have started; only the claiming
            // process may refund a unit, and only through cancellation of a
            // reservation whose job never launched.
            record.reservations.remove(&owner);
        }
        if record.hour_budget_used.count >= budget {
            return Ok(None);
        }
        record.hour_budget_used.count = record.hour_budget_used.count.saturating_add(1);
        record.reservations.insert(owner, reservation);
        Ok(Some(reservation))
    })
}

/// Remove an in-flight reservation. Only a job that never started gives its
/// hour unit back; a completed job's unit stays consumed. The guard keeps a
/// stale or refunded claim from decrementing twice.
fn clear_maintenance_reservation(state: &Path, reservation: &MaintenanceReservation, refund: bool) {
    let owner = format!("{}:{}", reservation.pid, reservation.start_time);
    let _ = with_maintenance_state(state, |record| {
        if record.reservations.remove(&owner).is_some() && refund {
            record.hour_budget_used.count = record.hour_budget_used.count.saturating_sub(1);
        }
        Ok(())
    });
}

/// The claimed job never launched, so the hour refunds the unused unit.
fn cancel_maintenance_reservation(state: &Path, reservation: &MaintenanceReservation) {
    clear_maintenance_reservation(state, reservation, true);
}

/// The claimed job launched and finished; its hour unit stays consumed.
fn finish_maintenance_reservation(state: &Path, reservation: &MaintenanceReservation) {
    clear_maintenance_reservation(state, reservation, false);
}

/// Run a read-only Git query with a clean environment.
fn git_text(git: &Path, project: &Path, args: &[&str]) -> Option<String> {
    let mut argv = vec![git.as_os_str().to_owned()];
    argv.extend(args.iter().map(OsString::from));
    let clean: Vec<(OsString, OsString)> = [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .into_iter()
    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
    .collect();
    let executor = SystemExecutor::new(1).ok()?;
    let mut options = ExecOptions::new(&argv, project, Duration::from_secs(10));
    options.env = Some(&clean);
    options.stdout = StreamPolicy::Capture { limit: 4096 };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let result = run_text(&executor, options).ok()?;
    if result.outcome.termination != Termination::Exit(0) || result.outcome.timed_out {
        return None;
    }
    let text = result.stdout.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Count admission slots that another job currently holds.
fn held_builds(state: &Path) -> Option<u64> {
    use provenant_os::admission::class_directory;
    let directory = Directory::open_anchor(&class_directory(state, "build")).ok()?;
    let mut held = 0;
    for index in 0..1024 {
        let name = OsString::from(format!("slot-{index}.lock"));
        match directory.try_lock_existing(&name, LockMode::Exclusive) {
            Ok(_guard) => {}
            Err(provenant_os::fs::FsError::LockTimeout { .. }) => held += 1,
            Err(_) => break,
        }
    }
    Some(held)
}

/// Whether this project recorded a job in the trailing 24 hours.
pub fn has_recent_job(state: &Path, project_key: &str, now_ms: u64) -> bool {
    use std::io::BufRead;
    let cutoff = now_ms.saturating_sub(86_400_000);
    let root = super::paths::build_root(state);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != "jobs.jsonl" && !(name.starts_with("jobs-") && name.ends_with(".jsonl")) {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(row) = serde_json::from_str::<super::JobOutcome>(&line)
                && row.project_key == project_key
                && row.finished_unix_ms >= cutoff
            {
                return true;
            }
        }
    }
    false
}

/// Try to take an idle slot without waiting; returns its index.
fn probe_slot(state: &Path, limit: u64) -> Option<provenant_os::admission::SlotGuard> {
    use provenant_os::admission::AdmissionSlots;
    let slots = AdmissionSlots::open(state, "build").ok()?;
    let order: Vec<u64> = (0..limit).collect();
    slots.try_acquire_ordered(&order).ok()?
}

/// A temporary detached checkout of the resolved verify base. Local strategy
/// maintenance must judge this commit, never the caller's working tree.
struct BaseCheckout {
    project: PathBuf,
    git: PathBuf,
    path: PathBuf,
}
impl Drop for BaseCheckout {
    fn drop(&mut self) {
        let path = self.path.to_string_lossy().into_owned();
        let _ = git_text(
            &self.git,
            &self.project,
            &["worktree", "remove", "--force", &path],
        );
    }
}

fn base_checkout(project: &Path, git: &Path, commit: &str) -> Option<BaseCheckout> {
    let path = std::env::temp_dir().join(format!(
        "provenant-maintenance-{}-{}",
        std::process::id(),
        provenant_os::clock::now_unix_millis()
    ));
    if path.exists() {
        return None;
    }
    let text = path.to_string_lossy().into_owned();
    git_text(
        git,
        project,
        &["worktree", "add", "--detach", &text, commit],
    )?;
    Some(BaseCheckout {
        project: project.to_path_buf(),
        git: git.to_path_buf(),
        path,
    })
}

/// Run one best-effort prewarm, or a freshness canary when the chosen slot is stale.
/// Budget, load, recency and admission are checked before any process is spawned;
/// every failure is a warning rather than a refusal.
pub fn maintain(state: &Path, home: &Path, project: &Path, now: SystemTime) -> MaintainOutcome {
    let mut outcome = MaintainOutcome::default();
    let now_ms = unix_millis(now);
    let ports = match PortTable::read_for(Some(project), state) {
        Ok(ports) => ports,
        Err(error) => {
            outcome.warning = Some(format!(
                "build maintenance configuration unavailable: {error}"
            ));
            return outcome;
        }
    };
    let budget = ports.build.prewarm_budget;
    if budget == 0 {
        outcome.warning = Some("build maintenance disabled by build.prewarm_budget=0".into());
        return outcome;
    }
    let key = provenant_config::project_key_for(project)
        .unwrap_or_else(|| provenant_config::project_key(project));
    let limit = ResolvedAdmissionClass::resolve("build", state, Some(project)).slots;
    if limit == 0 {
        outcome.warning = Some("build maintenance needs at least one build slot".into());
        return outcome;
    }
    if !has_recent_job(state, &key, now_ms) {
        outcome.warning = Some("no build job for this project in the last 24 hours".into());
        return outcome;
    }
    let cpus = std::thread::available_parallelism().map_or(1, |value| value.get()) as f64;
    let load_ceiling = (ports.verify.load_per_cpu * cpus).floor() as u64;
    match held_builds(state) {
        Some(held) if held < load_ceiling => {}
        Some(held) => {
            outcome.warning = Some(format!(
                "build load {held} is at or above the prewarm ceiling {load_ceiling}"
            ));
            return outcome;
        }
        None => {
            outcome.warning = Some("build load could not be sampled; skipping prewarm".into());
            return outcome;
        }
    }
    let hour = now_ms / 3_600_000;
    let reservation = match claim_maintenance_budget(state, hour, budget, now_ms) {
        Ok(Some(reservation)) => reservation,
        Ok(None) => {
            outcome.warning = Some("hourly build maintenance budget is spent".into());
            return outcome;
        }
        Err(error) => {
            outcome.warning = Some(format!("cannot claim build maintenance budget: {error}"));
            return outcome;
        }
    };
    let Some(slot_guard) = probe_slot(state, limit) else {
        cancel_maintenance_reservation(state, &reservation);
        outcome.warning = Some("no idle build slot for maintenance".into());
        return outcome;
    };
    let index = slot_guard.index();
    let paths = SlotPaths::new(home, &key, index);
    let canary = SlotRecord::read(&paths.record)
        .ok()
        .flatten()
        .and_then(|record| record.last_canary_unix_ms)
        .is_none_or(|last| {
            now_ms.saturating_sub(last)
                >= ports
                    .build
                    .canary_every_duration()
                    .value
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
        });
    let Some(git) = provenant_os::git::git_executable() else {
        cancel_maintenance_reservation(state, &reservation);
        outcome.warning = Some("Git executable unavailable for build maintenance".into());
        return outcome;
    };
    let base = ports.verify.base.clone();
    if base.trim().is_empty() {
        cancel_maintenance_reservation(state, &reservation);
        outcome.warning = Some("verify.base is unset; cannot pin the maintenance tree".into());
        return outcome;
    }
    let Some(commit) = git_text(
        &git,
        project,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )
    .filter(|commit| commit.bytes().all(|byte| byte.is_ascii_hexdigit())) else {
        cancel_maintenance_reservation(state, &reservation);
        outcome.warning = Some(format!("cannot resolve verify base {base} for maintenance"));
        return outcome;
    };
    let common_dir = match git_text(&git, project, &["rev-parse", "--git-common-dir"]) {
        Some(path) => {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                project.join(path)
            };
            std::fs::canonicalize(path).unwrap_or(project.to_path_buf())
        }
        None => {
            cancel_maintenance_reservation(state, &reservation);
            outcome.warning = Some("cannot locate the project's Git common directory".into());
            return outcome;
        }
    };
    let Some(checkout) = base_checkout(project, &git, &commit) else {
        cancel_maintenance_reservation(state, &reservation);
        outcome.warning = Some("cannot check out the resolved verify base for maintenance".into());
        return outcome;
    };
    let mut env = provenant_config::port_command_environment(
        std::env::vars_os().collect::<Vec<_>>(),
        &[] as &[(OsString, OsString)],
    );
    env.push((
        OsString::from("PROVENANT_BUILD_MAINTENANCE_SLOT"),
        OsString::from(index.to_string()),
    ));
    if canary {
        env.push((
            OsString::from("PROVENANT_BUILD_MAINTENANCE_CANARY"),
            OsString::from("1"),
        ));
    }
    // Commit-source jobs use this root under every strategy. Without it a
    // lane-strategy job would check the caller's working tree and could
    // report a false freshness failure for a base it never tested.
    env.push((
        OsString::from("PROVENANT_BUILD_LOCAL_ROOT"),
        checkout.path.clone().into_os_string(),
    ));
    let job = super::Job {
        id: format!(
            "maintain-{}-{}",
            std::process::id(),
            provenant_os::clock::now_unix_millis()
        ),
        project_key: key.clone(),
        project: project.to_path_buf(),
        origin: super::Origin::Maintenance,
        owner: format!("maintain-{}", std::process::id()),
        kind: super::JobKind::Maintenance { canary },
        priority: super::Priority::Prewarm,
        source: super::SnapshotSource::Commit {
            repo_common_dir: common_dir,
            commit: commit.clone(),
        },
        verify_base: Some(super::BaseRef {
            remote_ref: base,
            commit,
        }),
        steps: vec![super::Step {
            cwd_rel: PathBuf::new(),
            argv: super::rust::maintenance_argv(),
        }],
        stop_on_failure: true,
        env,
        output: super::Output::Capture {
            limit_bytes: 1024 * 1024,
        },
        timeout: Some(Duration::from_secs(
            provenant_core::MAX_LANE_RUNTIME_SECONDS,
        )),
        sandboxed: false,
        prelude: None,
    };
    let context = super::RunContext {
        state: state.to_path_buf(),
        home: home.to_path_buf(),
        strategy: super::strategy(&ports.build).value,
        cancel: CancelToken::new(),
    };
    let result = super::run_with_pinned_slot(job, &context, slot_guard);
    outcome.ran = true;
    outcome.canary = canary;
    match &result {
        Ok(result) => {
            outcome.exit_code = result.outcome.exit.map(|exit| exit.process_code());
            if let Some(warning) = result
                .outcome
                .warnings
                .iter()
                .find(|warning| warning.contains("freshness_failure"))
            {
                outcome.warning = Some(warning.clone());
            }
        }
        Err(error) => outcome.warning = Some(error.to_string()),
    }
    // The job launched, so its hour unit stays consumed. A canary's slot reset
    // and freshness stamp are applied by the slot executor itself, while it
    // still holds the custody of the exact slot it ran in.
    finish_maintenance_reservation(state, &reservation);
    if let Err(error) = with_maintenance_state(state, |record| {
        record.last_prewarm.insert(key.clone(), now_ms);
        Ok(())
    }) && outcome.warning.is_none()
    {
        outcome.warning = Some(format!("cannot record build maintenance budget: {error}"));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_budget_is_recorded_and_enforced() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        with_maintenance_state(&state, |record| {
            record.hour_budget_used = HourBudget { hour: 42, count: 0 };
            record.hour_budget_used.count += 1;
            Ok(())
        })
        .unwrap();
        let spent = with_maintenance_state(&state, |record| Ok(record.hour_budget_used.count >= 1))
            .unwrap();
        assert!(spent);
        let new_hour =
            with_maintenance_state(&state, |record| Ok(record.hour_budget_used.hour != 42))
                .unwrap();
        assert!(!new_hour);
    }

    #[test]
    fn maintenance_budget_is_claimed_before_the_job_runs() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let first = claim_maintenance_budget(&state, 7, 1, 7 * 3_600_000)
            .unwrap()
            .expect("the first worker reserves the only unit");
        let second = claim_maintenance_budget(&state, 7, 1, 7 * 3_600_000).unwrap();
        assert!(
            second.is_none(),
            "parallel workers cannot exceed the budget"
        );
        cancel_maintenance_reservation(&state, &first);
        assert!(
            claim_maintenance_budget(&state, 7, 1, 7 * 3_600_000)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn completed_maintenance_consumes_the_hourly_unit() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let now = 7 * 3_600_000;
        let first = claim_maintenance_budget(&state, 7, 1, now)
            .unwrap()
            .expect("the first worker reserves the only unit");
        // The job launched and finished: its unit stays spent for the hour.
        finish_maintenance_reservation(&state, &first);
        assert!(
            claim_maintenance_budget(&state, 7, 1, now)
                .unwrap()
                .is_none(),
            "a completed job must not refund its hourly unit"
        );

        // Only a claim whose job never started gives the unit back.
        let other = tempfile::tempdir().unwrap();
        let idle_state = other.path().join("state");
        let never_started = claim_maintenance_budget(&idle_state, 7, 1, now)
            .unwrap()
            .expect("the first worker reserves the only unit");
        cancel_maintenance_reservation(&idle_state, &never_started);
        assert!(
            claim_maintenance_budget(&idle_state, 7, 1, now)
                .unwrap()
                .is_some(),
            "a job that never launched refunds its unit"
        );
    }

    #[test]
    fn stale_recovery_keeps_the_dead_owners_unit_spent() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let now = 7 * 3_600_000;
        let dead_pid = u32::MAX;
        with_maintenance_state(&state, |record| {
            record.hour_budget_used = HourBudget { hour: 7, count: 1 };
            record.reservations.insert(
                format!("{dead_pid}:12345"),
                MaintenanceReservation {
                    pid: dead_pid,
                    start_time: 12_345,
                    reserved_unix_ms: now,
                },
            );
            Ok(())
        })
        .unwrap();

        // A new claimant finds the dead owner. Recovery drops the reservation
        // but must not refund a unit whose job may already have started.
        assert!(
            claim_maintenance_budget(&state, 7, 1, now)
                .unwrap()
                .is_none(),
            "a crashed claimant's unit stays spent for the hour"
        );
        let (count, remaining) = with_maintenance_state(&state, |record| {
            Ok((record.hour_budget_used.count, record.reservations.clone()))
        })
        .unwrap();
        assert_eq!(count, 1, "stale recovery must not refund the dead unit");
        assert!(
            remaining.values().all(|claim| claim.pid != dead_pid),
            "the dead owner's reservation must be removed"
        );
    }

    #[test]
    fn full_reset_removes_the_tmp_payload() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let paths = SlotPaths::new(&home, "project", 0);
        std::fs::create_dir_all(&paths.tmp).unwrap();
        std::fs::write(paths.tmp.join("index-copy"), b"scratch").unwrap();
        let custody = SlotCustody::acquire(&paths).unwrap();
        full_reset(&paths, &custody).unwrap();
        assert!(!paths.tmp.exists());
    }

    #[test]
    fn failure_ladder_window_and_cancellation() {
        let mut record = SlotRecord::default();
        let mut outcome = super::super::report::test_row();
        outcome.infra = Some(InfraKind::Git);
        assert_eq!(after_job(&mut record, &outcome, 1), Reset::Light);
        outcome.cancelled = true;
        assert_eq!(after_job(&mut record, &outcome, 2), Reset::None);
        outcome.cancelled = false;
        assert_eq!(after_job(&mut record, &outcome, 3), Reset::Full);
        assert_eq!(after_job(&mut record, &outcome, 4), Reset::Quarantine);
        assert_eq!(record.health.state, SlotState::Quarantined);
        let mut record = SlotRecord::default();
        assert_eq!(after_job(&mut record, &outcome, 1), Reset::Light);
        assert_eq!(after_job(&mut record, &outcome, 86_400_002), Reset::Light);
    }
    #[test]
    fn clock_rollback_requires_full_reset_without_ladder() {
        let mut record = SlotRecord {
            last_job: Some(super::super::slots::SlotJobMark {
                snapshot: "a".into(),
                base: None,
                owner: "lane".into(),
                finished_unix_ms: 100,
            }),
            ..SlotRecord::default()
        };
        assert_eq!(before_job(&mut record, 99), PreJob::FullReset);
        assert_eq!(before_job(&mut record, 100), PreJob::Proceed);
        let mut outcome = super::super::report::test_row();
        outcome.infra = Some(InfraKind::Clock);
        assert_eq!(after_job(&mut record, &outcome, 99), Reset::None);
        assert!(record.health.infra_failures_unix_ms.is_empty());
    }
    #[test]
    fn gc_skips_locked_slot() {
        use provenant_os::fs::{Directory, LockMode};
        use std::ffi::OsStr;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let paths = SlotPaths::new(&home, "project", 0);
        std::fs::create_dir_all(&paths.target).unwrap();
        std::fs::write(paths.target.join("artifact"), b"cached").unwrap();
        SlotRecord {
            schema: 1,
            project_key: "project".into(),
            index: 0,
            created_unix_ms: 1,
            ..SlotRecord::default()
        }
        .write(&paths.record)
        .unwrap();
        let dir = Directory::open_anchor(&paths.root).unwrap();
        let guard = dir
            .try_lock(OsStr::new("slot.lock"), LockMode::Exclusive)
            .unwrap();
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(900_000);
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 0);
        assert!(paths.target.join("artifact").exists());
        drop(guard);
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 1);
        assert!(!paths.target.exists());
    }

    #[test]
    fn gc_requires_live_custody_of_the_slot() {
        use provenant_os::fs::{Directory, LockMode};
        use std::ffi::OsStr;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let paths = SlotPaths::new(&home, "project", 0);
        std::fs::create_dir_all(&paths.target).unwrap();
        std::fs::write(paths.target.join("artifact"), b"cached").unwrap();
        SlotRecord {
            schema: 1,
            project_key: "project".into(),
            index: 0,
            created_unix_ms: 1,
            ..SlotRecord::default()
        }
        .write(&paths.record)
        .unwrap();
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(900_000);
        let dir = Directory::open_anchor(&paths.root).unwrap();

        // After the owner crashes, the slot lock is free but a surviving child
        // still holds the inherited custody lease. Collection must skip it.
        let lease = dir
            .try_lock(OsStr::new("intent.lock"), LockMode::Exclusive)
            .unwrap();
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 0);
        assert!(paths.target.join("artifact").exists());
        drop(lease);

        // A live running marker blocks collection even with a free lease: the
        // liveness check runs under the slot lock, not just the lease probe.
        let start = match SystemProcessObserver.observe(std::process::id() as i64) {
            ProcessObservation::Present(fact) => fact.identity.native_start,
            _ => 0,
        };
        std::fs::write(
            &paths.job,
            serde_json::json!({
                "pid": std::process::id(),
                "pgid": std::process::id() as i64,
                "start_time": start,
                "boot": "",
                "session_id": 0,
                "owner": "surviving-child",
                "state": "running"
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 0);
        assert!(paths.target.join("artifact").exists());

        // With neither a lease nor a live child the slot may be collected.
        std::fs::remove_file(&paths.job).unwrap();
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 1);
        assert!(!paths.target.exists());
    }

    #[test]
    fn gc_skips_a_slot_held_by_live_custody() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let paths = SlotPaths::new(&home, "project", 0);
        std::fs::create_dir_all(&paths.target).unwrap();
        std::fs::write(paths.target.join("artifact"), b"cached").unwrap();
        SlotRecord {
            schema: 1,
            project_key: "project".into(),
            index: 0,
            created_unix_ms: 1,
            ..SlotRecord::default()
        }
        .write(&paths.record)
        .unwrap();
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(900_000);

        // A live holder of the slot's custody blocks the reset and the record
        // unlink, even though the record itself looks idle to `status`.
        let custody = SlotCustody::acquire(&paths).unwrap();
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 0);
        assert!(paths.target.join("artifact").exists());
        assert!(paths.record.exists());

        drop(custody);
        assert_eq!(gc(&home, &state, 1, now).removed_slots, 1);
        assert!(!paths.target.exists());
        assert!(!paths.record.exists());
    }
}
