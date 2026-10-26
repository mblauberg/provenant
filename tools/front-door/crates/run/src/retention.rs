//! Provider-private state retention after terminal settlement.
use crate::{
    artifacts::{seal, subdir},
    db::decode,
    *,
};
use provenant_core::{AttemptId, WallTime};
use provenant_supervisor::fs::{Directory, FileKind, LockGuard, LockMode, PayloadTree};
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

const DAY_MICROS: i64 = 86_400_000_000;
const EXPIRED: &str = "provider-state-expired.json";
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
    pub kept: Vec<ProviderStateIssue>,
    pub errors: Vec<ProviderStateIssue>,
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
    /// this store. No records or digests are rewritten by retention.
    pub fn gc_provider_state(
        &self,
        expected: &Directory,
        days: u64,
        now: WallTime,
        dry_run: bool,
        call: &Request<'_>,
    ) -> Result<ProviderStateGc> {
        if !self.root.metadata()?.same_identity(&expected.metadata()?) {
            return Err(RunError::UnsafePath);
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
            let path = self
                .context
                .state_root
                .as_path()
                .join(format!("runs/{run}/attempts/{id}/provider-state"));
            let first_removal = report.removed.len();
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
                if continuing {
                    return Err(RunError::Validation(
                        "a continuation still needs this provider state",
                    ));
                }
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
                let _writer = match attempt
                    .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                {
                    Ok(lease) => Some(lease),
                    Err(error) if error.is_not_found() => None,
                    Err(error) => return Err(error.into()),
                };
                match attempt.metadata_at(OsStr::new("reservation.json"), false) {
                    Ok(_) => return Err(RunError::Validation("attempt reservation retained")),
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
                let age = now.as_micros().saturating_sub(finished_at.as_micros());
                if age >= DAY_MICROS {
                    credentials(&attempt, dry_run, &mut report)?;
                }
                prune_caches(&attempt, &record, dry_run, &mut report)?;
                if age >= DAY_MICROS.saturating_mul(days as i64) {
                    expire(&attempt, &record, dry_run, &mut report)?;
                }
                Ok(())
            })();
            prefix_removals(
                &mut report,
                first_removal,
                path.parent().ok_or(RunError::UnsafePath)?,
            );
            if let Err(error) = result {
                let cancelled = matches!(error, RunError::Cancelled);
                let issue = ProviderStateIssue {
                    path,
                    reason: error.to_string(),
                };
                if matches!(
                    error,
                    RunError::Validation(_)
                        | RunError::Filesystem(
                            provenant_supervisor::fs::FsError::LockTimeout { .. }
                        )
                ) {
                    report.kept.push(issue);
                } else {
                    report.errors.push(issue);
                }
                if cancelled {
                    break;
                }
            }
        }
        Ok(report)
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
    let Some(state) = existing(attempt, "provider-state")? else {
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
        prefix_removals(
            report,
            first,
            &std::path::Path::new("provider-state").join(name),
        );
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

fn credentials(attempt: &Directory, dry_run: bool, report: &mut ProviderStateGc) -> Result<()> {
    let Some(state) = existing(attempt, "provider-state")? else {
        return Ok(());
    };
    for name in candidates(&state)? {
        let candidate = subdir(&state, &name, false)?;
        for credential in CREDENTIALS {
            let first = report.removed.len();
            let mut parent = candidate.try_clone()?;
            let mut parts = credential.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    retry_tombstones(&parent, &[part], dry_run, report)?;
                    dispose(&parent, OsStr::new(part), dry_run, report)?;
                } else if let Some(child) = existing(&parent, part)? {
                    parent = child;
                } else {
                    break;
                }
            }
            let prefix = std::path::Path::new("provider-state").join(&name).join(
                std::path::Path::new(credential)
                    .parent()
                    .ok_or(RunError::UnsafePath)?,
            );
            prefix_removals(report, first, &prefix);
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
    retry_tombstones(attempt, &["provider-state"], dry_run, report)?;
    let Some(state) = existing(attempt, "provider-state")? else {
        return Ok(());
    };
    if !dry_run {
        seal(attempt, EXPIRED, b"{}\n", 64)?;
        let history = subdir(attempt, OsStr::new("provider-history"), true)?;
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
            if let Some(config) = existing(&candidate, "claude-config")? {
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
    dispose(attempt, OsStr::new("provider-state"), dry_run, report)
}

fn prefix_removals(report: &mut ProviderStateGc, first: usize, prefix: &std::path::Path) {
    for path in &mut report.removed[first..] {
        *path = prefix.join(&*path);
    }
}

fn names(directory: &Directory) -> Result<Vec<OsString>> {
    Ok(directory
        .entries()?
        .collect::<provenant_supervisor::fs::Result<_>>()?)
}

fn existing(parent: &Directory, name: &str) -> Result<Option<Directory>> {
    match subdir(parent, OsStr::new(name), false) {
        Ok(directory) => Ok(Some(directory)),
        Err(RunError::Filesystem(error)) if error.is_not_found() => Ok(None),
        Err(error) => Err(error),
    }
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
        return Err(RunError::UnsafePath);
    }
    let directory = parent.open_verified_directory(name)?;
    if !directory.metadata()?.same_identity(&metadata) {
        return Err(RunError::UnsafePath);
    }
    directory.make_private()?;
    let tree = PayloadTree::claim(parent, name, &metadata)?;
    remove_contents(&tree, 0)?;
    if !parent.metadata_at(name, false)?.same_identity(&metadata) {
        return Err(RunError::UnsafePath);
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
        .collect::<provenant_supervisor::fs::Result<Vec<_>>>()?;
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
        let run = store.run_dir(input.run_id, false)?;
        let publication =
            run.try_lock_existing(OsStr::new("publication.lock"), LockMode::Shared)?;
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
        drop(publication);
        let writer = directory.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
        drop(writer);
        fs::write(attempt.join("reservation.json"), "{}")?;
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
        assert!(
            attempt
                .join("provider-state/candidate-0/auth.json")
                .exists()
        );
        fs::remove_file(attempt.join("reservation.json"))?;
        let report = sweep(&store, &context, 1, DAY_MICROS, false, &call)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!attempt.join("provider-state").exists());
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
        assert_eq!(
            sweep(&store, &context, 1, DAY_MICROS, false, &call)?
                .kept
                .len(),
            1
        );
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
