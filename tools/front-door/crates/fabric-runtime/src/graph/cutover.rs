//! Read-only cutover checks shared by host admission and staged upgrades.
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
            let ids = super::FileJournal::graph_ids(state, project)
                .map_err(|e| e.to_string())?;
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
                match super::FileJournal::pending_execution(
                    state, project, &id,
                ) {
                    Ok(None) => continue,
                    Ok(Some(_)) => {}
                    Err(error) if error.is_unreadable() => {}
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
        let db = read_connection(&database)
            .map_err(|e| format!("{}: {e}", database.display()))?;
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
            if legacy_only && store == state.as_path() {
                let record: serde_json::Value =
                    serde_json::from_slice(&record).map_err(|e| e.to_string())?;
                let graph = record.pointer("/context/graph_id").and_then(serde_json::Value::as_str);
                let remote_template = record.pointer("/context/graph_template_launch") == Some(&serde_json::Value::Bool(true))
                    && graph.is_some_and(|id| !id.is_empty())
                    && record.pointer("/context/node_id").and_then(serde_json::Value::as_str).is_some_and(|id| !id.is_empty());
                if remote_template || graph.is_some_and(|id| current.contains(&(project.clone(), id.to_owned())))
                {
                    continue;
                }
            }
            findings.push(format!(
                "Live lane {project}/{run}/{attempt} ({})",
                store.display()
            ));
        }
    }
    findings.sort();
    Ok(findings)
}

/// Read a run database without overlooking committed WAL data.
pub fn read_connection(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let observed = (|| {
        let db = provenant_sqlite::open_read_only_connection(path)?;
        db.prepare("SELECT project, run, id, record FROM attempts WHERE terminal=0")?;
        Ok(db)
    })();
    match observed {
        Ok(db) => return Ok(db),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::CannotOpen => {}
        Err(error) => return Err(error),
    }
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    // A WAL-mode database can have no companions after its last writer closes.
    // An exclusive reader then uses a private WAL index. Unlike immutable=1,
    // this still reads committed WAL frames and refuses concurrent writers.
    db.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    Ok(db)
}

/// Refuse new admission while legacy graphs or lanes still require draining.
pub fn pending_guard(state: &provenant_core::StateRoot) -> Result<(), String> {
    let findings = pending_findings(state, true)?;
    if findings.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Graph-template cutover refused: old pending state remains. Run the previous release against this state directory, finish or cancel its pending graphs and drain its live lanes, then launch this release again. --yes cannot bypass this check. No child stores have been deleted.\n{}",
        findings.join("\n")
    ))
}

