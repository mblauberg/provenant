use crate::*;
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, StateRoot, WaitBudget};
use provenant_supervisor::fs::{
    Directory, FileKind, FsError, LockGuard, LockMode, Metadata, verify_directory_identity,
};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::{
    ffi::OsStr,
    marker::PhantomData,
    path::{Component, Path},
    rc::Rc,
    time::{Duration, Instant},
};

/// Identity and prompt-read authority verified by the calling edge. This store
/// does not infer identity from paths, the environment or provider records.
#[derive(Clone, Debug)]
pub struct ExecutionContext {
    pub state_root: StateRoot,
    pub project_id: ProjectId,
    pub actor_id: ActorId,
    pub read_prompts: bool,
}
pub struct Request<'a> {
    pub clock: &'a dyn Clock,
    pub budget: WaitBudget,
    pub cancel: &'a CancelToken,
}
impl Request<'_> {
    pub(crate) fn retry<T>(&self, mut action: impl FnMut() -> Result<T>) -> Result<T> {
        self.wait(|| match action() {
            Err(RunError::Sqlite(rusqlite::Error::SqliteFailure(code, _)))
                if matches!(
                    code.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                Ok(None)
            }
            result => result.map(Some),
        })
    }
    /// Retry an incomplete observation within the call's budget and cancellation.
    fn wait<T>(&self, mut action: impl FnMut() -> Result<Option<T>>) -> Result<T> {
        let started = Instant::now();
        let cap = Duration::from_micros(self.budget.remaining(self.clock.mono_now()).as_micros());
        let mut delay = Duration::from_millis(1);
        loop {
            if self.cancel.is_cancelled() {
                return Err(RunError::Cancelled);
            }
            let remaining = cap
                .saturating_sub(started.elapsed())
                .min(Duration::from_micros(
                    self.budget.remaining(self.clock.mono_now()).as_micros(),
                ));
            if remaining.is_zero() {
                return Err(RunError::Contention);
            }
            match action() {
                Ok(None) => {
                    std::thread::park_timeout(delay.min(remaining));
                    delay = (delay * 2).min(Duration::from_millis(10));
                }
                Ok(Some(value)) => return Ok(value),
                Err(error) => return Err(error),
            }
        }
    }
}
/// One connection, one owning thread. Call budgets are supplied afresh and no
/// transaction survives a call or includes filesystem publication.
pub struct RunStore {
    pub(crate) db: Connection,
    pub(crate) root: Directory,
    pub(crate) context: ExecutionContext,
    pub(crate) writer_registry: std::path::PathBuf,
    pub(crate) policy: crate::admission::AdmissionPolicy,
    _owner: PhantomData<Rc<()>>,
}
impl RunStore {
    /// The trusted host config root owns worktree registrations and the host
    /// admission ledger even when this store contains nested runs. Supplied by
    /// the host composition edge.
    pub fn with_writer_registry(mut self, state: &std::path::Path) -> Self {
        self.writer_registry = state.to_owned();
        self
    }
    /// Opens an existing, initialised store without creating or changing files.
    /// A missing store or schema is an empty read; unsupported versions fail.
    pub fn open_read_only(
        context: &ExecutionContext,
        request: &Request<'_>,
    ) -> Result<Option<Self>> {
        request.retry(|| Ok(()))?;
        let mut root = Directory::root(true)?;
        for component in context.state_root.as_path().components() {
            if let Component::Normal(name) = component {
                let opened = match root.open_directory(name) {
                    Ok(opened) => opened,
                    Err(error) if error.is_not_found() => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                root = opened;
            }
        }
        private(&root)?;
        let root = root.into_anchor()?;
        match root.metadata_at(OsStr::new("run.sqlite3"), false) {
            Ok(_) => {}
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        companions(&root, "run.sqlite3", request)?;
        let db = Connection::open_with_flags(
            context.state_root.as_path().join("run.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        if request.retry(|| version(&db))? == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            db,
            root,
            context: context.clone(),
            writer_registry: context.state_root.as_path().to_owned(),
            policy: Default::default(),
            _owner: PhantomData,
        }))
    }

    pub fn open(context: &ExecutionContext, request: &Request<'_>) -> Result<Self> {
        request.retry(|| Ok(()))?;
        let (root, db, _initialisation) =
            open_database(context.state_root.as_path(), "run.sqlite3", request)?;
        let schema_version = request.retry(|| version(&db))?;
        let store = Self {
            db,
            root,
            context: context.clone(),
            writer_registry: context.state_root.as_path().to_owned(),
            policy: Default::default(),
            _owner: PhantomData,
        };
        if schema_version < 3 {
            store.write(request, |tx| {
                if version(tx)? == 0 {
                    tx.execute_batch(include_str!("schema.sql"))?;
                }
                if version(tx)? == 1 {
                    tx.execute_batch(include_str!("schema_v2.sql"))?;
                }
                if version(tx)? == 2 {
                    tx.execute_batch(include_str!("schema_v3.sql"))?;
                }
                Ok(())
            })?;
        }
        companions(&store.root, "run.sqlite3", request)?;
        Ok(store)
    }
    pub(crate) fn write<T>(
        &self,
        request: &Request<'_>,
        action: impl Fn(&Connection) -> Result<T>,
    ) -> Result<T> {
        immediate(&self.db, request, action)
    }
}
/// One immediate transaction, retried while SQLite reports the file busy.
pub(crate) fn immediate<T>(
    db: &Connection,
    request: &Request<'_>,
    action: impl Fn(&Connection) -> Result<T>,
) -> Result<T> {
    request.retry(|| {
        let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
        let value = action(&tx)?;
        tx.commit()?;
        Ok(value)
    })
}
/// Create the private directory chain and database file, then open it in WAL
/// mode with full synchronisation. Every component is identity-checked.
/// The caller must hold the returned lock through schema initialisation.
pub(crate) fn open_database(
    path: &Path,
    name: &str,
    request: &Request<'_>,
) -> Result<(Directory, Connection, LockGuard)> {
    let mut root = Directory::root(true)?;
    for component in path.components() {
        if let Component::Normal(part) = component {
            match root.metadata_at(part, false) {
                Ok(_) => {}
                Err(error) if error.is_not_found() => match root.mkdir(part, 0o700) {
                    Ok(()) => root.sync()?,
                    Err(error) if error.is_exists() => {}
                    Err(error) => return Err(error.into()),
                },
                Err(error) => return Err(error.into()),
            }
            let before = root.metadata_at(part, false)?;
            let opened = root.open_directory(part)?;
            verify_directory_identity(
                part,
                before,
                opened.metadata()?,
                root.metadata_at(part, false)?,
                opened.metadata()?.device,
            )?;
            root = opened;
        }
    }
    private(&root)?;
    let root = root.into_anchor()?;
    // SQLite serialises transactions, but first-file and WAL setup precede them.
    let lock_name = format!("{name}.lock");
    // Concurrent non-exclusive O_CREAT opens can fail with ENOENT on macOS.
    // Establish the stable inode with exclusive creation before opening the lock.
    match root.create_new(OsStr::new(&lock_name)) {
        Ok(_) => {}
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let initialisation = root
        .lock(
            OsStr::new(&lock_name),
            LockMode::Exclusive,
            request.budget,
            request.cancel,
            request.clock,
        )
        .map_err(|error| match error {
            FsError::LockTimeout { .. } => RunError::Contention,
            FsError::Cancelled { .. } => RunError::Cancelled,
            error => error.into(),
        })?;
    match root.create_new(OsStr::new(name)) {
        Ok(file) => {
            file.sync()?;
            root.sync()?;
        }
        Err(e) if e.is_exists() => {}
        Err(e) => return Err(e.into()),
    }
    companions(&root, name, request)?;
    let db = Connection::open_with_flags(
        path.join(name),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::ZERO)?;
    request.retry(|| {
        let mode: String =
            db.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(RunError::Validation("WAL unavailable"));
        }
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        Ok(())
    })?;
    Ok((root, db, initialisation))
}
fn version(db: &Connection) -> Result<i64> {
    let (version, has_schema): (i64, bool) = db.query_row("SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_schema)", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if !(0..=3).contains(&version) || (version == 0 && has_schema) {
        return Err(RunError::Version(version));
    }
    Ok(version)
}
pub(crate) fn private(directory: &Directory) -> Result<()> {
    let m = directory.metadata()?;
    if m.mode & 0o077 != 0 || m.uid != provenant_supervisor::fs::effective_uid() {
        return Err(RunError::UnsafePath);
    }
    Ok(())
}
fn companions(directory: &Directory, base: &str, request: &Request<'_>) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        companion(request, || {
            directory.metadata_at(OsStr::new(&format!("{base}{suffix}")), false)
        })?;
    }
    Ok(())
}
fn companion(
    request: &Request<'_>,
    mut metadata: impl FnMut() -> provenant_supervisor::fs::Result<Metadata>,
) -> Result<()> {
    request.wait(|| match metadata() {
        Ok(m)
            if m.kind == FileKind::Regular
                && m.mode & 0o077 == 0
                && m.uid == provenant_supervisor::fs::effective_uid() =>
        {
            match m.links {
                1 => Ok(Some(())),
                // SQLite's last connection can unlink WAL/SHM files while
                // another opener's stat is in flight, outside our setup
                // lock. Re-stat the name; never accept the removed inode.
                0 => Ok(None),
                _ => Err(RunError::UnsafePath),
            }
        }
        Ok(_) => Err(RunError::UnsafePath),
        Err(e) if e.is_not_found() => Ok(Some(())),
        Err(e) => Err(e.into()),
    })
}
pub(crate) fn bytes<T: serde::Serialize>(record: &T) -> Result<Vec<u8>> {
    let bytes = provenant_records::CanonicalV1::bytes(record)?;
    if bytes.as_bytes().len() as u64 > RECORD_LIMIT {
        return Err(RunError::TooLarge {
            limit: RECORD_LIMIT,
        });
    }
    Ok(bytes.as_bytes().to_vec())
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(provenant_records::decode_v1(
        bytes,
        provenant_records::Limits {
            max_bytes: RECORD_LIMIT,
            ..Default::default()
        },
    )?)
}

pub(crate) fn sql_int(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| RunError::Validation("integer exceeds SQLite range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn companion_rechecks_unlinked_observations() -> TestResult {
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let request = crate::tests::call(&clock, &cancel);
        for replaced in [false, true] {
            let temp = tempfile::tempdir()?;
            let directory = Directory::open_anchor(temp.path())?;
            let name = OsStr::new("run.sqlite3-shm");
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
            companion(&request, || {
                observations += 1;
                if observations == 1 {
                    Ok(removed)
                } else {
                    directory.metadata_at(name, false)
                }
            })?;
            assert_eq!(observations, 2);
        }
        Ok(())
    }

    #[test]
    fn companion_refuses_unsafe_metadata_after_an_unlink() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        let safe = directory
            .create_new(OsStr::new("run.sqlite3-wal"))?
            .metadata()?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let request = crate::tests::call(&clock, &cancel);
        for unsafe_metadata in [
            Metadata { links: 2, ..safe },
            Metadata {
                kind: FileKind::Symlink,
                ..safe
            },
            Metadata {
                kind: FileKind::Directory,
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
            let result = companion(&request, || {
                observations += 1;
                Ok(if observations == 1 {
                    Metadata { links: 0, ..safe }
                } else {
                    unsafe_metadata
                })
            });
            assert!(matches!(result, Err(RunError::UnsafePath)));
            assert_eq!(observations, 2);
        }
        Ok(())
    }

    #[test]
    fn companion_unlink_retry_preserves_wait_errors() -> TestResult {
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        let removed = Metadata {
            links: 0,
            ..directory
                .create_new(OsStr::new("run.sqlite3-shm"))?
                .metadata()?
        };
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let request = crate::tests::call(&clock, &cancel);
        assert!(matches!(
            companion(&request, || {
                cancel.cancel();
                Ok(removed)
            }),
            Err(RunError::Cancelled)
        ));

        let cancel = CancelToken::new();
        let request = Request {
            // Only the final microsecond remains when the retry starts.
            budget: WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1)?,
            clock: &provenant_core::TestClock::new(
                clock.wall_now()?,
                provenant_core::MonoTime::from_micros(999_999),
            ),
            cancel: &cancel,
        };
        assert!(matches!(
            companion(&request, || Ok(removed)),
            Err(RunError::Contention)
        ));
        Ok(())
    }
}
