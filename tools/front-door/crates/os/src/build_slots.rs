//! Host-wide build slots: cooperative fair sharing of CPU and memory among
//! the lanes that share one state directory. This is a build throttle, not a
//! security boundary and not a hard cap.
//!
//! Each slot is a file the host (Provenant, outside any sandbox) pre-creates
//! with `provision`. A lane is granted read access to the directory and its
//! files only, so it can neither create, unlink nor replace a slot file. The
//! shim opens a slot file read-only and takes `flock` on it; flock works on a
//! read-only descriptor. The descriptor is close-on-exec, so only the shim
//! holds the slot and a daemon or orphaned grandchild never keeps it. The
//! kernel drops the lock when the shim exits, even after SIGKILL.
//!
//! A lane can still hold a slot by locking the file on purpose. That is
//! accepted: the aim is fair sharing among our own lanes.
use crate::fs::{Directory, FsError, LockGuard, LockMode, Result};
use crate::sys::fs as os;
use provenant_core::CancelToken;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Slot directory below the host state root. It sits beside `fabric/`, not
/// inside `fabric/locks`: a lane writes its landing locks under
/// `fabric/locks`, and a slot file a lane can unlink is no throttle at all.
pub const DIRECTORY: [&str; 1] = ["build-slots"];
const POLL: Duration = Duration::from_millis(100);
/// Default time a build waits for a slot before giving up.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30 * 60);
/// How often a waiting build repeats its progress message.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(60);

/// File a gated build writes in the lane's private temporary directory while
/// it waits, so the host can show the wait. It holds a kind and the host's
/// wall-clock seconds and is refreshed about every `PROGRESS_EVERY`, so a
/// build that died while waiting stops showing once the file goes stale.
pub const WAIT_MARKER: &str = "build-slot-wait";
/// How long after its last refresh a wait marker still counts.
pub const WAIT_MARKER_FRESH: Duration = Duration::from_secs(150);

/// What a gated build is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitKind {
    Slot,
    CargoLock,
}
impl WaitKind {
    /// The marker body for a wait observed at `now_secs`.
    pub fn marker(self, now_secs: u64) -> String {
        let name = match self {
            Self::Slot => "slot",
            Self::CargoLock => "lock",
        };
        format!("{name} {now_secs}")
    }
    /// The wait a marker body names, if it is well formed and fresh at
    /// `now_secs`. A lane writes this text, so anything else is ignored.
    pub fn from_marker(text: &str, now_secs: u64) -> Option<Self> {
        let (name, at) = text.trim().split_once(' ')?;
        let at: u64 = at.parse().ok()?;
        if now_secs.saturating_sub(at) > WAIT_MARKER_FRESH.as_secs() {
            return None;
        }
        match name {
            "slot" => Some(Self::Slot),
            "lock" => Some(Self::CargoLock),
            _ => None,
        }
    }
}

fn directory(state: &Path) -> PathBuf {
    DIRECTORY.iter().fold(state.to_owned(), |p, n| p.join(n))
}

fn slot_name(index: u64) -> OsString {
    format!("slot-{index}.lock").into()
}

/// How long `acquire` waits and how often it reports.
#[derive(Clone, Copy, Debug)]
pub struct Wait {
    pub timeout: Duration,
    pub progress_every: Duration,
}
impl Wait {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            progress_every: PROGRESS_EVERY,
        }
    }
}

/// Why `acquire` did not produce a slot.
#[derive(Debug)]
pub enum SlotError {
    /// Every slot stayed held for the whole wait.
    Timeout {
        limit: u64,
        held: u64,
        waited: Duration,
    },
    Cancelled,
    Fs(FsError),
}
impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout {
                limit: 0, waited, ..
            } => write!(
                f,
                "a Cargo build lock stayed held for {} s",
                waited.as_secs()
            ),
            Self::Timeout {
                limit,
                held,
                waited,
            } => write!(
                f,
                "all {limit} build slots stayed busy ({held} held) for {} s",
                waited.as_secs()
            ),
            Self::Cancelled => f.write_str("the wait for a build slot was cancelled"),
            Self::Fs(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for SlotError {}
impl From<FsError> for SlotError {
    fn from(error: FsError) -> Self {
        Self::Fs(error)
    }
}

/// An open slot directory, read-only for the lane.
pub struct BuildSlots {
    dir: Directory,
}

impl BuildSlots {
    /// Host side: create the slot directory (mode 0700) and `slot-0..limit`
    /// lock files (mode 0600), and repair what is there: a directory with
    /// the wrong mode gets 0700, and a slot file that is missing, not a
    /// regular file, owned by another user, shared by a hard link or with the
    /// wrong mode is recreated or corrected. A directory owned by another user
    /// is refused, since only its owner may repair it. Never called from a
    /// lane.
    pub fn provision(state: &Path, limit: u64) -> std::io::Result<()> {
        Self::provision_owned(state, limit, crate::fs::effective_uid())
    }

    /// `provision`, refusing a directory owned by anyone but `owner`.
    fn provision_owned(state: &Path, limit: u64, owner: u32) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        let path = directory(state);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_dir() => {
                return Err(std::io::Error::other(format!(
                    "{} is not a directory",
                    path.display()
                )));
            }
            Ok(meta) if meta.uid() != owner => {
                return Err(std::io::Error::other(format!(
                    "{} is owned by uid {}, not the host user (uid {owner})",
                    path.display(),
                    meta.uid()
                )));
            }
            Ok(meta) => {
                if meta.permissions().mode() & 0o7777 != 0o700 {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            Err(_) => std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)?,
        }
        for index in 0..limit {
            let file = path.join(slot_name(index));
            match std::fs::symlink_metadata(&file) {
                Ok(meta) if meta.is_file() && meta.nlink() == 1 && meta.uid() == owner => {
                    if meta.permissions().mode() & 0o7777 != 0o600 {
                        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
                    }
                    continue;
                }
                Ok(meta) if meta.is_dir() => std::fs::remove_dir(&file)?,
                Ok(_) => std::fs::remove_file(&file)?,
                Err(_) => {}
            }
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&file)?;
        }
        Ok(())
    }

    /// Open the slot directory without creating anything.
    pub fn open(state: &Path) -> Result<Self> {
        Ok(Self {
            dir: Directory::open_anchor(&directory(state))?,
        })
    }

    /// Take the first free slot below `limit`, or none when all are held. A
    /// missing slot file is skipped; when every file is missing that is an
    /// error, since no slot can ever be taken.
    pub fn try_acquire(&self, limit: u64) -> Result<Option<SlotGuard>> {
        let mut missing = None;
        let mut absent = 0;
        for index in 0..limit {
            match self
                .dir
                .try_lock_existing(&slot_name(index), LockMode::Exclusive)
            {
                Ok(guard) => return Ok(Some(SlotGuard { guard, index })),
                Err(FsError::LockTimeout { .. }) => {}
                Err(error) if error.is_not_found() => {
                    absent += 1;
                    missing = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        match missing {
            Some(error) if absent == limit => Err(error),
            _ => Ok(None),
        }
    }

    /// Wait for a slot below `limit` for at most `wait.timeout`. `progress`
    /// runs with the number of slots held and the time waited when the first
    /// try fails, then every `wait.progress_every`.
    #[allow(clippy::disallowed_methods)]
    pub fn acquire(
        &self,
        limit: u64,
        wait: Wait,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(u64, Duration),
    ) -> std::result::Result<SlotGuard, SlotError> {
        if let Some(guard) = self.try_acquire(limit)? {
            return Ok(guard);
        }
        let start = std::time::Instant::now();
        let mut reported = Duration::ZERO;
        progress(self.held(limit), Duration::ZERO);
        loop {
            if cancel.is_cancelled() {
                return Err(SlotError::Cancelled);
            }
            os::wait(POLL);
            if let Some(guard) = self.try_acquire(limit)? {
                return Ok(guard);
            }
            let waited = start.elapsed();
            if waited >= wait.timeout {
                return Err(SlotError::Timeout {
                    limit,
                    held: self.held(limit),
                    waited,
                });
            }
            if waited - reported >= wait.progress_every {
                reported = waited;
                progress(self.held(limit), waited);
            }
        }
    }

    /// How many of the first `limit` slots are held now.
    pub fn held(&self, limit: u64) -> u64 {
        (0..limit)
            .filter(|index| {
                matches!(
                    self.dir
                        .try_lock_existing(&slot_name(*index), LockMode::Shared),
                    Err(FsError::LockTimeout { .. })
                )
            })
            .count() as u64
    }
}

/// Lock files Cargo holds in a profile directory while it builds there.
const CARGO_LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// Whether another process holds a Cargo build lock below `target`: one of
/// `CARGO_LOCKS` in a profile directory (`target/debug`) or in a target
/// triple's (`target/<triple>/debug`). Cargo queues a second build behind that
/// lock, so a caller that waits for it first holds no build slot meanwhile.
pub fn cargo_lock_held(target: &Path) -> bool {
    let held = |profile: PathBuf| {
        CARGO_LOCKS.iter().any(|name| {
            std::fs::File::open(profile.join(name)).is_ok_and(|file| {
                matches!(
                    os::try_flock(&file, LockMode::Shared),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock
                )
            })
        })
    };
    let Ok(top) = std::fs::read_dir(target) else {
        return false;
    };
    top.flatten().any(|entry| {
        let path = entry.path();
        held(path.clone())
            || std::fs::read_dir(&path)
                .is_ok_and(|inner| inner.flatten().any(|child| held(child.path())))
    })
}

/// Wait until no process holds a Cargo build lock below `target`, for at most
/// `wait.timeout`. `progress` runs with the time waited when the first probe
/// finds a lock held, then every `wait.progress_every`. A timeout reports
/// `limit` and `held` as 0: it is not a slot wait.
#[allow(clippy::disallowed_methods)]
pub fn wait_for_cargo_lock(
    target: &Path,
    wait: Wait,
    progress: &mut dyn FnMut(Duration),
) -> std::result::Result<(), SlotError> {
    if !cargo_lock_held(target) {
        return Ok(());
    }
    let start = std::time::Instant::now();
    let mut reported = Duration::ZERO;
    progress(Duration::ZERO);
    while cargo_lock_held(target) {
        os::wait(POLL);
        let waited = start.elapsed();
        if waited >= wait.timeout {
            return Err(SlotError::Timeout {
                limit: 0,
                held: 0,
                waited,
            });
        }
        if waited - reported >= wait.progress_every {
            reported = waited;
            progress(waited);
        }
    }
    Ok(())
}

/// One held slot. Dropping it, or the process exiting, releases the slot. The
/// descriptor is close-on-exec, so a child never inherits it.
#[derive(Debug)]
pub struct SlotGuard {
    #[allow(dead_code, reason = "the open descriptor is the held lock")]
    guard: LockGuard,
    index: u64,
}
impl SlotGuard {
    pub fn index(&self) -> u64 {
        self.index
    }

    /// Whether the slot descriptor is close-on-exec.
    pub fn close_on_exec(&self) -> bool {
        os::is_cloexec(self.guard.handoff_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Forking while another test holds a lock lets the child keep a copy of
    /// that lock until it execs, so these tests run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn state() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // A lock directory must be private to its owner.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn provisioned(limit: u64) -> (tempfile::TempDir, BuildSlots) {
        let state = state();
        BuildSlots::provision(state.path(), limit).unwrap();
        let slots = BuildSlots::open(state.path()).unwrap();
        (state, slots)
    }

    #[test]
    fn slots_are_bounded_and_reusable() {
        let _serial = serial();
        let (_state, slots) = provisioned(2);
        let first = slots.try_acquire(2).unwrap().unwrap();
        let second = slots.try_acquire(2).unwrap().unwrap();
        assert_ne!(first.index(), second.index());
        assert!(slots.try_acquire(2).unwrap().is_none());
        assert_eq!(slots.held(2), 2);
        drop(first);
        assert_eq!(slots.held(2), 1);
        assert!(slots.try_acquire(2).unwrap().is_some());
    }

    #[test]
    fn contention_never_exceeds_the_limit() {
        let _serial = serial();
        let (state, _slots) = provisioned(2);
        let running = AtomicU64::new(0);
        let peak = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    let slots = BuildSlots::open(state.path()).unwrap();
                    let _slot = slots
                        .acquire(
                            2,
                            Wait::new(Duration::from_secs(60)),
                            &CancelToken::new(),
                            &mut |_, _| {},
                        )
                        .unwrap();
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    os::wait(Duration::from_millis(150));
                    running.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn read_only_slot_files_cannot_be_created_unlinked_or_replaced_yet_still_lock() {
        let _serial = serial();
        let (state, slots) = provisioned(2);
        let dir = directory(state.path());
        // The access a lane has: read the directory and its files, write
        // neither. A same-user process stands in for the sandbox.
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o400)).unwrap();
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let slot = dir.join("slot-0.lock");
        assert!(std::fs::File::create(dir.join("slot-9.lock")).is_err());
        assert!(std::fs::remove_file(&slot).is_err());
        assert!(std::fs::rename(&slot, dir.join("moved.lock")).is_err());
        assert!(
            std::fs::OpenOptions::new().write(true).open(&slot).is_err(),
            "the slot file is not writable"
        );
        // The read-only descriptor still takes the exclusive lock.
        let first = slots.try_acquire(2).unwrap().unwrap();
        let second = slots.try_acquire(2).unwrap().unwrap();
        assert_ne!(first.index(), second.index());
        assert!(slots.try_acquire(2).unwrap().is_none());
        drop((first, second));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn the_slot_descriptor_is_close_on_exec() {
        let _serial = serial();
        let (_state, slots) = provisioned(1);
        assert!(slots.try_acquire(1).unwrap().unwrap().close_on_exec());
    }

    #[test]
    fn a_process_that_outlives_the_holder_does_not_keep_the_slot() {
        let _serial = serial();
        let (_state, slots) = provisioned(1);
        let slot = slots.try_acquire(1).unwrap().unwrap();
        // A backgrounded grandchild, as a daemon or orphaned test process.
        #[allow(clippy::disallowed_methods)]
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(slots.try_acquire(1).unwrap().is_none());
        drop(slot);
        assert!(
            slots.try_acquire(1).unwrap().is_some(),
            "the running child holds no copy of the slot"
        );
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_crashed_holder_releases_its_slot() {
        let _serial = serial();
        let (_state, slots) = provisioned(1);
        drop(slots.try_acquire(1).unwrap().unwrap());
        assert!(slots.try_acquire(1).unwrap().is_some());
    }

    #[test]
    fn a_wait_reports_progress_then_times_out_naming_the_slots() {
        let _serial = serial();
        let (_state, slots) = provisioned(1);
        let _held = slots.try_acquire(1).unwrap().unwrap();
        let mut reports = Vec::new();
        let error = slots
            .acquire(
                1,
                Wait {
                    timeout: Duration::from_millis(700),
                    progress_every: Duration::from_millis(200),
                },
                &CancelToken::new(),
                &mut |held, waited| reports.push((held, waited)),
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                SlotError::Timeout {
                    limit: 1,
                    held: 1,
                    ..
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("all 1 build slots"), "{error}");
        assert!(reports.len() >= 3, "{reports:?}");
        assert_eq!(reports[0], (1, Duration::ZERO));
    }

    #[test]
    fn cancel_ends_a_wait() {
        let _serial = serial();
        let (_state, slots) = provisioned(1);
        let _held = slots.try_acquire(1).unwrap().unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let error = slots
            .acquire(
                1,
                Wait::new(Duration::from_secs(60)),
                &cancel,
                &mut |_, _| {},
            )
            .unwrap_err();
        assert!(matches!(error, SlotError::Cancelled));
    }

    #[test]
    fn provisioning_again_repairs_the_directory_and_its_slot_files() {
        use std::os::unix::fs::MetadataExt;
        let _serial = serial();
        let state = state();
        BuildSlots::provision(state.path(), 4).unwrap();
        let dir = directory(state.path());
        let mode = |name: &str| std::fs::metadata(dir.join(name)).unwrap().mode() & 0o7777;
        // A loose directory, a loose file, a missing file, a file that is
        // really a symlink, and one shared by a hard link.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).unwrap();
        std::fs::set_permissions(
            dir.join("slot-0.lock"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        std::fs::remove_file(dir.join("slot-1.lock")).unwrap();
        std::fs::remove_file(dir.join("slot-2.lock")).unwrap();
        std::os::unix::fs::symlink(state.path(), dir.join("slot-2.lock")).unwrap();
        std::fs::remove_file(dir.join("slot-3.lock")).unwrap();
        std::fs::write(state.path().join("elsewhere"), b"").unwrap();
        std::fs::hard_link(state.path().join("elsewhere"), dir.join("slot-3.lock")).unwrap();
        BuildSlots::provision(state.path(), 4).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().mode() & 0o7777,
            0o700,
            "directory mode"
        );
        for name in ["slot-0.lock", "slot-1.lock", "slot-2.lock", "slot-3.lock"] {
            let meta = std::fs::symlink_metadata(dir.join(name)).unwrap();
            assert!(meta.is_file() && meta.nlink() == 1, "{name} is private");
            assert_eq!(mode(name), 0o600, "{name}");
        }
        let slots = BuildSlots::open(state.path()).unwrap();
        let held: Vec<_> = (0..4)
            .map(|_| slots.try_acquire(4).unwrap().unwrap())
            .collect();
        assert_eq!(held.len(), 4);
    }

    #[test]
    fn provisioning_refuses_a_slot_directory_owned_by_another_user() {
        let _serial = serial();
        let state = state();
        BuildSlots::provision(state.path(), 2).unwrap();
        // Only the owner may repair the directory, since the repair writes
        // there; refuse rather than leave a directory another user controls.
        let stranger = crate::fs::effective_uid().wrapping_add(1);
        let error = BuildSlots::provision_owned(state.path(), 2, stranger).unwrap_err();
        assert!(error.to_string().contains("is owned by uid"), "{error}");
        // The real owner still repairs it.
        BuildSlots::provision(state.path(), 2).unwrap();
        BuildSlots::open(state.path()).unwrap();
    }

    #[test]
    fn missing_slot_files_are_an_error_not_a_free_slot() {
        let _serial = serial();
        let state = state();
        BuildSlots::provision(state.path(), 1).unwrap();
        let slots = BuildSlots::open(state.path()).unwrap();
        // Asking for more slots than were provisioned skips the absent ones.
        assert!(slots.try_acquire(3).unwrap().is_some());
        std::fs::remove_file(directory(state.path()).join("slot-0.lock")).unwrap();
        assert!(slots.try_acquire(3).is_err());
    }

    #[test]
    fn a_held_cargo_lock_is_seen_in_profile_and_triple_directories() {
        let _serial = serial();
        let target = tempfile::tempdir().unwrap();
        assert!(!cargo_lock_held(target.path()));
        for (relative, name) in [
            ("debug", ".cargo-lock"),
            ("aarch64-apple-darwin/debug", ".cargo-lock"),
            ("release", ".cargo-build-lock"),
            ("release", ".cargo-artifact-lock"),
        ] {
            let profile = target.path().join(relative);
            std::fs::create_dir_all(&profile).unwrap();
            let lock = std::fs::File::create(profile.join(name)).unwrap();
            assert!(!cargo_lock_held(target.path()), "an unheld lock is free");
            os::try_flock(&lock, LockMode::Exclusive).unwrap();
            assert!(cargo_lock_held(target.path()), "{relative}/{name}");
            drop(lock);
            assert!(!cargo_lock_held(target.path()));
        }
        assert!(!cargo_lock_held(&target.path().join("missing")));
    }

    #[test]
    fn waiting_for_a_cargo_lock_reports_progress_and_times_out() {
        let _serial = serial();
        let target = tempfile::tempdir().unwrap();
        let profile = target.path().join("debug");
        std::fs::create_dir_all(&profile).unwrap();
        let lock = std::fs::File::create(profile.join(".cargo-lock")).unwrap();
        os::try_flock(&lock, LockMode::Exclusive).unwrap();
        let mut reports = Vec::new();
        let error = wait_for_cargo_lock(
            target.path(),
            Wait {
                timeout: Duration::from_millis(600),
                // Six intervals per timeout leave room for a slow scheduler tick.
                progress_every: Duration::from_millis(100),
            },
            &mut |waited| reports.push(waited),
        )
        .unwrap_err();
        assert!(matches!(error, SlotError::Timeout { .. }));
        assert!(reports.len() >= 3, "{reports:?}");
        drop(lock);
        wait_for_cargo_lock(
            target.path(),
            Wait::new(Duration::from_secs(1)),
            &mut |_| {},
        )
        .unwrap();
    }

    #[test]
    fn opening_never_creates_the_directory() {
        let _serial = serial();
        let state = state();
        assert!(BuildSlots::open(state.path()).is_err());
        assert!(!state.path().join(DIRECTORY.join("/")).exists());
    }
}
