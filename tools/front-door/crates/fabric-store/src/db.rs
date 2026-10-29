use crate::*;
use provenant_core::{CancelToken, Clock, StateRoot};
use provenant_os::fs::Directory;
#[cfg(test)]
use provenant_os::fs::Metadata;
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
    include_str!("migrations/012-hook-delivery.sql"),
    include_str!("migrations/013-viewer-state.sql"),
];

/// Most recent database schema supported by this release.
pub const CURRENT_SCHEMA_VERSION: usize = MIGRATIONS.len();
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
    pub cancel: &'a CancelToken,
}
impl Request<'_> {
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
    /// The wait for an advisory lock that starts now: SQLite's busy timeout.
    pub fn lock_budget(&self) -> provenant_core::WaitBudget {
        provenant_sqlite::lock_budget(self.clock.mono_now())
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

    /// Open an existing store for bounded hook delivery updates.
    pub fn open_hook_delivery(
        root: &StateRoot,
        deadline: std::time::Instant,
    ) -> Result<Option<Self>> {
        Self::open_local_delivery(root, deadline, true)
    }

    #[allow(clippy::disallowed_methods)]
    pub(crate) fn open_local_delivery(
        root: &StateRoot,
        deadline: std::time::Instant,
        writable: bool,
    ) -> Result<Option<Self>> {
        Self::open_bounded_database(root, deadline, writable, "fabric.sqlite3", true)
    }

    pub(crate) fn open_read_only_host(
        root: &StateRoot,
        deadline: std::time::Instant,
    ) -> Result<Option<Self>> {
        Self::open_bounded_database(root, deadline, false, "host.sqlite3", false)
    }

    #[allow(clippy::disallowed_methods)]
    fn open_bounded_database(
        root: &StateRoot,
        deadline: std::time::Instant,
        writable: bool,
        database: &str,
        read_only_wal: bool,
    ) -> Result<Option<Self>> {
        crate::local_delivery::before(deadline)?;
        let open = || -> Result<Directory> {
            let state = Directory::open_anchor(root.as_path())?;
            private(&state, root.as_path())?;
            let fabric = state.open_verified_directory(OsStr::new("fabric"))?;
            private(&fabric, &root.as_path().join("fabric"))?;
            fabric.open_read(OsStr::new(database))?;
            Ok(fabric)
        };
        let fabric = match open() {
            Ok(fabric) => fabric,
            Err(Error::Filesystem(error)) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error),
        };
        let path = root.as_path().join("fabric");
        let names = ["", "-wal", "-shm", "-journal"]
            .map(|suffix| OsString::from(format!("{database}{suffix}")));
        provenant_sqlite::check_companions(&fabric, &names, Into::into, |name, _| {
            Error::UnsafePath(path.join(name))
        })?;
        let path = path.join(database);
        let db = if writable {
            provenant_sqlite::open_connection(&path, false)?
        } else if read_only_wal {
            provenant_sqlite::open_read_only_connection(&path)?
        } else {
            provenant_sqlite::open_connection(&path, true)?
        };
        // SQLite runs no progress handler while it waits on a lock, so the
        // caller's deadline also caps the busy timeout.
        db.busy_timeout(
            provenant_sqlite::busy_timeout()
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        )?;
        db.progress_handler(100, Some(move || std::time::Instant::now() >= deadline))?;
        // Filesystem discovery was validated before this connection opened.
        if writable {
            provenant_sqlite::retain_read_only_wal(&db)?;
        } else {
            // Pin the answer walk to one snapshot despite concurrent writes.
            db.execute_batch("BEGIN")?;
        }
        let store = Self {
            db,
            locks: None,
            _owner: PhantomData,
        };
        if schema_version(&store.db)? != MIGRATIONS.len() {
            return Err(Error::Invalid("local delivery index unavailable"));
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
        companions(&fabric, &path, "fabric.sqlite3")?;
        let db = provenant_sqlite::open_connection(&path.join("fabric.sqlite3"), true)?;
        let version = schema_version(&db)?;
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
        companions(&fabric, &path, database)?;
        let db = provenant_sqlite::open_connection(&db_path, false)?;
        // Refuse future versions before journal-mode changes or schema writes.
        schema_version(&db)?;
        enable_wal(&db)?;
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
        while schema_version(&store.db)? < MIGRATIONS.len() {
            let done = store.write(request, |tx| {
                let version = schema_version(tx)?;
                let Some(sql) = MIGRATIONS.get(version) else {
                    return Ok(true);
                };
                tx.execute_batch(sql)?;
                tx.pragma_update(None, "user_version", (version + 1) as i64)?;
                Ok(false)
            })?;
            if done {
                break;
            }
        }
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
        request.check()?;
        // BEGIN IMMEDIATE waits up to the busy timeout for the write lock.
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let value = action(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}
fn private(directory: &Directory, path: &std::path::Path) -> Result<()> {
    let metadata = directory.metadata()?;
    if !provenant_sqlite::is_private(metadata) {
        return Err(Error::UnsafePath(path.into()));
    }
    Ok(())
}
fn companions(directory: &Directory, path: &std::path::Path, database: &str) -> Result<()> {
    let names = ["", "-wal", "-shm", "-journal"]
        .map(|suffix| OsString::from(format!("{database}{suffix}")));
    provenant_sqlite::check_companions(directory, &names, Into::into, |name, _| {
        Error::UnsafePath(path.join(name))
    })
}
#[cfg(test)]
fn companion(
    path: PathBuf,
    mut metadata: impl FnMut() -> provenant_os::fs::Result<Metadata>,
) -> Result<()> {
    provenant_sqlite::check_companion(&mut metadata, Into::into, |_| {
        Error::UnsafePath(path.clone())
    })
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

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn companion_refuses_another_owner() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
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
                companion(path.clone(), || Ok(metadata)),
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
        // Entering WAL can leave a valid rollback journal for recovery.
        directory.create_new(OsStr::new("fabric.sqlite3-journal"))?;
        std::fs::write(&path, b"rollback journal data")?;
        companions(&directory, temp.path(), "fabric.sqlite3")?;
        directory.unlink(OsStr::new("fabric.sqlite3-journal"))?;
        std::os::unix::fs::symlink(temp.path().join("missing"), &path)?;
        let error = companions(&directory, temp.path(), "fabric.sqlite3").unwrap_err();
        assert!(matches!(&error, Error::UnsafePath(p) if p == &path));
        assert!(error.to_string().contains("fabric.sqlite3-journal"));
        Ok(())
    }

    #[test]
    fn refuses_a_vfs_that_cannot_enable_wal() {
        let db = Connection::open_in_memory().unwrap();
        assert!(matches!(enable_wal(&db), Err(Error::JournalMode(mode)) if mode == "memory"));
    }
}
