//! Host-sampled lane memory. The reader never enumerates host processes.
use crate::census::ProcessIdentity;
use provenant_core::{AttemptId, Clock, WallTime};
use provenant_os::{
    clock::SystemClock,
    fs::{Directory, FileHandle, FileKind, PublishMode},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::Path,
};

pub const ENV: &str = "PROVENANT_PROCESS_TREE";
pub const FILE: &str = "process-tree.json";
pub const MAX_PROCESSES: usize = 4096;
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_AGE_MICROS: u64 = 5_000_000;
const MAX_SAMPLE_MICROS: u64 = 250_000;

#[derive(Debug, thiserror::Error)]
#[error("lane process-tree sample unavailable; retry while the supervisor is running")]
pub struct Unavailable;
type Result<T> = std::result::Result<T, Unavailable>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationError {
    Churn,
    SizeLimit,
    TimeLimit,
    Unavailable,
}
pub(crate) type ObservationResult<T> = std::result::Result<T, ObservationError>;

trait TreeSource {
    fn member(&self, pid: u32) -> ObservationResult<bool>;
    fn process(&self, pid: u32) -> ObservationResult<NativeProcess>;
    fn children(&self, pid: u32) -> ObservationResult<Vec<u32>>;
}

struct NativeSource<'a>(&'a std::ffi::CStr);
impl TreeSource for NativeSource<'_> {
    fn member(&self, pid: u32) -> ObservationResult<bool> {
        crate::process_os::process_tree::member(pid, self.0)
    }
    fn process(&self, pid: u32) -> ObservationResult<NativeProcess> {
        crate::process_os::process_tree::process(pid)
    }
    fn children(&self, pid: u32) -> ObservationResult<Vec<u32>> {
        crate::process_os::process_tree::children(pid)
    }
}

/// One descriptor-derived path for the exact read grant and guardian publisher.
pub fn snapshot_path(attempt: &Directory) -> Result<std::path::PathBuf> {
    Ok(attempt.current_path().map_err(|_| Unavailable)?.join(FILE))
}

fn observe_refresh<T>(mut observe: impl FnMut() -> ObservationResult<T>) -> Option<T> {
    for _ in 0..3 {
        match observe() {
            Ok(snapshot) => return Some(snapshot),
            Err(ObservationError::Churn) => {}
            Err(
                ObservationError::SizeLimit
                | ObservationError::TimeLimit
                | ObservationError::Unavailable,
            ) => break,
        }
    }
    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMemory {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub rss_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub schema_version: u32,
    pub root_pid: u32,
    pub sampled_at: WallTime,
    pub processes: Vec<ProcessMemory>,
}

/// Private on-disk envelope. Native identities and the boot clock fence stale
/// files and PID reuse; the CLI exposes only `Sample`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    sample: Sample,
    starts: BTreeMap<u32, u64>,
    boot: String,
    monotonic_micros: u64,
}

#[derive(Clone)]
pub(crate) struct NativeProcess {
    pub memory: ProcessMemory,
    pub start: u64,
    pub zombie: bool,
}

/// One bounded worker per live provider. Sampling and fsync never block the
/// guardian's output, cancellation or deadline loop. Dropping it requests stop;
/// the worker invalidates the file after any already-running observation.
pub(crate) struct Sampler(std::sync::mpsc::Sender<()>);
impl Sampler {
    pub(crate) fn start(
        directory: &Directory,
        root: &ProcessIdentity,
        attempt: AttemptId,
    ) -> Option<Self> {
        let directory = directory.try_clone().ok()?;
        let root = root.clone();
        let (stop, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("lane-memory".into())
            .spawn(move || {
                loop {
                    // Reconcile transient fork/exit churn without overlapping
                    // walks. Retain the last good observation until it expires.
                    let snapshot = observe_refresh(|| observe(&root, attempt));
                    if receiver.try_recv() != Err(std::sync::mpsc::TryRecvError::Empty) {
                        break;
                    }
                    if let Some(snapshot) = &snapshot {
                        let _ = publish(&directory, Some(snapshot));
                    }
                    if receiver.recv_timeout(std::time::Duration::from_secs(1))
                        != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                }
                let _ = publish(&directory, None);
            })
            .ok()?;
        Some(Self(stop))
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Sample only descendants of the supervisor-held provider identity. A child
/// listing, identity or RSS failure invalidates the whole observation.
pub fn sample(root: &ProcessIdentity, attempt: AttemptId) -> Result<Snapshot> {
    observe(root, attempt).map_err(|_| Unavailable)
}

fn observe(root: &ProcessIdentity, attempt: AttemptId) -> ObservationResult<Snapshot> {
    let clock = SystemClock::new();
    let monotonic_micros = clock
        .host_monotonic_micros()
        .map_err(|_| ObservationError::Unavailable)?;
    let sampled_at = clock
        .wall_now()
        .map_err(|_| ObservationError::Unavailable)?;
    let boot = clock
        .boot_identity()
        .map_err(|_| ObservationError::Unavailable)?;
    if crate::census::SystemProcessObserver::boot_identity()
        .map_err(|_| ObservationError::Unavailable)?
        != root.boot
    {
        return Err(ObservationError::Unavailable);
    }
    let name = std::ffi::CString::new(attempt.sandbox_lookup_name())
        .map_err(|_| ObservationError::Unavailable)?;
    let pid = u32::try_from(root.pid).map_err(|_| ObservationError::Unavailable)?;
    walk(
        root,
        Snapshot {
            sample: Sample {
                schema_version: 1,
                root_pid: pid,
                sampled_at,
                processes: Vec::new(),
            },
            starts: BTreeMap::new(),
            boot,
            monotonic_micros,
        },
        || {
            clock
                .host_monotonic_micros()
                .map_err(|_| ObservationError::Unavailable)
        },
        &NativeSource(&name),
    )
}

fn deadline(
    now: &mut impl FnMut() -> ObservationResult<u64>,
    started: u64,
) -> ObservationResult<()> {
    let elapsed = now()?
        .checked_sub(started)
        .ok_or(ObservationError::Unavailable)?;
    if elapsed > MAX_SAMPLE_MICROS {
        return Err(ObservationError::TimeLimit);
    }
    Ok(())
}

fn within_deadline<T>(
    now: &mut impl FnMut() -> ObservationResult<u64>,
    started: u64,
    query: impl FnOnce() -> ObservationResult<T>,
) -> ObservationResult<T> {
    deadline(now, started)?;
    let result = query();
    // A slow query can both overrun and report churn. The limit takes priority
    // so the refresh cannot retry merely because the native read failed.
    deadline(now, started)?;
    result
}

fn walk(
    root: &ProcessIdentity,
    mut snapshot: Snapshot,
    mut now: impl FnMut() -> ObservationResult<u64>,
    source: &impl TreeSource,
) -> ObservationResult<Snapshot> {
    let pid = snapshot.sample.root_pid;
    let started = snapshot.monotonic_micros;
    let mut processes = Vec::new();
    let mut starts = BTreeMap::new();
    let mut child_lists = BTreeMap::new();
    let mut pending = vec![(pid, None)];
    while let Some((pid, parent)) = pending.pop() {
        if processes.len() >= MAX_PROCESSES {
            return Err(ObservationError::SizeLimit);
        }
        if !within_deadline(&mut now, started, || source.member(pid))? {
            if parent.is_none() {
                return Err(ObservationError::Unavailable);
            }
            continue;
        }
        let native = within_deadline(&mut now, started, || source.process(pid))?;
        if parent.is_none() && native.start != root.native_start {
            return Err(ObservationError::Unavailable);
        }
        if parent.is_some_and(|(ppid, start)| native.memory.ppid != ppid || native.start < start)
            || starts.insert(pid, native.start).is_some()
        {
            return Err(ObservationError::Churn);
        }
        let children = within_deadline(&mut now, started, || source.children(pid))?;
        child_lists.insert(pid, children.iter().copied().collect::<BTreeSet<_>>());
        for child in children {
            if pending.len() + processes.len() >= MAX_PROCESSES {
                return Err(ObservationError::SizeLimit);
            }
            pending.push((child, Some((pid, native.start))));
        }
        processes.push(native.memory);
    }
    // Revalidate every link after traversal. Never publish a recycled PID's
    // RSS or a tree whose parent changed while it was being walked.
    for old in &processes {
        let fresh = within_deadline(&mut now, started, || source.process(old.pid))?;
        if Some(&fresh.start) != starts.get(&old.pid) || fresh.memory.ppid != old.ppid {
            return Err(ObservationError::Churn);
        }
        let children = within_deadline(&mut now, started, || source.children(old.pid))?
            .into_iter()
            .collect::<BTreeSet<_>>();
        deadline(&mut now, started)?;
        if child_lists.get(&old.pid) != Some(&children) {
            return Err(ObservationError::Churn);
        }
    }
    deadline(&mut now, started)?;
    processes.sort_unstable_by_key(|process| process.pid);
    snapshot.sample.processes = processes;
    snapshot.starts = starts;
    Ok(snapshot)
}

/// Publish a new observation or explicit invalidation on provider exit. A
/// failed publication can leave old bytes, which the reader expires in 5 s.
pub fn publish(directory: &Directory, snapshot: Option<&Snapshot>) -> Result<()> {
    let bytes = serde_json::to_vec(&snapshot).map_err(|_| Unavailable)?;
    directory
        .atomic_write(
            OsStr::new(FILE),
            ".process-tree-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(|_| Unavailable)?;
    Ok(())
}

/// Read the caller-selected subtree from its lane's bounded, fresh snapshot.
/// Selecting an outside PID never causes a host-side query or enumeration.
pub fn read(path: &Path, root_pid: u32) -> Result<Sample> {
    let mut file = FileHandle::open_source(path).map_err(|_| Unavailable)?;
    if file.metadata().map_err(|_| Unavailable)?.kind != FileKind::Regular {
        return Err(Unavailable);
    }
    let bytes = file.read_bounded(MAX_BYTES).map_err(|_| Unavailable)?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| Unavailable)?;
    let clock = SystemClock::new();
    let now = clock.host_monotonic_micros().map_err(|_| Unavailable)?;
    let boot = clock.boot_identity().map_err(|_| Unavailable)?;
    let wall = clock.wall_now().map_err(|_| Unavailable)?;
    select(snapshot, root_pid, now, &boot, wall, |pid| {
        let process = crate::process_os::process_tree::process(pid).map_err(|_| Unavailable)?;
        if process.zombie {
            return Err(Unavailable);
        }
        Ok(process.start)
    })
}

fn select(
    snapshot: Snapshot,
    root_pid: u32,
    now: u64,
    boot: &str,
    wall: WallTime,
    identity: impl FnOnce(u32) -> Result<u64>,
) -> Result<Sample> {
    let sample = snapshot.sample;
    let age = now
        .checked_sub(snapshot.monotonic_micros)
        .ok_or(Unavailable)?;
    if age > MAX_AGE_MICROS
        || sample.sampled_at > wall
        || snapshot.boot != boot
        || sample.schema_version != 1
        || sample.processes.is_empty()
        || sample.processes.len() > MAX_PROCESSES
        || sample.processes.len() != snapshot.starts.len()
    {
        return Err(Unavailable);
    }
    let mut seen = BTreeSet::new();
    for process in &sample.processes {
        if process.pid == 0
            || process.pgid == 0
            || !seen.insert(process.pid)
            || !snapshot.starts.contains_key(&process.pid)
        {
            return Err(Unavailable);
        }
    }
    if !seen.contains(&sample.root_pid)
        || !seen.contains(&root_pid)
        || snapshot.starts.get(&root_pid).copied() != Some(identity(root_pid)?)
    {
        return Err(Unavailable);
    }
    let mut included = BTreeSet::from([root_pid]);
    loop {
        let before = included.len();
        for process in &sample.processes {
            if included.contains(&process.ppid) {
                included.insert(process.pid);
            }
        }
        if included.len() == before {
            break;
        }
    }
    Ok(Sample {
        root_pid,
        processes: sample
            .processes
            .into_iter()
            .filter(|process| included.contains(&process.pid))
            .collect(),
        ..sample
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct FixtureSource {
        now: Cell<u64>,
        reads: RefCell<Vec<u32>>,
        expire_during_revalidation: bool,
        oversized: bool,
    }
    impl TreeSource for FixtureSource {
        fn member(&self, _: u32) -> ObservationResult<bool> {
            Ok(true)
        }
        fn process(&self, pid: u32) -> ObservationResult<NativeProcess> {
            let mut reads = self.reads.borrow_mut();
            reads.push(pid);
            if self.expire_during_revalidation && reads.len() == 3 {
                self.now.set(1_000_000 + MAX_SAMPLE_MICROS + 1);
            }
            Ok(NativeProcess {
                memory: ProcessMemory {
                    pid,
                    ppid: if pid == 10 { 1 } else { 10 },
                    pgid: 10,
                    rss_bytes: 4096,
                },
                start: if pid == 10 { 100 } else { 110 },
                zombie: false,
            })
        }
        fn children(&self, pid: u32) -> ObservationResult<Vec<u32>> {
            Ok(if pid != 10 {
                Vec::new()
            } else if self.oversized {
                (100..100 + MAX_PROCESSES as u32 + 1).collect()
            } else {
                vec![11]
            })
        }
    }

    fn fixture_source(expire: bool, oversized: bool) -> FixtureSource {
        FixtureSource {
            now: Cell::new(1_000_000),
            reads: RefCell::new(Vec::new()),
            expire_during_revalidation: expire,
            oversized,
        }
    }

    fn root() -> ProcessIdentity {
        ProcessIdentity {
            boot: "boot".into(),
            pid: 10,
            native_start: 100,
            group_id: 10,
            session_id: 10,
        }
    }

    #[test]
    fn process_tree_snapshot_path_uses_the_held_attempt_directory() {
        let temp = tempfile::tempdir().unwrap();
        let base = provenant_os::fs::canonicalize(temp.path()).unwrap();
        let parent = base.join("real");
        let attempt = parent.join("attempt");
        std::fs::create_dir_all(&attempt).unwrap();
        let alias = base.join("alias");
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        let directory = Directory::open_anchor(&alias.join("attempt")).unwrap();
        assert_eq!(snapshot_path(&directory).unwrap(), attempt.join(FILE));
    }

    #[test]
    fn process_tree_revalidation_stops_at_the_deadline() {
        let source = fixture_source(true, false);
        let observed = walk(&root(), snapshot(), || Ok(source.now.get()), &source);
        assert_eq!(observed.err(), Some(ObservationError::TimeLimit));
        assert_eq!(
            *source.reads.borrow(),
            [10, 11, 10],
            "no more native queries after the revalidation deadline"
        );
    }

    #[test]
    fn process_tree_retries_churn_but_not_resource_bounds_or_unavailability() {
        let source = fixture_source(false, true);
        assert_eq!(
            walk(&root(), snapshot(), || Ok(source.now.get()), &source).err(),
            Some(ObservationError::SizeLimit)
        );
        for error in [
            ObservationError::SizeLimit,
            ObservationError::TimeLimit,
            ObservationError::Unavailable,
        ] {
            let mut attempts = 0;
            assert!(
                observe_refresh::<()>(|| {
                    attempts += 1;
                    Err(error)
                })
                .is_none()
            );
            assert_eq!(attempts, 1, "must not retry {error:?}");
        }
        let now = Cell::new(1_000_000);
        let mut attempts = 0;
        assert!(
            observe_refresh::<()>(|| {
                attempts += 1;
                within_deadline(&mut || Ok(now.get()), 1_000_000, || {
                    now.set(1_000_000 + MAX_SAMPLE_MICROS + 1);
                    Err(ObservationError::Churn)
                })
            })
            .is_none()
        );
        assert_eq!(attempts, 1, "a query overrun takes priority over churn");
        let mut attempts = 0;
        assert_eq!(
            observe_refresh(|| {
                attempts += 1;
                if attempts < 3 {
                    Err(ObservationError::Churn)
                } else {
                    Ok(42)
                }
            }),
            Some(42)
        );
        assert_eq!(attempts, 3);
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            sample: Sample {
                schema_version: 1,
                root_pid: 10,
                sampled_at: WallTime::from_micros(1_000_000).unwrap(),
                processes: [(10, 1), (11, 10), (12, 11), (13, 10)]
                    .into_iter()
                    .map(|(pid, ppid)| ProcessMemory {
                        pid,
                        ppid,
                        pgid: 10,
                        rss_bytes: 4096,
                    })
                    .collect(),
            },
            starts: [(10, 100), (11, 110), (12, 120), (13, 130)]
                .into_iter()
                .collect(),
            boot: "boot".into(),
            monotonic_micros: 1_000_000,
        }
    }

    #[test]
    fn process_tree_selects_only_the_owned_subtree() {
        let selected = select(
            snapshot(),
            11,
            2_000_000,
            "boot",
            WallTime::from_micros(2_000_000).unwrap(),
            |_| Ok(110),
        )
        .unwrap();
        assert_eq!(selected.root_pid, 11);
        assert_eq!(
            selected
                .processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            [11, 12]
        );
        assert!(
            selected
                .processes
                .iter()
                .all(|process| process.rss_bytes == 4096)
        );
        let json = serde_json::to_value(&selected).unwrap();
        assert_eq!(json["sampled_at"], "1970-01-01T00:00:01.000000Z");
        assert!(json.get("starts").is_none());
    }

    #[test]
    fn process_tree_refuses_stale_reused_foreign_and_incomplete_samples() {
        let wall = WallTime::from_micros(2_000_000).unwrap();
        assert!(select(snapshot(), 10, 6_000_001, "boot", wall, |_| Ok(100)).is_err());
        assert!(select(snapshot(), 10, 999_999, "boot", wall, |_| Ok(100)).is_err());
        assert!(select(snapshot(), 10, 2_000_000, "other-boot", wall, |_| Ok(100)).is_err());
        assert!(select(snapshot(), 10, 2_000_000, "boot", wall, |_| Ok(101)).is_err());
        assert!(select(snapshot(), 99, 2_000_000, "boot", wall, |_| Ok(100)).is_err());
        assert!(
            select(
                snapshot(),
                10,
                2_000_000,
                "boot",
                WallTime::from_micros(0).unwrap(),
                |_| Ok(100)
            )
            .is_err()
        );
        let mut invalid = snapshot();
        invalid.sample.processes.clear();
        assert!(select(invalid, 10, 2_000_000, "boot", wall, |_| Ok(100)).is_err());
        let mut invalid = snapshot();
        invalid.sample.processes[0].pid = 11;
        assert!(select(invalid, 10, 2_000_000, "boot", wall, |_| Ok(100)).is_err());
        let mut invalid = snapshot();
        invalid.sample.schema_version = 2;
        assert!(select(invalid, 10, 2_000_000, "boot", wall, |_| Ok(100)).is_err());
    }

    #[test]
    fn process_tree_reader_refuses_missing_malformed_and_oversized_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE);
        assert!(read(&path, 10).is_err());
        for bytes in [
            b"null".to_vec(),
            b"{}".to_vec(),
            b"{\"sample\":".to_vec(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(read(&path, 10).is_err());
        }
    }
}
