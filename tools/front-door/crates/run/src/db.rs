use crate::*;
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, StateRoot, WaitBudget};
use provenant_supervisor::fs::{Directory, FileKind, verify_directory_identity};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::{
    ffi::OsStr,
    marker::PhantomData,
    path::Component,
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
                Err(RunError::Sqlite(rusqlite::Error::SqliteFailure(code, _)))
                    if matches!(
                        code.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) =>
                {
                    std::thread::park_timeout(delay.min(remaining));
                    delay = (delay * 2).min(Duration::from_millis(10));
                }
                result => return result,
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
    pub(crate) policy: crate::admission::AdmissionPolicy,
    _owner: PhantomData<Rc<()>>,
}
impl RunStore {
    pub fn open(context: &ExecutionContext, request: &Request<'_>) -> Result<Self> {
        request.retry(|| Ok(()))?;
        let mut root = Directory::root(true)?;
        for component in context.state_root.as_path().components() {
            if let Component::Normal(name) = component {
                match root.mkdir(name, 0o700) {
                    Ok(()) => root.sync()?,
                    Err(e) if e.is_exists() => {}
                    Err(e) => return Err(e.into()),
                }
                let before = root.metadata_at(name, false)?;
                let opened = root.open_directory(name)?;
                verify_directory_identity(
                    name,
                    before,
                    opened.metadata()?,
                    root.metadata_at(name, false)?,
                    opened.metadata()?.device,
                )?;
                root = opened;
            }
        }
        private(&root)?;
        let root = root.into_anchor()?;
        match root.create_new(OsStr::new("run.sqlite3")) {
            Ok(file) => {
                file.sync()?;
                root.sync()?;
            }
            Err(e) if e.is_exists() => {}
            Err(e) => return Err(e.into()),
        }
        companions(&root)?;
        let db = Connection::open_with_flags(
            context.state_root.as_path().join("run.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        request.retry(|| version(&db))?;
        request.retry(|| {
            let mode: String =
                db.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                return Err(RunError::Validation("WAL unavailable"));
            }
            db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
            Ok(())
        })?;
        let store = Self {
            db,
            root,
            context: context.clone(),
            policy: Default::default(),
            _owner: PhantomData,
        };
        store.write(request, |tx| {
            if version(tx)? == 0 {
                tx.execute_batch(include_str!("schema.sql"))?;
            }
            Ok(())
        })?;
        companions(&store.root)?;
        Ok(store)
    }
    pub(crate) fn write<T>(
        &self,
        request: &Request<'_>,
        action: impl Fn(&Connection) -> Result<T>,
    ) -> Result<T> {
        request.retry(|| {
            let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
            let value = action(&tx)?;
            tx.commit()?;
            Ok(value)
        })
    }
}
fn version(db: &Connection) -> Result<i64> {
    let (version, has_schema): (i64, bool) = db.query_row("SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_schema)", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    if !(0..=1).contains(&version) || (version == 0 && has_schema) {
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
fn companions(directory: &Directory) -> Result<()> {
    for name in [
        "run.sqlite3",
        "run.sqlite3-wal",
        "run.sqlite3-shm",
        "run.sqlite3-journal",
    ] {
        match directory.metadata_at(OsStr::new(name), false) {
            Ok(m)
                if m.kind == FileKind::Regular
                    && m.links == 1
                    && m.mode & 0o077 == 0
                    && m.uid == provenant_supervisor::fs::effective_uid() => {}
            Ok(_) => return Err(RunError::UnsafePath),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
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
