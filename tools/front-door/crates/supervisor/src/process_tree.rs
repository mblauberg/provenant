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

#[derive(Debug, thiserror::Error)]
#[error("lane process-tree sample unavailable; retry while the supervisor is running")]
pub struct Unavailable;
type Result<T> = std::result::Result<T, Unavailable>;

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
                    let mut snapshot = None;
                    for _ in 0..3 {
                        snapshot = sample(&root, attempt).ok();
                        if snapshot.is_some() {
                            break;
                        }
                    }
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
    let clock = SystemClock::new();
    let monotonic_micros = clock.host_monotonic_micros().map_err(|_| Unavailable)?;
    let sampled_at = clock.wall_now().map_err(|_| Unavailable)?;
    let boot = clock.boot_identity().map_err(|_| Unavailable)?;
    if crate::census::SystemProcessObserver::boot_identity().map_err(|_| Unavailable)? != root.boot
    {
        return Err(Unavailable);
    }
    let name = std::ffi::CString::new(attempt.sandbox_lookup_name()).map_err(|_| Unavailable)?;
    let pid = u32::try_from(root.pid).map_err(|_| Unavailable)?;
    let mut processes = Vec::new();
    let mut starts = BTreeMap::new();
    let mut child_lists = BTreeMap::new();
    let mut pending = vec![(pid, None)];
    while let Some((pid, parent)) = pending.pop() {
        if processes.len() >= MAX_PROCESSES
            || clock
                .host_monotonic_micros()
                .map_err(|_| Unavailable)?
                .saturating_sub(monotonic_micros)
                > 250_000
        {
            return Err(Unavailable);
        }
        if !crate::process_os::process_tree::member(pid, &name)? {
            if parent.is_none() {
                return Err(Unavailable);
            }
            continue;
        }
        let native = crate::process_os::process_tree::process(pid)?;
        if parent.is_some_and(|(ppid, start)| native.memory.ppid != ppid || native.start < start)
            || (parent.is_none() && native.start != root.native_start)
            || starts.insert(pid, native.start).is_some()
        {
            return Err(Unavailable);
        }
        let children = crate::process_os::process_tree::children(pid)?;
        child_lists.insert(pid, children.iter().copied().collect::<BTreeSet<_>>());
        for child in children {
            if pending.len() + processes.len() >= MAX_PROCESSES {
                return Err(Unavailable);
            }
            pending.push((child, Some((pid, native.start))));
        }
        processes.push(native.memory);
    }
    // Revalidate every link after traversal. Never publish a recycled PID's
    // RSS or a tree whose parent changed while it was being walked.
    for old in &processes {
        let fresh = crate::process_os::process_tree::process(old.pid)?;
        if Some(&fresh.start) != starts.get(&old.pid) || fresh.memory.ppid != old.ppid {
            return Err(Unavailable);
        }
        let children = crate::process_os::process_tree::children(old.pid)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        if child_lists.get(&old.pid) != Some(&children) {
            return Err(Unavailable);
        }
    }
    if clock
        .host_monotonic_micros()
        .map_err(|_| Unavailable)?
        .saturating_sub(monotonic_micros)
        > 250_000
    {
        return Err(Unavailable);
    }
    processes.sort_unstable_by_key(|process| process.pid);
    Ok(Snapshot {
        sample: Sample {
            schema_version: 1,
            root_pid: pid,
            sampled_at,
            processes,
        },
        starts,
        boot,
        monotonic_micros,
    })
}

/// Replace failures and provider exit with an invalid sample immediately. A
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
    read_with(path, root_pid, || {})
}

/// Test-only scheduling edge for a pathname substitution after opening.
#[cfg(feature = "fixture")]
#[doc(hidden)]
pub fn read_with_before_trust(
    path: &Path,
    root_pid: u32,
    before_trust: impl FnOnce(),
) -> Result<Sample> {
    read_with(path, root_pid, before_trust)
}

fn read_with(path: &Path, root_pid: u32, before_trust: impl FnOnce()) -> Result<Sample> {
    let mut file = FileHandle::open_source(path).map_err(|_| Unavailable)?;
    if file.metadata().map_err(|_| Unavailable)?.kind != FileKind::Regular {
        return Err(Unavailable);
    }
    let bytes = file.read_bounded(MAX_BYTES).map_err(|_| Unavailable)?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| Unavailable)?;
    before_trust();
    // Permission checks must describe the descriptor that supplied the bytes,
    // not a caller-controlled alias which may now name a different file.
    let physical = file.current_path().map_err(|_| Unavailable)?;
    crate::process_os::process_tree::immutable(&physical)?;
    let verified = FileHandle::open_source(&physical).map_err(|_| Unavailable)?;
    if !file
        .metadata()
        .map_err(|_| Unavailable)?
        .same_identity(&verified.metadata().map_err(|_| Unavailable)?)
    {
        return Err(Unavailable);
    }
    let clock = SystemClock::new();
    let now = clock.host_monotonic_micros().map_err(|_| Unavailable)?;
    let boot = clock.boot_identity().map_err(|_| Unavailable)?;
    let wall = clock.wall_now().map_err(|_| Unavailable)?;
    select(snapshot, root_pid, now, &boot, wall, |pid| {
        let process = crate::process_os::process_tree::process(pid)?;
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
