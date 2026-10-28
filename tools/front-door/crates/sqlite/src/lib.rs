use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
#[allow(unsafe_code)]
mod read_only_vfs;
use provenant_os::fs::{Directory, FileKind, FsError, Metadata, verify_directory_identity};
use rusqlite::{Connection, OpenFlags};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    time::{Duration, Instant},
};

pub trait RetryWait {
    fn wait(&self, delay: DurationMicros, cancel: &CancelToken);
}

pub struct ThreadWait;

impl RetryWait for ThreadWait {
    // SQLite retry waiting must observe cancellation against real elapsed time.
    #[allow(clippy::disallowed_methods)]
    fn wait(&self, delay: DurationMicros, cancel: &CancelToken) {
        let started = Instant::now();
        let delay = Duration::from_micros(delay.as_micros().min(250_000));
        while !cancel.is_cancelled() {
            let remaining = delay.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            std::thread::park_timeout(remaining.min(Duration::from_millis(10)));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RetryError<E> {
    Busy,
    Cancelled,
    Action(E),
}

/// Retry only the errors selected by `is_busy`, within the caller's budget.
/// The shared capped exponential schedule: 1 ms initially, capped at 250 ms.
pub fn retry<T, E>(
    clock: &dyn Clock,
    budget: WaitBudget,
    cancel: &CancelToken,
    wait: &dyn RetryWait,
    mut is_busy: impl FnMut(&E) -> bool,
    mut action: impl FnMut() -> Result<T, E>,
) -> Result<T, RetryError<E>> {
    observe(clock, budget, cancel, wait, || match action() {
        Ok(value) => Ok(Some(value)),
        Err(error) if is_busy(&error) => Ok(None),
        Err(error) => Err(error),
    })
}

/// Retry incomplete observations on the same schedule as `retry`.
/// The real elapsed cap also bounds a frozen or inaccurate injected clock.
/// An explicit nonblocking budget makes one observation without retrying.
// An injected clock cannot replace the hard wall-time safety cap.
#[allow(clippy::disallowed_methods)]
pub fn observe<T, E>(
    clock: &dyn Clock,
    budget: WaitBudget,
    cancel: &CancelToken,
    wait: &dyn RetryWait,
    mut action: impl FnMut() -> Result<Option<T>, E>,
) -> Result<T, RetryError<E>> {
    if budget.is_nonblocking() {
        if cancel.is_cancelled() {
            return Err(RetryError::Cancelled);
        }
        return action()
            .map_err(RetryError::Action)?
            .ok_or(RetryError::Busy);
    }
    let started = Instant::now();
    let cap = Duration::from_micros(budget.remaining(clock.mono_now()).as_micros());
    let mut delay = 1_000_u64;
    loop {
        if cancel.is_cancelled() {
            return Err(RetryError::Cancelled);
        }
        let remaining = Duration::from_micros(budget.remaining(clock.mono_now()).as_micros())
            .min(cap.saturating_sub(started.elapsed()));
        if remaining.is_zero() {
            return Err(RetryError::Busy);
        }
        match action() {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {
                let micros = u64::try_from(remaining.as_micros())
                    .unwrap_or(u64::MAX)
                    .min(delay);
                wait.wait(DurationMicros::from_micros(micros), cancel);
                delay = delay.saturating_mul(2).min(250_000);
            }
            Err(error) => return Err(RetryError::Action(error)),
        }
    }
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

/// Return false when the observed inode disappeared so the caller can retry.
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

/// Check each database and companion name through the caller's budgeted
/// observation retry, preserving each caller's error type and path context.
pub fn check_companions<E>(
    directory: &Directory,
    names: &[OsString],
    mut retry: impl FnMut(&mut dyn FnMut() -> Result<Option<()>, E>) -> Result<(), E>,
    mut filesystem_error: impl FnMut(FsError) -> E,
    mut unsafe_name: impl FnMut(&OsStr, CompanionFault) -> E,
) -> Result<(), E> {
    for name in names {
        check_companion(
            || directory.metadata_at(name, false),
            &mut retry,
            &mut filesystem_error,
            |fault| unsafe_name(name, fault),
        )?;
    }
    Ok(())
}

/// Check one companion, retrying an observation when its name vanished after
/// stat. The supplied retry retains the caller's budget and cancellation.
pub fn check_companion<E>(
    mut metadata: impl FnMut() -> Result<Metadata, FsError>,
    mut retry: impl FnMut(&mut dyn FnMut() -> Result<Option<()>, E>) -> Result<(), E>,
    mut filesystem_error: impl FnMut(FsError) -> E,
    mut unsafe_error: impl FnMut(CompanionFault) -> E,
) -> Result<(), E> {
    let mut check = || match metadata() {
        Ok(metadata) => match companion_is_safe(metadata) {
            Ok(true) => Ok(Some(())),
            Ok(false) => Ok(None),
            Err(fault) => Err(unsafe_error(fault)),
        },
        Err(error) if error.is_not_found() => Ok(Some(())),
        Err(error) => Err(filesystem_error(error)),
    };
    retry(&mut check)
}

/// Open a database without following symlinks, with SQLite's internal mutexes
/// disabled because each store is owned by one thread. Busy waits are external.
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
    db.busy_timeout(Duration::ZERO)?;
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
    db.busy_timeout(Duration::ZERO)?;
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
    use provenant_core::{MonoTime, TestClock, WallTime};
    use std::cell::{Cell, RefCell};

    struct AdvancingWait {
        clock: RefCell<TestClock>,
        delays: RefCell<Vec<u64>>,
    }

    impl Clock for AdvancingWait {
        fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
            self.clock.borrow().wall_now()
        }

        fn mono_now(&self) -> MonoTime {
            self.clock.borrow().mono_now()
        }

        fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            self.clock.borrow().wait_until(deadline)
        }
    }

    impl RetryWait for AdvancingWait {
        fn wait(&self, delay: DurationMicros, _: &CancelToken) {
            self.delays.borrow_mut().push(delay.as_micros());
            self.clock.borrow_mut().advance(delay).unwrap();
        }
    }

    #[derive(Default)]
    struct CountingWait(Cell<usize>);

    impl RetryWait for CountingWait {
        fn wait(&self, _: DurationMicros, _: &CancelToken) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn nonblocking_retry_attempts_once_without_waiting() -> Result<(), Box<dyn std::error::Error>> {
        let mut clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let budget = WaitBudget::nonblocking(clock.mono_now());
        clock.advance(DurationMicros::from_micros(10))?;
        for (outcome, expected) in [
            (Ok(42), Ok(42)),
            (Err(true), Err(RetryError::Busy)),
            (Err(false), Err(RetryError::Action(false))),
        ] {
            for cancelled in [false, true] {
                let cancel = CancelToken::new();
                if cancelled {
                    cancel.cancel();
                }
                let wait = CountingWait::default();
                let mut attempts = 0;
                let result = retry(
                    &clock,
                    budget,
                    &cancel,
                    &wait,
                    |busy: &bool| *busy,
                    || {
                        attempts += 1;
                        outcome
                    },
                );
                if cancelled {
                    assert_eq!(result, Err(RetryError::Cancelled));
                } else {
                    assert_eq!(result, expected);
                }
                assert_eq!(attempts, usize::from(!cancelled));
                assert_eq!(wait.0.get(), 0);
            }
        }
        Ok(())
    }

    #[test]
    fn nonblocking_observe_attempts_once_without_waiting() -> Result<(), Box<dyn std::error::Error>>
    {
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        for (outcome, expected) in [
            (Ok(Some(42)), Ok(42)),
            (Ok(None), Err(RetryError::Busy)),
            (Err(false), Err(RetryError::Action(false))),
        ] {
            let wait = CountingWait::default();
            let mut attempts = 0;
            let result = observe(
                &clock,
                WaitBudget::nonblocking(clock.mono_now()),
                &cancel,
                &wait,
                || {
                    attempts += 1;
                    outcome
                },
            );
            assert_eq!(result, expected);
            assert_eq!(attempts, 1);
            assert_eq!(wait.0.get(), 0);
        }
        Ok(())
    }

    #[test]
    fn expired_retry_budget_never_starts_an_action() -> Result<(), Box<dyn std::error::Error>> {
        let mut clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let budget = WaitBudget::cli(clock.mono_now(), 1)?;
        clock.advance(DurationMicros::from_seconds(1)?)?;
        let cancel = CancelToken::new();
        let wait = CountingWait::default();
        let mut attempts = 0;
        let result = retry(
            &clock,
            budget,
            &cancel,
            &wait,
            |busy: &bool| *busy,
            || {
                attempts += 1;
                Ok(42)
            },
        );
        assert_eq!(result, Err(RetryError::Busy));
        assert_eq!(attempts, 0);
        assert_eq!(wait.0.get(), 0);
        Ok(())
    }

    #[test]
    fn retry_retries_busy_errors_and_stops_on_success() -> Result<(), Box<dyn std::error::Error>> {
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let wait = CountingWait::default();
        let budget = WaitBudget::cli(clock.mono_now(), 1)?;
        let mut attempts = 0;
        let result = retry(
            &clock,
            budget,
            &cancel,
            &wait,
            |error: &bool| *error,
            || {
                attempts += 1;
                if attempts < 3 { Err(true) } else { Ok(42) }
            },
        );
        assert_eq!(result, Ok(42));
        assert_eq!(attempts, 3);
        assert_eq!(wait.0.get(), 2);
        Ok(())
    }

    #[test]
    fn retry_exhausts_budget_with_capped_delays() -> Result<(), Box<dyn std::error::Error>> {
        let wait = AdvancingWait {
            clock: RefCell::new(TestClock::new(
                WallTime::from_micros(0)?,
                MonoTime::from_micros(0),
            )),
            delays: RefCell::default(),
        };
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(wait.mono_now(), 1)?;
        let mut attempts = 0;
        let result: Result<(), _> = retry(
            &wait,
            budget,
            &cancel,
            &wait,
            |error: &bool| *error,
            || {
                attempts += 1;
                Err(true)
            },
        );
        assert_eq!(result, Err(RetryError::Busy));
        assert_eq!(attempts, 11);
        assert_eq!(
            *wait.delays.borrow(),
            [
                1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 64_000, 128_000, 250_000, 250_000,
                245_000
            ]
        );
        assert_eq!(budget.remaining(wait.mono_now()).as_micros(), 0);
        Ok(())
    }

    #[test]
    fn retry_stops_on_cancellation() -> Result<(), Box<dyn std::error::Error>> {
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        for already_cancelled in [false, true] {
            let cancel = CancelToken::new();
            let wait = CountingWait::default();
            let budget = WaitBudget::cli(clock.mono_now(), 1)?;
            if already_cancelled {
                cancel.cancel();
            }
            let mut attempts = 0;
            let result: Result<(), _> = retry(
                &clock,
                budget,
                &cancel,
                &wait,
                |error: &bool| *error,
                || {
                    attempts += 1;
                    cancel.cancel();
                    Err(true)
                },
            );
            assert_eq!(result, Err(RetryError::Cancelled));
            assert_eq!(attempts, usize::from(!already_cancelled));
            assert_eq!(wait.0.get(), attempts);
        }
        Ok(())
    }

    #[test]
    fn retry_returns_non_busy_errors_without_waiting() -> Result<(), Box<dyn std::error::Error>> {
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let wait = CountingWait::default();
        let budget = WaitBudget::cli(clock.mono_now(), 1)?;
        let mut attempts = 0;
        let result: Result<(), _> = retry(
            &clock,
            budget,
            &cancel,
            &wait,
            |error: &bool| *error,
            || {
                attempts += 1;
                Err(false)
            },
        );
        assert_eq!(result, Err(RetryError::Action(false)));
        assert_eq!(attempts, 1);
        assert_eq!(wait.0.get(), 0);
        Ok(())
    }

    #[test]
    fn observe_retries_incomplete_observations() -> Result<(), Box<dyn std::error::Error>> {
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let cancel = CancelToken::new();
        let wait = CountingWait::default();
        let budget = WaitBudget::cli(clock.mono_now(), 1)?;
        let mut attempts = 0;
        let result: Result<_, RetryError<()>> = observe(&clock, budget, &cancel, &wait, || {
            attempts += 1;
            Ok((attempts == 3).then_some(42))
        });
        assert_eq!(result, Ok(42));
        assert_eq!(attempts, 3);
        assert_eq!(wait.0.get(), 2);
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
