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

#[derive(Debug)]
pub enum VersionError {
    Unsupported(i64),
    Unversioned,
    Sqlite(rusqlite::Error),
}

/// Read the user_version and reject schemas that have no version marker.
pub fn read_version(db: &Connection, maximum: usize) -> Result<usize, VersionError> {
    let (version, has_schema): (i64, bool) = db
        .query_row(
            "SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_schema)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(VersionError::Sqlite)?;
    let version = usize::try_from(version)
        .ok()
        .filter(|version| *version <= maximum)
        .ok_or(VersionError::Unsupported(version))?;
    if version == 0 && has_schema {
        return Err(VersionError::Unversioned);
    }
    Ok(version)
}

/// Apply ordered SQL steps on the connection supplied by the caller.
/// Transaction boundaries stay with the opener: Fabric applies one step per
/// transaction, while run applies its whole upgrade in one transaction.
pub fn apply_migrations(
    db: &Connection,
    steps: &[&str],
    start: usize,
    mut after_step: impl FnMut(usize, &Connection) -> rusqlite::Result<()>,
) -> rusqlite::Result<()> {
    for (index, sql) in steps.iter().enumerate().skip(start) {
        db.execute_batch(sql)?;
        after_step(index, db)?;
        db.pragma_update(None, "user_version", (index + 1) as i64)?;
    }
    Ok(())
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

    // The writer holds its lock across a real-time sleep to make the reader wait.
    #[allow(clippy::disallowed_methods)]
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
        let contender = open_connection(&path, false)?;
        let started = std::time::Instant::now();
        let released = std::thread::spawn(move || -> rusqlite::Result<()> {
            std::thread::sleep(Duration::from_millis(300));
            writer.execute_batch("COMMIT")
        });
        contender.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (2); COMMIT;")?;
        assert!(started.elapsed() >= Duration::from_millis(250));
        released.join().map_err(|_| "writer thread")??;
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

    #[test]
    fn migration_runner_applies_ordered_steps_and_versions() -> rusqlite::Result<()> {
        let db = Connection::open_in_memory()?;
        let steps = [
            "CREATE TABLE first (id INTEGER);",
            "CREATE TABLE second (id INTEGER);",
        ];
        apply_migrations(&db, &steps, 0, |_, _| Ok(()))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let tables: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('first','second')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 2);
        assert_eq!(tables, 2);
        Ok(())
    }

    #[test]
    fn migration_runner_resumes_and_calls_after_step() -> rusqlite::Result<()> {
        let db = Connection::open_in_memory()?;
        let steps = [
            "CREATE TABLE first (id INTEGER);",
            "CREATE TABLE second (id INTEGER);",
        ];
        apply_migrations(&db, &steps[..1], 0, |_, _| Ok(()))?;
        let mut applied = Vec::new();
        apply_migrations(&db, &steps, 1, |index, db| {
            applied.push(index);
            db.execute("INSERT INTO second VALUES (42)", [])?;
            Ok(())
        })?;
        apply_migrations(&db, &steps, steps.len(), |_, _| panic!("already migrated"))?;
        assert_eq!(applied, [1]);
        assert_eq!(read_version(&db, steps.len()).unwrap(), 2);
        assert_eq!(
            db.query_row("SELECT id FROM second", [], |row| row.get::<_, i64>(0))?,
            42
        );
        Ok(())
    }
}
