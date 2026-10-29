//! Bounded retention for the Cargo profile directories of lane build output.
//!
//! A lane's target lives in its private cache under
//! `Library/Caches/provenant-lane-cargo/<digest>/`, in `target/` and
//! `target-lint/`. A profile directory (`target/debug`, `target/<triple>/debug`)
//! is idle when a non-blocking exclusive flock succeeds on all of Cargo's
//! `.cargo-lock`, `.cargo-build-lock` and `.cargo-artifact-lock`. The cap counts
//! every profile directory, idle and busy. Idle directories are removed least
//! recently used first until the cap and the free-space floor both hold; when
//! busy directories alone exceed the cap, every eligible idle directory goes and
//! the total stays over. A directory whose locks are held, or cannot be probed,
//! is never removed, and neither is one written within the recency grace: a
//! test or run binary executes after Cargo drops its locks. The grace does not
//! cover Cargo opening an existing lock file without writing anything; the flock
//! probe covers that case. A write dated more than a few minutes in the future
//! is clock skew, not recent activity, and does not earn the grace.
//!
//! Sizes are allocated bytes (`st_blocks * 512`) throughout. A settlement does
//! not walk every lane cache: see `reclaim`.
use provenant_confinement::cache::is_cache_digest;
use provenant_os::fs::{
    Directory, FileKind, ForeignLock, LockGuard, LockMode, Metadata, PayloadTree, PublishMode,
    effective_uid,
};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

/// Cargo's per-profile lock files. `.cargo-lock` marks a profile directory;
/// the other two exist only under newer Cargo and are probed when present.
const CARGO_LOCK: &str = ".cargo-lock";
const LOCKS: [&str; 3] = [CARGO_LOCK, ".cargo-build-lock", ".cargo-artifact-lock"];
const TARGETS: [&str; 2] = ["target", "target-lint"];
const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 1_000_000;
/// How far ahead of now a modification time may be and still count as a
/// recent write.
const FUTURE_SKEW_NS: i128 = 5 * 60 * 1_000_000_000;
/// A cached total within this fraction of the cap (nine tenths) forces a walk.
const NEAR_CAP_NUM: u128 = 9;
const NEAR_CAP_DEN: u128 = 10;
const SURVEY_STATE: &str = "build-dir-gc-survey";
const SURVEY_LOCK: &str = "build-dir-gc.lock";

/// Size cap on all profile directories, the free-space floor to restore, and
/// the seconds after its newest write that a directory is spared.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: u64,
    pub free_floor: u64,
    pub grace_seconds: u64,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    /// Bytes reclaimed, or reclaimable on a dry run.
    pub bytes: u64,
    /// Allocated bytes in all profile directories after the sweep.
    pub cache_bytes: u64,
    /// Profile directories kept because their locks are held or unprobeable.
    pub busy: Vec<PathBuf>,
    /// Profile directories kept because they were written within the grace.
    pub recent: Vec<PathBuf>,
    pub errors: Vec<String>,
}

struct Candidate {
    cache: OsString,
    /// Components below the cache: `target`, optionally a triple, the profile.
    path: Vec<OsString>,
    display: PathBuf,
    bytes: u64,
    newest: i128,
}

enum Outcome {
    Removed,
    Busy,
    Recent,
}

/// What a walk of the lane caches found, before anything is removed.
struct Survey {
    report: Report,
    root: Option<Directory>,
    /// Least recently used first.
    candidates: Vec<Candidate>,
    total: u64,
    free: u64,
}

impl Survey {
    fn satisfied(&self, limits: Limits) -> bool {
        self.total <= limits.max_bytes && self.free >= limits.free_floor
    }
    /// The report when nothing is removed.
    fn finish(mut self) -> Report {
        self.report.cache_bytes = self.total;
        self.report
    }
}

pub fn cache_root(home: &Path) -> PathBuf {
    home.join("Library/Caches/provenant-lane-cargo")
}

/// Collect. The walk that measures every profile directory runs without any
/// lock, and nothing more happens when the cap and the floor already hold. Only
/// removal takes the host admission lock, so no lane starts mid-removal, and it
/// probes each candidate afresh. A dry run changes nothing and takes no lock.
/// `host` is the state root holding the admission ledger.
pub fn collect(
    host: &Path,
    home: &Path,
    limits: Limits,
    dry_run: bool,
) -> Result<Report, Box<dyn std::error::Error>> {
    let mut free = || free_bytes(home);
    let mut survey = survey(home, &mut free);
    if dry_run {
        return Ok(prune(survey, limits, true, wall_ns()?, &mut free));
    }
    if survey.satisfied(limits) {
        return Ok(survey.finish());
    }
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let request = crate::Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 5)?,
        cancel: &cancel,
    };
    crate::ledger::while_admission_held(
        host,
        &request,
        || -> Result<Report, Box<dyn std::error::Error>> {
            // The walk may have been long, and a lane may have settled meanwhile.
            survey.free = free().unwrap_or(survey.free);
            Ok(prune(survey, limits, false, wall_ns()?, &mut free))
        },
    )?
}

/// Collect for an event (admission below the floor, a settled lane) using the
/// host's configured cap, floor and grace. Best effort: a busy ledger,
/// unreadable settings or a failed sweep leave the caches as they were.
///
/// Cheap signals come first. The walk runs only when free space is below the
/// floor, when the last walk is older than `build_dir_survey_interval_seconds`,
/// or when it found the total within a tenth of the cap. The last walk's time
/// and total live in the state root. A try-lock keeps concurrent events from
/// walking twice: the loser skips and returns `None`.
pub fn reclaim(host: &Path, home: &Path) -> Option<Report> {
    let settings = provenant_config::LaneBuildSettings::read(host).ok()?;
    let floor = provenant_config::GcSettings::read(host)
        .and_then(|gc| gc.free_floor_bytes(host))
        .ok()?;
    let limits = Limits {
        max_bytes: settings.build_dir_cap(),
        free_floor: floor,
        grace_seconds: settings.build_dir_grace_seconds(),
    };
    let interval = settings.build_dir_survey_interval_seconds();
    let mut free = || free_bytes(home);
    reclaim_with(host, home, limits, interval, &mut free, wall_ns().ok()?)
}

fn reclaim_with(
    host: &Path,
    home: &Path,
    limits: Limits,
    interval_seconds: u64,
    free: &mut dyn FnMut() -> io::Result<u64>,
    now_ns: i128,
) -> Option<Report> {
    let anchor = Directory::open_anchor(host).ok()?;
    // An unreadable disk is not evidence of room: walk.
    let breached = free().map_or(true, |bytes| bytes < limits.free_floor);
    if !breached && !survey_due(&anchor, limits, interval_seconds, now_ns) {
        return None;
    }
    let _walking = anchor
        .try_lock(OsStr::new(SURVEY_LOCK), LockMode::Exclusive)
        .ok()?;
    // Another event may have walked while this one waited on the lock.
    if !breached && !survey_due(&anchor, limits, interval_seconds, now_ns) {
        return None;
    }
    let report = collect(host, home, limits, false).ok()?;
    let record = format!("{now_ns} {}\n", report.cache_bytes);
    // The cache only saves walks; failing to write it costs one extra walk.
    let _ = anchor.atomic_write(
        OsStr::new(SURVEY_STATE),
        ".build-dir-gc-survey",
        record.as_bytes(),
        PublishMode::Replace,
    );
    Some(report)
}

/// Whether the last walk is missing, stale, dated in the future, or found the
/// total near the cap.
fn survey_due(anchor: &Directory, limits: Limits, interval_seconds: u64, now_ns: i128) -> bool {
    let Ok(bytes) = anchor.read_bounded(OsStr::new(SURVEY_STATE), 256) else {
        return true;
    };
    let Some((at, total)) = std::str::from_utf8(&bytes).ok().and_then(|text| {
        let (at, total) = text.trim().split_once(' ')?;
        Some((at.parse::<i128>().ok()?, total.parse::<u64>().ok()?))
    }) else {
        return true;
    };
    let age = now_ns.saturating_sub(at);
    let interval = i128::from(interval_seconds).saturating_mul(1_000_000_000);
    age < 0
        || age >= interval
        || u128::from(total) * NEAR_CAP_DEN >= u128::from(limits.max_bytes) * NEAR_CAP_NUM
}

fn free_bytes(home: &Path) -> io::Result<u64> {
    provenant_confinement::disk::available_bytes_at_existing(&cache_root(home))
}

/// Wall time as nanoseconds since the Unix epoch, the unit of file times.
fn wall_ns() -> Result<i128, Box<dyn std::error::Error>> {
    let now = provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())?;
    Ok(i128::from(now.as_micros()) * 1_000)
}

/// The collection itself, with free space and the time injected. The caller
/// holds whatever excludes new admissions.
pub fn sweep(
    home: &Path,
    limits: Limits,
    dry_run: bool,
    now_ns: i128,
    free_space: &mut dyn FnMut() -> io::Result<u64>,
) -> Report {
    let survey = survey(home, free_space);
    prune(survey, limits, dry_run, now_ns, free_space)
}

/// Measure every profile directory and sample free space. No lock is needed:
/// removal probes each candidate again.
fn survey(home: &Path, free_space: &mut dyn FnMut() -> io::Result<u64>) -> Survey {
    let mut report = Report::default();
    let root = match Directory::open_anchor(&cache_root(home)) {
        Ok(root) => root,
        Err(error) => {
            if !error.is_not_found() {
                report.errors.push(error.to_string());
            }
            return Survey {
                report,
                root: None,
                candidates: Vec::new(),
                total: 0,
                free: u64::MAX,
            };
        }
    };
    let mut candidates = discover(&root, &cache_root(home), &mut report);
    let total = candidates
        .iter()
        .fold(0_u64, |sum, item| sum.saturating_add(item.bytes));
    let free = free_space().unwrap_or_else(|error| {
        report.errors.push(format!("free space: {error}"));
        u64::MAX
    });
    // Least recently used first; the path breaks ties deterministically.
    candidates.sort_by(|a, b| (a.newest, &a.display).cmp(&(b.newest, &b.display)));
    Survey {
        report,
        root: Some(root),
        candidates,
        total,
        free,
    }
}

/// Remove candidates until the cap and the floor hold. Each is probed, and its
/// recency checked, at the moment of removal.
fn prune(
    survey: Survey,
    limits: Limits,
    dry_run: bool,
    now_ns: i128,
    free_space: &mut dyn FnMut() -> io::Result<u64>,
) -> Report {
    let Survey {
        mut report,
        root,
        candidates,
        mut total,
        mut free,
    } = survey;
    let Some(root) = root else {
        return report;
    };
    let grace = grace_ns(limits);
    for candidate in candidates {
        if total <= limits.max_bytes && free >= limits.free_floor {
            break;
        }
        if within_grace(candidate.newest, now_ns, grace) {
            report.recent.push(candidate.display);
            continue;
        }
        match remove(&root, &candidate, dry_run, now_ns, grace) {
            Ok(Outcome::Removed) => {
                total = total.saturating_sub(candidate.bytes);
                report.bytes = report.bytes.saturating_add(candidate.bytes);
                report.removed.push(candidate.display);
                free = if dry_run {
                    free.saturating_add(candidate.bytes)
                } else {
                    free_space().unwrap_or(free.saturating_add(candidate.bytes))
                };
            }
            Ok(Outcome::Busy) => report.busy.push(candidate.display),
            Ok(Outcome::Recent) => report.recent.push(candidate.display),
            Err(error) => {
                report.busy.push(candidate.display.clone());
                report
                    .errors
                    .push(format!("{}: {error}", candidate.display.display()));
            }
        }
    }
    report.cache_bytes = total;
    report
}

fn grace_ns(limits: Limits) -> i128 {
    i128::from(limits.grace_seconds).saturating_mul(1_000_000_000)
}

/// A write at or after `now - grace` is recent. One dated more than `FUTURE_SKEW`
/// ahead of now comes from a wrong clock and is not.
fn within_grace(newest: i128, now_ns: i128, grace: i128) -> bool {
    let age = now_ns.saturating_sub(newest);
    age < grace && age > -FUTURE_SKEW_NS
}

/// Every profile directory in this user's lane caches, measured. Anything that
/// cannot be opened without following a link is left alone.
fn discover(root: &Directory, cache_root: &Path, report: &mut Report) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let names = match provenant_os::fs::entry_names(root, MAX_ENTRIES) {
        Ok(names) => names,
        Err(error) => {
            report.errors.push(error.to_string());
            return candidates;
        }
    };
    for name in names {
        let Some(digest) = name.to_str().filter(|name| is_cache_digest(name)) else {
            continue;
        };
        let Ok(metadata) = root.metadata_at(&name, false) else {
            continue;
        };
        if metadata.kind != FileKind::Directory
            || metadata.uid != effective_uid()
            || metadata.mode & 0o7777 != 0o700
        {
            continue;
        }
        let Ok(cache) = PayloadTree::claim(root, &name, &metadata) else {
            continue;
        };
        for target in TARGETS {
            let target = OsStr::new(target);
            let Ok(found) = profiles(&cache, target) else {
                continue;
            };
            for path in found {
                let display = path
                    .iter()
                    .fold(cache_root.join(digest), |base, part| base.join(part));
                let measured = open_path(&cache, &path).and_then(|tree| measure(&tree, 0));
                match measured {
                    Ok((bytes, newest)) => candidates.push(Candidate {
                        cache: name.clone(),
                        path,
                        display,
                        bytes,
                        newest,
                    }),
                    Err(error) => report
                        .errors
                        .push(format!("{}: {error}", display.display())),
                }
            }
        }
    }
    candidates
}

/// Component paths of profile directories under `cache/<target>`, at most one
/// triple directory deep.
fn profiles(cache: &PayloadTree, target: &OsStr) -> io::Result<Vec<Vec<OsString>>> {
    let mut found = Vec::new();
    if cache.metadata_at(target).map_err(io::Error::other)?.kind != FileKind::Directory {
        return Ok(found);
    }
    let target_tree = cache.child(target).map_err(io::Error::other)?;
    for name in directory_names(&target_tree)? {
        if !is_directory(&target_tree, &name) {
            continue;
        }
        let Ok(child) = target_tree.child(&name) else {
            continue;
        };
        if is_profile(&child) {
            found.push(vec![target.to_owned(), name]);
            continue;
        }
        for inner in directory_names(&child)? {
            if is_directory(&child, &inner)
                && child
                    .child(&inner)
                    .is_ok_and(|candidate| is_profile(&candidate))
            {
                found.push(vec![target.to_owned(), name.clone(), inner]);
            }
        }
    }
    Ok(found)
}

fn is_profile(directory: &PayloadTree) -> bool {
    directory
        .metadata_at(OsStr::new(CARGO_LOCK))
        .is_ok_and(|metadata| metadata.kind == FileKind::Regular)
}

fn is_directory(parent: &PayloadTree, name: &OsStr) -> bool {
    parent
        .metadata_at(name)
        .is_ok_and(|metadata| metadata.kind == FileKind::Directory)
}

fn directory_names(directory: &PayloadTree) -> io::Result<Vec<OsString>> {
    provenant_os::fs::entry_names(directory.directory(), MAX_ENTRIES).map_err(io::Error::other)
}

fn open_path(cache: &PayloadTree, path: &[OsString]) -> io::Result<PayloadTree> {
    let (first, rest) = path
        .split_first()
        .ok_or_else(|| io::Error::other("empty path"))?;
    let mut tree = cache.child(first).map_err(io::Error::other)?;
    for part in rest {
        tree = tree.child(part).map_err(io::Error::other)?;
    }
    Ok(tree)
}

/// Allocated bytes and the newest modification time below `directory`.
fn measure(directory: &PayloadTree, depth: usize) -> io::Result<(u64, i128)> {
    if depth >= MAX_DEPTH {
        return Err(io::Error::other("directory nesting too deep"));
    }
    let mut bytes = 0_u64;
    let mut newest = directory.metadata().map_err(io::Error::other)?.modified_ns;
    for name in directory_names(directory)? {
        let metadata = directory.metadata_at(&name).map_err(io::Error::other)?;
        bytes = bytes.saturating_add(metadata.allocated);
        newest = newest.max(metadata.modified_ns);
        if metadata.kind == FileKind::Directory {
            let child = directory.child(&name).map_err(io::Error::other)?;
            let (child_bytes, child_newest) = measure(&child, depth + 1)?;
            bytes = bytes.saturating_add(child_bytes);
            newest = newest.max(child_newest);
        }
    }
    Ok((bytes, newest))
}

/// Probe every lock, re-check recency, and remove only if all locks are free
/// and nothing was written lately. While the locks are held, Cargo waits on
/// them and the lock files go last. That protects only against a Cargo that
/// holds a lock at probe time. It does not cover a test or run binary that keeps
/// executing after Cargo drops its locks, nor a Cargo between opening a lock
/// file and locking it. Those leave recent writes, which the grace spares, and
/// a directory written since the survey is spared too.
fn remove(
    root: &Directory,
    candidate: &Candidate,
    dry_run: bool,
    now_ns: i128,
    grace: i128,
) -> io::Result<Outcome> {
    let metadata: Metadata = root
        .metadata_at(&candidate.cache, false)
        .map_err(io::Error::other)?;
    let cache = PayloadTree::claim(root, &candidate.cache, &metadata).map_err(io::Error::other)?;
    let (name, parents) = candidate
        .path
        .split_last()
        .ok_or_else(|| io::Error::other("empty path"))?;
    let parent = open_path(&cache, parents)?;
    let profile = parent.child(name).map_err(io::Error::other)?;
    let mut guards: Vec<LockGuard> = Vec::new();
    for lock in LOCKS {
        match profile
            .directory()
            .try_flock_foreign(OsStr::new(lock), LockMode::Exclusive)
            .map_err(io::Error::other)?
        {
            ForeignLock::Acquired(guard) => guards.push(guard),
            ForeignLock::Held => return Ok(Outcome::Busy),
            // A vanished `.cargo-lock` means another process is removing or
            // replacing this directory.
            ForeignLock::Absent if lock == CARGO_LOCK => return Ok(Outcome::Busy),
            ForeignLock::Absent => {}
        }
    }
    if dry_run {
        return Ok(Outcome::Removed);
    }
    let (_, newest) = measure(&profile, 0)?;
    if newest != candidate.newest || within_grace(newest, now_ns, grace) {
        return Ok(Outcome::Recent);
    }
    remove_contents(&profile, 0)?;
    for lock in LOCKS {
        match profile.unlink(OsStr::new(lock)) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    drop(profile);
    parent.rmdir(name).map_err(io::Error::other)?;
    drop(guards);
    Ok(Outcome::Removed)
}

/// Remove everything but the lock files, widening unreadable directories.
/// Links are unlinked, never followed.
fn remove_contents(directory: &PayloadTree, depth: usize) -> io::Result<()> {
    if depth >= MAX_DEPTH {
        return Err(io::Error::other("directory nesting too deep"));
    }
    directory.make_writable().map_err(io::Error::other)?;
    for name in directory_names(directory)? {
        if depth == 0 && LOCKS.iter().any(|lock| OsStr::new(lock) == name) {
            continue;
        }
        let metadata = directory.metadata_at(&name).map_err(io::Error::other)?;
        if metadata.kind == FileKind::Directory {
            let child = directory.child(&name).map_err(io::Error::other)?;
            remove_contents(&child, depth + 1)?;
            drop(child);
            directory.rmdir(&name).map_err(io::Error::other)?;
        } else {
            directory.unlink(&name).map_err(io::Error::other)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Fixtures age files with real timestamps.
    #![allow(clippy::disallowed_methods)]
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    struct Fixture {
        home: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            fs::create_dir_all(cache_root(home.path())).unwrap();
            Self { home }
        }
        fn cache(&self, digit: char) -> PathBuf {
            let path = cache_root(self.home.path()).join(digit.to_string().repeat(64));
            if !path.exists() {
                fs::create_dir(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            path
        }
        /// A profile directory of `bytes` of payload, last written `age` seconds ago.
        fn profile(&self, digit: char, name: &str, bytes: usize, age: u64) -> PathBuf {
            let dir = self.cache(digit).join("target").join(name);
            fs::create_dir_all(dir.join("deps")).unwrap();
            for lock in LOCKS {
                fs::write(dir.join(lock), b"").unwrap();
            }
            let payload = dir.join("deps/payload");
            fs::write(&payload, vec![1; bytes]).unwrap();
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            for path in [
                &payload,
                &dir.join("deps"),
                &dir,
                &dir.join(CARGO_LOCK),
                &dir.join(LOCKS[1]),
                &dir.join(LOCKS[2]),
            ] {
                fs::File::open(path).unwrap().set_modified(when).unwrap();
            }
            dir
        }
    }
    fn plenty() -> io::Result<u64> {
        Ok(u64::MAX)
    }
    /// No grace: the fixtures' ages are minutes, and the grace has its own tests.
    fn limits(max_bytes: u64, free_floor: u64) -> Limits {
        Limits {
            max_bytes,
            free_floor,
            grace_seconds: 0,
        }
    }
    fn graced(max_bytes: u64, grace_seconds: u64) -> Limits {
        Limits {
            grace_seconds,
            ..limits(max_bytes, 0)
        }
    }
    fn now() -> i128 {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        i128::try_from(since.as_nanos()).unwrap()
    }
    fn hold(dir: &Path, name: &str) -> LockGuard {
        let directory = Directory::open_anchor(dir).unwrap();
        match directory
            .try_flock_foreign(OsStr::new(name), LockMode::Exclusive)
            .unwrap()
        {
            ForeignLock::Acquired(guard) => guard,
            other => panic!("lock {name} is not free: {other:?}"),
        }
    }

    #[test]
    fn nothing_is_removed_while_under_the_cap_and_above_the_floor() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let report = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            false,
            now(),
            &mut plenty,
        );
        assert!(report.removed.is_empty() && debug.exists());
        assert!(report.cache_bytes >= 4096);
    }

    #[test]
    fn a_held_lock_prevents_deletion_whichever_of_the_three_is_held() {
        for lock in LOCKS {
            let fixture = Fixture::new();
            let debug = fixture.profile('a', "debug", 4096, 100);
            let guard = hold(&debug, lock);
            let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
            assert!(report.removed.is_empty(), "{lock}");
            assert_eq!(report.busy, vec![debug.clone()], "{lock}");
            assert!(debug.join("deps/payload").exists(), "{lock}");
            drop(guard);
            let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
            assert_eq!(report.removed, vec![debug.clone()], "{lock}");
            assert!(!debug.exists(), "{lock}");
        }
    }

    #[test]
    fn least_recently_used_goes_first_and_the_cap_stops_collection() {
        let fixture = Fixture::new();
        let oldest = fixture.profile('a', "debug", 8192, 3000);
        let middle = fixture.profile('b', "debug", 8192, 2000);
        let newest = fixture.profile('c', "debug", 8192, 1000);
        let before = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        );
        let one = before.cache_bytes / 3;
        // Room for two of the three: only the oldest goes.
        let report = sweep(
            fixture.home.path(),
            limits(before.cache_bytes - one, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![oldest.clone()]);
        assert!(!oldest.exists() && middle.exists() && newest.exists());
        assert_eq!(report.bytes, one);
        assert!(report.cache_bytes <= before.cache_bytes - one);
        // A tighter cap takes the next oldest, never the newest first.
        let report = sweep(
            fixture.home.path(),
            limits(one, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![middle.clone()]);
        assert!(newest.exists());
    }

    /// Free space that starts at `start` and rises by `step` per removal: the
    /// sweep samples once before collecting and once after each removal.
    fn rising(start: u64, step: u64) -> impl FnMut() -> io::Result<u64> {
        let mut samples = 0_u64;
        move || {
            let free = start + samples * step;
            samples += 1;
            Ok(free)
        }
    }

    #[test]
    fn the_free_space_floor_is_honoured_and_no_more_is_removed() {
        let floor = 1_000_000;
        for (shortfall, removed) in [(1, 1), (5_000, 1), (5_001, 2), (12_000, 3)] {
            let fixture = Fixture::new();
            let profiles = [
                fixture.profile('a', "debug", 8192, 3000),
                fixture.profile('b', "debug", 8192, 2000),
                fixture.profile('c', "debug", 8192, 1000),
            ];
            let size = sweep(
                fixture.home.path(),
                limits(u64::MAX, 0),
                true,
                now(),
                &mut plenty,
            )
            .cache_bytes
                / 3;
            let mut free = rising(floor - shortfall, size.min(5_000));
            let report = sweep(
                fixture.home.path(),
                limits(u64::MAX, floor),
                false,
                now(),
                &mut free,
            );
            assert_eq!(report.removed.len(), removed, "shortfall {shortfall}");
            for (index, profile) in profiles.iter().enumerate() {
                assert_eq!(profile.exists(), index >= removed, "{shortfall} {index}");
            }
        }
    }

    #[test]
    fn the_cap_and_the_floor_apply_together() {
        let fixture = Fixture::new();
        let oldest = fixture.profile('a', "debug", 8192, 3000);
        let middle = fixture.profile('b', "debug", 8192, 2000);
        let newest = fixture.profile('c', "debug", 8192, 1000);
        let total = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        )
        .cache_bytes;
        // The cap needs one removal and the floor a second.
        let mut free = rising(100, u64::MAX / 4);
        let report = sweep(
            fixture.home.path(),
            limits(total - 1, 200),
            false,
            now(),
            &mut free,
        );
        assert_eq!(report.removed, vec![oldest.clone()]);
        assert!(middle.exists() && newest.exists());
        let mut free = rising(100, 50);
        let report = sweep(
            fixture.home.path(),
            limits(u64::MAX, 175),
            false,
            now(),
            &mut free,
        );
        assert_eq!(report.removed, vec![middle.clone(), newest.clone()]);
    }

    #[test]
    fn a_dry_run_reports_without_deleting_or_holding_locks() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let report = sweep(fixture.home.path(), limits(0, 0), true, now(), &mut plenty);
        assert_eq!(report.removed, vec![debug.clone()]);
        assert!(report.bytes >= 4096);
        assert!(debug.join("deps/payload").exists());
        let _free = hold(&debug, CARGO_LOCK);
    }

    #[test]
    fn only_profile_directories_are_collected() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let triple = fixture.profile('a', "aarch64-apple-darwin/release", 4096, 100);
        let doc = fixture.cache('a').join("target/doc");
        fs::create_dir_all(&doc).unwrap();
        fs::write(doc.join("index.html"), b"x").unwrap();
        let lint = fixture.cache('a').join("target-lint/debug");
        fs::create_dir_all(&lint).unwrap();
        fs::write(lint.join(CARGO_LOCK), b"").unwrap();
        fs::write(lint.join("payload"), vec![1; 4096]).unwrap();
        let stray = fixture.cache('a').join("cargo-home");
        fs::create_dir_all(&stray).unwrap();
        fs::write(stray.join(CARGO_LOCK), b"").unwrap();
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert!(
            !debug.exists() && !triple.exists() && !lint.exists(),
            "{report:?}"
        );
        assert_eq!(report.removed.len(), 3);
        assert!(doc.join("index.html").exists());
        assert!(stray.join(CARGO_LOCK).exists());
        assert!(fixture.cache('a').join("target").is_dir());
    }

    #[test]
    fn links_are_never_followed_and_foreign_caches_are_skipped() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        let debug = fixture.profile('a', "debug", 4096, 100);
        std::os::unix::fs::symlink(outside.path(), debug.join("deps/escape")).unwrap();
        // A symlinked profile directory and a cache with loose permissions.
        let target = fixture.cache('a').join("target");
        std::os::unix::fs::symlink(outside.path(), target.join("linked")).unwrap();
        let loose = fixture.profile('b', "debug", 4096, 100);
        fs::set_permissions(fixture.cache('b'), fs::Permissions::from_mode(0o755)).unwrap();
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert_eq!(report.removed, vec![debug.clone()]);
        assert!(outside.path().join("keep").exists());
        assert!(loose.exists());
    }

    #[test]
    fn read_only_directories_are_removed() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        fs::set_permissions(debug.join("deps"), fs::Permissions::from_mode(0o500)).unwrap();
        let report = sweep(fixture.home.path(), limits(0, 0), false, now(), &mut plenty);
        assert_eq!(report.removed, vec![debug.clone()]);
        assert!(!debug.exists());
    }

    #[test]
    fn events_use_the_configured_cap_and_floor() {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Fixture::new();
        let old = fixture.profile('a', "debug", 8192, 2000);
        let new = fixture.profile('b', "debug", 8192, 1000);
        let size = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        )
        .cache_bytes
            / 2;
        let config = state.path().join("config.toml");
        // Under the cap and with a floor any disk meets: nothing to do.
        fs::write(
            &config,
            format!(
                "[gc]\nfree_floor='1B'\n[domains.fabric.lanes]\nbuild_dir_max_bytes={}\n",
                size * 2
            ),
        )
        .unwrap();
        let report = reclaim(state.path(), fixture.home.path()).unwrap();
        assert!(report.removed.is_empty() && old.exists() && new.exists());
        assert_eq!(report.cache_bytes, size * 2);
        // Over the cap by one byte: the least recently used goes.
        fs::write(
            &config,
            format!(
                "[gc]\nfree_floor='1B'\n[domains.fabric.lanes]\nbuild_dir_max_bytes={}\n",
                size * 2 - 1
            ),
        )
        .unwrap();
        let report = reclaim(state.path(), fixture.home.path()).unwrap();
        assert_eq!(report.removed, vec![old.clone()]);
        assert!(!old.exists() && new.exists());
        // A floor no volume can meet empties every idle directory, but the
        // default 15 minute grace spares one written a minute ago.
        let recent = fixture.profile('c', "debug", 8192, 60);
        fs::write(&config, "[gc]\nfree_floor='900000000000000B'\n").unwrap();
        let report = reclaim(state.path(), fixture.home.path()).unwrap();
        assert_eq!(report.removed, vec![new.clone()]);
        assert_eq!(report.recent, vec![recent.clone()]);
        // The configured grace applies.
        fs::write(
            &config,
            "[gc]\nfree_floor='900000000000000B'\n[domains.fabric.lanes]\nbuild_dir_grace_seconds=0\n",
        )
        .unwrap();
        let report = reclaim(state.path(), fixture.home.path()).unwrap();
        assert_eq!(report.removed, vec![recent.clone()]);
    }

    #[test]
    fn collection_waits_for_the_admission_lock_and_blocks_claims() {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 100);
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let request = crate::Request {
            clock: &clock,
            budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 1)
                .unwrap(),
            cancel: &cancel,
        };
        // A claim in flight holds the ledger: collection refuses and deletes nothing.
        let outcome = crate::ledger::while_admission_held(state.path(), &request, || {
            collect(state.path(), fixture.home.path(), limits(0, 0), false).is_err()
        })
        .unwrap();
        assert!(outcome);
        assert!(debug.exists());
        let report = collect(state.path(), fixture.home.path(), limits(0, 0), false).unwrap();
        assert_eq!(report.removed, vec![debug.clone()]);
    }

    #[test]
    fn a_directory_written_within_the_grace_is_kept_though_its_locks_are_free() {
        let fixture = Fixture::new();
        let running = fixture.profile('a', "debug", 4096, 60);
        let settled = fixture.profile('b', "debug", 4096, 1000);
        // A test binary still runs after Cargo dropped its locks.
        let report = sweep(
            fixture.home.path(),
            graced(0, 900),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![settled.clone()]);
        assert_eq!(report.recent, vec![running.clone()]);
        assert!(report.busy.is_empty());
        assert!(running.join("deps/payload").exists() && !settled.exists());
        assert!(report.cache_bytes >= 4096, "kept bytes still count");
        // A dry run predicts the same, and a shorter grace releases it.
        let preview = sweep(
            fixture.home.path(),
            graced(0, 900),
            true,
            now(),
            &mut plenty,
        );
        assert!(preview.removed.is_empty() && preview.recent == vec![running.clone()]);
        let report = sweep(
            fixture.home.path(),
            graced(0, 30),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![running.clone()]);
    }

    #[test]
    fn a_write_after_the_walk_keeps_the_directory() {
        // Cargo opens `.cargo-lock` or creates a directory after the walk
        // measured the profile and before the sweep takes the flocks.
        for touch in ["lock", "directory", "payload"] {
            let fixture = Fixture::new();
            let debug = fixture.profile('a', "debug", 4096, 1000);
            let mut free = plenty;
            let survey = survey(fixture.home.path(), &mut free);
            let when = std::time::SystemTime::now();
            match touch {
                "lock" => fs::File::open(debug.join(CARGO_LOCK))
                    .unwrap()
                    .set_modified(when)
                    .unwrap(),
                "directory" => fs::create_dir_all(debug.join("deps/incremental")).unwrap(),
                _ => fs::write(debug.join("deps/payload"), b"more").unwrap(),
            }
            let report = prune(survey, graced(0, 900), false, now(), &mut free);
            assert!(report.removed.is_empty(), "{touch}");
            assert_eq!(report.recent, vec![debug.clone()], "{touch}");
            assert!(debug.join("deps/payload").exists(), "{touch}");
        }
    }

    #[test]
    fn a_change_since_the_walk_is_noticed_even_without_a_grace() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 1000);
        let mut free = plenty;
        let survey = survey(fixture.home.path(), &mut free);
        // Newer than the walk saw, yet far older than any grace.
        let later = std::time::SystemTime::now() - std::time::Duration::from_secs(500);
        fs::File::open(debug.join("deps/payload"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        let report = prune(survey, limits(0, 0), false, now(), &mut free);
        assert!(report.removed.is_empty() && debug.exists());
        assert_eq!(report.recent, vec![debug.clone()]);
    }

    #[test]
    fn busy_directories_count_toward_the_cap_and_idle_ones_all_go_when_it_is_exceeded() {
        let fixture = Fixture::new();
        let busy_a = fixture.profile('a', "debug", 8192, 3000);
        let busy_b = fixture.profile('b', "debug", 8192, 2000);
        let idle_a = fixture.profile('c', "debug", 8192, 1000);
        let idle_b = fixture.profile('d', "debug", 8192, 900);
        let _a = hold(&busy_a, CARGO_LOCK);
        let _b = hold(&busy_b, LOCKS[1]);
        let one = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        )
        .cache_bytes
            / 4;
        // The cap would fit the busy pair's neighbours but not the busy pair.
        let report = sweep(
            fixture.home.path(),
            limits(one, 0),
            false,
            now(),
            &mut plenty,
        );
        assert_eq!(report.removed, vec![idle_a.clone(), idle_b.clone()]);
        assert_eq!(report.busy, vec![busy_a.clone(), busy_b.clone()]);
        assert!(busy_a.exists() && busy_b.exists());
        assert!(report.cache_bytes > one, "stays over the cap");
    }

    #[test]
    fn a_sweep_with_nothing_to_do_does_not_take_the_admission_lock() {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 4096, 1000);
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = provenant_core::CancelToken::new();
        let request = crate::Request {
            clock: &clock,
            budget: provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 1)
                .unwrap(),
            cancel: &cancel,
        };
        // A claim in flight holds the ledger; a sweep that has nothing to remove
        // neither waits on it nor fails, and still measures the caches.
        let report = crate::ledger::while_admission_held(state.path(), &request, || {
            collect(
                state.path(),
                fixture.home.path(),
                graced(u64::MAX, 0),
                false,
            )
        })
        .unwrap()
        .unwrap();
        assert!(report.removed.is_empty() && debug.exists());
        assert!(report.cache_bytes >= 4096);
    }

    #[test]
    fn a_future_modification_beyond_the_skew_is_not_recent() {
        let grace = 900 * 1_000_000_000_i128;
        let now = 10_000 * 1_000_000_000_i128;
        let sec = 1_000_000_000_i128;
        assert!(within_grace(now - 60 * sec, now, grace));
        assert!(!within_grace(now - 1000 * sec, now, grace));
        // A little skew is tolerated; a clock set days ahead is not trusted.
        assert!(within_grace(now + 60 * sec, now, grace));
        assert!(!within_grace(now + 6 * 60 * sec, now, grace));
        assert!(!within_grace(now + 86_400 * sec, now, grace));
    }

    #[test]
    fn sizes_are_allocated_bytes_not_apparent_size() {
        let fixture = Fixture::new();
        let debug = fixture.profile('a', "debug", 1, 100);
        // A sparse file is large by length and nearly empty on disk.
        let sparse = fs::File::create(debug.join("deps/sparse")).unwrap();
        sparse.set_len(1 << 30).unwrap();
        let report = sweep(
            fixture.home.path(),
            limits(u64::MAX, 0),
            true,
            now(),
            &mut plenty,
        );
        assert!(report.cache_bytes < (1 << 24), "{}", report.cache_bytes);
    }

    fn state_root() -> tempfile::TempDir {
        let state = tempfile::tempdir().unwrap();
        fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
        state
    }

    #[test]
    fn settlements_within_the_interval_walk_at_most_once() {
        let state = state_root();
        let fixture = Fixture::new();
        fixture.profile('a', "debug", 4096, 2000);
        let limit = limits(u64::MAX / 2, 1);
        let mut plenty_free = plenty;
        let at = now();
        let first = reclaim_with(
            state.path(),
            fixture.home.path(),
            limit,
            300,
            &mut plenty_free,
            at,
        );
        assert!(first.is_some(), "no earlier walk: survey");
        let again = reclaim_with(
            state.path(),
            fixture.home.path(),
            limit,
            300,
            &mut plenty_free,
            at + 60_000_000_000,
        );
        assert!(again.is_none(), "within the interval: no second walk");
        // Past the interval, or with the clock moved back, it walks again.
        let later = reclaim_with(
            state.path(),
            fixture.home.path(),
            limit,
            300,
            &mut plenty_free,
            at + 301_000_000_000,
        );
        assert!(later.is_some());
        let skewed = reclaim_with(
            state.path(),
            fixture.home.path(),
            limit,
            300,
            &mut plenty_free,
            at,
        );
        assert!(skewed.is_some(), "a record from the future is stale");
    }

    #[test]
    fn a_breached_floor_or_a_near_cap_total_forces_a_walk() {
        let state = state_root();
        let fixture = Fixture::new();
        fixture.profile('a', "debug", 4096, 2000);
        let at = now();
        let roomy = limits(u64::MAX / 2, 1);
        let mut plenty_free = plenty;
        assert!(
            reclaim_with(
                state.path(),
                fixture.home.path(),
                roomy,
                300,
                &mut plenty_free,
                at
            )
            .is_some()
        );
        assert!(
            reclaim_with(
                state.path(),
                fixture.home.path(),
                roomy,
                300,
                &mut plenty_free,
                at + 1
            )
            .is_none()
        );
        let mut none_free = || Ok(0_u64);
        assert!(
            reclaim_with(
                state.path(),
                fixture.home.path(),
                roomy,
                300,
                &mut none_free,
                at + 2
            )
            .is_some(),
            "floor breached"
        );
        let mut unreadable = || Err(io::Error::other("statvfs"));
        assert!(
            reclaim_with(
                state.path(),
                fixture.home.path(),
                roomy,
                300,
                &mut unreadable,
                at + 3
            )
            .is_some()
        );
        // The last total is within a tenth of a tiny cap.
        let tight = limits(1, 1);
        assert!(
            reclaim_with(
                state.path(),
                fixture.home.path(),
                tight,
                300,
                &mut plenty_free,
                at + 4
            )
            .is_some()
        );
    }

    #[test]
    fn a_concurrent_walk_makes_a_settlement_skip() {
        let state = state_root();
        let fixture = Fixture::new();
        fixture.profile('a', "debug", 4096, 2000);
        let anchor = Directory::open_anchor(state.path()).unwrap();
        let _walking = anchor
            .try_lock(OsStr::new(SURVEY_LOCK), LockMode::Exclusive)
            .unwrap();
        let mut none_free = || Ok(0_u64);
        let skipped = reclaim_with(
            state.path(),
            fixture.home.path(),
            limits(u64::MAX / 2, 1),
            300,
            &mut none_free,
            now(),
        );
        assert!(skipped.is_none());
    }
}
