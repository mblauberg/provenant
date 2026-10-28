use crate::*;
use provenant_core::{CancelToken, Clock, StateRoot, WaitBudget};
use provenant_os::fs::Directory;
#[cfg(test)]
use provenant_os::fs::Metadata;
pub use provenant_sqlite::{RetryWait, ThreadWait};
use rusqlite::{Connection, Transaction, TransactionBehavior};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    ffi::{OsStr, OsString},
    marker::PhantomData,
    path::Component,
    rc::Rc,
};

// Index + 1 is the schema version; each migration commits independently.
pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("migrations/002-import-skips.sql"),
    include_str!("migrations/003-claims-landing.sql"),
    include_str!("migrations/004-claim-lookup.sql"),
    include_str!("migrations/005-coordination-operations.sql"),
    include_str!("migrations/006-federation.sql"),
    include_str!("migrations/007-remove-legacy-import.sql"),
    include_str!("migrations/008-human-inbox.sql"),
    include_str!("migrations/009-run-chairs.sql"),
    include_str!("migrations/010-local-delivery.sql"),
    include_str!("migrations/011-queued-delivery.sql"),
];
// Index of 007, which may drop only the always-null legacy members and the
// traces of an import that brought nothing over. Anything the removed import
// actually wrote would be lost or would stop decoding, so the upgrade refuses
// it instead, naming the table and its row count.
const REMOVE_LEGACY_IMPORT: usize = 6;
const LEGACY_IMPORT_DATA: &[(&str, &str)] = &[
    (
        "imports",
        "SELECT count(*) FROM imports WHERE tasks<>0 OR messages<>0",
    ),
    // An empty import still appended one `imported` event, which 007 deletes.
    (
        "events",
        "SELECT count(*) FROM events e \
         WHERE json_extract(CAST(record AS TEXT),'$.kind')='imported' \
         AND NOT EXISTS(SELECT 1 FROM imports i \
         WHERE i.project=e.project AND i.tasks=0 AND i.messages=0)",
    ),
    (
        "messages",
        "SELECT count(*) FROM messages WHERE \
         json_type(CAST(record AS TEXT),'$.legacy')<>'null' \
         OR json_type(CAST(record AS TEXT),'$.legacy_output_path')<>'null' \
         OR 'legacy' IN (json_extract(CAST(record AS TEXT),'$.conversation.scope'), \
         json_extract(CAST(record AS TEXT),'$.reply_to.scope'), \
         json_extract(CAST(record AS TEXT),'$.task.scope'))",
    ),
    (
        "tasks",
        "SELECT count(*) FROM tasks WHERE json_type(CAST(record AS TEXT),'$.legacy')<>'null'",
    ),
    (
        "coordination_operations",
        "SELECT count(*) FROM coordination_operations \
         WHERE json_type(CAST(result AS TEXT),'$.task.legacy')<>'null'",
    ),
    (
        "peer_operations",
        "SELECT count(*) FROM peer_operations WHERE CASE WHEN json_valid(CAST(response AS TEXT)) \
         THEN json_type(CAST(response AS TEXT),'$.result.data.result.task.legacy')<>'null' END",
    ),
];
fn refuse_legacy_import_data(db: &Connection) -> Result<()> {
    for (table, sql) in LEGACY_IMPORT_DATA {
        let rows = db.query_row(sql, [], |row| row.get::<_, i64>(0))?;
        if rows != 0 {
            return Err(Error::LegacyImportData { table, rows });
        }
    }
    Ok(())
}
fn schema_version(db: &Connection) -> Result<usize> {
    match provenant_sqlite::read_version(db, MIGRATIONS.len()) {
        Ok(version) => Ok(version),
        Err(provenant_sqlite::VersionError::Unsupported(version)) => {
            Err(Error::SchemaVersion(version))
        }
        Err(provenant_sqlite::VersionError::Unversioned) => Err(Error::UnversionedDatabase),
        Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
    }
}
fn enable_wal(db: &Connection) -> Result<()> {
    match provenant_sqlite::enable_wal(db) {
        Ok(()) => Ok(()),
        Err(provenant_sqlite::WalError::Unavailable(mode)) => Err(Error::JournalMode(mode)),
        Err(provenant_sqlite::WalError::Sqlite(error)) => Err(error.into()),
    }
}

pub struct Request<'a> {
    pub clock: &'a dyn Clock,
    pub budget: WaitBudget,
    pub cancel: &'a CancelToken,
    pub backoff: &'a dyn RetryWait,
}
impl Request<'_> {
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.budget.remaining(self.clock.mono_now()).as_micros() == 0 {
            return Err(Error::Busy);
        }
        Ok(())
    }
    pub(crate) fn retry<T>(&self, mut action: impl FnMut() -> Result<T>) -> Result<T> {
        match provenant_sqlite::retry(
            self.clock,
            self.budget,
            self.cancel,
            self.backoff,
            |error: &Error| matches!(error, Error::Sqlite(rusqlite::Error::SqliteFailure(code, _)) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)),
            &mut action,
        ) {
            Ok(value) => Ok(value),
            Err(provenant_sqlite::RetryError::Busy) => Err(Error::Busy),
            Err(provenant_sqlite::RetryError::Cancelled) => Err(Error::Cancelled),
            Err(provenant_sqlite::RetryError::Action(error)) => Err(error),
        }
    }
    /// Retry an incomplete observation within the request's budget and cancellation.
    fn wait<T>(&self, action: impl FnMut() -> Result<Option<T>>) -> Result<T> {
        provenant_sqlite::observe(self.clock, self.budget, self.cancel, self.backoff, action)
            .map_err(|error| match error {
                provenant_sqlite::RetryError::Busy => Error::Busy,
                provenant_sqlite::RetryError::Cancelled => Error::Cancelled,
                provenant_sqlite::RetryError::Action(error) => error,
            })
    }
}

pub struct Store {
    pub(crate) db: Connection,
    pub(crate) locks: Option<Directory>,
    // SQLite supports movable connections, but Fabric requires worker ownership.
    _owner: PhantomData<Rc<()>>,
}
impl Store {
    /// No creation, migration, journal changes or retries. WAL and shared memory
    /// are opened read-only too; an unavailable WAL index fails closed.
    /// Filesystem calls are blocking. The deadline is checked between steps;
    /// callers needing a hard bound must run this off-thread with a timeout.
    #[allow(clippy::disallowed_methods)]
    pub fn open_read_only(root: &StateRoot, deadline: std::time::Instant) -> Result<Option<Self>> {
        Self::open_local_delivery(root, deadline, false)
    }

    #[allow(clippy::disallowed_methods)]
    pub(crate) fn open_local_delivery(
        root: &StateRoot,
        deadline: std::time::Instant,
        writable: bool,
    ) -> Result<Option<Self>> {
        crate::local_delivery::before(deadline)?;
        let open = || -> Result<Directory> {
            let state = Directory::open_anchor(root.as_path())?;
            private(&state, root.as_path())?;
            let fabric = state.open_verified_directory(OsStr::new("fabric"))?;
            private(&fabric, &root.as_path().join("fabric"))?;
            fabric.open_read(OsStr::new("fabric.sqlite3"))?;
            Ok(fabric)
        };
        let fabric = match open() {
            Ok(fabric) => fabric,
            Err(Error::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        let path = root.as_path().join("fabric");
        let names = [
            "fabric.sqlite3",
            "fabric.sqlite3-wal",
            "fabric.sqlite3-shm",
            "fabric.sqlite3-journal",
        ]
        .map(OsString::from);
        provenant_sqlite::check_companions(
            &fabric,
            &names,
            |check| {
                crate::local_delivery::before(deadline)?;
                check()?.ok_or(Error::Busy)
            },
            Into::into,
            |name, _| Error::UnsafePath(path.join(name)),
        )?;
        let path = path.join("fabric.sqlite3");
        let db = if writable {
            provenant_sqlite::open_connection(&path, false)?
        } else {
            provenant_sqlite::open_read_only_connection(&path)?
        };
        db.progress_handler(100, Some(move || std::time::Instant::now() >= deadline))?;
        // Pin one answer snapshot, including concurrent acknowledgements.
        // Filesystem discovery was validated before this connection opened.
        if writable {
            provenant_sqlite::retain_read_only_wal(&db)?;
        } else {
            db.execute_batch("BEGIN")?;
        }
        let store = Self {
            db,
            locks: None,
            _owner: PhantomData,
        };
        if schema_version(&store.db)? != MIGRATIONS.len() {
            return Err(Error::Invalid(
                "local delivery index unavailable; upgrade through a normal inbox command",
            ));
        }
        crate::local_delivery::before(deadline)?;
        Ok(Some(store))
    }

    /// Open existing owner state without creating directories, databases or migrations.
    /// SQLite may create its own WAL companions for an existing database.
    pub fn open_existing(root: &StateRoot, request: &Request<'_>) -> Result<Self> {
        request.check()?;
        let mut directory = Directory::root(true)?;
        for part in root.as_path().components() {
            if let Component::Normal(name) = part {
                match directory.metadata_at(name, false) {
                    Err(error) if error.is_not_found() => return Err(Error::NotFound),
                    Err(error) => return Err(error.into()),
                    Ok(_) => {}
                }
                directory = checked_directory(&directory, name, None)?;
            }
        }
        private(&directory, root.as_path())?;
        let directory = directory.into_anchor()?;
        let name = OsStr::new("fabric");
        match directory.metadata_at(name, false) {
            Err(error) if error.is_not_found() => return Err(Error::NotFound),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let fabric = checked_directory(&directory, name, Some(directory.metadata()?.device))?;
        let path = root.as_path().join("fabric");
        private(&fabric, &path)?;
        match fabric.metadata_at(OsStr::new("fabric.sqlite3"), false) {
            Err(error) if error.is_not_found() => return Err(Error::NotFound),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        companions(&fabric, &path, "fabric.sqlite3", request)?;
        let db = provenant_sqlite::open_connection(&path.join("fabric.sqlite3"), true)?;
        let version = request.retry(|| schema_version(&db))?;
        companions(&fabric, &path, "fabric.sqlite3", request)?;
        if version != MIGRATIONS.len() {
            return Err(Error::SchemaVersion(version as i64));
        }
        Ok(Self {
            db,
            locks: None,
            _owner: PhantomData,
        })
    }

    pub fn open(root: &StateRoot, request: &Request<'_>) -> Result<Self> {
        Self::open_database(root, request, "fabric.sqlite3", Some("locks"))
    }

    /// Host-only records live outside the exact coordination DB grants lanes
    /// receive. Reuse all directory, companion and migration checks.
    pub(crate) fn open_host(root: &StateRoot, request: &Request<'_>) -> Result<Self> {
        Self::open_database(root, request, "host.sqlite3", None)
    }

    fn open_database(
        root: &StateRoot,
        request: &Request<'_>,
        database: &str,
        locks_name: Option<&str>,
    ) -> Result<Self> {
        request.check()?;
        let mut directory = Directory::root(true)?;
        for part in root.as_path().components() {
            if let Component::Normal(name) = part {
                match directory.metadata_at(name, false) {
                    Ok(_) => {}
                    Err(error) if error.is_not_found() => match directory.mkdir(name, 0o700) {
                        Ok(()) => directory.sync()?,
                        Err(error) if error.is_exists() => {}
                        Err(error) => return Err(error.into()),
                    },
                    Err(error) => return Err(error.into()),
                }
                directory = checked_directory(&directory, name, None)?;
            }
        }
        private(&directory, root.as_path())?;
        // Traversal from / may cross a local mount before the configured
        // state root. Pin supervisor's device policy to this validated handle,
        // without reopening ancestors or relaxing descendant checks.
        let directory = directory.into_anchor()?;
        match directory.metadata_at(OsStr::new("fabric"), false) {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {
                match directory.mkdir(OsStr::new("fabric"), 0o700) {
                    Ok(()) => directory.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
        let fabric = checked_directory(
            &directory,
            OsStr::new("fabric"),
            Some(directory.metadata()?.device),
        )?;
        let path = root.as_path().join("fabric");
        private(&fabric, &path)?;
        let db_path = path.join(database);
        match fabric.create_new(OsStr::new(database)) {
            Ok(file) => {
                file.sync()?;
                fabric.sync()?;
            }
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        companions(&fabric, &path, database, request)?;
        let db = provenant_sqlite::open_connection(&db_path, false)?;
        // Refuse future versions before journal-mode changes or schema writes.
        request.retry(|| schema_version(&db))?;
        request.retry(|| enable_wal(&db))?;
        if database == "fabric.sqlite3" {
            provenant_sqlite::retain_read_only_wal(&db)?;
        }
        let locks = if let Some(locks_name) = locks_name {
            match fabric.mkdir(OsStr::new(locks_name), 0o700) {
                Ok(()) => fabric.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error.into()),
            }
            let locks = checked_directory(
                &fabric,
                OsStr::new(locks_name),
                Some(fabric.metadata()?.device),
            )?;
            private(&locks, &path.join(locks_name))?;
            Some(locks)
        } else {
            None
        };
        let store = Self {
            db,
            locks,
            _owner: PhantomData,
        };
        while request.retry(|| schema_version(&store.db))? < MIGRATIONS.len() {
            let done = store.write(request, |tx| {
                let version = schema_version(tx)?;
                let Some(sql) = MIGRATIONS.get(version) else {
                    return Ok(true);
                };
                if version == REMOVE_LEGACY_IMPORT {
                    refuse_legacy_import_data(tx)?;
                }
                tx.execute_batch(sql)?;
                tx.pragma_update(None, "user_version", (version + 1) as i64)?;
                Ok(false)
            })?;
            if done {
                break;
            }
        }
        companions(&fabric, &path, database, request)?;
        Ok(store)
    }
    pub(crate) fn write<T>(
        &self,
        request: &Request<'_>,
        action: impl Fn(&Connection) -> Result<T>,
    ) -> Result<T> {
        if !self.db.is_autocommit() {
            return action(&self.db);
        }
        request.retry(|| {
            let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
            let value = action(&tx)?;
            // Only BUSY/LOCKED can replay, after this transaction has rolled back.
            // All other commit failures surface unchanged, never as fresh writes.
            tx.commit()?;
            Ok(value)
        })
    }
}
fn private(directory: &Directory, path: &std::path::Path) -> Result<()> {
    let metadata = directory.metadata()?;
    if !provenant_sqlite::is_private(metadata) {
        return Err(Error::UnsafePath(path.into()));
    }
    Ok(())
}
fn companions(
    directory: &Directory,
    path: &std::path::Path,
    database: &str,
    request: &Request<'_>,
) -> Result<()> {
    let names = ["", "-wal", "-shm", "-journal"]
        .map(|suffix| OsString::from(format!("{database}{suffix}")));
    provenant_sqlite::check_companions(
        directory,
        &names,
        |check| request.wait(check),
        Into::into,
        |name, _| Error::UnsafePath(path.join(name)),
    )
}
#[cfg(test)]
fn companion(
    request: &Request<'_>,
    path: PathBuf,
    mut metadata: impl FnMut() -> provenant_os::fs::Result<Metadata>,
) -> Result<()> {
    provenant_sqlite::check_companion(
        &mut metadata,
        |check| request.wait(check),
        Into::into,
        |_| Error::UnsafePath(path.clone()),
    )
}

// The configured root may live on another local volume than `/`. Once there,
// Fabric descendants stay on that volume. Every component remains no-follow
// with the supervisor's stat/open/stat identity verification.
fn checked_directory(parent: &Directory, name: &OsStr, device: Option<u64>) -> Result<Directory> {
    provenant_sqlite::checked_directory(parent, name, device).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{DurationMicros, MonoTime, TestClock};
    use std::cell::Cell;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[derive(Default)]
    struct CountingWait(Cell<usize>);
    impl RetryWait for CountingWait {
        fn wait(&self, _: DurationMicros, _: &CancelToken) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn companion_refuses_another_owner() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let backoff = CountingWait::default();
        let request = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 1)?,
            cancel: &cancel,
            backoff: &backoff,
        };
        for name in [
            "fabric.sqlite3",
            "fabric.sqlite3-wal",
            "fabric.sqlite3-shm",
            "fabric.sqlite3-journal",
        ] {
            let metadata = Metadata {
                uid: provenant_os::fs::effective_uid().wrapping_add(1),
                ..directory.create_new(OsStr::new(name))?.metadata()?
            };
            let path = temp.path().join(name);
            assert!(matches!(
                companion(&request, path.clone(), || Ok(metadata)),
                Err(Error::UnsafePath(p)) if p == path
            ));
        }
        Ok(())
    }

    #[test]
    fn companions_allow_a_safe_journal_and_refuse_a_symlink() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        let path = temp.path().join("fabric.sqlite3-journal");
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let backoff = CountingWait::default();
        let request = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 1)?,
            cancel: &cancel,
            backoff: &backoff,
        };
        // Entering WAL can leave a valid rollback journal for recovery.
        directory.create_new(OsStr::new("fabric.sqlite3-journal"))?;
        std::fs::write(&path, b"rollback journal data")?;
        companions(&directory, temp.path(), "fabric.sqlite3", &request)?;
        directory.unlink(OsStr::new("fabric.sqlite3-journal"))?;
        std::os::unix::fs::symlink(temp.path().join("missing"), &path)?;
        let error = companions(&directory, temp.path(), "fabric.sqlite3", &request).unwrap_err();
        assert!(matches!(&error, Error::UnsafePath(p) if p == &path));
        assert!(error.to_string().contains("fabric.sqlite3-journal"));
        Ok(())
    }

    #[test]
    fn companion_rechecks_unlinked_observations() -> TestResult {
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        for replaced in [false, true] {
            let backoff = CountingWait::default();
            let request = Request {
                clock: &clock,
                budget: WaitBudget::cli(clock.mono_now(), 1)?,
                cancel: &cancel,
                backoff: &backoff,
            };
            let temp = tempfile::tempdir()?;
            let directory = Directory::open_anchor(temp.path())?;
            let name = OsStr::new("fabric.sqlite3-shm");
            let file = directory.create_new(name)?;
            let removed = Metadata {
                links: 0,
                ..file.metadata()?
            };
            directory.unlink(name)?;
            if replaced {
                directory.create_new(name)?;
            }
            let mut observations = 0;
            let result = companion(&request, temp.path().join(name), || {
                observations += 1;
                if observations == 1 {
                    Ok(removed)
                } else {
                    directory.metadata_at(name, false)
                }
            });
            assert!(result.is_ok(), "replaced={replaced}: {result:?}");
            assert_eq!(observations, 2);
            assert_eq!(backoff.0.get(), 1);
        }
        Ok(())
    }

    #[test]
    fn companion_refuses_unsafe_metadata_after_an_unlink() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        let path = temp.path().join("fabric.sqlite3-wal");
        let safe = directory
            .create_new(OsStr::new("fabric.sqlite3-wal"))?
            .metadata()?;
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let backoff = CountingWait::default();
        let request = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 1)?,
            cancel: &cancel,
            backoff: &backoff,
        };
        for unsafe_metadata in [
            Metadata { links: 2, ..safe },
            Metadata {
                kind: provenant_os::fs::FileKind::Symlink,
                ..safe
            },
            Metadata {
                kind: provenant_os::fs::FileKind::Directory,
                ..safe
            },
            Metadata {
                mode: 0o644,
                ..safe
            },
            Metadata {
                uid: safe.uid.wrapping_add(1),
                ..safe
            },
        ] {
            let mut observations = 0;
            let result = companion(&request, path.clone(), || {
                observations += 1;
                Ok(if observations == 1 {
                    Metadata { links: 0, ..safe }
                } else {
                    unsafe_metadata
                })
            });
            assert!(matches!(result, Err(Error::UnsafePath(p)) if p == path));
            assert_eq!(observations, 2);
        }
        Ok(())
    }

    #[test]
    fn companion_unlink_retry_preserves_wait_errors() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        let path = temp.path().join("fabric.sqlite3-shm");
        let removed = Metadata {
            links: 0,
            ..directory
                .create_new(OsStr::new("fabric.sqlite3-shm"))?
                .metadata()?
        };
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let backoff = CountingWait::default();
        let request = Request {
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 1)?,
            cancel: &cancel,
            backoff: &backoff,
        };
        assert!(matches!(
            companion(&request, path.clone(), || {
                cancel.cancel();
                Ok(removed)
            }),
            Err(Error::Cancelled)
        ));

        let cancel = CancelToken::new();
        let clock = TestClock::new(clock.wall_now()?, MonoTime::from_micros(999_999));
        let request = Request {
            // Only the final microsecond remains when the retry starts.
            budget: WaitBudget::cli(MonoTime::from_micros(0), 1)?,
            clock: &clock,
            cancel: &cancel,
            backoff: &backoff,
        };
        assert!(matches!(
            companion(&request, path, || Ok(removed)),
            Err(Error::Busy)
        ));
        Ok(())
    }

    #[test]
    fn refuses_a_vfs_that_cannot_enable_wal() {
        let db = Connection::open_in_memory().unwrap();
        assert!(matches!(enable_wal(&db), Err(Error::JournalMode(mode)) if mode == "memory"));
    }
}
