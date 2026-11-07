//! Cutover checks shared by host admission and staged upgrades.
use std::{io, path::Path};

/// Read-only drain check, shared by staged preflight and the first new launch.
/// New interface runs persist their resolved template library in the execution.
/// A legacy lane cannot become current merely by upgrading its executable.
pub fn pending_findings(
    state: &provenant_core::StateRoot,
    legacy_only: bool,
) -> Result<Vec<String>, String> {
    let mut findings = Vec::new();
    let mut current = std::collections::BTreeSet::new();
    let projects = state.as_path().join("fabric/graphs");
    let entries = match std::fs::read_dir(&projects) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", projects.display())),
    };
    if let Some(entries) = entries {
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let Some(project) = entry
                .file_name()
                .to_str()
                .and_then(|s| provenant_core::ProjectId::parse(s).ok())
            else {
                continue;
            };
            let ids = super::FileJournal::graph_ids(state, project).map_err(|e| e.to_string())?;
            for id in ids {
                if legacy_only
                    && super::FileJournal::execution(state, project, &id)
                        .ok()
                        .flatten()
                        .is_some_and(|execution| {
                            execution.document.extra.contains_key("template_library")
                        })
                {
                    current.insert((project.to_string(), id));
                    continue;
                }
                match super::FileJournal::pending_execution(state, project, &id) {
                    Ok(None) => continue,
                    Ok(Some(_)) => {}
                    Err(error) if error.is_unreadable() => {
                        if super::FileJournal::unreadable_settled(state, project, &id)
                            .map_err(|e| e.to_string())?
                        {
                            continue;
                        }
                    }
                    Err(error) => return Err(error.to_string()),
                }
                findings.push(format!(
                    "Pending graph {project}/{id}: finish or cancel it with the previous release before upgrading"
                ));
            }
        }
    }
    let mut stores = vec![state.as_path().to_path_buf()];
    while let Some(store) = stores.pop() {
        let children = store.join("children");
        match std::fs::read_dir(&children) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                        stores.push(entry.path());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", children.display())),
        }
        let database = store.join("run.sqlite3");
        if !database
            .try_exists()
            .map_err(|e| format!("{}: {e}", database.display()))?
        {
            continue;
        }
        let db = read_connection(&database).map_err(|e| format!("{}: {e}", database.display()))?;
        let mut query = db
            .prepare(
                "SELECT project, run, id, record FROM attempts WHERE terminal=0 ORDER BY project, run, id",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (project, run, attempt, record) = row.map_err(|e| e.to_string())?;
            if !supervisor_live(&store, &run, &attempt) {
                continue;
            }
            if legacy_only && store == state.as_path() {
                let record: serde_json::Value =
                    serde_json::from_slice(&record).map_err(|e| e.to_string())?;
                let graph = record
                    .pointer("/context/graph_id")
                    .and_then(serde_json::Value::as_str);
                let remote_template = record.pointer("/context/graph_template_launch")
                    == Some(&serde_json::Value::Bool(true))
                    && graph.is_some_and(|id| !id.is_empty())
                    && record
                        .pointer("/context/node_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| !id.is_empty());
                if remote_template
                    || graph.is_some_and(|id| current.contains(&(project.clone(), id.to_owned())))
                {
                    continue;
                }
            }
            let location = store.strip_prefix(state.as_path()).unwrap_or(Path::new(""));
            let location = if location.as_os_str().is_empty() {
                "state root".to_owned()
            } else {
                location.display().to_string()
            };
            findings.push(format!("Live lane {project}/{run}/{attempt} ({location})"));
        }
    }
    findings.sort();
    Ok(findings)
}

// The guardian holds this existing lock for the lifetime of supervision.
// Missing attempt directories or locks have no live guardian; other errors
// remain findings rather than being mistaken for absence.
fn supervisor_live(store: &Path, run: &str, attempt: &str) -> bool {
    use provenant_os::fs::{Directory, FsError, LockMode};
    use std::ffi::OsStr;
    if !super::valid_identifier(run)
        || !super::valid_identifier(attempt)
        || [run, attempt].iter().any(|id| matches!(*id, "." | ".."))
    {
        return true;
    }
    let path = store.join("runs").join(run).join("attempts").join(attempt);
    let directory = match Directory::open_anchor(&path) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return false,
        Err(_) => return true,
    };
    match directory.try_lock_existing(OsStr::new("custody.lock"), LockMode::Exclusive) {
        Ok(_) => false,
        Err(FsError::LockTimeout { .. }) => true,
        Err(error) if error.is_not_found() => false,
        Err(_) => true,
    }
}

/// Read a run database without overlooking committed WAL data.
pub fn read_connection(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let observed = (|| {
        let db = provenant_sqlite::open_read_only_connection(path)?;
        {
            let mut statement = db.prepare("SELECT 1 FROM attempts LIMIT 1")?;
            let mut rows = statement.query([])?;
            rows.next()?;
        }
        Ok(db)
    })();
    match observed {
        Ok(db) => return Ok(db),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::CannotOpen
                || matches!(
                    error.extended_code,
                    rusqlite::ffi::SQLITE_IOERR_LOCK
                        | rusqlite::ffi::SQLITE_IOERR_SHMOPEN
                        | rusqlite::ffi::SQLITE_IOERR_SHMMAP
                ) => {}
        Err(error) => return Err(error),
    }
    // A missing WAL index needs an exclusive reader with a lock-capable
    // descriptor. The VFS still refuses all data writes and sidecar creation.
    let db = provenant_sqlite::open_locking_read_only_connection(path)?;
    {
        let mut statement = db.prepare("SELECT 1 FROM attempts LIMIT 1")?;
        let mut rows = statement.query([])?;
        rows.next()?;
    }
    Ok(db)
}

/// Refuse new admission while legacy graphs or lanes still require draining.
pub fn pending_guard(state: &provenant_core::StateRoot) -> Result<(), String> {
    use provenant_os::fs::{Directory, FsError, LockMode, PublishMode};
    use std::{ffi::OsStr, os::unix::fs::DirBuilderExt};

    const MARKER: &str = "graph-template-cutover-complete";
    let path = state.as_path();
    if path.join(MARKER).is_file() {
        return Ok(());
    }
    // Hold the lock across the scan and publication: another launch must not
    // start an attempt while a first pass is still examining the old state.
    let directory = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| e.to_string())
        .and_then(|()| Directory::open_anchor(path).map_err(|e| e.to_string()));
    let directory = match directory {
        Ok(directory) => Some(directory),
        Err(error) => {
            eprintln!("Warning: cannot write cutover marker: {error}");
            None
        }
    };
    let _lock = if let Some(directory) = &directory {
        loop {
            match directory.try_lock(
                OsStr::new("graph-template-cutover.lock"),
                LockMode::Exclusive,
            ) {
                Ok(lock) => break Some(lock),
                Err(FsError::LockTimeout { .. }) => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(error) => {
                    eprintln!("Warning: cannot lock cutover marker: {error}");
                    break None;
                }
            }
        }
    } else {
        None
    };
    if path.join(MARKER).is_file() {
        return Ok(());
    }
    let findings = pending_findings(state, true).map_err(|_| {
        "Unable to inspect pending graphs and lanes; restore access to the state directory and retry.".to_owned()
    })?;
    if findings.is_empty() {
        if let Some(directory) = &directory
            && let Err(error) = directory.atomic_write(
                OsStr::new(MARKER),
                ".cutover-",
                b"complete\n",
                PublishMode::Replace,
            )
        {
            eprintln!("Warning: cannot write cutover marker: {error}");
        }
        return Ok(());
    }
    let rest = findings.len().saturating_sub(5);
    let mut summary = findings.into_iter().take(5).collect::<Vec<_>>().join("\n");
    if rest > 0 {
        summary.push_str(&format!("\n... and {rest} more finding(s)"));
    }
    Err(format!(
        "Graph-template cutover refused: old pending state remains. Run the previous release against this state directory, finish or cancel its pending graphs and drain its live lanes, then launch this release again. --yes cannot bypass this check. No child stores have been deleted.\n{}",
        summary
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{CancelToken, Clock, ProjectId, RootInputs, StateRoot, WaitBudget};
    use provenant_os::{
        clock::SystemClock,
        fs::{Directory, LockMode, SystemPathResolver},
    };
    use std::{ffi::OsStr, os::unix::fs::PermissionsExt};

    fn fresh_guard(state: &StateRoot) -> Result<(), String> {
        let _ = std::fs::remove_file(state.as_path().join("graph-template-cutover-complete"));
        pending_guard(state)
    }

    #[test]
    fn marker_skips_all_scans_but_preflight_still_reads_state() {
        let temp = crate::test_support::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let state = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        pending_guard(&state).unwrap();
        std::fs::create_dir_all(base.join("fabric")).unwrap();
        std::fs::write(base.join("fabric/graphs"), b"not a directory").unwrap();
        std::fs::write(base.join("children"), b"not a directory").unwrap();
        assert!(pending_guard(&state).is_ok());
        assert!(pending_findings(&state, false).is_err());
    }

    #[test]
    fn marker_write_failure_does_not_refuse_clean_state() {
        let temp = crate::test_support::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let state = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        std::fs::create_dir(base.join("graph-template-cutover-complete")).unwrap();
        assert!(pending_guard(&state).is_ok());
    }

    #[test]
    fn refusal_limits_findings_and_marker_is_not_written() {
        let temp = crate::test_support::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let state = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let db = rusqlite::Connection::open(base.join("run.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE attempts (project TEXT, run TEXT, id TEXT, record BLOB, terminal INTEGER)").unwrap();
        let mut locks = Vec::new();
        for id in 0..7 {
            let attempt = format!("attempt{id}");
            db.execute(
                "INSERT INTO attempts VALUES ('project','run',?1,X'7b7d',0)",
                [&attempt],
            )
            .unwrap();
            let path = base.join("runs/run/attempts").join(&attempt);
            std::fs::create_dir_all(&path).unwrap();
            let dir = Directory::open_anchor(&path).unwrap();
            locks.push(
                dir.try_lock(OsStr::new("custody.lock"), LockMode::Exclusive)
                    .unwrap(),
            );
        }
        let refusal = pending_guard(&state).unwrap_err();
        assert!(refusal.contains("Live lane project/run/attempt0"));
        assert!(refusal.contains("Live lane project/run/attempt4"));
        assert!(!refusal.contains("attempt5"));
        assert!(refusal.contains("2 more"));
        assert!(!base.join("graph-template-cutover-complete").exists());
        assert_eq!(pending_findings(&state, false).unwrap().len(), 7);
    }

    #[test]
    fn legacy_checkpoint_lifecycle_controls_both_cutover_checks() {
        let temp = crate::test_support::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let state = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let project = ProjectId::from_bytes([1; 16]);
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
        drop(
            super::super::FileJournal::open(&state, project, "old", budget, &cancel, &clock)
                .unwrap(),
        );
        let path = base.join(format!("fabric/graphs/{project}/graph-old.json"));
        let write = |checkpoint: serde_json::Value| {
            std::fs::write(
                &path,
                serde_json::to_vec(
                    &serde_json::json!({"checkpoint": checkpoint, "incompatible": true}),
                )
                .unwrap(),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        let write_delivery = |checkpoint: serde_json::Value, delivery: Option<&str>| {
            let mut envelope = serde_json::json!({"checkpoint": checkpoint, "incompatible": true});
            if let Some(delivery) = delivery {
                envelope["execution"]["document"]["delivery"] = delivery.into();
            }
            std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        for checkpoint in [
            serde_json::json!({"cancelled":true,"nodes":[{"state":"running"}],"asks":[{"closed":false}]}),
            serde_json::json!({"nodes":[{"state":"succeeded"},{"state":"failed"},{"state":"blocked"},{"state":"skipped"}],"asks":[{"closed":true}]}),
            serde_json::json!({"cancelled":false,"nodes":[{"state":"succeeded"}]}),
        ] {
            write(checkpoint);
            assert!(pending_findings(&state, false).unwrap().is_empty());
            assert!(fresh_guard(&state).is_ok());
            assert!(
                super::super::FileJournal::read_checkpoint_for_tick(&state, project, "old")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                super::super::FileJournal::collect_terminal(
                    &state,
                    project,
                    i128::MAX,
                    &cancel,
                    &clock
                )
                .unwrap(),
                0
            );
        }
        for checkpoint in [
            serde_json::json!({"cancelled":false,"nodes":[{"state":"pending"}]}),
            serde_json::json!({"cancelled":false,"nodes":[{"state":"running"}]}),
            serde_json::json!({"nodes":[{"state":"succeeded"}],"asks":[{"closed":false}]}),
            serde_json::json!({"nodes":[{"state":"unknown"}]}),
            serde_json::json!({}),
        ] {
            write(checkpoint);
            assert_eq!(pending_findings(&state, false).unwrap().len(), 1);
            assert!(fresh_guard(&state).is_err());
        }
        write_delivery(
            serde_json::json!({"cancelled":false,"delivered":false,"nodes":[{"state":"succeeded"}]}),
            Some("pr"),
        );
        assert_eq!(pending_findings(&state, false).unwrap().len(), 1);
        assert!(fresh_guard(&state).is_err());

        write_delivery(
            serde_json::json!({"cancelled":false,"delivered":true,"nodes":[{"state":"succeeded"}]}),
            Some("pr"),
        );
        assert!(pending_findings(&state, false).unwrap().is_empty());
        assert!(fresh_guard(&state).is_ok());

        write_delivery(
            serde_json::json!({"cancelled":true,"delivered":false,"nodes":[{"state":"running"}]}),
            Some("pr"),
        );
        assert!(pending_findings(&state, false).unwrap().is_empty());
        assert!(fresh_guard(&state).is_ok());

        write_delivery(
            serde_json::json!({"cancelled":false,"nodes":[{"state":"succeeded"}],"asks":[{"resolved":true,"closed":false}]}),
            Some("pr"),
        );
        assert_eq!(pending_findings(&state, false).unwrap().len(), 1);
        assert!(fresh_guard(&state).is_err());

        write_delivery(
            serde_json::json!({"cancelled":false,"nodes":[{"state":"succeeded"}]}),
            None,
        );
        assert!(pending_findings(&state, false).unwrap().is_empty());
        assert!(fresh_guard(&state).is_ok());

        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(pending_findings(&state, false).unwrap().len(), 1);
        assert!(fresh_guard(&state).is_err());
    }

    #[test]
    fn root_and_child_stores_need_live_custody() {
        let temp = crate::test_support::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.clone());
        let state = StateRoot::resolve(&inputs, &SystemPathResolver).unwrap();
        let child = base.join("children/child");
        let attempt = child.join("runs/run/attempts/attempt");
        std::fs::create_dir_all(&attempt).unwrap();
        let db = rusqlite::Connection::open(child.join("run.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE attempts (project TEXT, run TEXT, id TEXT, record BLOB, terminal INTEGER); INSERT INTO attempts VALUES ('project','run','attempt',X'7b7d',0);").unwrap();
        drop(db);
        assert!(pending_findings(&state, false).unwrap().is_empty());
        let directory = Directory::open_anchor(&attempt).unwrap();
        let lock = directory
            .try_lock(OsStr::new("custody.lock"), LockMode::Exclusive)
            .unwrap();
        assert_eq!(pending_findings(&state, false).unwrap().len(), 1);
        assert!(fresh_guard(&state).is_err());
        drop(lock);
        assert!(pending_findings(&state, false).unwrap().is_empty());
        let db = rusqlite::Connection::open(base.join("run.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE attempts (project TEXT, run TEXT, id TEXT, record BLOB, terminal INTEGER); INSERT INTO attempts VALUES ('project','run','root',X'7b7d',0);").unwrap();
        drop(db);
        for legacy_only in [false, true] {
            assert!(pending_findings(&state, legacy_only).unwrap().is_empty());
        }
        assert!(fresh_guard(&state).is_ok());
        let root_attempt = base.join("runs/run/attempts/root");
        std::fs::create_dir_all(&root_attempt).unwrap();
        let directory = Directory::open_anchor(&root_attempt).unwrap();
        let lock = directory
            .try_lock(OsStr::new("custody.lock"), LockMode::Exclusive)
            .unwrap();
        for legacy_only in [false, true] {
            let findings = pending_findings(&state, legacy_only).unwrap();
            assert_eq!(findings.len(), 1);
            assert!(findings[0].contains("/root "));
        }
        assert!(fresh_guard(&state).is_err());
        drop(lock);
        assert!(fresh_guard(&state).is_ok());
    }
}
