//! Read-only classification of physical run-store contents.
use crate::{Result, RunStore};
use provenant_os::{
    disk::{DiskEntry, DiskReport},
    fs::Directory,
};
use provenant_records::{AttemptFile, AttemptFileClass};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::Path,
};

impl RunStore {
    /// Add run-store usage without repairing records, settling attempts or
    /// creating missing directories. Nested child stores are counted once.
    pub fn disk_usage(&self, report: &mut DiskReport) -> Result<()> {
        let terminal = {
            let mut query = self.db.prepare("SELECT run,id,terminal FROM attempts")?;
            let rows = query.query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    row.get::<_, bool>(2)?,
                ))
            })?;
            rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()?
        };
        scan_store(
            &self.root,
            self.context.state_root.as_path(),
            &terminal,
            report,
        );
        Ok(())
    }

    /// Classify files even when a run-store database is absent or unavailable.
    /// An unindexed temporary directory has unknown terminal/leak status.
    pub fn disk_usage_unindexed(
        state: &provenant_core::StateRoot,
        report: &mut DiskReport,
    ) -> Result<()> {
        let root = match Directory::open_anchor(state.as_path()) {
            Ok(root) => root,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        scan_store(&root, state.as_path(), &BTreeMap::new(), report);
        Ok(())
    }
}

fn scan_store(
    root: &Directory,
    path: &Path,
    terminal: &BTreeMap<(String, String), bool>,
    report: &mut DiskReport,
) {
    let mut other = DiskEntry {
        class: "other_state".into(),
        path: path.into(),
        ..Default::default()
    };
    let mut observed_other = false;
    for name in entries(root, path, report) {
        let child = path.join(&name);
        if name == "runs" {
            if let Some(runs) = open(root, &name, path, report) {
                scan_runs(&runs, &child, terminal, report);
            }
        } else if name == "children" {
            if let Some(children) = open(root, &name, path, report) {
                for child_name in entries(&children, &child, report) {
                    report.measure_at(
                        "child_store",
                        &children,
                        &child_name,
                        &child.join(&child_name),
                    );
                }
            }
        } else if name
            .to_str()
            .is_some_and(|name| name.starts_with("run.sqlite3"))
        {
            report.measure_into_at(&mut other, root, &name, &child);
            observed_other = true;
        }
    }
    if observed_other {
        report.add_rollup(other);
    }
}

fn scan_runs(
    runs: &Directory,
    path: &Path,
    terminal: &BTreeMap<(String, String), bool>,
    report: &mut DiskReport,
) {
    let mut provider_state = DiskEntry {
        class: "provider_state".into(),
        path: path.into(),
        ..Default::default()
    };
    for run_name in entries(runs, path, report) {
        let run_path = path.join(&run_name);
        let Some(run) = open(runs, &run_name, path, report) else {
            continue;
        };
        let mut other = DiskEntry {
            class: "other_state".into(),
            path: run_path.clone(),
            ..Default::default()
        };
        let mut retained = DiskEntry {
            class: "attempt_retained".into(),
            path: run_path.clone(),
            ..Default::default()
        };
        let (mut observed_other, mut observed_retained) = (false, false);
        for name in entries(&run, &run_path, report) {
            let child = run_path.join(&name);
            if name != "attempts" {
                report.measure_into_at(&mut other, &run, &name, &child);
                observed_other = true;
                continue;
            }
            let Some(attempts) = open(&run, &name, &run_path, report) else {
                continue;
            };
            for attempt_name in entries(&attempts, &child, report) {
                let attempt_path = child.join(&attempt_name);
                let Some(attempt) = open(&attempts, &attempt_name, &child, report) else {
                    continue;
                };
                let is_terminal = run_name
                    .to_str()
                    .zip(attempt_name.to_str())
                    .and_then(|(run, attempt)| terminal.get(&(run.into(), attempt.into())))
                    .copied();
                for part in entries(&attempt, &attempt_path, report) {
                    let class = match part
                        .to_str()
                        .and_then(AttemptFile::from_name)
                        .map(AttemptFile::class)
                    {
                        Some(AttemptFileClass::Temporary) => "attempt_tmp",
                        Some(AttemptFileClass::ProviderState) => "provider_state",
                        Some(AttemptFileClass::Retained) => "attempt_retained",
                        _ => "other_state",
                    };
                    let part_path = attempt_path.join(&part);
                    match class {
                        "provider_state" => {
                            report.measure_into_at(
                                &mut provider_state,
                                &attempt,
                                &part,
                                &part_path,
                            );
                            provider_state.count = Some(provider_state.count.unwrap_or(0) + 1);
                        }
                        "attempt_tmp" => {
                            if let Some(index) =
                                report.measure_at(class, &attempt, &part, &part_path)
                            {
                                report.entries[index].leak = is_terminal;
                            }
                        }
                        "attempt_retained" => {
                            report.measure_into_at(&mut retained, &attempt, &part, &part_path);
                            observed_retained = true;
                        }
                        _ => {
                            report.measure_into_at(&mut other, &attempt, &part, &part_path);
                            observed_other = true;
                        }
                    }
                }
            }
        }
        if observed_other {
            report.add_rollup(other);
        }
        if observed_retained {
            report.add_rollup(retained);
        }
    }
    if provider_state.count.is_some() {
        report.add_rollup(provider_state);
    }
}

fn entries(directory: &Directory, path: &Path, report: &mut DiskReport) -> Vec<OsString> {
    let entries = match directory.entries() {
        Ok(entries) => entries,
        Err(error) => {
            report.error(path, error.to_string());
            return vec![];
        }
    };
    let mut names = vec![];
    for entry in entries {
        match entry {
            Ok(name) => names.push(name),
            Err(error) => report.error(path, error.to_string()),
        }
    }
    names.sort();
    names
}

fn open(
    parent: &Directory,
    name: &OsStr,
    path: &Path,
    report: &mut DiskReport,
) -> Option<Directory> {
    match parent.open_verified_directory(name) {
        Ok(directory) => Some(directory),
        Err(error) if error.is_not_found() => None,
        Err(error) => {
            report.error(path.join(name), error.to_string());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AttemptState, Outcome,
        tests::{call, clock, context, input},
    };
    use provenant_core::{CancelToken, WallTime};
    use std::fs;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn disk_usage_classifies_run_contents_and_flags_terminal_temporary_leaks() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let writer = RunStore::open(&context, &call)?;
        let input = input();
        writer.create(&input, &call)?;
        let state = context.state_root.as_path();
        let attempt = state.join(format!(
            "runs/{}/attempts/{}",
            input.run_id, input.attempts[0].attempt_id
        ));
        for part in ["tmp", "scratch", "provider-state", "quarantine"] {
            fs::create_dir(attempt.join(part))?;
            fs::write(attempt.join(part).join("keep"), part)?;
        }
        for name in ["stdout.log", "stderr.log"] {
            fs::write(attempt.join(name), "capture")?;
        }
        let child = state
            .join("children")
            .join(input.attempts[0].attempt_id.to_string());
        fs::create_dir_all(child.join("runs/nested/attempts/child/tmp"))?;
        fs::write(child.join("run.sqlite3"), "opaque child store")?;
        let before = snapshot(state)?;
        let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("missing store")?;
        let mut report = DiskReport::new(state.to_owned(), 0);
        reader.disk_usage(&mut report)?;
        drop(reader);
        assert_unchanged_except_sqlite_companions(&before, &snapshot(state)?);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let provider = report
            .entries
            .iter()
            .find(|entry| entry.class == "provider_state")
            .ok_or("provider-state class missing")?;
        assert!(provider.allocated_bytes > 0);
        assert_eq!(provider.count, Some(1));
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.class == "attempt_tmp"
                    && entry.path == attempt.join("tmp")
                    && entry.leak == Some(false))
        );
        assert_eq!(
            report
                .entries
                .iter()
                .filter(|entry| entry.class == "attempt_retained")
                .count(),
            1
        );
        assert_eq!(
            report
                .entries
                .iter()
                .filter(|entry| entry.class == "child_store")
                .count(),
            1
        );
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.class == "child_store" && entry.path == child)
        );
        writer.withdraw(
            input.attempts[0].attempt_id,
            AttemptState::Queued,
            Outcome::Cancelled,
            "fixture",
            WallTime::from_micros(150)?,
            &call,
        )?;
        fs::create_dir(attempt.join("tmp"))?;
        fs::write(attempt.join("tmp/keep"), "leak")?;
        let before = snapshot(state)?;
        let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("missing store")?;
        let mut report = DiskReport::new(state.to_owned(), 0);
        reader.disk_usage(&mut report)?;
        drop(reader);
        assert_unchanged_except_sqlite_companions(&before, &snapshot(state)?);
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.class == "attempt_tmp"
                    && entry.path == attempt.join("tmp")
                    && entry.leak == Some(true))
        );
        assert_eq!(fs::read_to_string(attempt.join("tmp/keep"))?, "leak");
        Ok(())
    }

    #[test]
    fn disk_reader_reads_live_wal_without_durable_writes() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let writer = RunStore::open(&context, &call)?;
        writer.create(&input(), &call)?;
        let before = snapshot(context.state_root.as_path())?;
        let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("missing store")?;
        let mut report = DiskReport::new(context.state_root.as_path().into(), 0);
        reader.disk_usage(&mut report)?;
        drop(reader);
        assert_unchanged_except_sqlite_companions(
            &before,
            &snapshot(context.state_root.as_path())?,
        );
        Ok(())
    }

    #[test]
    fn disk_reader_never_creates_missing_state_or_database() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let state = context.state_root.as_path();
        assert!(RunStore::open_disk_read_only(&context, &call)?.is_none());
        assert!(!state.exists());
        fs::create_dir(state)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
        assert!(RunStore::open_disk_read_only(&context, &call)?.is_none());
        assert_eq!(fs::read_dir(state)?.count(), 0);
        let writer = RunStore::open(&context, &call)?;
        drop(writer);
        let before = snapshot(state)?;
        let reader = RunStore::open_disk_read_only(&context, &call)?.ok_or("store")?;
        let mut report = DiskReport::new(state.into(), 0);
        reader.disk_usage(&mut report)?;
        drop(reader);
        assert_unchanged_except_sqlite_companions(&before, &snapshot(state)?);
        Ok(())
    }

    #[test]
    fn disk_reader_refuses_rollback_journal_without_modification() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        drop(store);
        let journal = context.state_root.as_path().join("run.sqlite3-journal");
        fs::write(&journal, b"uncheckpointed fixture")?;
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o600))?;
        let before = snapshot(context.state_root.as_path())?;
        assert!(RunStore::open_disk_read_only(&context, &call).is_err());
        assert!(
            snapshot(context.state_root.as_path())? == before,
            "disk reader modified state"
        );
        Ok(())
    }

    #[test]
    fn disk_usage_without_database_keeps_temporary_leak_status_unknown() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let state = context.state_root.as_path();
        let temporary = state.join("runs/run/attempts/attempt/tmp");
        fs::create_dir_all(&temporary)?;
        fs::write(temporary.join("keep"), "unindexed")?;
        let target = temporary.join("candidate-0/target");
        fs::create_dir_all(&target)?;
        fs::write(target.join("payload"), vec![7; 16384])?;
        fs::create_dir(state.join("lane-caches"))?;
        fs::write(state.join("lane-caches/keep"), "outside run classification")?;
        let before = snapshot(state)?;
        let mut report = DiskReport::new(state.to_owned(), 0);
        RunStore::disk_usage_unindexed(&context.state_root, &mut report)?;
        assert_eq!(snapshot(state)?, before);
        assert_eq!(report.entries.len(), 1);
        assert!(
            report
                .entries
                .iter()
                .any(|entry| entry.class == "attempt_tmp"
                    && entry.path == temporary
                    && entry.allocated_bytes >= 16384
                    && entry.leak.is_none())
        );
        assert!(!state.join("run.sqlite3").exists());
        Ok(())
    }

    #[test]
    fn disk_usage_rolls_many_attempts_up_per_run() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let state = context.state_root.as_path();
        for index in 0..100 {
            let attempt = state.join(format!("runs/run/attempts/{index}"));
            fs::create_dir_all(attempt.join("provider-state"))?;
            for part in [
                "result.md",
                "process-exit.json",
                "prompt.md",
                "stdout.log",
                "stderr.log",
            ] {
                fs::write(attempt.join(part), vec![1; 4096])?;
            }
            fs::write(attempt.join("provider-state/keep"), vec![2; 4096])?;
        }
        let before = snapshot(state)?;
        let mut report = DiskReport::new(state.into(), 0);
        RunStore::disk_usage_unindexed(&context.state_root, &mut report)?;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.entries.len(), 3);
        let (provider, runs): (Vec<_>, Vec<_>) = report
            .entries
            .iter()
            .partition(|entry| entry.class == "provider_state");
        assert_eq!(provider.len(), 1);
        assert_eq!(provider[0].count, Some(100));
        assert!(
            runs.iter()
                .all(|entry| entry.path == state.join("runs/run"))
        );
        assert!(
            report
                .entries
                .iter()
                .map(|entry| entry.allocated_bytes)
                .sum::<u64>()
                >= 600 * 4096
        );
        assert_eq!(snapshot(state)?, before);
        Ok(())
    }

    fn assert_unchanged_except_sqlite_companions(
        before: &BTreeMap<std::path::PathBuf, Vec<u8>>,
        after: &BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        let durable = |entries: &BTreeMap<std::path::PathBuf, Vec<u8>>| {
            entries
                .iter()
                .filter(|(path, _)| {
                    let companion = matches!(
                        path.file_name().and_then(OsStr::to_str),
                        Some("run.sqlite3-shm" | "run.sqlite3-wal")
                    );
                    !(companion
                        && path
                            .parent()
                            .is_some_and(|parent| before.contains_key(&parent.join("run.sqlite3"))))
                })
                .map(|(path, bytes)| (path.clone(), bytes.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            durable(before),
            durable(after),
            "disk reader modified durable state"
        );
    }

    #[cfg(unix)]
    #[test]
    fn disk_usage_refuses_symlinked_run_directories() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let state = context.state_root.as_path();
        fs::create_dir(state.join("runs"))?;
        let outside = temp.path().join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("keep"), "outside")?;
        std::os::unix::fs::symlink(&outside, state.join("runs/linked"))?;
        let mut report = DiskReport::new(state.to_owned(), 0);
        store.disk_usage(&mut report)?;
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.path == state.join("runs/linked"))
        );
        assert_eq!(fs::read_to_string(outside.join("keep"))?, "outside");
        Ok(())
    }

    fn snapshot(path: &Path) -> std::io::Result<BTreeMap<std::path::PathBuf, Vec<u8>>> {
        let mut result = BTreeMap::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                result.insert(path.clone(), vec![]);
                result.extend(snapshot(&path)?);
            } else {
                result.insert(path.clone(), fs::read(&path)?);
            }
        }
        Ok(result)
    }
}
