use provenant_core::{DurationMicros, MonoTime, WaitBudget};
#[allow(unsafe_code)]
mod read_only_vfs;
use provenant_os::fs::{Directory, FileKind, FsError, Metadata, verify_directory_identity};
use rusqlite::{Connection, OpenFlags};
use std::{
    cell::Cell,
    ffi::{OsStr, OsString},
    path::Path,
    sync::OnceLock,
    time::Duration,
};

/// How long a connection waits for a contended store before SQLite reports it
/// busy, unless the host configuration sets another wait.
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

static CONFIGURED: OnceLock<Duration> = OnceLock::new();
thread_local! {
    static THREAD_TIMEOUT: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// Set the process-wide busy timeout once, from host configuration, before
/// the first store opens. Later calls are ignored. Zero fails fast.
pub fn set_busy_timeout(timeout: Duration) {
    let _ = CONFIGURED.set(timeout);
}

/// The busy timeout every store connection sets once at open. The same wait
/// bounds the stores' own advisory locks, so one setting governs contention.
pub fn busy_timeout() -> Duration {
    THREAD_TIMEOUT
        .with(Cell::get)
        .or_else(|| CONFIGURED.get().copied())
        .unwrap_or(DEFAULT_BUSY_TIMEOUT)
}

/// Run `f` with another busy timeout for connections and locks this thread
/// opens or takes, so a contention test need not wait out the default.
pub fn with_busy_timeout<T>(timeout: Duration, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            THREAD_TIMEOUT.with(|timeout| timeout.set(self.0));
        }
    }
    let _restore = Restore(THREAD_TIMEOUT.with(|current| current.replace(Some(timeout))));
    f()
}

/// The busy timeout as a wait for a store's own advisory locks. Zero makes the
/// locks nonblocking too.
pub fn lock_budget(now: MonoTime) -> WaitBudget {
    let timeout = busy_timeout();
    if timeout.is_zero() {
        return WaitBudget::nonblocking(now);
    }
    let micros = u64::try_from(timeout.as_micros()).unwrap_or(u64::MAX);
    WaitBudget::up_to(now, DurationMicros::from_micros(micros))
}

/// Whether SQLite gave up on a contended store after the busy timeout.
pub fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Verify a directory's before/open/after identity and optional device.
pub fn checked_directory(
    parent: &Directory,
    name: &OsStr,
    device: Option<u64>,
) -> Result<Directory, FsError> {
    let before = parent.metadata_at(name, false)?;
    let child = parent.open_directory(name)?;
    let opened = child.metadata()?;
    let after = parent.metadata_at(name, false)?;
    verify_directory_identity(name, before, opened, after, device.unwrap_or(opened.device))?;
    Ok(child)
}

/// Whether a directory belongs to the current user and has no group/other access.
pub fn is_private(metadata: Metadata) -> bool {
    metadata.mode & 0o077 == 0 && metadata.uid == provenant_os::fs::effective_uid()
}

/// Why a database or companion file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionFault {
    NotRegular,
    Symlink,
    Owner,
    Mode,
    Links,
}

/// Return false when the observed inode disappeared so it is observed again.
fn companion_is_safe(metadata: Metadata) -> Result<bool, CompanionFault> {
    if metadata.kind == FileKind::Symlink {
        return Err(CompanionFault::Symlink);
    }
    if metadata.kind != FileKind::Regular {
        return Err(CompanionFault::NotRegular);
    }
    if metadata.uid != provenant_os::fs::effective_uid() {
        return Err(CompanionFault::Owner);
    }
    if metadata.mode & 0o077 != 0 {
        return Err(CompanionFault::Mode);
    }
    if metadata.links > 1 {
        return Err(CompanionFault::Links);
    }
    Ok(metadata.links == 1)
}

/// Check each database and companion name once, preserving each caller's
/// error type and path context.
pub fn check_companions<E>(
    directory: &Directory,
    names: &[OsString],
    mut filesystem_error: impl FnMut(FsError) -> E,
    mut unsafe_name: impl FnMut(&OsStr, CompanionFault) -> E,
) -> Result<(), E> {
    for name in names {
        check_companion(
            || directory.metadata_at(name, false),
            &mut filesystem_error,
            |fault| unsafe_name(name, fault),
        )?;
    }
    Ok(())
}

/// Check one companion. A name SQLite unlinked between lookup and stat is
/// observed again at once; one that keeps vanishing is absent, not an error.
pub fn check_companion<E>(
    mut metadata: impl FnMut() -> Result<Metadata, FsError>,
    mut filesystem_error: impl FnMut(FsError) -> E,
    mut unsafe_error: impl FnMut(CompanionFault) -> E,
) -> Result<(), E> {
    for _ in 0..COMPANION_OBSERVATIONS {
        match metadata() {
            Ok(metadata) => match companion_is_safe(metadata) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(fault) => return Err(unsafe_error(fault)),
            },
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(filesystem_error(error)),
        }
    }
    Ok(())
}
const COMPANION_OBSERVATIONS: usize = 4;

/// Open a database without following symlinks, with SQLite's internal mutexes
/// disabled because each store is owned by one thread. SQLite itself waits up
/// to [`busy_timeout`] for a contended store.
pub fn open_connection(path: &Path, read_only: bool) -> rusqlite::Result<Connection> {
    let access = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let db = Connection::open_with_flags(
        path,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(busy_timeout())?;
    Ok(db)
}

/// Unlike mode=ro alone, readonly_shm forbids opening WAL shared memory for
/// writing or creating it. Keep WAL visibility; immutable=1 would ignore WAL.
pub fn open_read_only_connection(path: &Path) -> rusqlite::Result<Connection> {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file:");
    for byte in path.as_os_str().as_bytes() {
        use std::fmt::Write;
        let _ = write!(uri, "%{byte:02X}");
    }
    uri.push_str("?mode=ro&readonly_shm=1");
    read_only_vfs::register()?;
    let db = Connection::open_with_flags_and_vfs(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        "provenant-read-only",
    )?;
    db.busy_timeout(busy_timeout())?;
    Ok(db)
}

/// Observe WAL without creating companions, taking SQLite's exclusive lock.
/// The native main-file descriptor supports locking only: VFS writes, truncation,
/// file controls and shared-memory creation are refused. Contended stores fail busy.
pub fn open_locking_read_only_connection(path: &Path) -> rusqlite::Result<Connection> {
    read_only_vfs::register_locking_reader()?;
    let db = Connection::open_with_flags_and_vfs(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        "provenant-locking-reader",
    )?;
    db.busy_timeout(busy_timeout())?;
    db.pragma_update(None, "query_only", true)?;
    db.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    Ok(db)
}

/// Writer-only preparation for a read-only WAL consumer after the last close.
pub fn retain_read_only_wal(db: &Connection) -> rusqlite::Result<()> {
    read_only_vfs::persist_wal(db)?;
    db.pragma_update(None, "journal_size_limit", 0)
}

#[derive(Debug)]
pub enum WalError {
    Sqlite(rusqlite::Error),
    Unavailable(String),
}

impl From<rusqlite::Error> for WalError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Enable WAL and the common durability and foreign-key settings.
pub fn enable_wal(db: &Connection) -> Result<(), WalError> {
    let mode: String = db
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(WalError::Sqlite)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(WalError::Unavailable(mode));
    }
    db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .map_err(WalError::Sqlite)?;
    Ok(())
}

/// One store's schema: the file that creates it and the only version it opens.
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    /// Statements that create every table, index and trigger, ending in
    /// `PRAGMA user_version=<version>;`.
    pub sql: &'static str,
    pub version: i64,
}

/// The refusal code for a store written by another release. The host is
/// drained with that release before this one opens the state.
pub const DRAIN_CODE: &str = "UPGRADE_DRAIN_REQUIRED";

#[derive(Debug)]
pub enum VersionError {
    /// The store is at another version, or has tables but no version. It is
    /// left exactly as found.
    Refused {
        found: i64,
        expected: i64,
    },
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for VersionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { found, expected } => write!(
                f,
                "store schema version {found} is not {expected}: drain the host with the release that wrote it, then remove the store"
            ),
            Self::Sqlite(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for VersionError {}

impl From<rusqlite::Error> for VersionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Whether the store has not been created yet. Refuses a store at any version
/// but the schema's, without writing to it.
fn is_fresh(db: &Connection, schema: Schema) -> Result<bool, VersionError> {
    let (version, has_schema): (i64, bool) = db.query_row(
        "SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_schema)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    match (version, has_schema) {
        (0, false) => Ok(true),
        (found, _) if found == schema.version => Ok(false),
        (found, _) => Err(VersionError::Refused {
            found,
            expected: schema.version,
        }),
    }
}

/// Refuse a store at any version but the schema's. An empty store passes. Reads
/// only, so a refused store is not modified.
pub fn check_version(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    is_fresh(db, schema).map(drop)
}

/// The version of an existing store, or 0 for one never created. Reads only.
pub fn stored_version(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row("SELECT user_version FROM pragma_user_version", [], |row| {
        row.get(0)
    })
}

/// Create an empty store from its schema in one immediate transaction, or
/// accept one already at the schema's version. Any other store is refused.
/// Re-checks inside the transaction, so concurrent openers create it once.
pub fn ensure_schema(db: &Connection, schema: Schema) -> Result<(), VersionError> {
    if !is_fresh(db, schema)? {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    if is_fresh(&tx, schema)? {
        tx.execute_batch(schema.sql)?;
    }
    tx.commit()?;
    Ok(())
}

/// Whether the store's `sqlite_schema` text is exactly the schema file's
/// statements, in order, byte for byte.
pub fn schema_matches(db: &Connection, schema: Schema) -> rusqlite::Result<bool> {
    let mut query = db.prepare(
        "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY rowid",
    )?;
    let statements: Vec<String> = query
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut rest = schema.sql;
    for statement in &statements {
        match rest.find(statement.as_str()) {
            Some(at) => rest = &rest[at + statement.len()..],
            None => return Ok(false),
        }
    }
    Ok(!rest.contains("CREATE "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_companion() -> Metadata {
        Metadata {
            kind: FileKind::Regular,
            device: 0,
            inode: 0,
            links: 1,
            size: 0,
            allocated: 0,
            modified_ns: 0,
            changed_ns: 0,
            uid: provenant_os::fs::effective_uid(),
            mode: 0o600,
        }
    }

    #[test]
    fn companion_rechecks_unlinked_observations() -> Result<(), Box<dyn std::error::Error>> {
        let safe = safe_companion();
        for replaced in [false, true] {
            let mut observations = 0;
            let result: Result<(), &'static str> = check_companion(
                || {
                    observations += 1;
                    if observations == 1 {
                        Ok(Metadata { links: 0, ..safe })
                    } else if replaced {
                        Ok(safe)
                    } else {
                        Err(FsError::Io {
                            operation: "stat",
                            path: "missing".into(),
                            source: std::io::Error::from_raw_os_error(2),
                        })
                    }
                },
                |_| "filesystem",
                |_| "unsafe",
            );
            assert_eq!(result, Ok(()));
            assert_eq!(observations, 2);
        }
        Ok(())
    }

    #[test]
    fn companion_refuses_unsafe_metadata_after_an_unlink() -> Result<(), Box<dyn std::error::Error>>
    {
        let safe = safe_companion();
        for (unsafe_metadata, expected) in [
            (Metadata { links: 2, ..safe }, CompanionFault::Links),
            (
                Metadata {
                    kind: FileKind::Symlink,
                    ..safe
                },
                CompanionFault::Symlink,
            ),
            (
                Metadata {
                    kind: FileKind::Directory,
                    ..safe
                },
                CompanionFault::NotRegular,
            ),
            (
                Metadata {
                    mode: 0o644,
                    ..safe
                },
                CompanionFault::Mode,
            ),
            (
                Metadata {
                    uid: safe.uid.wrapping_add(1),
                    ..safe
                },
                CompanionFault::Owner,
            ),
        ] {
            let mut observations = 0;
            let result = check_companion(
                || {
                    observations += 1;
                    Ok(if observations == 1 {
                        Metadata { links: 0, ..safe }
                    } else {
                        unsafe_metadata
                    })
                },
                |_| CompanionFault::NotRegular,
                |fault| fault,
            );
            assert_eq!(result, Err(expected));
            assert_eq!(observations, 2);
        }
        Ok(())
    }

    #[test]
    fn a_companion_that_keeps_vanishing_is_absent() {
        let removed = Metadata {
            links: 0,
            ..safe_companion()
        };
        let mut observations = 0;
        let result: Result<(), &'static str> = check_companion(
            || {
                observations += 1;
                Ok(removed)
            },
            |_| "filesystem",
            |_| "unsafe",
        );
        assert_eq!(result, Ok(()));
        assert_eq!(observations, COMPANION_OBSERVATIONS);
    }

    #[test]
    fn a_thread_timeout_bounds_locks_and_then_restores() {
        let now = MonoTime::from_micros(0);
        let outer = busy_timeout();
        with_busy_timeout(Duration::from_millis(50), || {
            assert_eq!(busy_timeout(), Duration::from_millis(50));
            assert_eq!(lock_budget(now).remaining(now).as_micros(), 50_000);
        });
        with_busy_timeout(Duration::ZERO, || {
            assert!(lock_budget(now).is_nonblocking());
        });
        assert_eq!(busy_timeout(), outer);
    }

    // Hold the writer until the contender has begun and is still waiting.
    #[test]
    fn a_contended_store_waits_for_the_writer_instead_of_failing()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::env::temp_dir().canonicalize()?.join(format!(
            "provenant-sqlite-busy-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&temp)?;
        let path = temp.join("store.sqlite3");
        std::fs::File::create(&path)?;
        let writer = open_connection(&path, false)?;
        enable_wal(&writer).map_err(|_| "wal")?;
        writer.execute_batch(
            "CREATE TABLE t (v INTEGER); BEGIN IMMEDIATE; INSERT INTO t VALUES (1);",
        )?;
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let contender_path = path.clone();
        let worker = std::thread::spawn(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                let contender = open_connection(&contender_path, false)?;
                started_tx.send(())?;
                let result =
                    contender.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (2); COMMIT;");
                done_tx.send(result)?;
                Ok(())
            },
        );
        started_rx.recv()?;
        let blocked = matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        writer.execute_batch("COMMIT")?;
        let result = done_rx.recv_timeout(Duration::from_secs(5));
        worker
            .join()
            .map_err(|_| "contender thread")?
            .map_err(|error| error.to_string())?;
        assert!(blocked, "contender returned while the write lock was held");
        result??;
        let contender = open_connection(&path, false)?;
        let rows: i64 = contender.query_row("SELECT count(*) FROM t", [], |row| row.get(0))?;
        assert_eq!(rows, 2);
        std::fs::remove_dir_all(&temp)?;
        Ok(())
    }

    #[test]
    fn a_store_held_past_the_timeout_reports_busy() -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::env::temp_dir().canonicalize()?.join(format!(
            "provenant-sqlite-held-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&temp)?;
        let path = temp.join("store.sqlite3");
        std::fs::File::create(&path)?;
        let writer = open_connection(&path, false)?;
        enable_wal(&writer).map_err(|_| "wal")?;
        writer.execute_batch("CREATE TABLE t (v INTEGER); BEGIN IMMEDIATE;")?;
        let contender =
            with_busy_timeout(Duration::from_millis(20), || open_connection(&path, false))?;
        let error = contender.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert!(is_busy(&error));
        writer.execute_batch("COMMIT")?;
        std::fs::remove_dir_all(&temp)?;
        Ok(())
    }

    const SCHEMA: Schema = Schema {
        sql: "CREATE TABLE t (v INTEGER) STRICT;\nCREATE INDEX t_v ON t(v);\nPRAGMA user_version=3;",
        version: 3,
    };

    #[test]
    fn a_fresh_store_is_created_from_the_schema_file() -> Result<(), Box<dyn std::error::Error>> {
        let db = Connection::open_in_memory()?;
        ensure_schema(&db, SCHEMA)?;
        ensure_schema(&db, SCHEMA)?;
        assert_eq!(stored_version(&db)?, 3);
        assert!(schema_matches(&db, SCHEMA)?);
        db.execute_batch("CREATE TABLE extra (v INTEGER)")?;
        assert!(!schema_matches(&db, SCHEMA)?);
        Ok(())
    }

    #[test]
    fn a_store_at_another_version_is_refused_unchanged() -> Result<(), Box<dyn std::error::Error>> {
        let temp = std::env::temp_dir().canonicalize()?.join(format!(
            "provenant-sqlite-version-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&temp)?;
        for (name, version) in [("older", 2), ("newer", 4), ("unversioned", 0)] {
            let path = temp.join(name);
            let db = Connection::open(&path)?;
            db.execute_batch(&format!(
                "CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1); PRAGMA user_version={version};"
            ))?;
            drop(db);
            let before = std::fs::read(&path)?;
            let db = open_connection(&path, false)?;
            assert!(matches!(
                ensure_schema(&db, SCHEMA),
                Err(VersionError::Refused { found, expected: 3 }) if found == version
            ));
            drop(db);
            assert_eq!(std::fs::read(&path)?, before, "{name}");
        }
        std::fs::remove_dir_all(&temp)?;
        Ok(())
    }
}
