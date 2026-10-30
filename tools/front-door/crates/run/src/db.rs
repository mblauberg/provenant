use crate::*;
use provenant_core::{ActorId, CancelToken, Clock, ProjectId, StateRoot, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, Metadata};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::{
    ffi::OsStr,
    marker::PhantomData,
    path::{Component, Path},
    rc::Rc,
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
/// How long a launch waits for its guardian to acknowledge custody.
const GUARDIAN_HANDSHAKE_MICROS: u64 = 30_000_000;
pub struct Request<'a> {
    pub clock: &'a dyn Clock,
    pub cancel: &'a CancelToken,
}
impl Request<'_> {
    /// The wait for a run-store advisory lock or custody proof that starts
    /// now: the same allowance as SQLite's busy timeout.
    pub fn lock_budget(&self) -> WaitBudget {
        provenant_sqlite::lock_budget(self.clock.mono_now())
    }
    /// The wait for a guardian's launch handshake that starts now. It is not
    /// store contention, so the store busy timeout never shortens it.
    pub fn handshake_budget(&self) -> WaitBudget {
        WaitBudget::up_to(
            self.clock.mono_now(),
            provenant_core::DurationMicros::from_micros(GUARDIAN_HANDSHAKE_MICROS),
        )
    }
    pub(crate) fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        Ok(())
    }
}
// Leave room below macOS's common 256-descriptor soft limit for SQLite,
// publication, guardian launch and the one-descriptor custody probe. Reclaiming
// exited guardians removes the lifetime limit without raising live FD pressure.
// A small unit-test cap exercises reclamation without many filesystem syncs.
pub(crate) const HELD_CHILD_LIMIT: usize = if cfg!(test) { 2 } else { 64 };

/// Keep the scope after supersession or guardian exit for owner checks.
pub(crate) struct Held {
    pub(crate) scope: provenant_supervisor::custody::Scope,
    pub(crate) handle: Option<provenant_supervisor::custody::CustodyHandle>,
    custody: Metadata,
}

/// One connection, one owning thread. No transaction survives a call or
/// includes filesystem publication.
pub struct RunStore {
    pub(crate) db: Connection,
    pub(crate) root: Directory,
    pub(crate) context: ExecutionContext,
    pub(crate) writer_registry: std::path::PathBuf,
    /// Canonical host home, used to refuse child roots that entry and compile
    /// must both reject. Absent on stores that never validate children.
    pub(crate) host_home: Option<std::path::PathBuf>,
    pub(crate) policy: crate::admission::AdmissionPolicy,
    pub(crate) sealed_requester: Option<ActorId>,
    pub(crate) sealed_parent_network: Option<NetworkAccess>,
    /// This host's ID, stamped on new attempts when the edge supplies it.
    pub(crate) host: Option<provenant_core::HostId>,
    /// Scopes and bounded custody handles this host view handed to a guardian.
    pub(crate) launched: std::cell::RefCell<Vec<Held>>,
    /// Host-only launch claims survive deletion or rewriting of lane rows.
    pub(crate) launched_tasks: std::cell::RefCell<
        std::collections::BTreeSet<(provenant_core::RunId, provenant_core::TaskId)>,
    >,
    _owner: PhantomData<Rc<()>>,
}
impl RunStore {
    pub(crate) fn check_held_capacity(&self, id: provenant_core::AttemptId) -> Result<()> {
        let mut launched = self.launched.borrow_mut();
        let active = || {
            launched
                .iter()
                .filter(|held| held.handle.is_some() && held.scope.attempt != id)
                .count()
        };
        if active() < HELD_CHILD_LIMIT {
            return Ok(());
        }
        for held in launched.iter_mut() {
            if held
                .handle
                .as_ref()
                .is_some_and(|handle| handle.custody_released(&held.custody))
            {
                held.handle = None;
            }
        }
        if launched
            .iter()
            .filter(|held| held.handle.is_some() && held.scope.attempt != id)
            .count()
            >= HELD_CHILD_LIMIT
        {
            return Err(RunError::Validation(
                "held child custody limit reached; earlier children are still running",
            ));
        }
        Ok(())
    }

    pub(crate) fn hold_child(
        &self,
        handle: provenant_supervisor::custody::CustodyHandle,
        custody: Metadata,
    ) {
        let scope = handle.scope();
        let mut launched = self.launched.borrow_mut();
        for held in launched
            .iter_mut()
            .filter(|held| held.scope.attempt == scope.attempt)
        {
            held.handle = None;
        }
        launched.push(Held {
            scope,
            handle: Some(handle),
            custody,
        });
    }

    /// The host's view of a child store, opened from the parent attempt's
    /// sealed holder context. The lane can write this store and its attempt
    /// summaries, so launch and the started attempt's owner use this requester
    /// instead of any actor they name. The view refuses a run or attempt it
    /// launched once its record names another owner, and stops each child
    /// from the scope and directory it held at hand-off, never from the record.
    pub fn with_sealed_requester(mut self, requester: ActorId) -> Self {
        self.sealed_requester = Some(requester);
        self
    }
    /// Network bound verified from the live parent holder's sealed task.
    pub fn with_sealed_parent_network(mut self, network: NetworkAccess) -> Self {
        self.sealed_parent_network = Some(network);
        self
    }
    /// Stamp new attempts and their events with this host. Supplied by the host
    /// composition edge; absent, the host stays unknown in the trace.
    pub fn with_host(mut self, host: provenant_core::HostId) -> Self {
        self.host = Some(host);
        self
    }
    /// The trusted host config root owns worktree registrations and the host
    /// admission ledger even when this store contains nested runs. Supplied by
    /// the host composition edge.
    pub fn with_writer_registry(mut self, state: &std::path::Path) -> Self {
        self.writer_registry = state.to_owned();
        self
    }
    /// The host's canonical home, wherever it lives. Child entry checks use it
    /// so they refuse a root that encloses it before compile would.
    pub fn with_host_home(mut self, home: &std::path::Path) -> Self {
        self.host_home = Some(home.to_owned());
        self
    }
    /// The host state root whose admission ledger and registrations this store
    /// reads, which is not its own directory inside a nested lane.
    pub fn writer_registry(&self) -> &std::path::Path {
        &self.writer_registry
    }
    /// Opens an existing, initialised store without changing durable state.
    /// SQLite may create or update WAL/shared-memory companions.
    /// A missing store or schema is an empty read; unsupported versions fail.
    pub fn open_read_only(
        context: &ExecutionContext,
        request: &Request<'_>,
    ) -> Result<Option<Self>> {
        Self::open_read_only_mode(context, request, false)
    }

    /// Inspect an existing store, including committed WAL frames. SQLite may
    /// update read marks or create WAL/shared-memory companions of that store.
    /// Never create a database, initialise a schema or recover a journal.
    pub fn open_disk_read_only(
        context: &ExecutionContext,
        request: &Request<'_>,
    ) -> Result<Option<Self>> {
        Self::open_read_only_mode(context, request, true)
    }

    fn open_read_only_mode(
        context: &ExecutionContext,
        request: &Request<'_>,
        disk_report: bool,
    ) -> Result<Option<Self>> {
        request.check()?;
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
        private_root(&root)?;
        let root = root.into_anchor()?;
        match root.metadata_at(OsStr::new("run.sqlite3"), false) {
            Ok(_) => {}
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        companions(&root, "run.sqlite3")?;
        if disk_report {
            match root.metadata_at(OsStr::new("run.sqlite3-journal"), false) {
                Ok(metadata) if metadata.size > 0 => {
                    return Err(RunError::Validation(
                        "run database has rollback journal data; terminal state is unknown in the disk report",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        let path = context.state_root.as_path().join("run.sqlite3");
        let db = provenant_sqlite::open_connection(&path, true)?;
        if version(&db)? == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            db,
            root,
            context: context.clone(),
            writer_registry: context.state_root.as_path().to_owned(),
            host_home: None,
            policy: Default::default(),
            sealed_requester: None,
            sealed_parent_network: None,
            host: None,
            launched: Default::default(),
            launched_tasks: Default::default(),
            _owner: PhantomData,
        }))
    }

    pub fn open(context: &ExecutionContext, request: &Request<'_>) -> Result<Self> {
        request.check()?;
        let (root, db, _initialisation) =
            open_database(context.state_root.as_path(), "run.sqlite3", request)?;
        let schema_version = version(&db)?;
        let store = Self {
            db,
            root,
            context: context.clone(),
            writer_registry: context.state_root.as_path().to_owned(),
            host_home: None,
            policy: Default::default(),
            sealed_requester: None,
            sealed_parent_network: None,
            host: None,
            launched: Default::default(),
            launched_tasks: Default::default(),
            _owner: PhantomData,
        };
        if schema_version < 6 {
            // Read legacy paths once, outside the migration transaction. The
            // initialisation lock serialises migrations. Unreadable paths and
            // in-flight attempts keep the conservative retained default.
            let absent_temporary = if (1..4).contains(&schema_version) {
                store.terminal_without_temporary(request)?
            } else {
                Vec::new()
            };
            store.write(request, |tx| {
                let current = version(tx)?;
                let start = usize::try_from(current).map_err(|_| RunError::Version(current))?;
                let steps = [
                    include_str!("schema.sql"),
                    include_str!("schema_v2.sql"),
                    include_str!("schema_v3.sql"),
                    include_str!("schema_v4.sql"),
                    include_str!("schema_v5.sql"),
                    include_str!("schema_v6.sql"),
                ];
                provenant_sqlite::apply_migrations(tx, &steps, start, |index, db| {
                    if index == 3 {
                        for id in &absent_temporary {
                            db.execute("UPDATE attempts SET tmp_retained=0 WHERE id=?1", [id])?;
                        }
                    }
                    Ok(())
                })?;
                Ok(())
            })?;
        }
        Ok(store)
    }

    fn terminal_without_temporary(&self, request: &Request<'_>) -> Result<Vec<String>> {
        let mut query = self
            .db
            .prepare("SELECT run,id FROM attempts WHERE terminal=1")?;
        let candidates: Vec<(String, String)> = query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut absent = Vec::new();
        for (run, id) in candidates {
            request.check()?;
            let run = provenant_core::RunId::parse(&run)
                .map_err(|_| RunError::Validation("run index ID"))?;
            let attempt = provenant_core::AttemptId::parse(&id)
                .map_err(|_| RunError::Validation("attempt index ID"))?;
            let remaining = self
                .attempt_dir(run, attempt, false)
                .and_then(|dir| Ok(dir.metadata_at(OsStr::new("tmp"), false)?));
            if matches!(remaining, Err(RunError::Filesystem(ref error)) if error.is_not_found()) {
                absent.push(id);
            }
        }
        Ok(absent)
    }
    pub(crate) fn write<T>(
        &self,
        request: &Request<'_>,
        action: impl Fn(&Connection) -> Result<T>,
    ) -> Result<T> {
        immediate(&self.db, request, action)
    }
}
/// One immediate transaction. BEGIN IMMEDIATE waits up to the busy timeout.
pub(crate) fn immediate<T>(
    db: &Connection,
    request: &Request<'_>,
    action: impl Fn(&Connection) -> Result<T>,
) -> Result<T> {
    request.check()?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let value = action(&tx)?;
    tx.commit()?;
    Ok(value)
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
            root = provenant_sqlite::checked_directory(&root, part, None)?;
        }
    }
    private_root(&root)?;
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
            request.lock_budget(),
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
    companions(&root, name)?;
    let db = provenant_sqlite::open_connection(&path.join(name), false)?;
    match provenant_sqlite::enable_wal(&db) {
        Ok(()) => {}
        Err(provenant_sqlite::WalError::Unavailable(_)) => {
            return Err(RunError::Validation("WAL unavailable"));
        }
        Err(provenant_sqlite::WalError::Sqlite(error)) => return Err(error.into()),
    }
    Ok((root, db, initialisation))
}
fn version(db: &Connection) -> Result<i64> {
    match provenant_sqlite::read_version(db, 6) {
        Ok(version) => Ok(version as i64),
        Err(provenant_sqlite::VersionError::Unsupported(version)) => {
            Err(RunError::Version(version))
        }
        Err(provenant_sqlite::VersionError::Unversioned) => Err(RunError::Version(0)),
        Err(provenant_sqlite::VersionError::Sqlite(error)) => Err(error.into()),
    }
}
pub(crate) fn private(directory: &Directory) -> Result<()> {
    let m = directory.metadata()?;
    if let Some(check) = PathCheck::directory(m) {
        return Err(RunError::unsafe_path(directory.anchored_path(), check));
    }
    Ok(())
}
/// The state root has not been anchored yet, so name it relative to itself.
fn private_root(directory: &Directory) -> Result<()> {
    if let Some(check) = PathCheck::directory(directory.metadata()?) {
        return Err(RunError::unsafe_path(".", check));
    }
    Ok(())
}
pub(crate) fn companions(directory: &Directory, base: &str) -> Result<()> {
    let names = ["", "-wal", "-shm", "-journal"]
        .map(|suffix| std::ffi::OsString::from(format!("{base}{suffix}")));
    provenant_sqlite::check_companions(directory, &names, Into::into, |name, fault| {
        RunError::unsafe_entry(directory, name, fault.into())
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
