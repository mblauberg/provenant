//! Temporary and provider-private state retention after terminal settlement.
use crate::{
    artifacts::{seal, subdir},
    db::decode,
    *,
};
use provenant_core::{AttemptId, WallTime};
use provenant_os::fs::{Directory, FileKind, LockGuard, LockMode, PayloadTree};
use provenant_records::AttemptFile;
use serde::Serialize;
use std::{
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
        let record = self.record(id, call)?;
        let run = self.run_dir(record.run_id, false)?;
        let lease = run.lock(
            OsStr::new("publication.lock"),
            LockMode::Shared,
            call.budget,
            call.cancel,
            call.clock,
        )?;
        let attempt = self.attempt_dir(record.run_id, id, false)?;
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
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(expected, 7, 30, now, dry_run, true, None, call)
    }

    /// Pressure collection restricted to the selected project identities.
    pub fn gc_temporary_for_projects(
        &self,
        expected: &Directory,
        now: WallTime,
        dry_run: bool,
        projects: &[provenant_core::ProjectId],
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        self.gc_retention(expected, 7, 30, now, dry_run, true, Some(projects), call)
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
        let rows: Vec<(String, String, String, Vec<u8>)> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT id,run,project,record FROM attempts WHERE terminal=1 ORDER BY sequence",
            )?;
            let rows = query.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut report = ProviderStateGc::default();
        for (id, run, project, raw) in rows {
            if projects.is_some_and(|projects| !projects.iter().any(|id| id.to_string() == project))
            {
                continue;
            }
            let path = self
                .context
                .state_root
                .as_path()
                .join(format!("runs/{run}/attempts/{id}/provider-state"));
            let first_removal = report.removed.len();
            let first_tightened = report.tightened.len();
            let result = (|| -> Result<()> {
                let record: AttemptRecord = decode(&raw)?;
                crate::reader::validate_attempt(&record)?;
                if record.attempt_id.to_string() != id
                    || record.run_id.to_string() != run
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
                let run = match self.run_dir(record.run_id, false) {
                    Ok(run) => run,
                    Err(error) if missing_directory(&error) => return Ok(()),
                    Err(error) => return Err(error),
                };
                let _publication =
                    run.try_lock_existing(OsStr::new("publication.lock"), LockMode::Exclusive)?;
                // Collection may spend longer than one DB wait budget
                // deleting payloads. Bound each new observation separately.
                let observation = Request {
                    clock: call.clock,
                    cancel: call.cancel,
                    budget: provenant_core::WaitBudget::cli(call.clock.mono_now(), 5)
                        .map_err(|_| RunError::Contention)?,
                };
                let continuing: bool = observation.retry(|| Ok(self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM continuations c JOIN attempts a ON a.run=c.run WHERE c.target=?1 AND a.terminal=0)",
                    [&id], |row| row.get(0))?))?;
                let reservations = self.writer_registry.join("worktrees").join(project);
                match Directory::open_anchor(&reservations) {
                    Ok(directory) => {
                        let listing =
                            provenant_supervisor::custody::list_writer_reservations(&directory)
                                .map_err(|error| std::io::Error::other(error.to_string()))?;
                        if !listing.issues.is_empty()
                            || listing.reservations.iter().any(|reservation| {
                                reservation.scope.run == record.run_id
                                    && reservation.scope.attempt == record.attempt_id
                            })
                        {
                            return Err(RunError::Validation(
                                "writer reservation retained or unreadable",
                            ));
                        }
                    }
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
                let attempt = match self.attempt_dir(record.run_id, record.attempt_id, false) {
                    Ok(attempt) => attempt,
                    Err(error) if missing_directory(&error) => return Ok(()),
                    Err(error) => return Err(error),
                };
                let _writer = match attempt.try_lock_existing(
                    OsStr::new(AttemptFile::WriterLock.name()),
                    LockMode::Exclusive,
                ) {
                    Ok(lease) => Some(lease),
                    Err(error) if error.is_not_found() => None,
                    Err(error) => return Err(error.into()),
                };
                match attempt.metadata_at(OsStr::new(AttemptFile::Reservation.name()), false) {
                    Ok(_) => return Err(RunError::Validation("attempt reservation retained")),
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
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
                        collect_temporary(&attempt, &temporary, dry_run, &mut report)?;
                        if !dry_run {
                            let update = Request {
                                budget: provenant_core::WaitBudget::cli(call.clock.mono_now(), 5)
                                    .map_err(|_| RunError::Contention)?,
                                ..observation
                            };
                            self.write(&update, |tx| {
                                tx.execute(
                                    "UPDATE attempts SET tmp_retained=0 WHERE id=?1 AND run=?2 AND project=?3 AND terminal=1",
                                    rusqlite::params![id, record.run_id.to_string(), record.project_id.to_string()],
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
                    // A terminal attempt in an open run still contributes to live
                    // diagnostics. Expiry requires every sibling to be terminal.
                    let open: bool = observation.retry(|| {
                        Ok(self.db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM attempts WHERE run=?1 AND terminal=0)",
                            [record.run_id.to_string()],
                            |row| row.get(0),
                        )?)
                    })?;
                    if !open {
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
                            observation.retry(|| {
                                Ok(self.db.query_row(
                                    "SELECT COUNT(*) FROM events WHERE attempt=?1 AND project=?2 AND sequence < (SELECT MAX(sequence) FROM events WHERE attempt=?1 AND project=?2)",
                                    rusqlite::params![id, record.project_id.to_string()],
                                    |row| row.get::<_, i64>(0),
                                )?)
                            })
                            .map(|count| u64::try_from(count).unwrap_or(0))?
                        } else {
                            let update = Request {
                                budget: provenant_core::WaitBudget::cli(call.clock.mono_now(), 5)
                                    .map_err(|_| RunError::Contention)?,
                                ..observation
                            };
                            self.write(&update, |tx| {
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
                    expire(&attempt, &record, dry_run, &mut report)?;
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
                return Err(RunError::unsafe_path(path.join(name), check));
            }
            if before.size <= TAIL {
                return Ok(());
            }
            if !dry_run {
                let mut file = attempt.open_read(name_os)?;
                if !before.unchanged(&file.metadata()?) {
                    return Err(RunError::unsafe_path(path.join(name), PathCheck::Identity));
                }
                file.seek(SeekFrom::End(-(TAIL as i64)))?;
                let mut tail = Vec::new();
                file.take(TAIL).read_to_end(&mut tail)?;
                if !before.unchanged(&attempt.metadata_at(name_os, false)?) {
                    return Err(RunError::unsafe_path(path.join(name), PathCheck::Identity));
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
                    return Err(RunError::unsafe_path(path.join(name), PathCheck::Identity));
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
) -> Result<()> {
    let name = OsStr::new("tmp");
    match attempt.metadata_at(name, false) {
        Ok(_) => {}
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let mut measured = provenant_os::disk::DiskReport::new(path.into(), 0);
    measured.measure_at("attempt_tmp", attempt, name, path);
    for error in measured.errors {
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
        let provider = candidate_provider(&candidate, || {
            candidate_provider_fallback(&candidate, record, candidates.len())
        })?;
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

fn contains_session(candidate: &Directory, session: &str) -> bool {
    fn visit(directory: &Directory, session: &str, depth: u8, remaining: &mut u32) -> bool {
        let Ok(entries) = directory.entries() else {
            return false;
        };
        for entry in entries {
            if *remaining == 0 {
                return false;
            }
            *remaining -= 1;
            let Ok(name) = entry else {
                return false;
            };
            if name.to_string_lossy().contains(session) {
                return true;
            }
            if depth > 0
                && directory
                    .open_verified_directory(&name)
                    .is_ok_and(|child| visit(&child, session, depth - 1, remaining))
            {
                return true;
            }
        }
        false
    }
    !session.is_empty() && visit(candidate, session, 32, &mut 100_000)
}

fn missing_directory(error: &RunError) -> bool {
    matches!(error, RunError::Filesystem(error) if error.is_not_found())
}

fn candidate_provider_fallback<'a>(
    candidate: &Directory,
    record: &'a AttemptRecord,
    candidate_count: usize,
) -> Option<&'a str> {
    record.applied_route.as_ref().and_then(|route| {
        route.provider.as_deref().filter(|_| {
            candidate_count == 1
                || record
                    .session
                    .as_deref()
                    .is_some_and(|session| contains_session(candidate, session))
        })
    })
}

fn candidate_provider<'a>(
    candidate: &Directory,
    fallback: impl FnOnce() -> Option<&'a str>,
) -> Result<String> {
    match candidate.read_bounded(OsStr::new("provider.txt"), 64) {
        Ok(raw) => String::from_utf8(raw).map_err(|_| RunError::Validation("provider marker")),
        Err(error) if error.is_not_found() => Ok(fallback().unwrap_or_default().into()),
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
                    RunError::unsafe_path(credential_path.clone(), PathCheck::Containment)
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
    record: &AttemptRecord,
    dry_run: bool,
    report: &mut ProviderStateGc,
) -> Result<()> {
    retry_tombstones(attempt, &[STATE], dry_run, report)?;
    let Some(state) = existing(attempt, STATE)? else {
        return Ok(());
    };
    if !dry_run {
        seal(attempt, EXPIRED, b"{}\n", 64)?;
        let history = subdir(attempt, OsStr::new(HISTORY), true)?;
        let candidate_names = candidates(&state)?;
        // Whole candidates may already be archived by an interrupted sweep.
        let mut all_candidates = candidates(&history)?;
        all_candidates.extend(candidate_names.iter().cloned());
        all_candidates.sort();
        all_candidates.dedup();
        for name in &candidate_names {
            let candidate = subdir(&state, name, false)?;
            let provider = candidate_provider(&candidate, || {
                candidate_provider_fallback(&candidate, record, all_candidates.len()).or_else(
                    || {
                        // A previous sweep may already have moved the session.
                        history
                            .open_verified_directory(name)
                            .ok()
                            .and_then(|archive| {
                                candidate_provider_fallback(&archive, record, all_candidates.len())
                            })
                    },
                )
            })?;
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
        return Err(RunError::unsafe_path(
            std::path::Path::new(STATE)
                .join(candidate)
                .join("claude-config"),
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
    use crate::tests::{call, clock, context, input, route};
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
        let attempt = context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}",
            input.run_id, input.attempts[0].attempt_id
        ));
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
            input.attempts[0].attempt_id,
            AttemptState::Queued,
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
        let attempt = context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}",
            input.run_id, input.attempts[0].attempt_id
        ));
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
                input.attempts[0].attempt_id,
                AttemptState::Queued,
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
            call.budget,
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
                input.attempts[0].attempt_id,
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
        let id = input.attempts[0].attempt_id;
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
        let id = input.attempts[0].attempt_id;
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
        for targets in [
            vec![],
            vec![LaneTarget {
                run: input.run_id,
                task: None,
                attempt: Some(id),
            }],
        ] {
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
    fn gc_retains_events_while_sibling_attempt_in_same_run_is_open() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let mut input = input();
        let task2 = provenant_core::TaskId::from_bytes([20; 16]);
        let attempt2 = AttemptId::from_bytes([21; 16]);
        let mut task_def = input.request.tasks[0].clone();
        task_def.task_id = task2;
        input.request.tasks.push(task_def);
        input.attempts.push(NewAttempt {
            task_id: task2,
            attempt_id: attempt2,
            prompt: "exact prompt".into(),
        });
        store.create(&input, &call)?;
        let first = input.attempts[0].attempt_id;
        store.withdraw(
            first,
            AttemptState::Queued,
            Outcome::Cancelled,
            "first finished",
            WallTime::from_micros(200)?,
            &call,
        )?;
        let events = store.events(0, 64, Some(first), &call)?;
        assert!(!events.is_empty());
        // Sibling attempt2 is still Queued (terminal == 0).
        sweep(&store, &context, 7, 31 * DAY_MICROS, false, &call)?;
        assert_eq!(
            store.events(0, 64, Some(first), &call)?.len(),
            events.len(),
            "events must survive while run has an open attempt"
        );
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
            WallTime::from_micros(200 + 31 * DAY_MICROS)?,
            false,
            &[provenant_core::ProjectId::from_bytes([99; 16])],
            &call,
        )?;
        assert!(scoped.attempt_tmp.is_empty());
        assert!(attempt.join("tmp/payload").is_file());
        let report = store.gc_temporary(
            &Directory::open_anchor(context.state_root.as_path())?,
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
    fn gc_logs_use_configured_age_and_keep_an_open_runs_captures() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let mut input = input();
        let sibling_id = AttemptId::from_bytes([91; 16]);
        let sibling_task = provenant_core::TaskId::from_bytes([92; 16]);
        let mut task = input.request.tasks[0].clone();
        task.task_id = sibling_task;
        input.request.tasks.push(task);
        input.attempts.push(NewAttempt {
            attempt_id: sibling_id,
            task_id: sibling_task,
            prompt: input.attempts[0].prompt.clone(),
        });
        store.create(&input, &call)?;
        let attempt = context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}",
            input.run_id, input.attempts[0].attempt_id
        ));
        withdraw(&store, &input, &call)?;
        fs::write(attempt.join("stdout.log"), vec![1; 200_000])?;
        let root = Directory::open_anchor(context.state_root.as_path())?;
        let now = WallTime::from_micros(200 + 2 * DAY_MICROS)?;
        assert!(
            store
                .gc_provider_state_with_logs(&root, 7, 1, now, false, &call)?
                .attempt_logs
                .is_empty()
        );
        store.withdraw(
            sibling_id,
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(200)?,
            &call,
        )?;
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
        let record = store.record(input.attempts[0].attempt_id, &call)?;
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
        use provenant_core::{Clock, MonoTime, TimeError, WaitBudget};
        // Each DB observation completes within its own five-second budget,
        // while time between observations exceeds the original deadline.
        struct AdvancingClock(std::cell::Cell<u64>);
        impl Clock for AdvancingClock {
            fn mono_now(&self) -> MonoTime {
                let now = self.0.get();
                self.0.set(now + 2_000_000);
                MonoTime::from_micros(now)
            }
            fn wall_now(&self) -> std::result::Result<WallTime, TimeError> {
                WallTime::from_micros(200 + DAY_MICROS)
            }
            fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), TimeError> {
                self.0.set(self.0.get().max(deadline.as_micros()));
                Ok(())
            }
        }
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let id = input.attempts[0].attempt_id;
        withdraw(&store, &input, &call)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(attempt.join("tmp"))?;
        store.set_temporary_retained(id, true, &call)?;
        let slow = AdvancingClock(std::cell::Cell::new(0));
        let call = Request {
            clock: &slow,
            cancel: &cancel,
            budget: WaitBudget::cli(MonoTime::from_micros(0), 100)?,
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
            let id = input.attempts[0].attempt_id;
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
        let id = input.attempts[0].attempt_id;
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
        assert_eq!(decode::<AttemptRecord>(&raw)?, terminal);
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
        let run = store.run_dir(input.run_id, false)?;
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
        fs::write(attempt.join("reservation.json"), "{}")?;
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
        fs::remove_file(attempt.join("reservation.json"))?;
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
    fn candidate_provider_only_uses_fallback_for_a_missing_marker() -> TestResult {
        let temp = tempfile::tempdir()?;
        let candidate = Directory::open_anchor(temp.path())?;
        let calls = std::cell::Cell::new(0);
        let fallback = || {
            calls.set(calls.get() + 1);
            Some("claude")
        };
        fs::write(temp.path().join("provider.txt"), "codex")?;
        assert_eq!(candidate_provider(&candidate, fallback)?, "codex");
        fs::write(temp.path().join("provider.txt"), [0xff])?;
        assert!(candidate_provider(&candidate, fallback).is_err());
        assert_eq!(calls.get(), 0);
        fs::remove_file(temp.path().join("provider.txt"))?;
        assert_eq!(candidate_provider(&candidate, fallback)?, "claude");
        assert_eq!(calls.get(), 1);
        Ok(())
    }

    #[test]
    fn interrupted_expiry_keeps_legacy_candidate_classification() -> TestResult {
        for whole_candidate in [false, true] {
            let (_temp, _context, store, input, attempt) = seeded()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let mut record = withdraw(&store, &input, &call)?;
            let mut applied = route();
            applied.provider = Some("codex".into());
            record.applied_route = Some(applied);
            record.session = Some("legacy-session".into());
            fs::rename(
                attempt.join("provider-state/candidate-0/sessions/session.jsonl"),
                attempt.join("provider-state/candidate-0/sessions/legacy-session.jsonl"),
            )?;
            let state = attempt.join("provider-state");
            for name in ["candidate-0", "candidate-1"] {
                fs::remove_file(state.join(name).join("provider.txt"))?;
            }
            let history = attempt.join("provider-history");
            fs::DirBuilder::new().mode(0o700).create(&history)?;
            if whole_candidate {
                fs::rename(state.join("candidate-0"), history.join("candidate-0"))?;
            } else {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(history.join("candidate-0"))?;
                fs::rename(
                    state.join("candidate-0/sessions"),
                    history.join("candidate-0/sessions"),
                )?;
            }
            let directory = Directory::open_anchor(&attempt)?;
            seal(&directory, EXPIRED, b"{}\n", 64)?;
            expire(&directory, &record, false, &mut ProviderStateGc::default())?;
            assert!(!state.exists());
            assert!(
                history
                    .join("candidate-0/sessions/legacy-session.jsonl")
                    .exists()
            );
            assert!(history.join("candidate-1/unknown-session").exists());
            if !whole_candidate {
                assert!(!history.join("candidate-0/other").exists());
            }
        }
        Ok(())
    }

    #[test]
    fn legacy_candidates_use_provider_of_candidate_holding_session() -> TestResult {
        let (_temp, _context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let candidate0 = attempt.join("provider-state/candidate-0");
        let candidate1 = attempt.join("provider-state/candidate-1");
        fs::remove_file(candidate0.join("provider.txt"))?;
        fs::remove_file(candidate1.join("provider.txt"))?;
        fs::create_dir(candidate0.join("cache"))?;
        fs::create_dir(candidate1.join("cache"))?;
        fs::write(candidate0.join("cache/keep"), "first")?;
        fs::write(candidate1.join("cache/remove"), "applied")?;
        fs::create_dir(candidate1.join("sessions"))?;
        fs::write(candidate1.join("sessions/legacy-session.jsonl"), "session")?;
        fs::write(candidate1.join("config.toml"), "disposable")?;

        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        record.state = AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            exit_code: None,
            finished_at: WallTime::from_micros(200)?,
            reason: None,
        };
        let mut applied = route();
        applied.provider = Some("codex".into());
        record.applied_route = Some(applied);
        record.session = Some("legacy-session".into());

        let attempt_dir = Directory::open_anchor(&attempt)?;
        let mut report = ProviderStateGc::default();
        prune_caches(&attempt_dir, &record, false, &mut report)?;
        assert!(candidate0.join("cache/keep").exists());
        assert!(!candidate1.join("cache").exists());

        expire(&attempt_dir, &record, false, &mut report)?;
        assert!(!attempt.join("provider-state").exists());
        let history = attempt.join("provider-history");
        assert!(history.join("candidate-0/cache/keep").exists());
        assert_eq!(
            fs::read_to_string(history.join("candidate-1/sessions/legacy-session.jsonl"))?,
            "session"
        );
        assert!(!history.join("candidate-1/config.toml").exists());
        Ok(())
    }

    #[test]
    fn gc_ignores_indexed_attempt_with_missing_run_directory() -> TestResult {
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let terminal = withdraw(&store, &input, &call)?;
        let run = attempt
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("run path")?;
        fs::remove_dir_all(&attempt)?;

        let report = sweep(&store, &context, 7, 0, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.kept.is_empty(), "{:?}", report.kept);
        assert!(store.record(terminal.attempt_id, &call).is_ok());

        fs::remove_dir_all(run)?;
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
        use provenant_core::{OperationId, RunId};
        let (_temp, context, store, input, attempt) = seeded()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        withdraw(&store, &input, &call)?;
        let resume = Resume {
            prior: input.attempts[0].attempt_id,
            expected_generation: 0,
            operation_id: OperationId::parse("continue-fixture")?,
            run_id: RunId::from_bytes([8; 16]),
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
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(400)?,
            &call,
        )?;
        let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join("provider-state").exists());
        assert_eq!(store.resume(&resume, &call)?.run_id, resume.run_id);
        let fresh = Resume {
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
}
