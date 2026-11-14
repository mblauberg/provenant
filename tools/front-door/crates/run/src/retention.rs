//! Temporary and provider-private state retention after terminal settlement.
use crate::{
    artifacts::{seal, subdir},
    *,
};
use provenant_core::{AttemptId, WallTime};
use provenant_os::fs::{Directory, FileKind, LockGuard, LockMode, PayloadTree};
use provenant_records::AttemptFile;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::PathBuf,
};

const DAY_MICROS: i64 = 86_400_000_000;
const EXPIRED: &str = AttemptFile::ProviderStateExpired.name();
const STATE: &str = AttemptFile::ProviderState.name();
const HISTORY: &str = AttemptFile::ProviderHistory.name();
pub const TOMBSTONE: &str = ".provider-gc-";

// These are fetched or generated again by Codex at startup. Sessions and
// history databases deliberately do not appear here. Other adapters need
// evidence of regenerability before adding a row.
const PROVIDER_CACHES: &[(&str, &[&str])] = &[(
    "codex",
    &["cache", "plugins", "skills", "models_cache.json"],
)];
const CREDENTIALS: &[&str] = &[
    "auth.json",
    ".credentials.json",
    "credentials.json",
    "credentials.toml",
    "claude-config/.credentials.json",
    "data/opencode/auth.json",
    "Library/Keychains/login.keychain-db",
];

#[derive(Debug, Default, Serialize)]
pub struct ProviderStateGc {
    pub removed: Vec<PathBuf>,
    pub tightened: Vec<PathBuf>,
    pub attempt_tmp: Vec<AttemptTmpRemoval>,
    pub attempt_logs: Vec<AttemptLogRemoval>,
    /// Event rows expired (or eligible to expire in a dry run).
    pub expired_events: u64,
    pub kept: Vec<ProviderStateIssue>,
    pub errors: Vec<ProviderStateIssue>,
    /// Byte-bounded eviction of bulk payload; see [`RunStore::gc_payloads`].
    pub payload: PayloadReport,
}

#[derive(Debug, Serialize)]
pub struct AttemptLogRemoval {
    pub path: PathBuf,
    pub bytes: u64,
    pub tail_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct AttemptTmpRemoval {
    pub path: PathBuf,
    /// Best-effort observed allocated bytes; measurement errors leave a partial estimate.
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ProviderStateIssue {
    pub path: PathBuf,
    pub reason: String,
}

impl RunStore {
    /// Retain the source until a continuation has been accepted. GC also
    /// spares sources of nonterminal continuations until their copy completes.
    pub(crate) fn retain_provider_state(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<LockGuard> {
        self.record(id, call)?;
        let run = self.attempt_dir(id, false)?;
        let lease = run.lock(
            OsStr::new("publication.lock"),
            LockMode::Shared,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        let attempt = self.attempt_dir(id, false)?;
        match attempt.metadata_at(OsStr::new(EXPIRED), false) {
            Ok(_) => return Err(RunError::ProviderStateExpired),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        Ok(lease)
    }

    /// Host maintenance: only indexed terminal attempts, across projects in
    /// this store. Only the temporary cleanup hint changes, never records or digests.
    pub fn gc_provider_state(
        &self,
        expected: &Directory,
        days: u64,
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_provider_state_with_logs(expected, days, 30, now, dry_run, call)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn gc_provider_state_with_logs(
        &self,
        expected: &Directory,
        days: u64,
        logs_days: u64,
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(expected, days, logs_days, now, dry_run, false, None, call)
    }

    /// Pressure collection is limited to expired temporary payloads.
    pub fn gc_temporary(
        &self,
        expected: &Directory,
        days: u64,
        logs_days: u64,
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(expected, days, logs_days, now, dry_run, true, None, call)
    }

    /// Pressure collection restricted to the selected project identities.
    #[allow(clippy::too_many_arguments)]
    pub fn gc_temporary_for_projects(
        &self,
        expected: &Directory,
        days: u64,
        logs_days: u64,
        now: WallTime,
        dry_run: bool,
        projects: &[provenant_core::ProjectId],
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(
            expected,
            days,
            logs_days,
            now,
            dry_run,
            true,
            Some(projects),
            call,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn gc_retention(
        &self,
        expected: &Directory,
        days: u64,
        logs_days: u64,
        now: WallTime,
        dry_run: bool,
        temporary_only: bool,
        projects: Option<&[provenant_core::ProjectId]>,
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        if !(1..=3650).contains(&logs_days) {
            return Err(RunError::Validation("logs_days_range"));
        }
        if !self.root.metadata()?.same_identity(&expected.metadata()?) {
            return Err(RunError::unsafe_path(".", PathCheck::Identity));
        }
        if !(1..=3650).contains(&days) {
            return Err(RunError::Validation("provider_state_days_range"));
        }
        if !dry_run {
            // Finished reaction rows age out with the run's logs.
            self.prune_reactions(
                now.as_micros().div_euclid(1_000_000) - (logs_days as i64) * 86_400,
                call,
            )?;
        }
        let rows: Vec<(String, String, Vec<u8>)> = {
            let mut query = self.db.prepare(
                "SELECT id,project,record FROM attempts WHERE terminal=1 AND refused=0 ORDER BY sequence",
            )?;
            let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut report = ProviderStateGc::default();
        let mut temporary_sizes = provenant_os::disk::DiskReport::with_walk_budget(
            self.context.state_root.as_path().into(),
            0,
            10_000,
            std::time::Duration::from_millis(500),
        );
        #[allow(clippy::disallowed_methods)] // Bound read-only maintenance in real time.
        let preview_started = std::time::Instant::now();
        for (id, project, raw) in rows {
            if dry_run && preview_started.elapsed() >= std::time::Duration::from_secs(1) {
                report.kept.push(ProviderStateIssue {
                    path: self.context.state_root.as_path().into(),
                    reason: "preview budget reached; remaining attempts require an applied GC pass"
                        .into(),
                });
                break;
            }
            if projects.is_some_and(|projects| !projects.iter().any(|id| id.to_string() == project))
            {
                continue;
            }
            let path = self
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{id}/provider-state"));
            let first_removal = report.removed.len();
            let first_tightened = report.tightened.len();
            let result = (|| -> Result<()> {
                let record: AttemptRecord = crate::db::decode_attempt(&raw)?;
                crate::reader::validate_attempt(&record)?;
                if record.attempt_id.to_string() != id || record.project_id.to_string() != project {
                    return Err(RunError::Validation("retention attempt index identity"));
                }
                let AttemptState::Terminal { finished_at, .. } = record.state else {
                    return Err(RunError::Validation("retention attempt is not terminal"));
                };
                if call.cancel.is_cancelled() {
                    return Err(RunError::Cancelled);
                }
                let Some(Held {
                    attempt,
                    continuing,
                    _publication,
                    _writer,
                }) = self.hold_attempt(&record, &id)?
                else {
                    return Ok(());
                };
                let age = now.as_micros().saturating_sub(finished_at.as_micros());
                let uncertain_loss = matches!(
                    record.state,
                    AttemptState::Terminal {
                        outcome: Outcome::Lost,
                        exit_code: None,
                        ..
                    }
                ) && record.exit_evidence.is_none();
                if age >= DAY_MICROS && !uncertain_loss {
                    let temporary = path.with_file_name("tmp");
                    let collection = (|| -> Result<()> {
                        collect_temporary(
                            &attempt,
                            &temporary,
                            dry_run,
                            &mut report,
                            &mut temporary_sizes,
                        )?;
                        let pending_writer_cleanup = if record.provider_launched == Some(false)
                            && matches!(
                                record.state,
                                AttemptState::Terminal {
                                    outcome: Outcome::Rejected,
                                    ..
                                }
                            )
                            && crate::artifacts::read_sealed(
                                &attempt,
                                "writer-git-pins.json",
                                crate::RECORD_LIMIT,
                            )?
                            .is_some()
                        {
                            let raw: Option<Vec<u8>> = rusqlite::OptionalExtension::optional(
                                self.db.query_row(
                                    "SELECT record FROM settlement_receipts WHERE attempt=?1 AND generation=?2",
                                    rusqlite::params![id, crate::db::sql_int(record.generation)?],
                                    |row| row.get(0),
                                ),
                            )?;
                            // Without a receipt the cleanup state is unknown: keep the
                            // attempt for reconciliation rather than failing collection.
                            match raw {
                                Some(raw) => {
                                    let receipts: provenant_records::run::SettlementReceipts =
                                        crate::db::decode_column(&raw)?;
                                    !matches!(
                                        receipts.cleanup_custody_release,
                                        provenant_records::run::EffectDisposition::Done
                                    )
                                }
                                None => true,
                            }
                        } else {
                            false
                        };
                        if !dry_run && !pending_writer_cleanup {
                            self.write(call, |tx| {
                                tx.execute(
                                    "UPDATE attempts SET tmp_retained=0 WHERE id=?1 AND project=?2 AND terminal=1",
                                    rusqlite::params![id, record.project_id.to_string()],
                                )?;
                                Ok(())
                            })?;
                        }
                        Ok(())
                    })();
                    if let Err(error) = collection {
                        record_issue(&mut report, temporary, &error);
                    }
                }
                if temporary_only {
                    return Ok(());
                }
                if continuing {
                    return Err(RunError::Validation(
                        "a continuation still needs this provider state",
                    ));
                }
                if age >= DAY_MICROS.saturating_mul(logs_days as i64) {
                    {
                        collect_logs(
                            &attempt,
                            path.parent().ok_or_else(|| {
                                RunError::unsafe_path(path.clone(), PathCheck::Containment)
                            })?,
                            dry_run,
                            &mut report,
                        )?;
                        // Keep the newest event so unseen-settlement cursors and
                        // last_event_sequence still observe terminal attempts.
                        let expired_events = if dry_run {
                            let count = self.db.query_row(
                                "SELECT COUNT(*) FROM events WHERE attempt=?1 AND project=?2 AND sequence < (SELECT MAX(sequence) FROM events WHERE attempt=?1 AND project=?2)",
                                rusqlite::params![id, record.project_id.to_string()],
                                |row| row.get::<_, i64>(0),
                            )?;
                            u64::try_from(count).unwrap_or(0)
                        } else {
                            self.write(call, |tx| {
                                let deleted = tx.execute(
                                    "DELETE FROM events WHERE attempt=?1 AND project=?2 AND sequence < (SELECT MAX(sequence) FROM events WHERE attempt=?1 AND project=?2)",
                                    rusqlite::params![id, record.project_id.to_string()],
                                )?;
                                Ok(u64::try_from(deleted).unwrap_or(u64::MAX))
                            })?
                        };
                        report.expired_events += expired_events;
                    }
                }
                if age >= DAY_MICROS
                    && let Err(error) = credentials(&attempt, &path, dry_run, &mut report)
                {
                    record_issue(&mut report, path.clone(), &error);
                }
                if let Err(error) = prune_caches(&attempt, &record, dry_run, &mut report) {
                    record_issue(&mut report, path.clone(), &error);
                }
                if age >= DAY_MICROS.saturating_mul(days as i64) {
                    expire(&attempt, dry_run, &mut report)?;
                }
                Ok(())
            })();
            let parent = path
                .parent()
                .ok_or_else(|| RunError::unsafe_path(path.clone(), PathCheck::Containment))?;
            prefix_removals(&mut report, first_removal, parent);
            for tightened in &mut report.tightened[first_tightened..] {
                *tightened = parent.join(&*tightened);
            }
            if let Err(error) = result {
                let cancelled = matches!(error, RunError::Cancelled);
                record_issue(&mut report, path, &error);
                if cancelled {
                    break;
                }
            }
        }
        Ok(report)
    }
}

/// Every custody check that protects an attempt's payload: the publication
/// and writer locks, writer reservations, abandonment and refused Git imports.
/// `None` means the attempt is pinned or its directory is gone.
struct Held {
    attempt: Directory,
    continuing: bool,
    _publication: LockGuard,
    _writer: Option<LockGuard>,
}

impl RunStore {
    fn hold_attempt(&self, record: &AttemptRecord, id: &str) -> Result<Option<Held>> {
        let run = match self.attempt_dir(record.attempt_id, false) {
            Ok(run) => run,
            Err(error) if missing_directory(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let _publication =
            run.try_lock_existing(OsStr::new("publication.lock"), LockMode::Exclusive)?;
        let continuing: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE continues=?1 AND terminal=0)",
            [&id],
            |row| row.get(0),
        )?;
        // Host retention spans attribution projects. Verify the sealed
        // request against its own indexed run before following a writer's
        // persisted source-project custody namespace.
        let custody_project =
            match crate::artifacts::read_sealed(&run, "request.json", crate::RECORD_LIMIT)? {
                Some(raw) => {
                    let indexed: String = self.db.query_row(
                        "SELECT digest FROM attempts WHERE id=?1 AND project=?2",
                        rusqlite::params![id, record.project_id.to_string()],
                        |row| row.get(0),
                    )?;
                    let digest = provenant_records::stored_record_digest("run-request", 1, &raw)?;
                    if indexed != digest.to_string() {
                        return Err(RunError::Validation("retention request identity"));
                    }
                    let request: CanonicalRequest = crate::db::decode(&raw)?;
                    request
                        .task
                        .writer
                        .as_ref()
                        .map(crate::writer::writer_project_id)
                        .transpose()?
                        .unwrap_or(record.project_id)
                }
                None => return Err(RunError::Validation("retention request unavailable")),
            };
        let reservations = self
            .writer_registry
            .join("worktrees")
            .join(custody_project.to_string());
        match Directory::open_anchor(&reservations) {
            Ok(directory) => {
                let listing = provenant_supervisor::custody::list_writer_reservations(&directory)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                if !listing.issues.is_empty()
                    || listing
                        .reservations
                        .iter()
                        .any(|reservation| reservation.scope.attempt == record.attempt_id)
                {
                    return Err(RunError::Validation(
                        "writer reservation retained or unreadable",
                    ));
                }
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        let attempt = match self.attempt_dir(record.attempt_id, false) {
            Ok(attempt) => attempt,
            Err(error) if missing_directory(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        if crate::settlement::abandoned(&attempt, Self::scope(record))?.is_some()
            || crate::settlement::queued_abandonment_pending(&attempt, record)?
            || crate::holder::object_import_refused(&attempt, Self::scope(record))?.is_some()
        {
            return Ok(None);
        }
        let _writer = match attempt.try_lock_existing(
            OsStr::new(AttemptFile::WriterLock.name()),
            LockMode::Exclusive,
        ) {
            Ok(lease) => Some(lease),
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error.into()),
        };
        match provenant_supervisor::custody::reservation_present(&attempt, Self::scope(record)) {
            Ok(true) => return Err(RunError::Validation("attempt reservation retained")),
            Ok(false) => {}
            Err(error) => return Err(error.into()),
        }
        Ok(Some(Held {
            attempt,
            continuing,
            _publication,
            _writer,
        }))
    }
}

const LEDGER: &str = "payload-ledger.json";
const LEDGER_LIMIT: u64 = 64 * 1024 * 1024;
const SCRATCH: &str = AttemptFile::Scratch.name();
const EVICTED: &str = AttemptFile::PayloadEvicted.name();
const LOG_TAIL: u64 = 64 * 1024;

/// Background size accounting. A full collection walks payloads and rewrites
/// this ledger; settlement-time pressure collection only reads it, so the
/// settlement path never walks a directory tree to learn a size.
#[derive(Debug, Default, Serialize, serde::Deserialize)]
struct Ledger {
    #[serde(default)]
    measured_micros: i64,
    #[serde(default)]
    accounted_bytes: u64,
    #[serde(default)]
    pinned_bytes: u64,
    /// Bytes reclaimed by eviction over the store's lifetime.
    #[serde(default)]
    reclaimed_bytes: u64,
    #[serde(default)]
    attempts: BTreeMap<String, LedgerEntry>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
struct LedgerEntry {
    project: String,
    finished_micros: i64,
    bytes: u64,
    /// Custody pinned it when measured: private Git objects, abandonment,
    /// reservations, a continuation or unproven loss.
    pinned: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct PayloadEviction {
    pub path: PathBuf,
    pub bytes: u64,
}

/// Outcome of the byte bound. `pinned_bytes` cannot be evicted by age or
/// pressure; `reclaimed_bytes` is this pass (planned, in a dry run) and
/// `total_reclaimed_bytes` the store's lifetime figure.
#[derive(Debug, Default, Serialize)]
pub struct PayloadReport {
    pub cap_bytes: u64,
    pub low_water_bytes: u64,
    pub accounted_bytes: u64,
    pub pinned_bytes: u64,
    pub reclaimed_bytes: u64,
    pub total_reclaimed_bytes: u64,
    pub evicted: Vec<PayloadEviction>,
    pub kept: Vec<ProviderStateIssue>,
    pub errors: Vec<ProviderStateIssue>,
}

impl PayloadReport {
    pub fn absorb(&mut self, other: Self) {
        self.cap_bytes = self.cap_bytes.max(other.cap_bytes);
        self.low_water_bytes = self.low_water_bytes.max(other.low_water_bytes);
        self.accounted_bytes = self.accounted_bytes.saturating_add(other.accounted_bytes);
        self.pinned_bytes = self.pinned_bytes.saturating_add(other.pinned_bytes);
        self.reclaimed_bytes = self.reclaimed_bytes.saturating_add(other.reclaimed_bytes);
        self.total_reclaimed_bytes = self
            .total_reclaimed_bytes
            .saturating_add(other.total_reclaimed_bytes);
        self.evicted.extend(other.evicted);
        self.kept.extend(other.kept);
        self.errors.extend(other.errors);
    }
}

/// Pinned and lifetime-reclaimed bytes from the last accounting, for the disk report.
pub fn payload_totals(root: &Directory) -> Option<(u64, u64)> {
    let ledger = read_ledger(root);
    (ledger.measured_micros != 0 || ledger.reclaimed_bytes != 0)
        .then_some((ledger.pinned_bytes, ledger.reclaimed_bytes))
}

fn read_ledger(root: &Directory) -> Ledger {
    root.read_bounded(OsStr::new(LEDGER), LEDGER_LIMIT)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

fn write_ledger(root: &Directory, ledger: &Ledger) -> Result<()> {
    let raw = serde_json::to_vec(ledger).map_err(|_| RunError::Validation("payload ledger"))?;
    root.atomic_write(
        OsStr::new(LEDGER),
        ".payload-ledger-",
        &raw,
        provenant_os::fs::PublishMode::Replace,
    )?
    .require_durable()
    .map_err(RunError::Publication)
}

fn issue(report: &mut PayloadReport, path: PathBuf, error: &RunError) {
    let entry = ProviderStateIssue {
        path,
        reason: error.to_string(),
    };
    if matches!(
        error,
        RunError::Validation(_)
            | RunError::Filesystem(provenant_os::fs::FsError::LockTimeout { .. })
    ) {
        report.kept.push(entry);
    } else {
        report.errors.push(entry);
    }
}

/// Whether the attempt's payload holds anything custody still needs. Callers
/// hold the attempt's locks.
fn pinned_payload(attempt: &Directory, record: &AttemptRecord) -> Result<bool> {
    let uncertain_loss = matches!(
        record.state,
        AttemptState::Terminal {
            outcome: Outcome::Lost,
            exit_code: None,
            ..
        }
    ) && record.exit_evidence.is_none();
    // Private Git objects live in `tmp` until their import settles.
    let private_git = match attempt.metadata_at(OsStr::new(AttemptFile::Tmp.name()), false) {
        Ok(_) => match attempt.metadata_at(OsStr::new("writer-objects.json"), false) {
            Ok(_) => true,
            Err(error) if error.is_not_found() => false,
            Err(error) => return Err(error.into()),
        },
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.into()),
    };
    Ok(uncertain_loss || private_git)
}

/// Allocated bytes of the evictable payload: provider state and history,
/// scratch, and capture bytes beyond the retained tail.
fn payload_bytes(attempt: &Directory, path: &std::path::Path) -> u64 {
    let mut measured = provenant_os::disk::DiskReport::new(path.into(), 0);
    let mut bytes = 0_u64;
    for name in [STATE, HISTORY, SCRATCH] {
        let name = OsStr::new(name);
        if attempt.metadata_at(name, false).is_ok() {
            restore_owned_temporary_directories(attempt, name);
            measured.measure_at("payload", attempt, name, &path.join(name));
        }
    }
    for entry in &measured.entries {
        bytes = bytes.saturating_add(entry.allocated_bytes);
    }
    for name in [AttemptFile::Stdout.name(), AttemptFile::Stderr.name()] {
        if let Ok(meta) = attempt.metadata_at(OsStr::new(name), false) {
            bytes = bytes.saturating_add(meta.size.saturating_sub(LOG_TAIL));
        }
    }
    bytes
}

impl RunStore {
    /// Keep bulk payload under `cap_bytes`. Over the cap, the oldest eligible
    /// payloads go first until the total is at 75% of it. Nothing younger than
    /// 24 hours goes, and neither does an attempt that holds unique private Git
    /// objects, is abandoned, reserved or a continuation source. Records,
    /// results, exits, receipts and 64 KiB capture tails stay. Each eviction
    /// runs under the attempt's publication and writer locks.
    ///
    /// `measure` walks payloads and rewrites the size ledger; leave it off on
    /// the settlement path, which then reads the ledger only.
    #[allow(clippy::too_many_arguments)]
    pub fn gc_payloads(
        &self,
        expected: &Directory,
        cap_bytes: u64,
        now: WallTime,
        dry_run: bool,
        measure: bool,
        projects: Option<&[provenant_core::ProjectId]>,
        call: &Request<'_>,
    ) -> Result<PayloadReport> {
        if !self.root.metadata()?.same_identity(&expected.metadata()?) {
            return Err(RunError::unsafe_path(".", PathCheck::Identity));
        }
        let low_water = (u128::from(cap_bytes) * 3 / 4) as u64;
        let mut report = PayloadReport {
            cap_bytes,
            low_water_bytes: low_water,
            ..Default::default()
        };
        let mut ledger = read_ledger(&self.root);
        if measure {
            self.measure_payloads(&mut ledger, now, &mut report, call)?;
        }
        let now_micros = now.as_micros();
        let young =
            |entry: &LedgerEntry| now_micros.saturating_sub(entry.finished_micros) < DAY_MICROS;
        let mut total = ledger
            .attempts
            .values()
            .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
        ledger.accounted_bytes = total;
        ledger.pinned_bytes = ledger
            .attempts
            .values()
            .filter(|entry| entry.pinned || young(entry))
            .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
        let mut changed = measure;
        if total > cap_bytes {
            let mut order: Vec<(String, LedgerEntry)> = ledger
                .attempts
                .iter()
                .filter(|(_, entry)| {
                    entry.bytes > 0
                        && !entry.pinned
                        && !young(entry)
                        && projects.is_none_or(|projects| {
                            projects.iter().any(|id| id.to_string() == entry.project)
                        })
                })
                .map(|(id, entry)| (id.clone(), entry.clone()))
                .collect();
            order.sort_by(|a, b| (a.1.finished_micros, &a.0).cmp(&(b.1.finished_micros, &b.0)));
            for (id, entry) in order {
                if total <= low_water {
                    break;
                }
                if call.cancel.is_cancelled() {
                    break;
                }
                let path = self
                    .context
                    .state_root
                    .as_path()
                    .join(format!("attempts/{id}"));
                match self.evict_attempt(&id, &entry, now, dry_run, &path) {
                    Ok(true) => {
                        total = total.saturating_sub(entry.bytes);
                        report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(entry.bytes);
                        report.evicted.push(PayloadEviction {
                            path,
                            bytes: entry.bytes,
                        });
                        if !dry_run {
                            ledger.attempts.remove(&id);
                            ledger.reclaimed_bytes =
                                ledger.reclaimed_bytes.saturating_add(entry.bytes);
                            changed = true;
                        }
                    }
                    Ok(false) => {
                        if let Some(stored) = ledger.attempts.get_mut(&id) {
                            stored.pinned = true;
                            changed = true;
                        }
                    }
                    Err(error) => issue(&mut report, path, &error),
                }
            }
            ledger.accounted_bytes = total;
            ledger.pinned_bytes = ledger
                .attempts
                .values()
                .filter(|entry| entry.pinned || young(entry))
                .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
        }
        report.accounted_bytes = ledger.accounted_bytes;
        report.pinned_bytes = ledger.pinned_bytes;
        report.total_reclaimed_bytes = ledger.reclaimed_bytes;
        if changed && !dry_run {
            write_ledger(&self.root, &ledger)?;
        }
        Ok(report)
    }

    /// Rebuild the ledger from the terminal attempts that still hold payload.
    fn measure_payloads(
        &self,
        ledger: &mut Ledger,
        now: WallTime,
        report: &mut PayloadReport,
        call: &Request<'_>,
    ) -> Result<()> {
        let rows: Vec<(String, String, Vec<u8>)> = {
            let mut query = self.db.prepare(
                "SELECT id,project,record FROM attempts WHERE terminal=1 AND refused=0 ORDER BY sequence",
            )?;
            let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut attempts = BTreeMap::new();
        for (id, project, raw) in rows {
            if call.cancel.is_cancelled() {
                return Err(RunError::Cancelled);
            }
            let path = self
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{id}"));
            let result = (|| -> Result<Option<LedgerEntry>> {
                let record: AttemptRecord = crate::db::decode_attempt(&raw)?;
                crate::reader::validate_attempt(&record)?;
                if record.attempt_id.to_string() != id || record.project_id.to_string() != project {
                    return Err(RunError::Validation("retention attempt index identity"));
                }
                let AttemptState::Terminal { finished_at, .. } = record.state else {
                    return Ok(None);
                };
                let attempt = match self.attempt_dir(record.attempt_id, false) {
                    Ok(attempt) => attempt,
                    Err(error) if missing_directory(&error) => return Ok(None),
                    Err(error) => return Err(error),
                };
                match attempt.metadata_at(OsStr::new(EVICTED), false) {
                    Ok(_) => return Ok(None),
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
                let (held, pinned) = match self.hold_attempt(&record, &id) {
                    Ok(Some(held)) => {
                        let pinned = held.continuing || pinned_payload(&held.attempt, &record)?;
                        (Some(held), pinned)
                    }
                    Ok(None) => (None, true),
                    // Reservations and held locks pin it; the payload still counts.
                    Err(RunError::Validation(_))
                    | Err(RunError::Filesystem(provenant_os::fs::FsError::LockTimeout {
                        ..
                    })) => (None, true),
                    Err(error) => return Err(error),
                };
                let bytes = payload_bytes(&attempt, &path);
                drop(held);
                Ok(Some(LedgerEntry {
                    project: project.clone(),
                    finished_micros: finished_at.as_micros(),
                    bytes,
                    pinned,
                }))
            })();
            match result {
                Ok(Some(entry)) => {
                    attempts.insert(id, entry);
                }
                Ok(None) => {}
                Err(error) => issue(report, path, &error),
            }
        }
        ledger.attempts = attempts;
        ledger.measured_micros = now.as_micros().max(1);
        Ok(())
    }

    /// Evict one attempt's payload under its locks. `false` means custody
    /// refused it on recheck and nothing changed.
    fn evict_attempt(
        &self,
        id: &str,
        entry: &LedgerEntry,
        now: WallTime,
        dry_run: bool,
        path: &std::path::Path,
    ) -> Result<bool> {
        let raw: Vec<u8> = self.db.query_row(
            "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND terminal=1",
            rusqlite::params![id, entry.project],
            |row| row.get(0),
        )?;
        let record: AttemptRecord = crate::db::decode_attempt(&raw)?;
        crate::reader::validate_attempt(&record)?;
        let AttemptState::Terminal { finished_at, .. } = record.state else {
            return Ok(false);
        };
        if record.attempt_id.to_string() != id
            || now.as_micros().saturating_sub(finished_at.as_micros()) < DAY_MICROS
        {
            return Ok(false);
        }
        let Some(held) = self.hold_attempt(&record, id)? else {
            return Ok(false);
        };
        if held.continuing || pinned_payload(&held.attempt, &record)? {
            return Ok(false);
        }
        let attempt = &held.attempt;
        if dry_run {
            return Ok(true);
        }
        // Resume is refused from here on, even if removal stops part-way.
        seal(attempt, EXPIRED, b"{}\n", 64)?;
        let mut removed = ProviderStateGc::default();
        for name in [STATE, HISTORY, SCRATCH] {
            retry_tombstones(attempt, &[name], false, &mut removed)?;
            dispose(attempt, OsStr::new(name), false, &mut removed)?;
        }
        collect_logs(attempt, path, false, &mut removed)?;
        if !removed.errors.is_empty() || !removed.kept.is_empty() {
            return Err(RunError::Validation("payload removal incomplete"));
        }
        seal(attempt, EVICTED, b"{}\n", 64)?;
        attempt.sync()?;
        Ok(true)
    }
}

fn record_issue(report: &mut ProviderStateGc, path: PathBuf, error: &RunError) {
    let issue = ProviderStateIssue {
        path,
        reason: error.to_string(),
    };
    if matches!(
        error,
        RunError::Validation(_)
            | RunError::Filesystem(provenant_os::fs::FsError::LockTimeout { .. })
    ) {
        report.kept.push(issue);
    } else {
        report.errors.push(issue);
    }
}

/// Retain captures of at most 64 KiB and all sealed result/exit/digest records.
/// Large captures move to a distinct tail name so old capture digests are never
/// silently presented as describing the truncated content.
fn collect_logs(
    attempt: &Directory,
    path: &std::path::Path,
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 * 1024;
    for name in [AttemptFile::Stdout.name(), AttemptFile::Stderr.name()] {
        let result = (|| -> Result<()> {
            let name_os = OsStr::new(name);
            let before = match attempt.metadata_at(name_os, false) {
                Ok(meta) => meta,
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            // Captures keep the provider's mode; only kind, links and owner matter.
            if let Some(check) = PathCheck::file(before).filter(|check| *check != PathCheck::Mode) {
                return Err(RunError::unsafe_entry(attempt, name_os, check));
            }
            if before.size <= TAIL {
                return Ok(());
            }
            if !dry_run {
                let mut file = attempt.open_read(name_os)?;
                if !before.unchanged(&file.metadata()?) {
                    return Err(RunError::unsafe_entry(
                        attempt,
                        name_os,
                        PathCheck::Identity,
                    ));
                }
                file.seek(SeekFrom::End(-(TAIL as i64)))?;
                let mut tail = Vec::new();
                file.take(TAIL).read_to_end(&mut tail)?;
                if !before.unchanged(&attempt.metadata_at(name_os, false)?) {
                    return Err(RunError::unsafe_entry(
                        attempt,
                        name_os,
                        PathCheck::Identity,
                    ));
                }
                attempt
                    .atomic_write(
                        OsStr::new(&format!("{name}.tail")),
                        ".log-tail-",
                        &tail,
                        provenant_os::fs::PublishMode::Replace,
                    )?
                    .require_durable()
                    .map_err(RunError::Publication)?;
                if !before.unchanged(&attempt.metadata_at(name_os, false)?) {
                    return Err(RunError::unsafe_entry(
                        attempt,
                        name_os,
                        PathCheck::Identity,
                    ));
                }
                attempt.unlink(name_os)?;
                attempt.sync()?;
            }
            report.attempt_logs.push(AttemptLogRemoval {
                path: path.join(name),
                bytes: before.size.saturating_sub(TAIL),
                tail_bytes: TAIL,
            });
            Ok(())
        })();
        if let Err(error) = result {
            record_issue(report, path.join(name), &error);
        }
    }
    Ok(())
}

fn collect_temporary(
    attempt: &Directory,
    path: &std::path::Path,
    dry_run: bool,
    report: &mut ProviderStateGc,
    preview_sizes: &mut provenant_os::disk::DiskReport,
) -> Result<()> {
    let name = OsStr::new("tmp");
    match attempt.metadata_at(name, false) {
        Ok(_) => {}
        // A crash after removing `tmp` can leave its short alias dangling.
        Err(error) if error.is_not_found() => {
            if !dry_run {
                crate::short_temporary::remove(attempt);
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    let mut measured = provenant_os::disk::DiskReport::new(path.into(), 0);
    let measured = if dry_run {
        // A preview must not repair permissions or restart its walk budget for
        // each attempt. Size errors are explicit lower bounds only.
        preview_sizes.entries.clear();
        preview_sizes
    } else {
        restore_owned_temporary_directories(attempt, name);
        &mut measured
    };
    measured.measure_at("attempt_tmp", attempt, name, path);
    for error in measured.errors.drain(..) {
        report.errors.push(ProviderStateIssue {
            path: error.path,
            reason: format!("temporary size estimate incomplete: {}", error.reason),
        });
    }
    let bytes = measured.entries.iter().fold(0_u64, |sum, entry| {
        sum.saturating_add(entry.allocated_bytes)
    });
    if !dry_run {
        crate::artifacts::remove_unreserved_terminal_temporary(attempt)?;
        attempt.sync()?;
    }
    report.attempt_tmp.push(AttemptTmpRemoval {
        path: path.into(),
        bytes,
    });
    Ok(())
}

/// A directory with no owner read/search bits cannot be traversed by the
/// estimator. Restore owner rwx through each held parent before measuring;
/// symlinks and foreign-owned directories are deliberately left alone.
fn restore_owned_temporary_directories(parent: &Directory, name: &OsStr) {
    let Ok(metadata) = parent.metadata_at(name, false) else {
        return;
    };
    if metadata.kind != FileKind::Directory
        || !parent.make_owner_accessible_at(name).unwrap_or(false)
    {
        return;
    }
    let Ok(directory) = parent.open_verified_directory(name) else {
        return;
    };
    let Ok(entries) = directory.entries() else {
        return;
    };
    for entry in entries.flatten() {
        restore_owned_temporary_directories(&directory, &entry);
    }
}

pub(crate) fn prune_caches(
    attempt: &Directory,
    record: &AttemptRecord,
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<()> {
    if !matches!(record.state, AttemptState::Terminal { .. }) {
        return Err(RunError::Nonterminal);
    }
    match attempt.metadata_at(OsStr::new(EXPIRED), false) {
        Ok(_) => return Ok(()),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }
    let Some(state) = existing(attempt, STATE)? else {
        return Ok(());
    };
    let candidates = candidates(&state)?;
    for name in &candidates {
        let candidate = subdir(&state, name, false)?;
        let provider = candidate_provider(&candidate)?;
        let first = report.removed.len();
        let removal = (|| -> Result<()> {
            if let Some((_, caches)) = PROVIDER_CACHES.iter().find(|(id, _)| *id == provider) {
                retry_tombstones(&candidate, caches, dry_run, report)?;
                for cache in *caches {
                    dispose(&candidate, OsStr::new(cache), dry_run, report)?;
                }
            }
            Ok(())
        })();
        prefix_removals(report, first, &std::path::Path::new(STATE).join(name));
        removal?;
    }
    Ok(())
}

fn missing_directory(error: &RunError) -> bool {
    matches!(error, RunError::Filesystem(error) if error.is_not_found())
}

fn candidate_provider(candidate: &Directory) -> Result<String> {
    match candidate.read_bounded(OsStr::new("provider.txt"), 64) {
        Ok(raw) => String::from_utf8(raw).map_err(|_| RunError::Validation("provider marker")),
        Err(error) if error.is_not_found() => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn candidates(state: &Directory) -> Result<Vec<OsString>> {
    Ok(names(state)?
        .into_iter()
        .filter(|name| {
            name.to_str()
                .and_then(|name| name.strip_prefix("candidate-"))
                .is_some_and(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
        .collect())
}

fn credentials(
    attempt: &Directory,
    path: &std::path::Path,
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<()> {
    let Some(state) = existing(attempt, STATE)? else {
        return Ok(());
    };
    for name in candidates(&state)? {
        let candidate = match subdir(&state, &name, false) {
            Ok(candidate) => candidate,
            Err(error) => {
                record_issue(report, path.join(&name), &error);
                continue;
            }
        };
        for credential in CREDENTIALS {
            let first = report.removed.len();
            let result = (|| -> Result<()> {
                let mut parent = candidate.try_clone()?;
                let mut parts = credential.split('/').peekable();
                while let Some(part) = parts.next() {
                    if parts.peek().is_none() {
                        retry_tombstones(&parent, &[part], dry_run, report)?;
                        dispose(&parent, OsStr::new(part), dry_run, report)?;
                    } else if let Some(child) = if part == "claude-config" {
                        existing_config(&parent, &name, dry_run, report)?
                    } else {
                        existing(&parent, part)?
                    } {
                        parent = child;
                    } else {
                        break;
                    }
                }
                Ok(())
            })();
            let credential_path = std::path::Path::new(STATE).join(&name).join(credential);
            let prefix = std::path::Path::new(STATE).join(&name).join(
                std::path::Path::new(credential).parent().ok_or_else(|| {
                    RunError::unsafe_path(
                        attempt.anchored_path().join(&credential_path),
                        PathCheck::Containment,
                    )
                })?,
            );
            prefix_removals(report, first, &prefix);
            if let Err(error) = result {
                record_issue(report, path.join(&name).join(credential), &error);
            }
        }
    }
    Ok(())
}

fn transcript(name: &str) -> bool {
    matches!(
        name,
        "sessions" | "archived_sessions" | "history.jsonl" | "session_index.jsonl"
    ) || ["thread_history_", "state_"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .and_then(|suffix| suffix.split_once(".sqlite"))
            .is_some_and(|(version, tail)| {
                !version.is_empty()
                    && version.bytes().all(|b| b.is_ascii_digit())
                    && matches!(tail, "" | "-wal" | "-shm" | "-journal")
            })
    })
}

fn expire(attempt: &Directory, dry_run: bool, report: &mut ProviderStateGc) -> Result<()> {
    retry_tombstones(attempt, &[STATE], dry_run, report)?;
    let Some(state) = existing(attempt, STATE)? else {
        return Ok(());
    };
    if !dry_run {
        seal(attempt, EXPIRED, b"{}\n", 64)?;
        let history = subdir(attempt, OsStr::new(HISTORY), true)?;
        let candidate_names = candidates(&state)?;
        // Whole candidates may already be archived by an interrupted sweep.
        for name in &candidate_names {
            let candidate = subdir(&state, name, false)?;
            let provider = candidate_provider(&candidate)?;
            if !matches!(
                provider.as_str(),
                "codex" | "claude" | "cursor" | "opencode"
            ) {
                // Until an adapter's transcript layout is established, keep
                // its unclassified payload as history rather than lose it.
                state.rename_noreplace(name, &history, name)?;
                history.sync()?;
                state.sync()?;
                continue;
            }
            let archive = subdir(&history, name, true)?;
            for part in names(&candidate)? {
                if part.to_str().is_some_and(transcript) {
                    candidate.rename_noreplace(&part, &archive, &part)?;
                    archive.sync()?;
                    candidate.sync()?;
                }
            }
            for (parent, artifact) in [(".cursor", "chats"), ("data", "opencode")] {
                if let Some(source) = existing(&candidate, parent)? {
                    match source.metadata_at(OsStr::new(artifact), false) {
                        Ok(_) => {
                            if artifact == "opencode"
                                && let Some(data) = existing(&source, artifact)?
                            {
                                // The launcher places an authentication link here.
                                // Never archive it if earlier credential cleanup was refused.
                                let first = report.removed.len();
                                dispose(&data, OsStr::new("auth.json"), false, report)?;
                                prefix_removals(
                                    report,
                                    first,
                                    &PathBuf::from(STATE).join(name).join(parent).join(artifact),
                                );
                            }
                            let target = subdir(&archive, OsStr::new(parent), true)?;
                            source.rename_noreplace(
                                OsStr::new(artifact),
                                &target,
                                OsStr::new(artifact),
                            )?;
                            target.sync()?;
                            source.sync()?;
                        }
                        Err(error) if error.is_not_found() => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if let Some(config) = existing_config(&candidate, name, dry_run, report)? {
                match config.metadata_at(OsStr::new("projects"), false) {
                    Ok(_) => {
                        let archived_config = subdir(&archive, OsStr::new("claude-config"), true)?;
                        config.rename_noreplace(
                            OsStr::new("projects"),
                            &archived_config,
                            OsStr::new("projects"),
                        )?;
                        archived_config.sync()?;
                        config.sync()?;
                    }
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    dispose(attempt, OsStr::new(STATE), dry_run, report)
}

fn prefix_removals(report: &mut ProviderStateGc, first: usize, prefix: &std::path::Path) {
    for path in &mut report.removed[first..] {
        *path = prefix.join(&*path);
    }
}

fn names(directory: &Directory) -> Result<Vec<OsString>> {
    Ok(directory
        .entries()?
        .collect::<provenant_os::fs::Result<_>>()?)
}

fn existing(parent: &Directory, name: &str) -> Result<Option<Directory>> {
    match subdir(parent, OsStr::new(name), false) {
        Ok(directory) => Ok(Some(directory)),
        Err(RunError::Filesystem(error)) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
}

fn existing_config(
    parent: &Directory,
    candidate: &OsStr,
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<Option<Directory>> {
    let directory = match parent.open_verified_directory(OsStr::new("claude-config")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = directory.metadata()?;
    if metadata.uid != provenant_os::fs::effective_uid() {
        return Err(RunError::unsafe_entry(
            parent,
            OsStr::new("claude-config"),
            PathCheck::Owner,
        ));
    }
    if metadata.mode & 0o077 != 0 {
        if !dry_run {
            directory.make_private()?;
        }
        report.tightened.push(
            std::path::Path::new(STATE)
                .join(candidate)
                .join("claude-config"),
        );
    }
    if !dry_run {
        crate::db::private(&directory)?;
    }
    Ok(Some(directory))
}

fn dispose(
    parent: &Directory,
    name: &OsStr,
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<()> {
    let metadata = match parent.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !dry_run {
        if metadata.kind == FileKind::Directory {
            let detached = OsString::from(format!(
                "{TOMBSTONE}{:016x}{:016x}-{}",
                metadata.device,
                metadata.inode,
                name.to_string_lossy()
            ));
            parent.rename_noreplace(name, parent, &detached)?;
            parent.sync()?;
            remove_detached(parent, &detached, metadata.device, metadata.inode)?;
        } else {
            parent.unlink(name)?;
            parent.sync()?;
        }
    }
    report.removed.push(PathBuf::from(name));
    Ok(())
}

fn retry_tombstones(
    parent: &Directory,
    allowed: &[&str],
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<()> {
    for name in names(parent)? {
        let Some((identity, original)) = name
            .to_str()
            .and_then(|name| name.strip_prefix(TOMBSTONE))
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        if identity.len() != 32
            || !identity.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !allowed.contains(&original)
        {
            continue;
        }
        let (Ok(device), Ok(inode)) = (
            u64::from_str_radix(&identity[..16], 16),
            u64::from_str_radix(&identity[16..], 16),
        ) else {
            continue;
        };
        if !dry_run {
            remove_detached(parent, &name, device, inode)?;
        }
        report.removed.push(PathBuf::from(name));
    }
    Ok(())
}

fn remove_detached(parent: &Directory, name: &OsStr, device: u64, inode: u64) -> Result<()> {
    let metadata = parent.metadata_at(name, false)?;
    if metadata.device != device || metadata.inode != inode {
        return Err(RunError::unsafe_entry(parent, name, PathCheck::Identity));
    }
    let directory = parent.open_verified_directory(name)?;
    if !directory.metadata()?.same_identity(&metadata) {
        return Err(RunError::unsafe_entry(parent, name, PathCheck::Identity));
    }
    directory.make_private()?;
    let tree = PayloadTree::claim(parent, name, &metadata)?;
    remove_contents(&tree, 0)?;
    if !parent.metadata_at(name, false)?.same_identity(&metadata) {
        return Err(RunError::unsafe_entry(parent, name, PathCheck::Identity));
    }
    parent.rmdir(name)?;
    parent.sync()?;
    Ok(())
}

fn remove_contents(tree: &PayloadTree, depth: usize) -> Result<()> {
    if depth > 256 {
        return Err(RunError::Validation("provider state depth limit"));
    }
    tree.make_writable()?;
    let entries = tree
        .entries()?
        .collect::<provenant_os::fs::Result<Vec<_>>>()?;
    for name in entries {
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            remove_contents(&tree.child(&name)?, depth + 1)?;
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::{CancelToken, WallTime};
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
    };
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn settlement_prunes_codex_caches_and_keeps_resume_state() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let attempt = context
            .state_root
            .as_path()
            .join(format!("attempts/{}", input.attempt_id));
        let candidate = attempt.join("provider-state/candidate-0");
        fs::create_dir_all(candidate.join("sessions"))?;
        fs::set_permissions(
            attempt.join("provider-state"),
            fs::Permissions::from_mode(0o700),
        )?;
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
        fs::write(candidate.join("provider.txt"), "codex")?;
        for name in ["cache", "plugins", "skills"] {
            fs::create_dir(candidate.join(name))?;
            fs::set_permissions(candidate.join(name), fs::Permissions::from_mode(0o755))?;
            fs::write(candidate.join(name).join("payload.lock"), "cache")?;
        }
        fs::write(candidate.join("models_cache.json"), "cache")?;
        fs::write(candidate.join("sessions/session.jsonl"), "transcript")?;
        fs::write(candidate.join("thread_history_1.sqlite"), "history")?;
        fs::write(candidate.join("auth.json"), "credential")?;
        store.withdraw(
            input.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(200)?,
            &call,
        )?;
        for name in ["cache", "plugins", "skills", "models_cache.json"] {
            assert!(!candidate.join(name).exists(), "{name} survived settlement");
        }
        for name in [
            "sessions/session.jsonl",
            "thread_history_1.sqlite",
            "auth.json",
        ] {
            assert!(candidate.join(name).exists(), "{name} was removed");
        }
        Ok(())
    }
    type Seeded = (
        tempfile::TempDir,
        ExecutionContext,
        RunStore,
        PreparedRun,
        std::path::PathBuf,
    );

    fn seeded() -> std::result::Result<Seeded, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let attempt = context
            .state_root
            .as_path()
            .join(format!("attempts/{}", input.attempt_id));
        for path in [
            "provider-state/candidate-0/sessions",
            "provider-state/candidate-0/claude-config/projects",
            "provider-state/candidate-1",
        ] {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(attempt.join(path))?;
        }
        let candidate = attempt.join("provider-state/candidate-0");
        for (name, content) in [
            ("provider.txt", "codex"),
            ("sessions/session.jsonl", "transcript"),
            ("thread_history_1.sqlite", "history"),
            ("thread_history_1.sqlite-wal", "wal"),
            ("state_5.sqlite", "state history"),
            ("state_5.sqlite-shm", "shm"),
            ("claude-config/projects/session.jsonl", "claude transcript"),
            ("auth.json", "credential"),
            ("claude-config/.credentials.json", "credential"),
            ("other", "disposable"),
        ] {
            fs::write(candidate.join(name), content)?;
        }
        fs::write(
            attempt.join("provider-state/candidate-1/provider.txt"),
            "agy",
        )?;
        fs::write(
            attempt.join("provider-state/candidate-1/unknown-session"),
            "unknown transcript",
        )?;
        fs::write(
            attempt.join("provider-state/candidate-1/auth.json"),
            "credential",
        )?;
        Ok((temp, context, store, input, attempt))
    }

    fn withdraw(
        store: &RunStore,
        input: &PreparedRun,
        call: &Request<'_>,
    ) -> crate::Result<AttemptRecord> {
        store
            .withdraw(
                input.attempt_id,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                WallTime::from_micros(200).map_err(|_| RunError::Validation("fixture time"))?,
                call,
            )?
            .ok_or(RunError::Conflict)
    }

    fn sweep(
        store: &RunStore,
        context: &ExecutionContext,
        days: u64,
        age: i64,
        dry_run: bool,
        call: &Request<'_>,
    ) -> crate::Result<ProviderStateGc> {
        store.gc_provider_state(
            &Directory::open_anchor(context.state_root.as_path())?,
            days,
            WallTime::from_micros(200 + age).map_err(|_| RunError::Validation("fixture time"))?,
            dry_run,
            call,
        )
    }

    #[test]
    fn gc_repairs_open_claude_config_and_expires_state() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let config = attempt.join("provider-state/candidate-0/claude-config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o755))?;

        let preview = sweep(&store, &context, 7, DAY_MICROS, true, &call)?;
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        assert_eq!(preview.tightened, vec![config.clone()]);
        assert_eq!(fs::metadata(&config)?.permissions().mode() & 0o777, 0o755);
        assert!(config.join(".credentials.json").exists());
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.tightened, vec![config.clone()]);
        assert_eq!(fs::metadata(&config)?.permissions().mode() & 0o777, 0o700);
        assert!(!config.join(".credentials.json").exists());
        let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join("provider-state").exists());
        Ok(())
    }

    #[test]
    fn gc_expiry_sweep_tightens_open_claude_config_itself() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let config = attempt.join("provider-state/candidate-0/claude-config");
        let other = attempt.join("provider-state/candidate-1/claude-config");
        fs::create_dir(&other)?;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(&other, fs::Permissions::from_mode(0o707))?;

        let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.tightened.contains(&config), "{:?}", report.tightened);
        assert!(report.tightened.contains(&other), "{:?}", report.tightened);
        assert!(!attempt.join("provider-state").exists());
        Ok(())
    }

    #[test]
    fn gc_continues_after_unsafe_credential_path() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let candidate = attempt.join("provider-state/candidate-0");
        let config = candidate.join("claude-config");
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join(".credentials.json"), "outside")?;
        fs::remove_dir_all(&config)?;
        std::os::unix::fs::symlink(outside.path(), &config)?;
        fs::create_dir(candidate.join("cache"))?;
        fs::write(candidate.join("cache/item"), "cache")?;

        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(!report.errors.is_empty());
        assert!(
            !attempt
                .join("provider-state/candidate-1/auth.json")
                .exists()
        );
        assert!(!candidate.join("cache").exists());
        assert_eq!(
            fs::read_to_string(outside.path().join(".credentials.json"))?,
            "outside"
        );
        Ok(())
    }

    #[test]
    fn gc_logs_keep_tail_and_evidence_and_obey_age_custody_and_preview() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let mut capture = vec![1; 200_000];
        capture[200_000 - 65_536..].fill(2);
        fs::write(attempt.join("stdout.log"), &capture)?;
        fs::write(attempt.join("stderr.log"), "small capture")?;

        assert!(
            sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?
                .attempt_logs
                .is_empty()
        );
        withdraw(&store, &input, &call)?;
        fs::write(attempt.join("result.md"), "result evidence")?;
        fs::write(attempt.join("process-exit.json"), "exit evidence")?;
        assert!(
            sweep(&store, &context, 7, 30 * DAY_MICROS - 1, false, &call)?
                .attempt_logs
                .is_empty()
        );
        let preview = sweep(&store, &context, 7, 30 * DAY_MICROS, true, &call)?;
        assert_eq!(preview.attempt_logs.len(), 1);
        assert_eq!(preview.attempt_logs[0].bytes, 200_000 - 65_536);
        assert_eq!(fs::read(attempt.join("stdout.log"))?, capture);
        assert!(!attempt.join("stdout.log.tail").exists());
        let directory = Directory::open_anchor(&attempt)?;
        let lease = directory.lock(
            OsStr::new("writer.lock"),
            LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )?;
        assert!(
            sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?
                .attempt_logs
                .is_empty()
        );
        drop(lease);
        let report = sweep(&store, &context, 7, 30 * DAY_MICROS, false, &call)?;
        assert_eq!(report.attempt_logs.len(), 1);
        assert!(!attempt.join("stdout.log").exists());
        assert_eq!(fs::read(attempt.join("stdout.log.tail"))?, vec![2; 65_536]);
        assert_eq!(
            fs::read_to_string(attempt.join("stderr.log"))?,
            "small capture"
        );
        assert_eq!(
            fs::read_to_string(attempt.join("result.md"))?,
            "result evidence"
        );
        assert_eq!(
            fs::read_to_string(attempt.join("process-exit.json"))?,
            "exit evidence"
        );
        assert!(
            sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?
                .attempt_logs
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn gc_stderr_tail_remains_readable_and_reports_truncation() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let directory = Directory::open_anchor(&attempt)?;
        let capture = vec![b'x'; 200_000];
        crate::artifacts::seal(&directory, "stderr.log", &capture, 300_000)?;
        crate::artifacts::seal(&directory, "stderr.log.tail", b"older tail", 65_536)?;
        let read = || {
            store.read_output(
                input.attempt_id,
                crate::ArtifactPart::Stderr,
                crate::ByteRange {
                    offset: 0,
                    max_bytes: 65_536,
                },
                &context,
                &call,
            )
        };
        let original = read()?;
        assert!(original.complete);
        assert_eq!(original.len, 200_000);
        assert_eq!(original.bytes, vec![b'x'; 65_536]);
        let report = sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?;
        assert_eq!(report.attempt_logs.len(), 1, "{report:?}");
        let tail = read()?;
        assert!(!tail.complete);
        assert!(tail.eof);
        assert_eq!(tail.len, 65_536);
        assert_eq!(tail.bytes, vec![b'x'; 65_536]);
        assert_eq!(
            tail.digest,
            provenant_records::CanonicalV1::file_digest(&tail.bytes)
        );
        Ok(())
    }

    #[test]
    fn gc_expires_closed_run_events_after_log_retention() -> TestResult {
        let (_temp, context, store, input, _attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let id = input.attempt_id;
        withdraw(&store, &input, &call)?;

        let events_before = store.events(0, 64, Some(id), &call)?;
        assert!(
            events_before.len() > 1,
            "seeded terminal attempt must record multiple events"
        );

        // Younger than logs_days (e.g. 29 days): events survive.
        let young = sweep(&store, &context, 7, 30 * DAY_MICROS - 1, false, &call)?;
        assert_eq!(young.expired_events, 0);
        assert_eq!(
            store.events(0, 64, Some(id), &call)?.len(),
            events_before.len()
        );

        // Dry run at logs_days (30 days): events survive.
        let preview = sweep(&store, &context, 7, 30 * DAY_MICROS, true, &call)?;
        assert_eq!(preview.expired_events, (events_before.len() - 1) as u64);
        assert_eq!(
            store.events(0, 64, Some(id), &call)?.len(),
            events_before.len()
        );

        // Applied sweep expires intermediate events, retaining the newest row.
        let applied = sweep(&store, &context, 7, 30 * DAY_MICROS, false, &call)?;
        assert_eq!(applied.expired_events, preview.expired_events);
        let retained = store.events(0, 64, Some(id), &call)?;
        assert_eq!(retained.len(), 1);
        assert_eq!(
            retained[0].sequence,
            events_before.last().ok_or("last event")?.sequence
        );
        for dry_run in [true, false] {
            assert_eq!(
                sweep(&store, &context, 7, 31 * DAY_MICROS, dry_run, &call)?.expired_events,
                0
            );
        }
        Ok(())
    }

    #[test]
    fn gc_keeps_expired_unacknowledged_settlements_visible() -> TestResult {
        let (_temp, context, store, input, _attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let id = input.attempt_id;
        let selection = "retention-unseen";
        // Save a cursor before settlement, then leave the settlement unacknowledged.
        store.ack_seen(selection, store.last_event_sequence(id, &call)?, &call)?;
        withdraw(&store, &input, &call)?;
        let newest = store.last_event_sequence(id, &call)?;
        let cursor = store.seen_cursor(selection, false, &call)?;
        assert!(newest > cursor);
        sweep(&store, &context, 7, 30 * DAY_MICROS, false, &call)?;
        let mut query = LaneQuery {
            targets: vec![],
            all: true,
            limit: 10,
            after: 0,
            newest_first: false,
            owner: Some(context.actor_id),
            unseen_after: Some(cursor),
        };
        // Exercise both the owned and explicit-target unseen filters.
        for targets in [vec![], vec![id]] {
            query.targets = targets;
            let page = store.lanes(&query, &call)?;
            assert_eq!(page.lanes.len(), 1, "expired settlement must remain unseen");
            assert_eq!(page.lanes[0].attempt_id, id);
            assert_eq!(store.last_event_sequence(id, &call)?, newest);
            store.ack_seen(selection, newest, &call)?;
            query.unseen_after = Some(store.seen_cursor(selection, false, &call)?);
            assert!(store.lanes(&query, &call)?.lanes.is_empty());
            query.unseen_after = Some(cursor);
        }
        Ok(())
    }

    #[test]
    fn pressure_only_collects_temporary_and_keeps_captures_and_provider_state() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        fs::write(attempt.join("tmp/payload"), vec![1; 8192])?;
        fs::write(attempt.join("stdout.log"), vec![1; 200_000])?;
        let scoped = store.gc_temporary_for_projects(
            &Directory::open_anchor(context.state_root.as_path())?,
            7,
            30,
            WallTime::from_micros(200 + 31 * DAY_MICROS)?,
            false,
            &[provenant_core::ProjectId::from_bytes([99; 16])],
            &call,
        )?;
        assert!(scoped.attempt_tmp.is_empty());
        assert!(attempt.join("tmp/payload").is_file());
        let report = store.gc_temporary(
            &Directory::open_anchor(context.state_root.as_path())?,
            7,
            30,
            WallTime::from_micros(200 + 31 * DAY_MICROS)?,
            false,
            &call,
        )?;
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.attempt_tmp.len(), 1);
        assert!(report.attempt_logs.is_empty());
        assert!(
            attempt
                .join("provider-state/candidate-0/auth.json")
                .exists()
        );
        assert_eq!(fs::metadata(attempt.join("stdout.log"))?.len(), 200_000);
        assert!(!attempt.join("tmp").exists());
        Ok(())
    }

    #[test]
    fn gc_logs_use_configured_age() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let attempt = context
            .state_root
            .as_path()
            .join(format!("attempts/{}", input.attempt_id));
        withdraw(&store, &input, &call)?;
        fs::write(attempt.join("stdout.log"), vec![1; 200_000])?;
        let root = Directory::open_anchor(context.state_root.as_path())?;
        let now = WallTime::from_micros(200 + 2 * DAY_MICROS)?;
        assert!(
            store
                .gc_provider_state_with_logs(&root, 7, 3, now, false, &call)?
                .attempt_logs
                .is_empty(),
            "logs younger than the configured age stay"
        );
        assert_eq!(
            store
                .gc_provider_state_with_logs(&root, 7, 1, now, false, &call)?
                .attempt_logs
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn gc_logs_refuse_symlinks() -> TestResult {
        {
            let (temp, context, store, input, attempt) = seeded()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            withdraw(&store, &input, &call)?;
            let outside = temp.path().join("outside");
            fs::write(&outside, vec![3; 200_000])?;
            std::os::unix::fs::symlink(&outside, attempt.join("stdout.log"))?;
            let report = sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?;
            assert!(report.attempt_logs.is_empty());
            assert!(!report.errors.is_empty());
            assert!(
                !attempt.join("provider-state").exists(),
                "log refusal must not block provider expiry"
            );
            assert_eq!(fs::read(outside)?, vec![3; 200_000]);
        }
        Ok(())
    }

    #[test]
    fn gc_preserves_tmp_for_lost_attempt_without_exit_evidence() -> TestResult {
        use provenant_core::Clock;
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let record = store.record(input.attempt_id, &call)?;
        store.commit_lost(RunStore::scope(&record), "fixture", &call)?;
        let temporary = attempt.join("tmp");
        fs::DirBuilder::new().mode(0o700).create(&temporary)?;
        fs::write(temporary.join("payload"), b"scratch")?;
        let report = store.gc_provider_state(
            &Directory::open_anchor(context.state_root.as_path())?,
            7,
            WallTime::from_micros(clock.wall_now()?.as_micros() + DAY_MICROS)?,
            false,
            &call,
        )?;
        assert!(report.attempt_tmp.is_empty());
        assert!(temporary.exists());
        Ok(())
    }

    #[test]
    fn gc_reclaims_only_old_terminal_tmp_and_previews_paths_and_bytes() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let temporary = attempt.join("tmp");
        fs::DirBuilder::new().mode(0o700).create(&temporary)?;
        fs::write(temporary.join("payload"), vec![1; 8192])?;
        assert!(
            sweep(&store, &context, 7, 100 * DAY_MICROS, false, &call)?
                .attempt_tmp
                .is_empty()
        );
        assert!(temporary.exists());
        withdraw(&store, &input, &call)?;
        // Recreate the leak after settlement's best-effort removal.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&temporary)?;
        fs::write(temporary.join("payload"), vec![1; 8192])?;
        fs::create_dir(temporary.join("candidate-0"))?;
        fs::write(temporary.join("candidate-0/payload.lock"), "payload")?;
        assert!(
            sweep(&store, &context, 7, DAY_MICROS - 1, false, &call)?
                .attempt_tmp
                .is_empty()
        );
        assert!(temporary.exists());
        let preview = sweep(&store, &context, 7, DAY_MICROS, true, &call)?;
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        assert_eq!(preview.attempt_tmp.len(), 1);
        assert_eq!(preview.attempt_tmp[0].path, temporary);
        assert!(preview.attempt_tmp[0].bytes >= 8192);
        assert!(temporary.join("candidate-0/payload.lock").exists());
        let applied = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(applied.errors.is_empty(), "{:?}", applied.errors);
        assert_eq!(applied.attempt_tmp.len(), 1);
        assert_eq!(applied.attempt_tmp[0].path, temporary);
        assert_eq!(applied.attempt_tmp[0].bytes, preview.attempt_tmp[0].bytes);
        assert!(!temporary.exists());
        assert!(attempt.join("terminal-pending.json").exists());
        assert!(
            sweep(&store, &context, 7, DAY_MICROS, false, &call)?
                .attempt_tmp
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn temporary_preview_preserves_directory_permissions() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir()?;
        let temporary = fixture.path().join("tmp");
        std::fs::create_dir(&temporary)?;
        std::fs::write(temporary.join("keep"), b"diagnostics")?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o500))?;
        let attempt = Directory::open_anchor(fixture.path())?;
        let mut report = ProviderStateGc::default();
        let mut sizes = provenant_os::disk::DiskReport::with_walk_budget(
            fixture.path().into(),
            0,
            100,
            std::time::Duration::from_secs(1),
        );
        collect_temporary(&attempt, &temporary, true, &mut report, &mut sizes)?;
        assert_eq!(
            std::fs::metadata(&temporary)?.permissions().mode() & 0o777,
            0o500
        );
        assert_eq!(std::fs::read(temporary.join("keep"))?, b"diagnostics");
        assert_eq!(report.attempt_tmp.len(), 1);
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    #[test]
    fn gc_incomplete_tmp_measurement_does_not_block_removal_or_provider_retention() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let temporary = attempt.join("tmp");
        fs::DirBuilder::new().mode(0o700).create(&temporary)?;
        fs::write(temporary.join("payload"), vec![1; 8192])?;
        // Exceed the measurement depth budget without a million-entry fixture.
        let mut deepest = temporary.clone();
        for _ in 0..258 {
            deepest.push("d");
            fs::create_dir(&deepest)?;
        }
        let candidate = attempt.join("provider-state/candidate-0");
        fs::create_dir(candidate.join("cache"))?;
        fs::write(candidate.join("cache/payload"), "regenerable")?;
        let preview = sweep(&store, &context, 7, DAY_MICROS, true, &call)?;
        assert_eq!(preview.attempt_tmp.len(), 1);
        assert!(preview.attempt_tmp[0].bytes >= 8192);
        assert!(!preview.errors.is_empty());
        assert!(temporary.exists());
        let applied = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert_eq!(applied.attempt_tmp.len(), 1);
        assert!(applied.attempt_tmp[0].bytes >= 8192);
        assert!(!applied.errors.is_empty());
        assert!(!temporary.exists());
        assert!(!candidate.join("auth.json").exists());
        assert!(!candidate.join("cache").exists());
        assert_eq!(
            fs::read(candidate.join("sessions/session.jsonl"))?,
            b"transcript"
        );
        Ok(())
    }

    #[test]
    fn gc_refreshes_the_hint_update_budget_after_collection() -> TestResult {
        use provenant_core::{Clock, MonoTime, TimeError};
        // Each DB observation completes within its own five-second budget,
        // while time between observations exceeds the original deadline.
        struct AdvancingClock(std::sync::Mutex<u64>);
        impl Clock for AdvancingClock {
            fn mono_now(&self) -> MonoTime {
                let mut value = self.0.lock().unwrap();
                let now = *value;
                *value += 2_000_000;
                MonoTime::from_micros(now)
            }
            fn wall_now(&self) -> std::result::Result<WallTime, TimeError> {
                WallTime::from_micros(200 + DAY_MICROS)
            }
            fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), TimeError> {
                let mut value = self.0.lock().unwrap();
                *value = (*value).max(deadline.as_micros());
                Ok(())
            }
        }
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let id = input.attempt_id;
        withdraw(&store, &input, &call)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        store.set_temporary_retained(id, true, &call)?;
        let slow = AdvancingClock(std::sync::Mutex::new(0));
        let call = Request {
            clock: &slow,
            cancel: &cancel,
        };
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.kept.is_empty(), "{:?}", report.kept);
        assert!(!attempt.join("tmp").exists());
        let retained: bool = store.db.query_row(
            "SELECT tmp_retained FROM attempts WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert!(!retained);
        Ok(())
    }

    #[test]
    fn gc_tmp_removal_refusal_does_not_block_credentials_caches_or_expiry() -> TestResult {
        for days in [1, 7] {
            let (_temp, context, store, input, attempt) = seeded()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            withdraw(&store, &input, &call)?;
            let id = input.attempt_id;
            store.set_temporary_retained(id, true, &call)?;
            // Payload disposal refuses a root accessible to other users.
            let temporary = attempt.join("tmp");
            fs::DirBuilder::new().mode(0o755).create(&temporary)?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
            let candidate = attempt.join("provider-state/candidate-0");
            fs::create_dir(candidate.join("cache"))?;
            fs::write(candidate.join("cache/payload"), "regenerable")?;
            let report = sweep(&store, &context, days, DAY_MICROS, false, &call)?;
            assert!(
                report
                    .errors
                    .iter()
                    .chain(&report.kept)
                    .any(|issue| issue.path == temporary)
            );
            assert!(temporary.exists());
            assert!(!candidate.join("auth.json").exists());
            assert!(!candidate.join("cache").exists());
            assert_eq!(attempt.join(EXPIRED).exists(), days == 1);
            let retained: bool = store.db.query_row(
                "SELECT tmp_retained FROM attempts WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?;
            assert!(retained);
        }
        Ok(())
    }

    #[test]
    fn gc_clears_temporary_retained_only_after_applied_removal_across_projects() -> TestResult {
        let (_temp, mut context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let id = input.attempt_id;
        let terminal = withdraw(&store, &input, &call)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        store.set_temporary_retained(id, true, &call)?;
        drop(store);
        // Host GC covers all indexed projects, not just the opening project.
        context.project_id = provenant_core::ProjectId::from_bytes([99; 16]);
        let store = RunStore::open(&context, &call)?;
        let retained = || -> crate::Result<bool> {
            Ok(store.db.query_row(
                "SELECT tmp_retained FROM attempts WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?)
        };
        sweep(&store, &context, 7, DAY_MICROS, true, &call)?;
        assert!(retained()?);
        sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(!attempt.join("tmp").exists());
        assert!(!retained()?);
        let raw: Vec<u8> = store.db.query_row(
            "SELECT record FROM attempts WHERE id=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        // The table row stores the decision; receipts are projected on read.
        let mut stored = terminal;
        stored.settlement_receipts = None;
        assert_eq!(crate::db::decode_attempt(&raw)?, stored);
        Ok(())
    }

    #[test]
    fn gc_expires_credentials_at_one_day_and_archives_history_at_configured_age() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let terminal = withdraw(&store, &input, &call)?;
        let candidate = attempt.join("provider-state/candidate-0");
        let before = fs::read(attempt.join("terminal-pending.json"))?;
        fs::write(attempt.join("process-exit.json"), "exit")?;
        fs::write(attempt.join("stdout.log"), "capture")?;
        let report = sweep(&store, &context, 7, DAY_MICROS - 1, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(candidate.join("auth.json").exists());
        let report = sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.removed.contains(&candidate.join("auth.json")));
        assert!(candidate.join("auth.json").exists());
        assert!(!attempt.join(EXPIRED).exists());
        assert!(!attempt.join("provider-history").exists());
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!candidate.join("auth.json").exists());
        assert!(!candidate.join("claude-config/.credentials.json").exists());
        assert!(candidate.join("sessions/session.jsonl").exists());
        assert!(
            sweep(&store, &context, 7, 7 * DAY_MICROS - 1, false, &call)?
                .removed
                .is_empty()
        );
        let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join("provider-state").exists());
        let history = attempt.join("provider-history/candidate-0");
        for name in [
            "sessions/session.jsonl",
            "thread_history_1.sqlite",
            "thread_history_1.sqlite-wal",
            "state_5.sqlite",
            "state_5.sqlite-shm",
            "claude-config/projects/session.jsonl",
        ] {
            assert!(history.join(name).exists(), "{name} lost");
        }
        assert!(!history.join("other").exists());
        assert!(
            attempt
                .join("provider-history/candidate-1/unknown-session")
                .exists()
        );
        assert!(
            !attempt
                .join("provider-history/candidate-1/auth.json")
                .exists()
        );
        assert_eq!(fs::read(attempt.join("terminal-pending.json"))?, before);
        assert_eq!(
            fs::read_to_string(attempt.join("process-exit.json"))?,
            "exit"
        );
        assert_eq!(fs::read_to_string(attempt.join("stdout.log"))?, "capture");
        assert_eq!(store.record(terminal.attempt_id, &call)?, terminal);
        assert!(matches!(
            store.retain_provider_state(terminal.attempt_id, &call),
            Err(RunError::ProviderStateExpired)
        ));
        assert!(
            sweep(&store, &context, 7, 8 * DAY_MICROS, false, &call)?
                .removed
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn gc_archives_cursor_chats_before_disposing_private_state() -> TestResult {
        relocated_history_lifecycle(
            "cursor",
            ".cursor/chats",
            &["project/session/store.db", "project/session/store.db-wal"],
        )
    }

    #[test]
    fn gc_archives_opencode_data_before_disposing_private_state() -> TestResult {
        relocated_history_lifecycle(
            "opencode",
            "data/opencode",
            &[
                "opencode.db",
                "opencode.db-wal",
                "log/session.log",
                "storage/message/session.json",
                "snapshot/project/HEAD",
                "shell/session.sh",
            ],
        )
    }

    fn relocated_history_lifecycle(provider: &str, layout: &str, artifacts: &[&str]) -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let candidate = attempt.join("provider-state/candidate-0");
        fs::write(candidate.join("provider.txt"), provider)?;
        for artifact in artifacts {
            let path = candidate.join(layout).join(artifact);
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path.parent().ok_or("parent")?)?;
            fs::write(path, format!("retained {artifact}"))?;
        }
        let host_auth = attempt.join("synthetic-host-credential");
        if provider == "opencode" {
            fs::write(&host_auth, "synthetic credential")?;
            std::os::unix::fs::symlink(&host_auth, candidate.join(layout).join("auth.json"))?;
        }
        let disposable = if provider == "cursor" {
            ".cursor/settings.json"
        } else {
            "data/other"
        };
        fs::write(candidate.join(disposable), "disposable")?;
        let preview = sweep(&store, &context, 7, 7 * DAY_MICROS, true, &call)?;
        assert!(preview.errors.is_empty(), "{:?}", preview.errors);
        assert!(candidate.join(layout).exists());
        assert!(!attempt.join(HISTORY).exists());
        let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join(STATE).exists());
        let archive = attempt.join("provider-history/candidate-0");
        for artifact in artifacts {
            assert_eq!(
                fs::read_to_string(archive.join(layout).join(artifact))?,
                format!("retained {artifact}")
            );
        }
        assert!(!archive.join(disposable).exists());
        if provider == "opencode" {
            assert!(fs::symlink_metadata(archive.join(layout).join("auth.json")).is_err());
            assert_eq!(fs::read_to_string(&host_auth)?, "synthetic credential");
        }
        let repeated = sweep(&store, &context, 7, 8 * DAY_MICROS, false, &call)?;
        assert!(repeated.errors.is_empty(), "{:?}", repeated.errors);
        assert!(repeated.removed.is_empty());
        Ok(())
    }

    #[test]
    fn gc_spares_nonterminal_attempts_and_contention_and_reservations() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        assert!(
            sweep(&store, &context, 1, 100 * DAY_MICROS, false, &call)?
                .removed
                .is_empty()
        );
        assert!(
            attempt
                .join("provider-state/candidate-0/auth.json")
                .exists()
        );
        withdraw(&store, &input, &call)?;
        let directory = Directory::open_anchor(&attempt)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        fs::write(attempt.join("tmp/keep"), "locked payload")?;
        let run = store.attempt_dir(input.attempt_id, false)?;
        let publication =
            run.try_lock_existing(OsStr::new("publication.lock"), LockMode::Shared)?;
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
        assert!(attempt.join("tmp/keep").exists());
        drop(publication);
        let writer = directory.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
        assert!(attempt.join("tmp/keep").exists());
        drop(writer);
        let evidence = attempt.join(input.attempt_id.to_string());
        fs::create_dir_all(&evidence)?;
        fs::write(evidence.join("reservation.json"), "{}")?;
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
        assert!(attempt.join("tmp/keep").exists());
        assert!(
            attempt
                .join("provider-state/candidate-0/auth.json")
                .exists()
        );
        fs::remove_file(evidence.join("reservation.json"))?;
        let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join("provider-state").exists());
        assert!(!attempt.join("tmp").exists());
        Ok(())
    }

    #[test]
    fn gc_temporary_never_follows_links_outside_the_attempt() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("keep"), "outside")?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        std::os::unix::fs::symlink(outside.path(), attempt.join("tmp/escape"))?;
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.attempt_tmp.len(), 1);
        assert!(!attempt.join("tmp").exists());
        assert_eq!(fs::read_to_string(outside.path().join("keep"))?, "outside");
        std::os::unix::fs::symlink(outside.path(), attempt.join("tmp"))?;
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.attempt_tmp.len(), 1);
        assert!(!attempt.join("tmp").exists());
        assert_eq!(fs::read_to_string(outside.path().join("keep"))?, "outside");
        Ok(())
    }

    #[test]
    fn gc_removes_unreadable_owned_tmp_directories_without_following_links() -> TestResult {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;

        let temporary = attempt.join("tmp");
        let nested = temporary.join("candidate-0/.tmpX/child");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("payload"), b"recoverable scratch")?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o000))?;

        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("keep"), "outside")?;
        symlink(outside.path(), temporary.join("candidate-0/.tmpX/escape"))?;

        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.attempt_tmp.len(), 1);
        assert_eq!(report.attempt_tmp[0].path, temporary);
        assert!(report.attempt_tmp[0].bytes > 0);
        assert!(!temporary.exists());
        assert_eq!(fs::read_to_string(outside.path().join("keep"))?, "outside");
        Ok(())
    }

    #[test]
    fn candidate_provider_uses_only_its_current_marker() -> TestResult {
        let temp = tempfile::tempdir()?;
        let candidate = Directory::open_anchor(temp.path())?;
        fs::write(temp.path().join("provider.txt"), "codex")?;
        assert_eq!(candidate_provider(&candidate)?, "codex");
        fs::write(temp.path().join("provider.txt"), [0xff])?;
        assert!(candidate_provider(&candidate).is_err());
        fs::remove_file(temp.path().join("provider.txt"))?;
        assert!(candidate_provider(&candidate)?.is_empty());
        Ok(())
    }

    #[test]
    fn gc_ignores_indexed_attempt_with_missing_attempt_directory() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let terminal = withdraw(&store, &input, &call)?;
        let attempts = attempt.parent().ok_or("attempt collection path")?;
        fs::remove_dir_all(&attempt)?;

        let report = sweep(&store, &context, 7, 0, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.kept.is_empty(), "{:?}", report.kept);
        assert!(store.record(terminal.attempt_id, &call).is_ok());

        fs::remove_dir_all(attempts)?;
        let report = sweep(&store, &context, 7, 0, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        Ok(())
    }

    #[test]
    fn gc_retries_only_authorised_tombstones_and_never_follows_links() -> TestResult {
        use std::os::unix::fs::{MetadataExt, symlink};
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let candidate = attempt.join("provider-state/candidate-0");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(candidate.join("cache"))?;
        fs::write(candidate.join("cache/payload.lock"), "cache")?;
        let metadata = fs::metadata(candidate.join("cache"))?;
        let cache_tombstone = candidate.join(format!(
            "{TOMBSTONE}{:016x}{:016x}-cache",
            metadata.dev(),
            metadata.ino()
        ));
        fs::rename(candidate.join("cache"), &cache_tombstone)?;
        let metadata = fs::metadata(candidate.join("sessions"))?;
        let unapproved = candidate.join(format!(
            "{TOMBSTONE}{:016x}{:016x}-sessions",
            metadata.dev(),
            metadata.ino()
        ));
        fs::rename(candidate.join("sessions"), &unapproved)?;
        let host = context
            .state_root
            .as_path()
            .parent()
            .ok_or("state parent")?
            .join("host-auth");
        fs::write(&host, "host credential")?;
        fs::remove_file(candidate.join("auth.json"))?;
        symlink(&host, candidate.join("auth.json"))?;
        symlink(&host, candidate.join("plugins"))?;
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!cache_tombstone.exists());
        assert!(unapproved.join("session.jsonl").exists());
        assert!(fs::symlink_metadata(candidate.join("auth.json")).is_err());
        assert!(fs::symlink_metadata(candidate.join("plugins")).is_err());
        assert_eq!(fs::read_to_string(host)?, "host credential");
        // A forged identity never grants disposal authority.
        let forged = candidate.join(format!("{TOMBSTONE}{:032x}-cache", 0));
        fs::DirBuilder::new().mode(0o700).create(&forged)?;
        fs::write(forged.join("keep"), "keep")?;
        assert_eq!(
            sweep(&store, &context, 7, DAY_MICROS, false, &call)?
                .errors
                .len(),
            1
        );
        assert!(forged.join("keep").exists());
        Ok(())
    }

    #[test]
    fn a_pending_continuation_keeps_its_source_and_expired_resume_spends_nothing() -> TestResult {
        use provenant_core::{AttemptId, OperationId};
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let resume = Resume {
            extend: Vec::new(),
            prior: input.attempt_id,
            expected_generation: 0,
            operation_id: OperationId::parse("continue-fixture")?,
            attempt_id: AttemptId::from_bytes([9; 16]),
            prompt: "continue".into(),
            created_at: WallTime::from_micros(300)?,
        };
        store.resume(&resume, &call)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        fs::write(attempt.join("tmp/payload"), "disposable")?;
        let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
        assert_eq!(report.kept.len(), 1);
        assert_eq!(report.attempt_tmp.len(), 1);
        assert!(!attempt.join("tmp").exists());
        assert!(
            attempt
                .join("provider-state/candidate-0/auth.json")
                .exists()
        );
        store.withdraw(
            resume.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(400)?,
            &call,
        )?;
        let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join("provider-state").exists());
        assert_eq!(store.resume(&resume, &call)?.attempt_id, resume.attempt_id);
        let fresh = Resume {
            extend: Vec::new(),
            operation_id: OperationId::parse("expired-fixture")?,
            ..resume
        };
        assert!(matches!(
            store.resume(&fresh, &call),
            Err(RunError::ProviderStateExpired)
        ));
        assert!(store.operation(&fresh.operation_id, &call)?.is_none());
        Ok(())
    }
    #[test]
    fn interrupted_expiry_keeps_markerless_provider_history_and_retries_credential_tombstones()
    -> TestResult {
        use std::os::unix::fs::MetadataExt;
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let state = attempt.join("provider-state");
        let candidate = state.join("candidate-0");
        // An interrupted directory-shaped credential removal is retried at
        // the credential deadline, rather than waiting for state expiry.
        fs::remove_file(candidate.join("auth.json"))?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(candidate.join("auth.json"))?;
        fs::write(candidate.join("auth.json/token"), "credential")?;
        let metadata = fs::metadata(candidate.join("auth.json"))?;
        let tombstone = candidate.join(format!(
            "{TOMBSTONE}{:016x}{:016x}-auth.json",
            metadata.dev(),
            metadata.ino()
        ));
        fs::rename(candidate.join("auth.json"), &tombstone)?;
        let malformed = candidate.join(format!(
            "{TOMBSTONE}{}é{}-cache",
            "a".repeat(15),
            "a".repeat(15)
        ));
        fs::DirBuilder::new().mode(0o700).create(&malformed)?;
        let report = sweep(&store, &context, 7, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!tombstone.exists());
        assert!(
            malformed.exists(),
            "malformed names grant no disposal authority"
        );
        // A prior sweep already moved one markerless candidate. The remaining
        // candidate must not change provider classification on retry.
        fs::remove_file(candidate.join("provider.txt"))?;
        fs::remove_file(state.join("candidate-1/provider.txt"))?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("provider-history"))?;
        fs::rename(&candidate, attempt.join("provider-history/candidate-0"))?;
        seal(&Directory::open_anchor(&attempt)?, EXPIRED, b"{}\n", 64)?;
        let report = sweep(&store, &context, 7, 7 * DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!state.exists());
        assert_eq!(
            fs::read_to_string(attempt.join("provider-history/candidate-1/unknown-session"))?,
            "unknown transcript"
        );
        Ok(())
    }

    struct Pair {
        _temp: tempfile::TempDir,
        context: ExecutionContext,
        store: RunStore,
        older: std::path::PathBuf,
        newer: std::path::PathBuf,
        newer_input: PreparedRun,
    }

    /// Two terminal attempts, each holding 256 KiB of provider state; the
    /// first finished earlier.
    fn pair() -> std::result::Result<Pair, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let first = input();
        let mut second = input();
        second.attempt_id = provenant_core::AttemptId::from_bytes([14; 16]);
        second.operation_id = provenant_core::OperationId::parse("run-test-b")?;
        second.attempt_id = AttemptId::from_bytes([16; 16]);
        crate::tests::rekey(&mut second);
        let mut paths = vec![];
        for (index, prepared) in [&first, &second].into_iter().enumerate() {
            store.create(prepared, &call)?;
            let attempt = context
                .state_root
                .as_path()
                .join(format!("attempts/{}", prepared.attempt_id));
            let candidate = attempt.join("provider-state/candidate-0");
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&candidate)?;
            fs::write(candidate.join("provider.txt"), "codex")?;
            fs::write(candidate.join("blob"), vec![7_u8; 262_144])?;
            store.withdraw(
                prepared.attempt_id,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                WallTime::from_micros(200 + index as i64 * 1_000)?,
                &call,
            )?;
            paths.push(attempt);
        }
        let newer = paths.pop().ok_or("attempt")?;
        let older = paths.pop().ok_or("attempt")?;
        Ok(Pair {
            _temp: temp,
            context,
            store,
            older,
            newer,
            newer_input: second,
        })
    }

    fn payload(
        pair: &Pair,
        cap: u64,
        age: i64,
        dry_run: bool,
        measure: bool,
    ) -> crate::Result<PayloadReport> {
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        pair.store.gc_payloads(
            &Directory::open_anchor(pair.context.state_root.as_path())?,
            cap,
            WallTime::from_micros(200 + age).map_err(|_| RunError::Validation("fixture time"))?,
            dry_run,
            measure,
            None,
            &call,
        )
    }

    #[test]
    fn payload_pressure_evicts_oldest_first_down_to_low_water() -> TestResult {
        let pair = pair()?;
        let report = payload(&pair, 400_000, 8 * DAY_MICROS, false, true)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.low_water_bytes, 300_000);
        assert_eq!(report.evicted.len(), 1, "{report:?}");
        assert_eq!(report.evicted[0].path, pair.older);
        assert!(report.reclaimed_bytes >= 262_144);
        assert!(!pair.older.join("provider-state").exists());
        assert!(pair.older.join("payload-evicted.json").exists());
        assert!(pair.older.join("provider-state-expired.json").exists());
        assert!(pair.newer.join("provider-state/candidate-0/blob").exists());
        // Under the low-water mark nothing more goes, and a rerun is a no-op.
        let again = payload(&pair, 400_000, 8 * DAY_MICROS, false, true)?;
        assert!(again.evicted.is_empty());
        assert_eq!(again.total_reclaimed_bytes, report.reclaimed_bytes);
        assert_eq!(again.pinned_bytes, 0);
        Ok(())
    }

    #[test]
    fn payload_pressure_spares_recent_and_reports_them_pinned() -> TestResult {
        let pair = pair()?;
        let report = payload(&pair, 1_000, DAY_MICROS / 2, false, true)?;
        assert!(report.evicted.is_empty());
        assert!(report.accounted_bytes >= 524_288);
        assert_eq!(report.pinned_bytes, report.accounted_bytes);
        assert!(pair.older.join("provider-state/candidate-0/blob").exists());
        assert!(pair.newer.join("provider-state/candidate-0/blob").exists());
        Ok(())
    }

    #[test]
    fn payload_pressure_never_evicts_private_git_or_dry_runs() -> TestResult {
        let pair = pair()?;
        fs::create_dir(pair.older.join("tmp"))?;
        fs::write(pair.older.join("writer-objects.json"), "{}")?;
        let preview = payload(&pair, 1_000, 8 * DAY_MICROS, true, true)?;
        assert_eq!(preview.evicted.len(), 1, "{preview:?}");
        assert_eq!(preview.evicted[0].path, pair.newer);
        assert!(preview.pinned_bytes >= 262_144);
        assert!(
            pair.newer.join("provider-state").exists(),
            "dry run removes nothing"
        );
        let report = payload(&pair, 1_000, 8 * DAY_MICROS, false, true)?;
        assert_eq!(report.evicted.len(), 1);
        assert!(pair.older.join("provider-state/candidate-0/blob").exists());
        assert!(!pair.newer.join("provider-state").exists());
        Ok(())
    }

    #[test]
    fn payload_pressure_reads_the_ledger_without_measuring() -> TestResult {
        let pair = pair()?;
        let measured = payload(&pair, u64::MAX / 2, 8 * DAY_MICROS, false, true)?;
        assert!(measured.evicted.is_empty());
        // Growth after accounting is invisible to the ledger-only pass.
        fs::write(
            pair.older.join("provider-state/candidate-0/late"),
            vec![1_u8; 1 << 20],
        )?;
        let report = payload(&pair, 400_000, 8 * DAY_MICROS, false, false)?;
        assert_eq!(report.evicted.len(), 1, "{report:?}");
        assert!(report.evicted[0].bytes < 600_000, "{report:?}");
        Ok(())
    }

    #[test]
    fn evicted_payload_keeps_records_tails_and_refuses_resume() -> TestResult {
        let pair = pair()?;
        let directory = Directory::open_anchor(&pair.older)?;
        crate::artifacts::seal(&directory, "stdout.log", &vec![b'x'; 200_000], 300_000)?;
        let result = fs::read(pair.older.join("result.md"))?;
        let report = payload(&pair, 400_000, 8 * DAY_MICROS, false, true)?;
        assert_eq!(report.evicted.len(), 1, "{report:?}");
        assert_eq!(fs::read(pair.older.join("stdout.log.tail"))?.len(), 65_536);
        assert!(!pair.older.join("stdout.log").exists());
        assert_eq!(fs::read(pair.older.join("result.md"))?, result);
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        assert!(matches!(
            pair.store
                .retain_provider_state(AttemptId::from_bytes([6; 16]), &call),
            Err(RunError::ProviderStateExpired)
        ));
        assert!(
            pair.store
                .record(pair.newer_input.attempt_id, &call)
                .is_ok()
        );
        assert!(
            pair.store
                .record(AttemptId::from_bytes([6; 16]), &call)
                .is_ok()
        );
        let totals = payload_totals(&Directory::open_anchor(pair.context.state_root.as_path())?)
            .ok_or("totals")?;
        assert_eq!(totals.1, report.reclaimed_bytes);
        Ok(())
    }
}
