//! Batch 1 run-store cut-over: sealed writer ownership becomes a worktree
//! lease. The lease state (live or lapsed) is derived from the holder's attempt
//! state on every read, so no sweeper runs and a crash leaves no stale lease.
//!
//! The transform is pure and deterministic; [`migrate_store`] is the only
//! file-touching path. It runs under the attempt's writer lock and publishes
//! the lease under the named worktree slot's writer lock.

use provenant_confinement::worktrees::{LEASE_FILE, lease_path, state_dir_name};
use provenant_core::{AttemptId, ProjectId};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use provenant_records::custody::{Sharing, WorktreeLease};
use provenant_records::migrate::MigrationError;
use provenant_records::run::CanonicalRequest;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A sealed `writer-owned.json` record is version 1. The replacement lease
/// store is version 2; the marker names this target so an interrupted pass is
/// resumed by the next install.
pub const WRITER_OWNERSHIP_VERSION: u64 = 1;
pub const LEASE_STORE_VERSION: u64 = 2;

/// The legacy sealed launch record. Only this migration decodes it; no live
/// path writes it again.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct LegacyWriterOwnership {
    #[serde(default, flatten)]
    pub extra: provenant_records::ExtraFields,
    pub schema_version: u64,
    pub lane_owned: bool,
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub owner: Option<provenant_confinement::worktrees::WorktreeOwner>,
}

/// Locate the worktree slot named by an attempt's sealed launch request. The
/// slot is the one place a confinement reader looks for `lease.json`, so the
/// migrated lease must be published there and nowhere else.
fn slot_for_request(state_root: &Path, directory: &Directory) -> Option<PathBuf> {
    let raw = crate::artifacts::read_sealed(directory, "request.json", crate::RECORD_LIMIT)
        .ok()
        .flatten()?;
    let request: CanonicalRequest = crate::db::decode(&raw).ok()?;
    let writer = request.task.writer?;
    let project: ProjectId = writer.custody_project?;
    Some(
        state_root
            .join("worktrees")
            .join(project.to_string())
            .join(state_dir_name(&writer.worktree)),
    )
}

/// Pure transform of one sealed ownership record. A record that is not owned
/// by a lane has no lease; a record already at the target version is left as
/// it was. The caller removes the old record and publishes the new one under
/// the attempt's writer lock.
pub(crate) fn migrate(
    from: u64,
    to: u64,
    record: &LegacyWriterOwnership,
) -> Result<Option<WorktreeLease>, MigrationError> {
    if from == to {
        return Ok(None);
    }
    if (from, to) != (WRITER_OWNERSHIP_VERSION, LEASE_STORE_VERSION) {
        return Err(MigrationError::new(format!(
            "unsupported writer ownership migration {from} -> {to}"
        )));
    }
    if !record.lane_owned {
        return Ok(None);
    }
    let Some(owner) = record.owner.as_ref() else {
        return Ok(None);
    };
    let Some(holder) = owner.attempt_id else {
        // An adopted human checkout has no attempt to hold a lease.
        return Ok(None);
    };
    Ok(Some(WorktreeLease {
        holder,
        since: owner.created_at,
        sharing: Sharing::Inherit,
    }))
}

/// Bring an existing run database to the current schema during cut-over.
pub fn migrate_database(state_root: &Path) -> Result<(), MigrationError> {
    let path = state_root.join("run.sqlite3");
    if !path.exists() {
        return Ok(());
    }
    let db = provenant_sqlite::open_connection(&path, false)
        .map_err(|error| MigrationError::new(error.to_string()))?;
    provenant_sqlite::ensure_schema(&db, crate::db::SCHEMA)
        .map_err(|error| MigrationError::new(error.to_string()))
}

/// One cut-over for the run store. Every attempt's sealed ownership record is
/// rewritten under that attempt's writer lock. A `migrating-<to>` marker makes
/// an interrupted pass resumable, and records already converted are skipped.
pub fn migrate_store(state_root: &Path) -> Result<Vec<String>, MigrationError> {
    let attempts = state_root.join("attempts");
    let mut changed = Vec::new();
    let entries = match std::fs::read_dir(&attempts) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(changed),
        Err(error) => return Err(error.into()),
    };
    let marker = attempts.join(format!("migrating-{LEASE_STORE_VERSION}"));
    std::fs::write(&marker, b"")?;
    let result = (|| -> Result<(), MigrationError> {
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if AttemptId::parse(&name).is_err() {
                continue;
            }
            let directory = Directory::open_anchor(&entry.path())
                .map_err(|error| MigrationError::new(error.to_string()))?;
            let _lock = directory
                .try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)
                .map_err(|error| {
                    MigrationError::new(format!("{}: writer lock: {error}", entry.path().display()))
                })?;
            let Some(raw) =
                crate::artifacts::read_sealed(&directory, "writer-owned.json", crate::RECORD_LIMIT)
                    .map_err(|error| MigrationError::new(error.to_string()))?
            else {
                continue;
            };
            let ownership: LegacyWriterOwnership =
                crate::db::decode(&raw).map_err(|error| MigrationError::new(error.to_string()))?;
            if ownership.schema_version != WRITER_OWNERSHIP_VERSION {
                // A future writer, or a record already handled by a resumed
                // pass, is never rewritten.
                continue;
            }
            let lease = migrate(WRITER_OWNERSHIP_VERSION, LEASE_STORE_VERSION, &ownership)?;
            // Publish the replacement before removing the old record: a
            // failure here must leave the ownership evidence for a resumed
            // pass, never an attempt with neither record. The lease goes to
            // the worktree slot, the one path confinement reads; there is no
            // second lease in the attempt directory.
            if let Some(lease) = lease {
                let slot = slot_for_request(state_root, &directory).ok_or_else(|| {
                    MigrationError::new(format!(
                        "{}: no sealed writer slot for migrated lease",
                        entry.path().display()
                    ))
                })?;
                let slot_directory = Directory::open_anchor(&slot)
                    .map_err(|error| MigrationError::new(error.to_string()))?;
                let _slot_lock = slot_directory
                    .try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)
                    .map_err(|error| {
                        MigrationError::new(format!(
                            "{}: slot writer lock: {error}",
                            slot.display()
                        ))
                    })?;
                let bytes = serde_json::to_vec(&lease)
                    .map_err(|error| MigrationError::new(error.to_string()))?;
                slot_directory
                    .atomic_write(
                        OsStr::new(LEASE_FILE),
                        ".lease-",
                        &bytes,
                        PublishMode::Replace,
                    )
                    .and_then(|publication| publication.require_durable())
                    .map_err(|error| MigrationError::new(error.to_string()))?;
                changed.push(lease_path(&slot).display().to_string());
            }
            directory
                .unlink(OsStr::new("writer-owned.json"))
                .map_err(|error| MigrationError::new(error.to_string()))?;
            changed.push(entry.path().join("writer-owned.json").display().to_string());
        }
        Ok(())
    })();
    result?;
    std::fs::remove_file(&marker)?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_confinement::worktrees::{WorktreeOrigin, WorktreeOwner};
    use provenant_core::{HostId, WallTime};

    fn owner(holder: Option<AttemptId>) -> WorktreeOwner {
        WorktreeOwner {
            attempt_id: holder,
            operation_id: None,
            branch: "slice/E1-33".to_owned(),
            base: "origin/main".to_owned(),
            host: HostId::from_bytes([3; 16]),
            created_at: WallTime::from_unix_seconds(1_700_000_000).expect("wall time"),
            origin: WorktreeOrigin::Lane,
            label: None,
            reference: None,
        }
    }

    fn write_ownership(directory: &Directory, lane_owned: bool, holder: Option<AttemptId>) {
        let record = LegacyWriterOwnership {
            extra: Default::default(),
            schema_version: WRITER_OWNERSHIP_VERSION,
            lane_owned,
            owner: Some(owner(holder)),
        };
        let bytes = crate::db::bytes(&record).expect("encode ownership");
        crate::artifacts::seal(directory, "writer-owned.json", &bytes, crate::RECORD_LIMIT)
            .expect("seal ownership");
    }

    /// Seal the launch request that names the one worktree slot, and create it.
    fn write_slot_request(root: &Path, directory: &Directory) -> PathBuf {
        let submission = crate::tests::input();
        let project = ProjectId::from_bytes([3; 16]);
        let worktree = root.join("bound-worktree");
        let mut request = submission.request;
        request.task.writer = Some(provenant_records::run::WriterRequest {
            extra: Default::default(),
            custody_project: Some(project),
            project: root.to_path_buf(),
            worktree: worktree.clone(),
        });
        let bytes = crate::db::bytes(&request).expect("encode request");
        crate::artifacts::seal(directory, "request.json", &bytes, crate::RECORD_LIMIT)
            .expect("seal request");
        let slot = root
            .join("worktrees")
            .join(project.to_string())
            .join(state_dir_name(&worktree));
        std::fs::create_dir_all(&slot).expect("create slot");
        slot
    }

    #[test]
    fn schema_16_resource_origins_are_migrated() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("run.sqlite3");
        let db = rusqlite::Connection::open(&path).expect("create database");
        db.execute_batch(include_str!("fixtures/schema-16.sql"))
            .expect("base schema");
        let record = serde_json::json!({"skipped_routes": [
            {"guard_resource": {"kind": "pacing_reserve", "origin": "configuration"}},
            {"guard_resource": {"kind": "lane_ports", "origin": "shipped_default"}},
            {"guard_resource": {"kind": "other", "origin": "configuration"}}
        ]});
        db.execute("INSERT INTO attempts (id,project,graph,node,attempt,digest,actor,operation,number,operation_record,revision,terminal,record) VALUES ('a','p','g','n',1,'d','actor','op',1,x'7b7d',1,1,?1)", [serde_json::to_vec(&record).unwrap()]).expect("insert legacy record");
        drop(db);
        migrate_database(root.path()).expect("migrate database");
        let db = provenant_sqlite::open_connection(&path, true).expect("open migrated database");
        assert_eq!(provenant_sqlite::stored_version(&db).unwrap(), 17);
        let bytes: Vec<u8> = db
            .query_row("SELECT record FROM attempts WHERE id='a'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let migrated: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            migrated["skipped_routes"][0]["guard_resource"]["origin"],
            "host"
        );
        assert_eq!(
            migrated["skipped_routes"][1]["guard_resource"]["origin"],
            "default"
        );
        assert_eq!(
            migrated["skipped_routes"][2]["guard_resource"]["origin"],
            "configuration"
        );
    }

    #[test]
    fn writer_owned_reads_as_lease() {
        let root = tempfile::tempdir().expect("tempdir");
        let holder = AttemptId::from_bytes([7; 16]);
        let attempt = root.path().join("attempts").join(holder.to_string());
        std::fs::create_dir_all(&attempt).expect("attempt dir");
        let directory = Directory::open_anchor(&attempt).expect("attempt anchor");
        write_ownership(&directory, true, Some(holder));
        let slot = write_slot_request(root.path(), &directory);

        let changed = migrate_store(root.path()).expect("migrate");
        assert!(changed.iter().any(|path| path.ends_with("lease.json")));
        assert!(
            !attempt.join("writer-owned.json").exists(),
            "old ownership record was replaced"
        );
        assert!(
            !attempt.join("lease.json").exists(),
            "no second lease is written into the attempt directory"
        );
        let bytes = std::fs::read(slot.join(LEASE_FILE)).expect("slot lease exists");
        let lease: WorktreeLease = serde_json::from_slice(&bytes).expect("decode lease");
        assert_eq!(lease.holder, holder);
        assert_eq!(lease.since, owner(Some(holder)).created_at);
        assert_eq!(lease.sharing, Sharing::Inherit);
        assert!(
            !root.path().join("attempts").join("migrating-2").exists(),
            "marker removed after the cut-over"
        );

        let again = migrate_store(root.path()).expect("migrate twice");
        assert!(again.is_empty(), "second pass changes nothing: {again:?}");
    }

    #[test]
    fn unowned_record_has_no_lease() {
        let root = tempfile::tempdir().expect("tempdir");
        let id = AttemptId::from_bytes([8; 16]);
        let attempt = root.path().join("attempts").join(id.to_string());
        std::fs::create_dir_all(&attempt).expect("attempt dir");
        let directory = Directory::open_anchor(&attempt).expect("attempt anchor");
        write_ownership(&directory, false, Some(id));

        migrate_store(root.path()).expect("migrate");
        assert!(!attempt.join("writer-owned.json").exists());
        assert!(!attempt.join("lease.json").exists());
    }

    #[test]
    fn unsupported_version_pair_is_refused() {
        let record = LegacyWriterOwnership {
            extra: Default::default(),
            schema_version: WRITER_OWNERSHIP_VERSION,
            lane_owned: true,
            owner: Some(owner(Some(AttemptId::from_bytes([9; 16])))),
        };
        assert!(migrate(1, 3, &record).is_err());
        assert!(migrate(2, 1, &record).is_err());
    }
}
