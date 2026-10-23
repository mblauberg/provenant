use crate::*;
use provenant_core::{CancelToken, Clock, DurationMicros, StateRoot, WaitBudget};
use provenant_supervisor::fs::{Directory, FileKind};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::{
    ffi::OsStr,
    marker::PhantomData,
    path::Component,
    rc::Rc,
    time::{Duration, Instant},
};

// Index + 1 is the schema version; each migration commits independently.
pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("migrations/002-import-skips.sql"),
    include_str!("migrations/003-claims-landing.sql"),
];
fn schema_version(db: &Connection) -> Result<usize> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let version = usize::try_from(version)
        .ok()
        .filter(|v| *v <= MIGRATIONS.len())
        .ok_or(Error::SchemaVersion(version))?;
    if version == 0
        && db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |r| {
            r.get::<_, bool>(0)
        })?
    {
        return Err(Error::UnversionedDatabase);
    }
    Ok(version)
}
fn enable_wal(db: &Connection) -> Result<()> {
    let mode: String = db.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::JournalMode(mode));
    }
    db.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    Ok(())
}

/// The only blocking seam: called after rollback, with a bounded delay.
pub trait RetryWait {
    fn wait(&self, delay: DurationMicros, cancel: &CancelToken);
}
pub struct ThreadWait;
impl RetryWait for ThreadWait {
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
pub struct Request<'a> {
    pub clock: &'a dyn Clock,
    pub budget: WaitBudget,
    pub cancel: &'a CancelToken,
    pub backoff: &'a dyn RetryWait,
}
impl Request<'_> {
    pub(crate) fn retry<T>(&self, mut action: impl FnMut() -> Result<T>) -> Result<T> {
        // The real elapsed cap also bounds a frozen/inaccurate injected clock.
        let started = Instant::now();
        let cap = Duration::from_micros(self.budget.remaining(self.clock.mono_now()).as_micros());
        let mut delay = 1_000_u64;
        loop {
            if self.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let remaining =
                Duration::from_micros(self.budget.remaining(self.clock.mono_now()).as_micros())
                    .min(cap.saturating_sub(started.elapsed()));
            if remaining.is_zero() {
                return Err(Error::Busy);
            }
            match action() {
                Err(Error::Sqlite(rusqlite::Error::SqliteFailure(code, _)))
                    if matches!(
                        code.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) =>
                {
                    let micros = u64::try_from(remaining.as_micros())
                        .unwrap_or(u64::MAX)
                        .min(delay);
                    self.backoff
                        .wait(DurationMicros::from_micros(micros), self.cancel);
                    delay = delay.saturating_mul(2).min(250_000);
                }
                result => return result,
            }
        }
    }
}

pub struct Store {
    pub(crate) db: Connection,
    pub(crate) locks: Directory,
    // SQLite supports movable connections, but Fabric requires worker ownership.
    _owner: PhantomData<Rc<()>>,
}
impl Store {
    pub fn open(root: &StateRoot, request: &Request<'_>) -> Result<Self> {
        request.retry(|| Ok(()))?;
        let mut directory = Directory::root(true)?;
        for part in root.as_path().components() {
            if let Component::Normal(name) = part {
                match directory.mkdir(name, 0o700) {
                    Ok(()) => directory.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                }
                directory = checked_directory(&directory, name, None)?;
            }
        }
        private(&directory, root.as_path())?;
        match directory.mkdir(OsStr::new("fabric"), 0o700) {
            Ok(()) => directory.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        let fabric = checked_directory(
            &directory,
            OsStr::new("fabric"),
            Some(directory.metadata()?.device),
        )?;
        let path = root.as_path().join("fabric");
        private(&fabric, &path)?;
        let db_path = path.join("fabric.sqlite3");
        match fabric.create_new(OsStr::new("fabric.sqlite3")) {
            Ok(file) => {
                file.sync()?;
                fabric.sync()?;
            }
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        companions(&fabric, &path)?;
        let db = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        // Refuse future versions before journal-mode changes or schema writes.
        request.retry(|| schema_version(&db))?;
        request.retry(|| enable_wal(&db))?;
        match fabric.mkdir(OsStr::new("locks"), 0o700) {
            Ok(()) => fabric.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        let locks = checked_directory(
            &fabric,
            OsStr::new("locks"),
            Some(fabric.metadata()?.device),
        )?;
        private(&locks, &path.join("locks"))?;
        let store = Self {
            db,
            locks,
            _owner: PhantomData,
        };
        loop {
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
        companions(&fabric, &path)?;
        Ok(store)
    }
    pub(crate) fn write<T>(
        &self,
        request: &Request<'_>,
        action: impl Fn(&Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
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
    if directory.metadata()?.mode & 0o077 != 0 {
        return Err(Error::UnsafePath(path.into()));
    }
    Ok(())
}
fn companions(directory: &Directory, path: &std::path::Path) -> Result<()> {
    for name in ["fabric.sqlite3", "fabric.sqlite3-wal", "fabric.sqlite3-shm"] {
        match directory.metadata_at(OsStr::new(name), false) {
            Ok(meta)
                if meta.kind == FileKind::Regular && meta.links == 1 && meta.mode & 0o077 == 0 => {}
            Ok(_) => return Err(Error::UnsafePath(path.join(name))),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

// The configured root may live on another local volume than `/`. Once there,
// Fabric descendants stay on that volume. Every component remains no-follow
// with the supervisor's stat/open/stat identity verification.
fn checked_directory(parent: &Directory, name: &OsStr, device: Option<u64>) -> Result<Directory> {
    let before = parent.metadata_at(name, false)?;
    let child = parent.open_directory(name)?;
    let opened = child.metadata()?;
    let after = parent.metadata_at(name, false)?;
    provenant_supervisor::fs::verify_directory_identity(
        name,
        before,
        opened,
        after,
        device.unwrap_or(opened.device),
    )?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_a_vfs_that_cannot_enable_wal() {
        let db = Connection::open_in_memory().unwrap();
        assert!(matches!(enable_wal(&db), Err(Error::JournalMode(mode)) if mode == "memory"));
    }
}
