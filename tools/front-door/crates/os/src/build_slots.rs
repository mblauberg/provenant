//! Host-wide build semaphore. Each slot is a file lock under the host state
//! root, so every project and lane that shares the state directory contends
//! for the same slots. The kernel drops a flock when its last descriptor
//! closes, even after SIGKILL, so a crashed build never strands a slot.
//!
//! A holder keeps its slot for as long as any process holds the descriptor.
//! Handing it across exec (`SlotGuard::inherit`) lets the build replace the
//! process that took the slot while the slot stays held until the build and
//! its children exit.
//!
//! A process that has to wait registers a `wait-<attempt>-<pid>.lock` file and
//! locks it for the duration of the wait. Observers read a held waiter lock as
//! a live waiter and a free one as a leftover from a crashed process.
use crate::fs::{Directory, FsError, LockGuard, LockMode, Result};
use crate::sys::fs as os;
use provenant_core::CancelToken;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Slot directory below the host state root. `fabric/locks` is already the
/// one writable lock area every lane is granted, so no wider grant is needed.
pub const DIRECTORY: [&str; 3] = ["fabric", "locks", "build-slots"];
const WAIT_PREFIX: &str = "wait-";
const CREATE_TRIES: usize = 4;
const POLL: Duration = Duration::from_millis(100);

fn slot_name(index: u64) -> OsString {
    format!("slot-{index}.lock").into()
}

/// Whether `attempt` is safe inside a waiter file name.
fn attempt_name(attempt: &str) -> bool {
    !attempt.is_empty()
        && attempt.len() <= 64
        && attempt
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// An open slot directory.
pub struct BuildSlots {
    dir: Directory,
    path: PathBuf,
}

impl BuildSlots {
    /// Open the slot directory under `state`, creating it (mode 0700) when
    /// missing. Lanes use this; `state` is the host state root.
    pub fn open(state: &Path) -> Result<Self> {
        let mut dir = Directory::open_anchor(state)?;
        for name in DIRECTORY {
            match dir.mkdir(name.as_ref(), 0o700) {
                Ok(()) => {}
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error),
            }
            dir = dir.open_directory(name.as_ref())?;
        }
        Ok(Self {
            dir,
            path: DIRECTORY.iter().fold(state.to_owned(), |p, n| p.join(n)),
        })
    }

    /// Open the slot directory only when it exists, never creating it.
    pub fn observe(state: &Path) -> Result<Self> {
        let mut dir = Directory::open_anchor(state)?;
        for name in DIRECTORY {
            dir = dir.open_directory(name.as_ref())?;
        }
        Ok(Self {
            dir,
            path: DIRECTORY.iter().fold(state.to_owned(), |p, n| p.join(n)),
        })
    }

    /// Take the first free slot below `limit`, or none when all are held.
    pub fn try_acquire(&self, limit: u64) -> Result<Option<SlotGuard>> {
        for index in 0..limit {
            // Two processes creating the same slot file can make one create
            // report ENOENT on macOS. The file exists by the next try.
            for attempt in 0..CREATE_TRIES {
                match self.dir.try_lock(&slot_name(index), LockMode::Exclusive) {
                    Ok(guard) => return Ok(Some(SlotGuard { guard, index })),
                    Err(FsError::LockTimeout { .. }) => break,
                    Err(error) if error.is_not_found() && attempt + 1 < CREATE_TRIES => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(None)
    }

    /// Wait for a slot below `limit`. While waiting, `waiter` (the lane's
    /// attempt id) is registered so observers can report the wait; `blocked`
    /// runs once, with the number of slots then held, when the first try fails.
    pub fn acquire(
        &self,
        limit: u64,
        waiter: Option<&str>,
        cancel: &CancelToken,
        blocked: &mut dyn FnMut(u64),
    ) -> Result<SlotGuard> {
        if let Some(guard) = self.try_acquire(limit)? {
            return Ok(guard);
        }
        blocked(self.held(limit));
        self.sweep_waiters();
        let _registration = waiter.and_then(|attempt| self.register(attempt));
        loop {
            if cancel.is_cancelled() {
                return Err(FsError::Cancelled {
                    path: self.path.clone(),
                });
            }
            os::wait(POLL);
            if let Some(guard) = self.try_acquire(limit)? {
                return Ok(guard);
            }
        }
    }

    /// How many of the first `limit` slots are held now.
    pub fn held(&self, limit: u64) -> u64 {
        (0..limit)
            .filter(|index| self.slot_held(&slot_name(*index)))
            .count() as u64
    }

    /// Whether a live process is waiting for a slot on behalf of `attempt`.
    pub fn waiting(&self, attempt: &str) -> bool {
        if !attempt_name(attempt) {
            return false;
        }
        let prefix = format!("{WAIT_PREFIX}{attempt}-");
        let Ok(entries) = self.dir.entries() else {
            return false;
        };
        entries.flatten().any(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".lock"))
                && self.slot_held(&name)
        })
    }

    /// A shared probe fails exactly while another descriptor holds the lock
    /// exclusively. A missing file is a free slot.
    fn slot_held(&self, name: &OsString) -> bool {
        matches!(
            self.dir.try_lock_existing(name, LockMode::Shared),
            Err(FsError::LockTimeout { .. })
        )
    }

    fn register(&self, attempt: &str) -> Option<Registration> {
        if !attempt_name(attempt) {
            return None;
        }
        let name: OsString = format!("{WAIT_PREFIX}{attempt}-{}.lock", std::process::id()).into();
        let guard = self.dir.try_lock(&name, LockMode::Exclusive).ok()?;
        Some(Registration {
            path: self.path.join(&name),
            _guard: guard,
        })
    }

    /// Remove waiter files whose owner died. Holding the lock proves the
    /// owner is gone; the file is removed while it is held.
    fn sweep_waiters(&self) {
        let Ok(entries) = self.dir.entries() else {
            return;
        };
        for name in entries.flatten() {
            if !name
                .to_str()
                .is_some_and(|name| name.starts_with(WAIT_PREFIX) && name.ends_with(".lock"))
            {
                continue;
            }
            if let Ok(guard) = self.dir.try_lock_existing(&name, LockMode::Exclusive) {
                // The directory API reserves `.lock` names, so unlink by path.
                // The root is host-trusted and this sweeper holds the lock.
                let _ = std::fs::remove_file(self.path.join(&name));
                drop(guard);
            }
        }
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

/// One held slot. Dropping it, or the process exiting, releases the slot.
#[derive(Debug)]
pub struct SlotGuard {
    guard: LockGuard,
    index: u64,
}
impl SlotGuard {
    pub fn index(&self) -> u64 {
        self.index
    }

    /// Keep the slot across exec: the descriptor stays open in the new
    /// process image and in its children, and the slot is held until the
    /// last of them exits.
    pub fn inherit(&self) -> io::Result<()> {
        os::inherit_on_exec(self.guard.handoff_file())
    }
}

/// A waiter's registration, removed when the wait ends.
struct Registration {
    path: PathBuf,
    _guard: LockGuard,
}
impl Drop for Registration {
    fn drop(&mut self) {
        // This registration still holds the lock, so the name is ours.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        std::fs::set_permissions(dir.path(), {
            use std::os::unix::fs::PermissionsExt;
            std::fs::Permissions::from_mode(0o700)
        })
        .unwrap();
        dir
    }

    #[test]
    fn slots_are_bounded_and_reusable() {
        let _serial = serial();
        let state = state();
        let slots = BuildSlots::open(state.path()).unwrap();
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
        let state = state();
        let running = AtomicU64::new(0);
        let peak = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    let slots = BuildSlots::open(state.path()).unwrap();
                    let _slot = slots
                        .acquire(2, None, &CancelToken::new(), &mut |_| {})
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
    fn a_crashed_holder_releases_its_slot() {
        let _serial = serial();
        let state = state();
        let slots = BuildSlots::open(state.path()).unwrap();
        let slot = slots.try_acquire(1).unwrap().unwrap();
        slot.inherit().unwrap();
        // The child inherits the descriptor; the parent's copy then closes, so
        // only the child keeps the slot, as a build that replaced the shim.
        #[allow(clippy::disallowed_methods)]
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        drop(slot);
        assert!(slots.try_acquire(1).unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(slots.try_acquire(1).unwrap().is_some());
    }

    #[test]
    fn a_waiter_is_visible_only_while_it_waits() {
        let _serial = serial();
        let state = state();
        let slots = BuildSlots::open(state.path()).unwrap();
        let held = slots.try_acquire(1).unwrap().unwrap();
        let attempt = "0b0e1d54-3c1e-4a53-8f0b-1d7c2a9a4f10";
        assert!(!slots.waiting(attempt));
        let cancel = CancelToken::new();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let slots = BuildSlots::open(state.path()).unwrap();
                let mut blocked = None;
                let slot =
                    slots.acquire(1, Some(attempt), &cancel, &mut |held| blocked = Some(held));
                (slot.map(|_| ()), blocked)
            });
            let mut seen = false;
            for _ in 0..100 {
                if slots.waiting(attempt) {
                    seen = true;
                    break;
                }
                os::wait(Duration::from_millis(20));
            }
            assert!(seen, "waiter registers while blocked");
            assert!(!slots.waiting("someone-else"));
            drop(held);
            let (slot, blocked) = waiter.join().unwrap();
            slot.unwrap();
            assert_eq!(blocked, Some(1));
        });
        assert!(!slots.waiting(attempt));
    }

    #[test]
    fn a_leftover_waiter_file_is_not_a_waiter_and_is_swept() {
        let _serial = serial();
        let state = state();
        let slots = BuildSlots::open(state.path()).unwrap();
        let attempt = "dead-attempt";
        let stale = slots.path.join(format!("{WAIT_PREFIX}{attempt}-1.lock"));
        std::fs::write(&stale, b"").unwrap();
        assert!(!slots.waiting(attempt));
        slots.sweep_waiters();
        assert!(!stale.exists());
    }

    #[test]
    fn cancel_ends_a_wait() {
        let _serial = serial();
        let state = state();
        let slots = BuildSlots::open(state.path()).unwrap();
        let _held = slots.try_acquire(1).unwrap().unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let error = slots.acquire(1, None, &cancel, &mut |_| {}).unwrap_err();
        assert!(matches!(error, FsError::Cancelled { .. }));
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
    fn observing_never_creates_the_directory() {
        let _serial = serial();
        let state = state();
        assert!(BuildSlots::observe(state.path()).is_err());
        assert!(!state.path().join("fabric").exists());
    }
}
