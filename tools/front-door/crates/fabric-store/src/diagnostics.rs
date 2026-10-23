//! Diagnostic policy over a private, consistent snapshot supplied by the supervisor.
use crate::*;
use std::collections::BTreeSet;

const REQUIRED_TABLES: &[&str] = &[
    "agents",
    "messages",
    "deliveries",
    "delivery_claims",
    "teams",
    "team_members",
    "tasks",
    "task_dependencies",
    "activity",
    "work_claims",
    "landing_leases",
    "run_observations",
];
const REQUIRED_CLAIM_COLUMNS: &[&str] = &[
    "message_id",
    "project",
    "recipient_id",
    "claim_id",
    "claimed_at",
    "expires_at",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticStatus {
    Absent,
    Ok,
    Error,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseDiagnostic {
    pub status: DiagnosticStatus,
    pub exists: bool,
    pub read_only: bool,
    pub database: String,
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<DiagnosticCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<DiagnosticCheck>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
/// Field order is observable in the CLI's JSON.stringify output.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCounts {
    pub agents: i64,
    pub messages: i64,
    pub pending_deliveries: i64,
    pub tasks: i64,
    pub activity: i64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
#[derive(Clone, Copy)]
pub enum DiagnosticMode {
    Status,
    Doctor,
}

/// Inspect only an isolated snapshot, never a live Fabric database.
/// Snapshot creation/version checks/cleanup await the safe filesystem owner.
pub fn inspect_snapshot(
    db: &Connection,
    database: &str,
    project: &str,
    mode: DiagnosticMode,
) -> DatabaseDiagnostic {
    let mut report = DatabaseDiagnostic {
        status: DiagnosticStatus::Ok,
        exists: true,
        read_only: true,
        database: database.into(),
        project: project.into(),
        counts: None,
        checks: None,
        error: None,
    };
    match inspect_policy(db, project, mode) {
        Ok((counts, checks)) => {
            if checks.as_ref().is_some_and(|c| c.iter().any(|c| !c.ok)) {
                report.status = DiagnosticStatus::Error;
            }
            report.counts = Some(counts);
            report.checks = checks;
        }
        Err(error) => {
            report.status = DiagnosticStatus::Error;
            report.error = Some(error.to_string());
        }
    }
    report
}
type DiagnosticPolicy = (DiagnosticCounts, Option<Vec<DiagnosticCheck>>);
fn inspect_policy(
    db: &Connection,
    project: &str,
    mode: DiagnosticMode,
) -> Result<DiagnosticPolicy> {
    db.pragma_update(None, "query_only", "ON")?;
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    let mut counts = DiagnosticCounts::default();
    for (slot, table, extra) in [
        (&mut counts.agents, "agents", ""),
        (&mut counts.messages, "messages", ""),
        (
            &mut counts.pending_deliveries,
            "deliveries",
            "AND read_at IS NULL",
        ),
        (&mut counts.tasks, "tasks", ""),
        (&mut counts.activity, "activity", ""),
    ] {
        let n = if tables.contains(table) {
            db.query_row(
                &format!("SELECT count(*) FROM {table} WHERE project = ? {extra}"),
                [project],
                |r| r.get(0),
            )?
        } else {
            0
        };
        *slot = n;
    }
    if matches!(mode, DiagnosticMode::Status) {
        return Ok((counts, None));
    }
    let missing: Vec<_> = REQUIRED_TABLES
        .iter()
        .copied()
        .filter(|t| !tables.contains(*t))
        .collect();
    let claim_columns = db
        .prepare("PRAGMA table_info(delivery_claims)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    let missing_columns: Vec<_> = REQUIRED_CLAIM_COLUMNS
        .iter()
        .copied()
        .filter(|c| !claim_columns.contains(*c))
        .collect();
    let integrity = db
        .prepare("PRAGMA quick_check")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let foreign_keys = db
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let collisions = if tables.contains("agents") && tables.contains("teams") {
        db.prepare("SELECT a.agent_id FROM agents a JOIN teams t ON t.project = a.project AND t.team_id = a.agent_id WHERE a.project = ? ORDER BY a.agent_id")?.query_map([project], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let checks = vec![
        DiagnosticCheck {
            name: "schema".into(),
            ok: missing.is_empty(),
            detail: if missing.is_empty() {
                format!("{} required tables present", REQUIRED_TABLES.len())
            } else {
                format!("missing tables: {}", missing.join(", "))
            },
        },
        DiagnosticCheck {
            name: "integrity".into(),
            ok: integrity.iter().all(|s| s == "ok"),
            detail: integrity.join("; "),
        },
        DiagnosticCheck {
            name: "delivery-claim-schema".into(),
            ok: missing_columns.is_empty(),
            detail: if missing_columns.is_empty() {
                "delivery claim columns present".into()
            } else {
                format!("missing columns: {}", missing_columns.join(", "))
            },
        },
        DiagnosticCheck {
            name: "foreign-keys".into(),
            ok: foreign_keys.is_empty(),
            detail: if foreign_keys.is_empty() {
                "no foreign-key violations".into()
            } else {
                foreign_keys
                    .iter()
                    .map(|(table, row, parent)| {
                        format!(
                            "{table} row {} -> {parent}",
                            row.map(|r| r.to_string()).unwrap_or_else(|| "null".into())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            },
        },
        DiagnosticCheck {
            name: "recipient-namespace".into(),
            ok: collisions.is_empty(),
            detail: if collisions.is_empty() {
                "agent labels and team ids are distinct".into()
            } else {
                format!(
                    "agent/team ids overlap (team-first routing): {}",
                    collisions.join(", ")
                )
            },
        },
    ];
    Ok((counts, Some(checks)))
}

/// Inspect through a private version-checked main/WAL pair. No SQLite connection
/// ever opens the live path. Final source symlinks currently remain a port gap:
/// the supervisor exposes no read-open operation that follows those links.
pub fn inspect_database(path: &Path, project: &str, mode: DiagnosticMode) -> DatabaseDiagnostic {
    inspect_database_with_filesystem(
        path,
        project,
        mode,
        &provenant_supervisor::fs::LocalFs,
        &std::env::temp_dir(),
    )
}
pub fn inspect_database_with_filesystem(
    path: &Path,
    project: &str,
    mode: DiagnosticMode,
    fs: &dyn provenant_supervisor::fs::SafeFs,
    temp_root: &Path,
) -> DatabaseDiagnostic {
    let database = path.to_string_lossy().into_owned();
    let failure = |error: Error| DatabaseDiagnostic {
        status: DiagnosticStatus::Error,
        exists: true,
        read_only: true,
        database: database.clone(),
        project: project.into(),
        counts: None,
        checks: None,
        error: Some(error.to_string()),
    };
    match file_version(fs, path) {
        Ok(None) => {
            return DatabaseDiagnostic {
                status: DiagnosticStatus::Absent,
                exists: false,
                read_only: true,
                database,
                project: project.into(),
                counts: None,
                checks: None,
                error: None,
            }
        }
        Err(error) => return failure(error),
        Ok(Some(_)) => {}
    }
    let mut snapshot = match PrivateSnapshot::create(fs, temp_root) {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    let mut report = match copy_consistent_snapshot(fs, path, &snapshot).and_then(|()| {
        let db = Connection::open_with_flags(
            snapshot.path.join("fabric.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        let report = inspect_snapshot(&db, &database, project, mode);
        db.close().map_err(|(_, e)| Error::from(e))?;
        Ok(report)
    }) {
        Ok(report) => report,
        Err(error) => failure(error),
    };
    if let Err(error) = snapshot.cleanup() {
        report = failure(error);
    }
    report
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileVersion {
    device: u64,
    inode: u64,
    size: u64,
    modified_ns: i128,
}
fn file_version(
    fs: &dyn provenant_supervisor::fs::SafeFs,
    path: &Path,
) -> Result<Option<FileVersion>> {
    let stat = || -> std::io::Result<provenant_supervisor::fs::Metadata> {
        if let Some(name) = path.file_name() {
            let (parent, _) = filesystem::parent_and_name(path);
            filesystem::directory(fs, parent, false)?.metadata_at(name, true)
        } else {
            filesystem::directory(fs, path, false)?.metadata()
        }
    };
    match stat() {
        Ok(m) => Ok(Some(FileVersion {
            device: m.device,
            inode: m.inode,
            size: m.size,
            modified_ns: m.modified_ns,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FilesystemError::new("stat", path, e).into()),
    }
}
fn copy_consistent_snapshot(
    fs: &dyn provenant_supervisor::fs::SafeFs,
    path: &Path,
    snapshot: &PrivateSnapshot,
) -> Result<()> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    for attempt in 0..3 {
        let before_main = file_version(fs, path)?;
        let before_wal = file_version(fs, &wal)?;
        let copied = (|| {
            copy_file(
                fs,
                path,
                &*snapshot.dir,
                std::ffi::OsStr::new("fabric.sqlite3"),
            )?;
            if before_wal.is_some() {
                copy_file(
                    fs,
                    &wal,
                    &*snapshot.dir,
                    std::ffi::OsStr::new("fabric.sqlite3-wal"),
                )?;
            } else {
                remove_if_present(&*snapshot.dir, std::ffi::OsStr::new("fabric.sqlite3-wal"))?;
            }
            Ok(())
        })();
        if let Err(error) = copied {
            remove_pair(&*snapshot.dir)?;
            if attempt == 2 {
                return Err(error);
            }
            continue;
        }
        if before_main == file_version(fs, path)? && before_wal == file_version(fs, &wal)? {
            return Ok(());
        }
        remove_pair(&*snapshot.dir)?;
    }
    Err(Error::SnapshotChanged)
}
fn copy_file(
    fs: &dyn provenant_supervisor::fs::SafeFs,
    path: &Path,
    destination: &dyn provenant_supervisor::fs::SafeDir,
    name: &std::ffi::OsStr,
) -> Result<()> {
    use std::io::Read;
    let source = fs
        .open_source(path)
        .map_err(|e| FilesystemError::new("copyfile", path, e))?;
    // The version recheck detects growth/shrinkage; cap this read at the opened
    // source length so a concurrent appender cannot prolong a copy forever.
    let size = source.metadata()?.size;
    let mut source = source.take(size);
    let mut target = destination.create_new(name)?;
    std::io::copy(&mut source, &mut target)?;
    Ok(())
}
fn remove_if_present(
    dir: &dyn provenant_supervisor::fs::SafeDir,
    name: &std::ffi::OsStr,
) -> Result<()> {
    match dir.unlink(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn remove_pair(dir: &dyn provenant_supervisor::fs::SafeDir) -> Result<()> {
    remove_if_present(dir, std::ffi::OsStr::new("fabric.sqlite3"))?;
    remove_if_present(dir, std::ffi::OsStr::new("fabric.sqlite3-wal"))
}
struct PrivateSnapshot {
    parent: Box<dyn provenant_supervisor::fs::SafeDir>,
    dir: Box<dyn provenant_supervisor::fs::SafeDir>,
    name: std::ffi::OsString,
    path: std::path::PathBuf,
    armed: bool,
}
impl PrivateSnapshot {
    fn create(fs: &dyn provenant_supervisor::fs::SafeFs, temp_root: &Path) -> Result<Self> {
        // The trusted temp root may be on a different device from /. Anchor
        // there so verified children use its device. Appending . keeps a temp
        // root symlink in the allowed ancestor path, preserving link following.
        let parent = fs.open_anchor(&temp_root.join("."))?;
        for _ in 0..128 {
            let mut name = std::ffi::OsString::from("fabric-inspect-");
            name.push(fs.temporary_name()?);
            match parent.mkdir(&name, 0o700) {
                Ok(()) => {
                    let dir = parent.open_verified_directory(&name)?;
                    return Ok(Self {
                        parent,
                        dir,
                        path: temp_root.join(&name),
                        name,
                        armed: true,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::SnapshotNameExhausted)
    }
    fn cleanup(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }
        // Only this exclusively created, mode-0700 namespace is traversed.
        remove_contents(&*self.dir)?;
        self.parent.rmdir(&self.name)?;
        self.armed = false;
        Ok(())
    }
}
fn remove_contents(dir: &dyn provenant_supervisor::fs::SafeDir) -> Result<()> {
    for name in dir.entries()? {
        let name = name?;
        let metadata = dir.metadata_at(&name, false)?;
        if metadata.kind == provenant_supervisor::fs::FileKind::Directory {
            let child = dir.open_verified_directory(&name)?;
            remove_contents(&*child)?;
            dir.rmdir(&name)?;
        } else {
            dir.unlink(&name)?;
        }
    }
    Ok(())
}
impl Drop for PrivateSnapshot {
    fn drop(&mut self) {
        if self.armed {
            if let Err(e) = self.cleanup() {
                eprintln!("diagnostic snapshot cleanup failed: {e}");
            }
        }
    }
}
