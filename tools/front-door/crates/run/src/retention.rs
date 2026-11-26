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
    path::{Path, PathBuf},
};

const DAY_MICROS: i64 = 86_400_000_000;
const EXPIRED: &str = AttemptFile::ProviderStateExpired.name();
const STATE: &str = AttemptFile::ProviderState.name();
const HISTORY: &str = AttemptFile::ProviderHistory.name();
pub const TOMBSTONE: &str = ".provider-gc-";
const PAYLOAD_LOW_WATER_RATIO: (u64, u64) = (3, 4);

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
    /// Exact attempt to retry after a cooperative sweep stop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
    pub removed: Vec<PathBuf>,
    pub tightened: Vec<PathBuf>,
    pub attempt_tmp: Vec<AttemptTmpRemoval>,
    /// Bytes removed from scratch directories that remain for a later pass.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub partial_tmp: Vec<AttemptTmpRemoval>,
    /// Scratch candidates examined but not fully collected, with bounded size observations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scratch_deferred: Vec<DeferredScratch>,
    pub attempt_logs: Vec<AttemptLogRemoval>,
    /// Event rows expired (or eligible to expire in a dry run).
    pub expired_events: u64,
    pub kept: Vec<ProviderStateIssue>,
    pub errors: Vec<ProviderStateIssue>,
    /// Byte-bounded eviction of bulk payload; see [`RunStore::gc_payloads`].
    pub payload: PayloadReport,
}

impl ProviderStateGc {
    pub fn temporary_bytes(&self) -> u64 {
        self.attempt_tmp
            .iter()
            .chain(&self.partial_tmp)
            .map(|entry| entry.bytes)
            .fold(0_u64, u64::saturating_add)
    }
}

#[derive(Debug, Serialize)]
pub struct DeferredScratch {
    pub path: PathBuf,
    /// None when the bounded observation could not measure the entire tree.
    pub bytes: Option<u64>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetentionPass {
    ProviderState { days: u64, logs_days: u64 },
    AttemptScratch,
}

/// Work ownership is independent of an attempt's terminal/released state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScratchOwner {
    Finished,
    Owned,
    Unowned,
}

pub type ScratchOwnerResolver<'a> = dyn Fn(&AttemptRecord, &CanonicalRequest) -> ScratchOwner + 'a;

/// One retention scan's resume position and cooperative work budget.
#[derive(Default)]
pub struct RetentionScan<'a> {
    pub start_at: Option<AttemptId>,
    pub detail: Option<&'a str>,
    pub keep_going: Option<&'a dyn Fn() -> bool>,
}

impl RetentionScan<'_> {
    pub fn permits(&self, call: &Request<'_>) -> bool {
        if call.cancel.is_cancelled() {
            return false;
        }
        if self.keep_going.is_some_and(|check| !check()) {
            call.cancel.cancel();
            return false;
        }
        true
    }
}

/// SQLite's interrupt handle is designed for cross-thread cancellation. The
/// scoped monitor cannot outlive this scan or interrupt a later store request.
struct SqlCancellation {
    stopped: provenant_core::CancelToken,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl SqlCancellation {
    fn arm(
        db: &rusqlite::Connection,
        scan: &RetentionScan<'_>,
        call: &Request<'_>,
    ) -> Option<Self> {
        scan.keep_going?;
        let stopped = provenant_core::CancelToken::new();
        let done = stopped.clone();
        let cancel = call.cancel.clone();
        let interrupt = db.get_interrupt_handle();
        let worker = std::thread::Builder::new()
            .name("gc-sql-cancel".into())
            .spawn(move || {
                while !done.is_cancelled() && !cancel.is_cancelled() {
                    provenant_core::Pause::wait(
                        &provenant_core::ThreadPause,
                        provenant_core::DurationMicros::from_micros(10_000),
                        &done,
                    );
                }
                if cancel.is_cancelled() && !done.is_cancelled() {
                    interrupt.interrupt();
                }
            });
        match worker {
            Ok(worker) => Some(Self {
                stopped,
                worker: Some(worker),
            }),
            Err(error) => {
                use provenant_records::warning::{RESOURCE_PROBE_FAILED, Warning, warn};
                // Spec: docs/specs/cli.md#warnings-are-typed-values
                warn(
                    Warning::new(
                        RESOURCE_PROBE_FAILED,
                        format!("cannot monitor retention SQLite cancellation; using bounded batches: {error}"),
                    )
                    .subject("retention cancellation monitor"),
                );
                None
            }
        }
    }
}
impl Drop for SqlCancellation {
    fn drop(&mut self) {
        self.stopped.cancel();
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

pub struct ScratchPolicy<'a> {
    pub idle_seconds: u64,
    /// Per-attempt scratch size above which a settled attempt's scratch is
    /// collectible before `idle_seconds`; zero disables the cap.
    pub scratch_cap_bytes: u64,
    pub projects: Option<&'a [provenant_core::ProjectId]>,
    pub attempts: Option<&'a [AttemptId]>,
    pub owner: Option<&'a ScratchOwnerResolver<'a>>,
}
impl ScratchPolicy<'_> {
    pub fn new(idle_seconds: u64) -> Self {
        Self {
            idle_seconds,
            scratch_cap_bytes: 0,
            projects: None,
            attempts: None,
            owner: None,
        }
    }
}

pub(crate) fn scratch_owner(record: &AttemptRecord, request: &CanonicalRequest) -> ScratchOwner {
    // Read-only attempts have no branch or writer checkout to retain. Their
    // scratch lifetime is solely the configured idle period, even when the
    // launch was admitted from a graph or names a source checkout.
    if request.task.writer.is_none() {
        return ScratchOwner::Unowned;
    }
    if record.superseded_by.is_some() {
        return ScratchOwner::Finished;
    }
    let path = request.task.writer.as_ref().map(|w| &w.worktree).or(request
        .task
        .access
        .source_root
        .as_ref());
    match path {
        Some(path) => match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ScratchOwner::Finished,
            _ => ScratchOwner::Owned,
        },
        None if record
            .context
            .as_ref()
            .is_some_and(|context| context.graph_id.is_some()) =>
        {
            ScratchOwner::Owned
        }
        None => ScratchOwner::Unowned,
    }
}

impl RunStore {
    /// Retain the source until a continuation has been accepted. GC also
    /// spares sources of nonterminal continuations until their copy completes.
    pub(crate) fn retain_provider_state(
        &self,
        id: AttemptId,
        call: &Request<'_>,
        policy: &provenant_records::policy::Policy,
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
        self.ensure_provider_state_not_expired(id, policy)?;
        Ok(lease)
    }

    /// Check the retention marker while the caller holds the attempt's
    /// publication fence. The caller's exclusive lock also excludes GC, so it
    /// can preserve the usual retention guarantee without reacquiring a lock.
    pub(crate) fn ensure_provider_state_not_expired(
        &self,
        id: AttemptId,
        _policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
        let attempt = self.attempt_dir(id, false)?;
        match attempt.metadata_at(OsStr::new(EXPIRED), false) {
            Ok(_) => return Err(RunError::ProviderStateExpired),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Collect provider state and logs for indexed terminal attempts across
    /// projects, without changing attempt records or their digests.
    pub fn gc_provider_state(
        &self,
        expected: &Directory,
        days: u64,
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
        scan: &RetentionScan<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_provider_state_with_logs(expected, days, 30, now, dry_run, call, scan)
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
        scan: &RetentionScan<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(
            expected,
            now,
            dry_run,
            RetentionPass::ProviderState { days, logs_days },
            None,
            call,
            scan,
        )
    }

    /// Collect released scratch only with finished-work proof, or an unowned idle lifetime.
    pub fn gc_temporary(
        &self,
        expected: &Directory,
        policy: &ScratchPolicy<'_>,
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
        scan: &RetentionScan<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(
            expected,
            now,
            dry_run,
            RetentionPass::AttemptScratch,
            Some(policy),
            call,
            scan,
        )
    }

    #[allow(clippy::too_many_arguments)] // Policy, binding and cooperative scan are independent inputs.
    fn gc_retention(
        &self,
        expected: &Directory,
        now: WallTime,
        dry_run: bool,
        pass: RetentionPass,
        scratch: Option<&ScratchPolicy<'_>>,
        call: &Request<'_>,
        scan: &RetentionScan<'_>,
    ) -> Result<ProviderStateGc> {
        let _sql_cancellation = SqlCancellation::arm(&self.db, scan, call);
        let keep_going = &|| scan.permits(call);
        if !keep_going() {
            return Ok(ProviderStateGc {
                interrupted_at: Some(scan.start_at.map_or_else(
                    || self.context.state_root.as_path().join("attempts"),
                    |id| {
                        self.context
                            .state_root
                            .as_path()
                            .join(format!("attempts/{id}"))
                    },
                )),
                continuation: scan.detail.map(str::to_owned),
                ..Default::default()
            });
        }

        if !self.root.metadata()?.same_identity(&expected.metadata()?) {
            return Err(RunError::unsafe_path(".", PathCheck::Identity));
        }
        let (days, logs_days) = match pass {
            RetentionPass::ProviderState { days, logs_days } => {
                if !(1..=3650).contains(&logs_days) {
                    return Err(RunError::Validation("logs_days_range"));
                }
                (days, Some(logs_days))
            }
            RetentionPass::AttemptScratch => (0, None),
        };
        if scratch.is_none() && !(1..=3650).contains(&days) {
            return Err(RunError::Validation("provider_state_days_range"));
        }
        if !dry_run
            && scan.start_at.is_none()
            && let Some(logs_days) = logs_days
        {
            let pruned = self.prune_reactions(
                now.as_micros().div_euclid(1_000_000) - (logs_days as i64) * 86_400,
                call,
                scan,
            );
            match pruned {
                Ok(Some(after)) => {
                    return Ok(ProviderStateGc {
                        interrupted_at: Some(self.context.state_root.as_path().join("attempts")),
                        continuation: Some(format!("reactions:{after}")),
                        ..Default::default()
                    });
                }
                Err(_) if call.cancel.is_cancelled() => {
                    return Ok(ProviderStateGc {
                        interrupted_at: Some(self.context.state_root.as_path().join("attempts")),
                        continuation: scan.detail.map(str::to_owned),
                        ..Default::default()
                    });
                }
                Err(error) => return Err(error),
                Ok(None) => {}
            }
        }
        let mut after = self.retention_after(scan.start_at)?;
        let mut report = ProviderStateGc::default();
        let mut temporary_sizes = provenant_os::disk::DiskReport::with_walk_budget(
            self.context.state_root.as_path().into(),
            0,
            10_000,
            std::time::Duration::from_millis(500),
        )
        .with_cancellation(call.cancel.clone());
        #[allow(clippy::disallowed_methods)] // Bound read-only maintenance in real time.
        let preview_started = std::time::Instant::now();
        loop {
            let rows =
                self.retention_page(scratch.is_some(), scratch.and_then(|p| p.attempts), after)?;
            if rows.is_empty() {
                break;
            }
            for (sequence, id, project, raw) in rows {
                let attempt_path = self
                    .context
                    .state_root
                    .as_path()
                    .join(format!("attempts/{id}"));
                if !scan.permits(call) {
                    report.interrupted_at = Some(attempt_path);
                    return Ok(report);
                }
                after = sequence;
                if dry_run && preview_started.elapsed() >= std::time::Duration::from_secs(1) {
                    report.kept.push(ProviderStateIssue {
                        path: self.context.state_root.as_path().into(),
                        reason:
                            "preview budget reached; remaining attempts require an applied GC pass"
                                .into(),
                    });
                    report.interrupted_at = Some(attempt_path);
                    return Ok(report);
                }
                if scratch
                    .and_then(|policy| policy.projects)
                    .is_some_and(|projects| !projects.iter().any(|id| id.to_string() == project))
                {
                    continue;
                }
                let path = self
                    .context
                    .state_root
                    .as_path()
                    .join(format!("attempts/{id}/provider-state"));
                let first_scratch = report.attempt_tmp.len();
                let first_removal = report.removed.len();
                let first_tightened = report.tightened.len();
                let result = (|| -> Result<()> {
                    let mut record: AttemptRecord = crate::db::decode_attempt(&raw)?;
                    crate::reader::validate_attempt(&record)?;
                    record.superseded_by = self.same_session_successor(record.attempt_id)?;
                    if record.attempt_id.to_string() != id
                        || record.project_id.to_string() != project
                    {
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
                        _settlement,
                    }) = self.hold_attempt(&record, call)?
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
                    // Settlement and custody are checked separately; every
                    // attempt observes the same minimum scratch idle period,
                    // unless a bounded measurement finds its scratch over the
                    // configured cap. An unmeasured tree is never over cap.
                    let cap = scratch.and_then(|policy| {
                        (policy.scratch_cap_bytes > 0).then_some(policy.scratch_cap_bytes)
                    });
                    let over_cap = match cap {
                        Some(cap) if !continuing => {
                            let temporary = self
                                .context
                                .state_root
                                .as_path()
                                .join(crate::plan::ATTEMPT_SCRATCH)
                                .join(&id)
                                .join("tmp");
                            temporary_sizes.entries.clear();
                            temporary_sizes.errors.clear();
                            let present = !std::fs::symlink_metadata(&temporary)
                                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
                            let measured = if present && scan.permits(call) {
                                temporary_sizes.measure("attempt_tmp", &temporary)
                            } else {
                                None
                            };
                            measured
                                .filter(|_| temporary_sizes.errors.is_empty())
                                .map(|index| temporary_sizes.entries[index].allocated_bytes)
                                .is_some_and(|bytes| bytes > cap)
                        }
                        _ => false,
                    };
                    let scratch_eligible = if let Some(policy) = scratch {
                        !continuing
                            && (age
                                >= i64::try_from(policy.idle_seconds)
                                    .unwrap_or(i64::MAX)
                                    .saturating_mul(1_000_000)
                                || over_cap)
                    } else {
                        false
                    };
                    // The sealed effect list names the effects that apply. The
                    // mutable receipts are authoritative for their completion.
                    let effects_done = if scratch_eligible {
                        match crate::artifacts::read_sealed(
                            &attempt,
                            "terminal-effects.json",
                            crate::RECORD_LIMIT,
                        )? {
                            Some(raw) => {
                                let effects: crate::store::TerminalEffects =
                                    crate::db::decode(&raw)?;
                                if effects.schema_version != 1
                                    || effects.scope != Self::scope(&record)
                                {
                                    return Err(RunError::Conflict);
                                }
                                let raw: Option<Vec<u8>> = rusqlite::OptionalExtension::optional(self.db.query_row(
                                    "SELECT record FROM settlement_receipts WHERE attempt=?1 AND generation=?2",
                                    rusqlite::params![id, crate::db::sql_int(record.generation)?], |row| row.get(0),
                                ))?;
                                let receipts: Option<provenant_records::run::SettlementReceipts> =
                                    raw.as_deref().map(crate::db::decode_column).transpose()?;
                                match receipts {
                                    Some(receipts) => effects.effects.iter().all(|(effect, _)| {
                                        use provenant_records::run::{
                                            EffectDisposition, SettlementEffect,
                                        };
                                        let disposition = match effect {
                                            SettlementEffect::GitImport => &receipts.git_import,
                                            SettlementEffect::ManifestConflict => {
                                                &receipts.manifest_conflict
                                            }
                                            SettlementEffect::CleanupCustodyRelease => {
                                                &receipts.cleanup_custody_release
                                            }
                                            SettlementEffect::Delivery => &receipts.delivery,
                                        };
                                        *disposition == EffectDisposition::Done
                                    }),
                                    None => false,
                                }
                            }
                            // Unlaunched attempts have no provider effects.
                            None => record.provider_launched == Some(false),
                        }
                    } else {
                        false
                    };
                    if scratch_eligible && effects_done && !uncertain_loss {
                        let temporary = crate::plan::scratch_root(
                            self.context.state_root.as_path(),
                            record.attempt_id,
                        )
                        .join("tmp");
                        let collection = (|| -> Result<()> {
                            let scratch = match self.attempt_scratch_dir(record.attempt_id, false) {
                                Ok(scratch) => scratch,
                                Err(error) if missing_directory(&error) => return Ok(()),
                                Err(error) => return Err(error),
                            };
                            // A launched writer's cleanup can be deferred by a
                            // busy writer lock too; keep its scratch until done.
                            let pending_writer_cleanup = if matches!(
                                record.state,
                                AttemptState::Terminal { .. }
                            ) && crate::artifacts::read_sealed(
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
                            if pending_writer_cleanup {
                                report.kept.push(ProviderStateIssue {
                                    path: temporary.clone(),
                                    reason: "writer cleanup remains pending".into(),
                                });
                                return Ok(());
                            }
                            collect_temporary(
                                &scratch,
                                &temporary,
                                dry_run,
                                &mut report,
                                &mut temporary_sizes,
                                &|| scan.permits(call),
                            )?;
                            if !dry_run
                                && scratch
                                    .metadata_at(OsStr::new("tmp"), false)
                                    .is_err_and(|error| error.is_not_found())
                            {
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
                            if retention_interrupted(&error) {
                                return Err(error);
                            }
                            record_issue(&mut report, temporary, &error);
                        }
                    }
                    if matches!(pass, RetentionPass::AttemptScratch) {
                        return Ok(());
                    }
                    if continuing {
                        return Err(RunError::Validation(
                            "a continuation still needs this provider state",
                        ));
                    }
                    if logs_days.is_some_and(|days| age >= DAY_MICROS.saturating_mul(days as i64)) {
                        {
                            collect_logs(
                                &attempt,
                                path.parent().ok_or_else(|| {
                                    RunError::unsafe_path(path.clone(), PathCheck::Containment)
                                })?,
                                dry_run,
                                &mut report,
                                keep_going,
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
                                loop {
                                    if !keep_going() {
                                        return Err(RunError::Cancelled);
                                    }
                                    let count = self.write(call, |tx| {
                                        Ok(tx.execute(
                                            "DELETE FROM events WHERE sequence IN (SELECT sequence FROM events WHERE attempt=?1 AND project=?2 AND sequence < (SELECT MAX(sequence) FROM events WHERE attempt=?1 AND project=?2) ORDER BY sequence LIMIT 64)",
                                            rusqlite::params![id,record.project_id.to_string()],
                                        )?)
                                    })?;
                                    report.expired_events =
                                        report.expired_events.saturating_add(count as u64);
                                    if count < 64 {
                                        break;
                                    }
                                }
                                0
                            };
                            report.expired_events += expired_events;
                        }
                    }
                    if age >= DAY_MICROS
                        && let Err(error) =
                            credentials(&attempt, &path, dry_run, &mut report, keep_going)
                    {
                        if retention_interrupted(&error) {
                            return Err(error);
                        }
                        record_issue(&mut report, path.clone(), &error);
                    }
                    if let Err(error) =
                        prune_caches(&attempt, &record, dry_run, &mut report, keep_going)
                    {
                        if retention_interrupted(&error) {
                            return Err(error);
                        }
                        record_issue(&mut report, path.clone(), &error);
                    }
                    if age >= DAY_MICROS.saturating_mul(days as i64) {
                        expire(&attempt, dry_run, &mut report, keep_going)?;
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
                if scratch.is_some() && report.attempt_tmp.len() == first_scratch {
                    let temporary = self
                        .context
                        .state_root
                        .as_path()
                        .join(crate::plan::ATTEMPT_SCRATCH)
                        .join(&id)
                        .join("tmp");
                    if !std::fs::symlink_metadata(&temporary)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                    {
                        temporary_sizes.entries.clear();
                        temporary_sizes.errors.clear();
                        let measured = if scan.permits(call) {
                            temporary_sizes.measure("attempt_tmp", &temporary)
                        } else {
                            None
                        };
                        let bytes = measured
                            .filter(|_| temporary_sizes.errors.is_empty())
                            .map(|index| temporary_sizes.entries[index].allocated_bytes);
                        report.scratch_deferred.push(DeferredScratch {
                            path: temporary,
                            bytes,
                        });
                    }
                }
                if let Err(error) = result {
                    let cancelled = call.cancel.is_cancelled()
                        || matches!(
                            error,
                            RunError::Cancelled
                                | RunError::Filesystem(provenant_os::fs::FsError::Cancelled { .. })
                        );
                    if cancelled {
                        report.interrupted_at = Some(attempt_path);
                        return Ok(report);
                    }
                    // This closure can fail before provider state is touched, so
                    // name the attempt row that failed to read, not a child path.
                    record_issue(&mut report, attempt_path, &error);
                }
            }
        }
        Ok(report)
    }

    fn retention_after(&self, start: Option<AttemptId>) -> Result<i64> {
        use rusqlite::OptionalExtension;
        Ok(match start {
            None => 0,
            Some(id) => self
                .db
                .query_row(
                    "SELECT sequence FROM attempts WHERE id=?1",
                    [id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(1)
                .saturating_sub(1),
        })
    }

    #[allow(clippy::type_complexity)] // SQLite page rows mirror the indexed attempt columns.
    fn retention_page(
        &self,
        scratch: bool,
        selected: Option<&[AttemptId]>,
        after: i64,
    ) -> Result<Vec<(i64, String, String, Vec<u8>)>> {
        let filter = if scratch { " AND tmp_retained=1" } else { "" };
        if let Some(ids) = selected {
            use rusqlite::OptionalExtension;
            let sql = format!(
                "SELECT sequence,id,project,record FROM attempts WHERE sequence>?1 AND id=?2 AND terminal=1 AND refused=0{filter}"
            );
            let mut query = self.db.prepare(&sql)?;
            let mut rows = BTreeMap::new();
            for id in ids {
                let row = query
                    .query_row(rusqlite::params![after, id.to_string()], |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Vec<u8>>(3)?,
                        ))
                    })
                    .optional()?;
                if let Some(row) = row {
                    rows.insert(row.0, row);
                    if rows.len() > 64 {
                        rows.pop_last();
                    }
                }
            }
            return Ok(rows.into_values().collect());
        }
        let sql = format!(
            "SELECT sequence,id,project,record FROM attempts WHERE sequence>?1 AND terminal=1 AND refused=0{filter} ORDER BY sequence LIMIT 64"
        );
        let mut query = self.db.prepare(&sql)?;
        let rows = query.query_map([after], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

fn import_pin_attempt(reference: &str) -> Option<AttemptId> {
    let rest = reference.strip_prefix("refs/provenant/import-extra/")?;
    AttemptId::parse(rest.split('/').next()?).ok()
}

fn import_pin_gc_eligible(store: &RunStore, id: AttemptId, call: &Request<'_>) -> Result<bool> {
    let record = match store.record(id, call) {
        Ok(record) => record,
        // No attempt owns the ref: it outlived its attempt.
        Err(RunError::NotFound) => return Ok(true),
        Err(error) => return Err(error),
    };
    if !matches!(record.state, AttemptState::Terminal { .. }) {
        return Ok(false);
    }
    // A refused import keeps its pins as recovery evidence: its cleanup effect
    // never runs, so it must not be mistaken for a settled attempt here.
    if let Ok(attempt) = store.attempt_dir(id, false)
        && crate::holder::object_import_refused(&attempt, RunStore::scope(&record))?.is_some()
    {
        return Ok(false);
    }
    store.reconcile_is_settled(id, id, call)
}

/// GC backstop: remove the import-extra pins of every settled or missing
/// attempt in one batched `update-ref --stdin` transaction. Live attempts and
/// refused imports keep their pins until settlement releases them. A preview
/// reports what it would remove without touching the ref store.
pub fn gc_import_pins(
    git: &Path,
    repo: &provenant_confinement::repo::VerifiedRepo,
    store: &RunStore,
    call: &Request<'_>,
    dry_run: bool,
) -> Result<usize> {
    let mut eligible = Vec::new();
    for reference in crate::objects::all_import_extra_refs(git, repo)? {
        call.check()?;
        let Some(id) = import_pin_attempt(&reference) else {
            continue;
        };
        // An unreadable attempt is not proof of settlement: keep its pins.
        if let Ok(true) = import_pin_gc_eligible(store, id, call) {
            eligible.push(reference);
        }
    }
    if !dry_run {
        crate::objects::delete_refs(git, repo, &eligible)?;
    }
    Ok(eligible.len())
}

#[cfg(test)]
thread_local! {
    static BEFORE_SETTLEMENT_LEASE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// Custody of an attempt's payload throughout retention operations.
struct Held {
    attempt: Directory,
    continuing: bool,
    _publication: LockGuard,
    _writer: Option<LockGuard>,
    _settlement: LockGuard,
}

impl RunStore {
    pub(crate) fn scratch_in_use(&self, id: AttemptId) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE continues=?1 AND terminal=0) OR EXISTS(SELECT 1 FROM steer_intents WHERE attempt=?1 AND resolved=0)",
            [id.to_string()], |row| row.get(0),
        )?)
    }

    fn hold_attempt(&self, record: &AttemptRecord, call: &Request<'_>) -> Result<Option<Held>> {
        let run = match self.attempt_dir(record.attempt_id, false) {
            Ok(run) => run,
            Err(error) if missing_directory(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let _publication =
            run.try_lock_existing(OsStr::new("publication.lock"), LockMode::Exclusive)?;
        if record.provider_launched == Some(true)
            && crate::artifacts::read_sealed(
                &run,
                AttemptFile::Release.name(),
                crate::RECORD_LIMIT,
            )?
            .is_none()
        {
            return Err(RunError::Validation("attempt release receipt unavailable"));
        }
        let continuing = self.scratch_in_use(record.attempt_id)?;
        // Host retention spans attribution projects. Verify the sealed
        // request against its own indexed run before following a writer's
        // persisted source-project custody namespace.
        let mut context = self.context.clone();
        context.project_id = record.project_id;
        let scoped = RunStore::open_disk_read_only(
            &context,
            &Request {
                clock: &provenant_os::clock::SystemClock,
                cancel: &provenant_core::CancelToken::new(),
            },
        )?;
        let Some(scoped) = scoped else {
            return Ok(None);
        };
        let scoped = scoped.with_writer_registry(&self.writer_registry);
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        if !scoped.custody_released(
            record,
            &Request {
                clock: &clock,
                cancel: &cancel,
            },
        )? {
            return Err(RunError::Validation("attempt custody is not released"));
        }
        let attempt = match self.attempt_dir(record.attempt_id, false) {
            Ok(attempt) => attempt,
            Err(error) if missing_directory(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        if crate::settlement::abandoned(&attempt, Self::scope(record))?.is_some()
            || crate::settlement::queued_abandonment_pending(&attempt, record)?
            || crate::holder::object_import_refused(&attempt, Self::scope(record))?.is_some()
            || crate::artifacts::read_sealed(&attempt, "retained-index.json", crate::RECORD_LIMIT)?
                .is_some()
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
            Ok(true)
                if !scoped.terminal_read_only_without_git_work(
                    record,
                    call,
                    Self::scope(record),
                ) =>
            {
                return Err(RunError::Validation("attempt reservation retained"));
            }
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        // Age cannot grant custody of private, unimported Git objects or an
        // attempt whose loss has not been proven. Apply this to every payload
        // collector, including temporary cleanup and log retention.
        if pinned_payload(&attempt, record)? {
            return Err(RunError::Validation(
                "payload retained: custody proof unavailable",
            ));
        }
        // Keep the driver's lifetime lease until every payload operation ends.
        // A free lease alone is insufficient: settlement can be between retries.
        #[cfg(test)]
        BEFORE_SETTLEMENT_LEASE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let Some(_settlement) = crate::settlement::collection_lease(
            &run,
            Self::scope(record),
            call.clock,
            record.provider_launched != Some(false),
        ) else {
            return Ok(None);
        };
        Ok(Some(Held {
            attempt,
            continuing,
            _publication,
            _writer,
            _settlement,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_phase: Option<&'static str>,
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
        if other.interrupted_at.is_some() {
            self.interrupted_at = other.interrupted_at;
            self.interrupted_phase = other.interrupted_phase;
        }
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
    // Private Git objects live in the sibling scratch's `tmp` until their
    // import settles (#3947). The old attempt-record path is never probed.
    // Check the binding first: without one there is nothing to pin, and a
    // linked or loose-mode tmp must stay collectable so GC can report the
    // refusal at its own path instead of aborting the whole census.
    let private_git = match attempt.metadata_at(OsStr::new("writer-objects.json"), false) {
        Ok(_) => crate::plan::temporary_exists(attempt)?,
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.into()),
    };
    Ok(uncertain_loss || private_git)
}

/// Allocated bytes of the evictable payload: provider state and history,
/// scratch, and capture bytes beyond the retained tail.
fn payload_bytes(
    attempt: &Directory,
    path: &std::path::Path,
    keep_going: &dyn Fn() -> bool,
    cancel: &provenant_core::CancelToken,
) -> Result<u64> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    let mut measured =
        provenant_os::disk::DiskReport::new(path.into(), 0).with_cancellation(cancel.clone());
    let mut bytes = 0_u64;
    for name in [STATE, HISTORY, SCRATCH] {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
        let name = OsStr::new(name);
        if attempt.metadata_at(name, false).is_ok() {
            restore_owned_temporary_directories(attempt, name, keep_going);
            measured.measure_at_while("payload", attempt, name, &path.join(name), keep_going);
        }
    }
    for entry in &measured.entries {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
        bytes = bytes.saturating_add(entry.allocated_bytes);
    }
    for name in [AttemptFile::Stdout.name(), AttemptFile::Stderr.name()] {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
        if let Ok(meta) = attempt.metadata_at(OsStr::new(name), false) {
            bytes = bytes.saturating_add(meta.size.saturating_sub(LOG_TAIL));
        }
    }
    if !keep_going() {
        return Err(RunError::Cancelled);
    }
    if !measured.errors.is_empty() || measured.traversal_exhausted() {
        return Err(RunError::Io(std::io::Error::other(
            "payload measurement incomplete; observed bytes remain uncertain",
        )));
    }
    Ok(bytes)
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
        scan: &RetentionScan<'_>,
    ) -> Result<PayloadReport> {
        let _sql_cancellation = SqlCancellation::arm(&self.db, scan, call);
        let keep_going = &|| scan.permits(call);
        if !keep_going() {
            return Ok(PayloadReport {
                interrupted_at: Some(scan.start_at.map_or_else(
                    || self.context.state_root.as_path().join("attempts"),
                    |id| {
                        self.context
                            .state_root
                            .as_path()
                            .join(format!("attempts/{id}"))
                    },
                )),
                interrupted_phase: Some(if scan.detail == Some("evict") {
                    "evict"
                } else {
                    "measure"
                }),
                cap_bytes,
                ..Default::default()
            });
        }

        if !self.root.metadata()?.same_identity(&expected.metadata()?) {
            return Err(RunError::unsafe_path(".", PathCheck::Identity));
        }
        let (numerator, denominator) = PAYLOAD_LOW_WATER_RATIO;
        let low_water =
            (u128::from(cap_bytes) * u128::from(numerator) / u128::from(denominator)) as u64;
        let mut report = PayloadReport {
            cap_bytes,
            low_water_bytes: low_water,
            ..Default::default()
        };
        let mut ledger = read_ledger(&self.root);
        if measure && scan.detail != Some("evict") {
            self.measure_payloads(&mut ledger, now, &mut report, call, scan)?;
            if report.interrupted_at.is_some() || !report.errors.is_empty() {
                report.accounted_bytes = ledger
                    .attempts
                    .values()
                    .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
                report.pinned_bytes = ledger
                    .attempts
                    .values()
                    .filter(|entry| {
                        entry.pinned
                            || now.as_micros().saturating_sub(entry.finished_micros) < DAY_MICROS
                    })
                    .fold(0_u64, |sum, entry| sum.saturating_add(entry.bytes));
                report.total_reclaimed_bytes = ledger.reclaimed_bytes;
                if !dry_run {
                    write_ledger(&self.root, &ledger)?;
                }
                return Ok(report);
            }
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
        if ledger.measured_micros == 0 {
            report.accounted_bytes = total;
            report.pinned_bytes = ledger.pinned_bytes;
            report.total_reclaimed_bytes = ledger.reclaimed_bytes;
            report.errors.push(ProviderStateIssue {
                path: self.context.state_root.as_path().join(LEDGER),
                reason: "payload census is incomplete; a complete measurement is required before cap eviction".into(),
            });
            return Ok(report);
        }
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
                let path = self
                    .context
                    .state_root
                    .as_path()
                    .join(format!("attempts/{id}"));
                if !scan.permits(call) {
                    report.interrupted_at = Some(path);
                    report.interrupted_phase = Some("evict");
                    break;
                }
                match self.evict_attempt(&id, &entry, now, dry_run, &path, keep_going, call) {
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
                    Err(RunError::Cancelled) => {
                        report.interrupted_at = Some(path);
                        report.interrupted_phase = Some("evict");
                        break;
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
        scan: &RetentionScan<'_>,
    ) -> Result<()> {
        let keep_going = &|| scan.permits(call);

        let mut complete = true;
        let mut after = self.retention_after(scan.start_at)?;
        let mut attempts = if scan.start_at.is_some() {
            ledger.attempts.clone()
        } else {
            BTreeMap::new()
        };
        loop {
            let rows = self.retention_page(false, None, after)?;
            if rows.is_empty() {
                break;
            }
            for (sequence, id, project, raw) in rows {
                let path = self
                    .context
                    .state_root
                    .as_path()
                    .join(format!("attempts/{id}"));
                if !scan.permits(call) {
                    ledger.attempts = attempts;
                    ledger.measured_micros = 0;
                    report.interrupted_at = Some(path);
                    report.interrupted_phase = Some("measure");
                    return Ok(());
                }
                after = sequence;
                let result = (|| -> Result<Option<LedgerEntry>> {
                    let record: AttemptRecord = crate::db::decode_attempt(&raw)?;
                    crate::reader::validate_attempt(&record)?;
                    if record.attempt_id.to_string() != id
                        || record.project_id.to_string() != project
                    {
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
                    let (held, pinned) = match self.hold_attempt(&record, call) {
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
                    let bytes = payload_bytes(&attempt, &path, keep_going, call.cancel)?;
                    if !keep_going() {
                        return Err(RunError::Cancelled);
                    }
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
                    Ok(None) => {
                        attempts.remove(&id);
                    }
                    Err(RunError::Cancelled) => {
                        ledger.attempts = attempts;
                        ledger.measured_micros = 0;
                        report.interrupted_at = Some(path);
                        report.interrupted_phase = Some("measure");
                        return Ok(());
                    }
                    Err(error) => {
                        complete = false;
                        if let Some(mut prior) = ledger.attempts.get(&id).cloned() {
                            prior.pinned = true;
                            attempts.insert(id, prior);
                        }
                        issue(report, path, &error);
                    }
                }
            }
        }
        ledger.attempts = attempts;
        ledger.measured_micros = if complete { now.as_micros().max(1) } else { 0 };
        Ok(())
    }

    /// Evict one attempt's payload under its locks. `false` means custody
    /// refused it on recheck and nothing changed.
    #[allow(clippy::too_many_arguments)] // Revalidated custody and cooperative scan accompany the payload ledger entry.
    fn evict_attempt(
        &self,
        id: &str,
        entry: &LedgerEntry,
        now: WallTime,
        dry_run: bool,
        path: &std::path::Path,
        keep_going: &dyn Fn() -> bool,
        call: &Request<'_>,
    ) -> Result<bool> {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }

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
        let Some(held) = self.hold_attempt(&record, call)? else {
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
            if !keep_going() {
                return Err(RunError::Cancelled);
            }
            retry_tombstones(attempt, &[name], false, &mut removed, keep_going)?;
            dispose(attempt, OsStr::new(name), false, &mut removed, keep_going)?;
        }
        collect_logs(attempt, path, false, &mut removed, keep_going)?;
        if !removed.errors.is_empty() || !removed.kept.is_empty() {
            return Err(RunError::Validation("payload removal incomplete"));
        }
        seal(attempt, EVICTED, b"{}\n", 64)?;
        attempt.sync()?;
        Ok(true)
    }
}

fn retention_interrupted(error: &RunError) -> bool {
    matches!(
        error,
        RunError::Cancelled | RunError::Filesystem(provenant_os::fs::FsError::Cancelled { .. })
    )
}

fn record_issue(report: &mut ProviderStateGc, path: PathBuf, error: &RunError) {
    if retention_interrupted(error) {
        return;
    }
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
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 * 1024;
    for name in [AttemptFile::Stdout.name(), AttemptFile::Stderr.name()] {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
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

/// Dispose of the per-attempt scratch root once its `tmp` has been collected.
/// The short alias is unlinked with `tmp`; this only removes the empty `<id>`
/// directory so terminal attempts do not leave one directory each behind.
pub(crate) fn collect_empty_scratch_root(scratch: &Directory) -> Result<()> {
    if !scratch
        .metadata_at(OsStr::new("tmp"), false)
        .is_err_and(|error| error.is_not_found())
        || scratch.entries()?.next().is_some()
    {
        return Ok(());
    }
    let path = scratch.current_path()?;
    let parent = path
        .parent()
        .ok_or(RunError::Validation("scratch parent directory"))?;
    let name = path
        .file_name()
        .ok_or(RunError::Validation("scratch directory name"))?;
    let root = Directory::open_anchor(parent)?;
    match root.rmdir(name) {
        Ok(()) => root.sync()?,
        Err(error) if error.is_not_found() => {}
        Err(error) => {
            // A new entry from a concurrent writer means it is no longer ours
            // to remove; anything else is a real cleanup failure.
            if scratch.entries()?.next().is_some() {
                return Ok(());
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn collect_temporary(
    scratch: &Directory,
    path: &std::path::Path,
    dry_run: bool,
    report: &mut ProviderStateGc,
    preview_sizes: &mut provenant_os::disk::DiskReport,
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    let name = OsStr::new("tmp");
    let metadata = match scratch.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => {
            if !dry_run {
                crate::short_temporary::remove(path);
                collect_empty_scratch_root(scratch)?;
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let _cargo_guards = if metadata.kind == FileKind::Directory {
        let tmp_dir = scratch.open_verified_directory(name)?;
        match provenant_confinement::cache::lock_scratch_cargo_profiles(&tmp_dir, keep_going) {
            Some(guards) => guards,
            None if !keep_going() => return Err(RunError::Cancelled),
            None => {
                report.kept.push(ProviderStateIssue {
                    path: path.into(),
                    reason: "Cargo lock held or tmp could not be probed".into(),
                });
                return Ok(());
            }
        }
    } else {
        Vec::new()
    };
    let bytes = if dry_run {
        // A preview must not repair permissions or restart its walk budget for
        // each attempt. Size errors are explicit lower bounds only.
        preview_sizes.entries.clear();
        let measured = preview_sizes;
        measured.measure_at("attempt_tmp", scratch, name, path);
        for error in measured.errors.drain(..) {
            report.errors.push(ProviderStateIssue {
                path: error.path,
                reason: format!("temporary size estimate incomplete: {}", error.reason),
            });
        }
        measured.entries.iter().fold(0_u64, |sum, entry| {
            sum.saturating_add(entry.allocated_bytes)
        })
    } else {
        let mut bytes = 0;
        let result = crate::artifacts::remove_unreserved_terminal_temporary(
            scratch, path, keep_going, &mut bytes,
        );
        if result.is_err() && bytes > 0 {
            report.partial_tmp.push(AttemptTmpRemoval {
                path: path.into(),
                bytes,
            });
        }
        result?;
        scratch.sync()?;
        bytes
    };
    report.attempt_tmp.push(AttemptTmpRemoval {
        path: path.into(),
        bytes,
    });
    if !dry_run {
        collect_empty_scratch_root(scratch)?;
    }
    Ok(())
}

/// A directory with no owner read/search bits cannot be traversed by the
/// estimator. Restore owner rwx through each held parent before measuring;
/// symlinks and foreign-owned directories are deliberately left alone.
fn restore_owned_temporary_directories(
    parent: &Directory,
    name: &OsStr,
    keep_going: &dyn Fn() -> bool,
) {
    if !keep_going() {
        return;
    }

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
        if !keep_going() {
            return;
        }
        restore_owned_temporary_directories(&directory, &entry, keep_going);
    }
}

pub(crate) fn prune_caches(
    attempt: &Directory,
    record: &AttemptRecord,
    dry_run: bool,
    report: &mut ProviderStateGc,
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

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
    let candidates = candidates(&state, keep_going)?;
    for name in &candidates {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
        let candidate = subdir(&state, name, false)?;
        let provider = candidate_provider(&candidate)?;
        let first = report.removed.len();
        let removal = (|| -> Result<()> {
            if let Some((_, caches)) = PROVIDER_CACHES.iter().find(|(id, _)| *id == provider) {
                retry_tombstones(&candidate, caches, dry_run, report, keep_going)?;
                for cache in *caches {
                    if !keep_going() {
                        return Err(RunError::Cancelled);
                    }
                    dispose(&candidate, OsStr::new(cache), dry_run, report, keep_going)?;
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

fn candidates(state: &Directory, keep_going: &dyn Fn() -> bool) -> Result<Vec<OsString>> {
    Ok(names(state, keep_going)?
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
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    let Some(state) = existing(attempt, STATE)? else {
        return Ok(());
    };
    for name in candidates(&state, keep_going)? {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
        let candidate = match subdir(&state, &name, false) {
            Ok(candidate) => candidate,
            Err(error) => {
                record_issue(report, path.join(&name), &error);
                continue;
            }
        };
        for credential in CREDENTIALS {
            if !keep_going() {
                return Err(RunError::Cancelled);
            }
            let first = report.removed.len();
            let result = (|| -> Result<()> {
                let mut parent = candidate.try_clone()?;
                let mut parts = credential.split('/').peekable();
                while let Some(part) = parts.next() {
                    if parts.peek().is_none() {
                        retry_tombstones(&parent, &[part], dry_run, report, keep_going)?;
                        dispose(&parent, OsStr::new(part), dry_run, report, keep_going)?;
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

fn expire(
    attempt: &Directory,
    dry_run: bool,
    report: &mut ProviderStateGc,
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    retry_tombstones(attempt, &[STATE], dry_run, report, keep_going)?;
    let Some(state) = existing(attempt, STATE)? else {
        return Ok(());
    };
    if !dry_run {
        seal(attempt, EXPIRED, b"{}\n", 64)?;
        let history = subdir(attempt, OsStr::new(HISTORY), true)?;
        let candidate_names = candidates(&state, keep_going)?;
        // Whole candidates may already be archived by an interrupted sweep.
        for name in &candidate_names {
            if !keep_going() {
                return Err(RunError::Cancelled);
            }
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
            for part in names(&candidate, keep_going)? {
                if !keep_going() {
                    return Err(RunError::Cancelled);
                }
                if part.to_str().is_some_and(transcript) {
                    candidate.rename_noreplace(&part, &archive, &part)?;
                    archive.sync()?;
                    candidate.sync()?;
                }
            }
            for (parent, artifact) in [(".cursor", "chats"), ("data", "opencode")] {
                if !keep_going() {
                    return Err(RunError::Cancelled);
                }
                if let Some(source) = existing(&candidate, parent)? {
                    match source.metadata_at(OsStr::new(artifact), false) {
                        Ok(_) => {
                            if artifact == "opencode"
                                && let Some(data) = existing(&source, artifact)?
                            {
                                // The launcher places an authentication link here.
                                // Never archive it if earlier credential cleanup was refused.
                                let first = report.removed.len();
                                dispose(&data, OsStr::new("auth.json"), false, report, keep_going)?;
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
    dispose(attempt, OsStr::new(STATE), dry_run, report, keep_going)
}

fn prefix_removals(report: &mut ProviderStateGc, first: usize, prefix: &std::path::Path) {
    for path in &mut report.removed[first..] {
        *path = prefix.join(&*path);
    }
}

fn names(directory: &Directory, keep_going: &dyn Fn() -> bool) -> Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in directory.entries()? {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
        names.push(entry?);
    }
    Ok(names)
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
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    let metadata = match parent.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !dry_run {
        if metadata.kind == FileKind::Directory {
            let detached = tombstone_name(metadata.device, metadata.inode, name);
            parent.rename_noreplace(name, parent, &detached)?;
            parent.sync()?;
            remove_detached(
                parent,
                &detached,
                metadata.device,
                metadata.inode,
                keep_going,
            )?;
        } else {
            parent.unlink(name)?;
            parent.sync()?;
        }
    }
    report.removed.push(PathBuf::from(name));
    Ok(())
}

fn tombstone_name(device: u64, inode: u64, original: &OsStr) -> OsString {
    OsString::from(format!(
        "{TOMBSTONE}{device:016x}{inode:016x}-{}",
        original.to_string_lossy()
    ))
}

fn retry_tombstones(
    parent: &Directory,
    allowed: &[&str],
    dry_run: bool,
    report: &mut ProviderStateGc,
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

    for name in names(parent, keep_going)? {
        if !keep_going() {
            return Err(RunError::Cancelled);
        }
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
            remove_detached(parent, &name, device, inode, keep_going)?;
        }
        report.removed.push(PathBuf::from(name));
    }
    Ok(())
}

fn remove_detached(
    parent: &Directory,
    name: &OsStr,
    device: u64,
    inode: u64,
    keep_going: &dyn Fn() -> bool,
) -> Result<()> {
    if !keep_going() {
        return Err(RunError::Cancelled);
    }

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
    crate::artifacts::remove_payload_contents(&tree, keep_going, &mut 0)?;
    if !parent.metadata_at(name, false)?.same_identity(&metadata) {
        return Err(RunError::unsafe_entry(parent, name, PathCheck::Identity));
    }
    parent.rmdir(name)?;
    parent.sync()?;
    Ok(())
}

#[cfg(test)]
impl RunStore {
    pub(crate) fn temporary_retained(&self, id: AttemptId, call: &Request<'_>) -> Result<bool> {
        call.check()?;
        Ok(self.db.query_row(
            "SELECT tmp_retained FROM attempts WHERE id=?1 AND project=?2",
            rusqlite::params![id.to_string(), self.context.project_id.to_string()],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests;
