//! Fixed-capacity utility cleanup. One process-wide worker, no destructor signalling.
use std::io;
use std::process::Child;
#[cfg(any(test, feature = "fixture"))]
use std::sync::Condvar;
#[cfg(any(test, feature = "fixture"))]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
#[cfg(any(test, feature = "fixture"))]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar as WakeCondvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;
#[cfg(any(test, feature = "fixture"))]
use std::time::Instant;

#[derive(Debug)]
enum Slot {
    Free,
    Reserved,
    Child(Child),
}
#[derive(Debug)]
struct State {
    slots: Mutex<Vec<Slot>>,
    worker: Weak<Worker>,
    #[cfg(test)]
    unknown_outcomes: AtomicUsize,
    #[cfg(any(test, feature = "fixture"))]
    paused: AtomicBool,
    #[cfg(any(test, feature = "fixture"))]
    changed: Condvar,
}
impl State {
    fn slots(&self) -> MutexGuard<'_, Vec<Slot>> {
        // No user callback runs under this mutex. Poison cannot revoke custody.
        self.slots
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}
#[derive(Debug)]
struct Worker {
    states: Mutex<Vec<Arc<State>>>,
    wake_lock: Mutex<()>,
    wake: WakeCondvar,
}
impl Worker {
    fn states(&self) -> MutexGuard<'_, Vec<Arc<State>>> {
        self.states
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    fn wake_lock(&self) -> MutexGuard<'_, ()> {
        self.wake_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    fn notify(&self) {
        let _guard = self.wake_lock();
        self.wake.notify_one();
    }
    fn register(&self, state: Arc<State>) {
        {
            let mut states = self.states();
            if !states.iter().any(|current| Arc::ptr_eq(current, &state)) {
                states.push(state);
            }
        }
        self.notify();
    }
    fn run(self: Arc<Self>) {
        loop {
            // Keep the wake lock across the scan and into wait: a transfer
            // cannot notify between an empty scan and the worker parking.
            let guard = self.wake_lock();
            let mut has_child = false;
            {
                let mut states = self.states();
                let mut index = 0;
                while index < states.len() {
                    let state = &states[index];
                    #[cfg(any(test, feature = "fixture"))]
                    if state.paused.load(Ordering::Acquire) {
                        index += 1;
                        continue;
                    }
                    let mut slots = state.slots();
                    #[cfg(any(test, feature = "fixture"))]
                    let mut released = false;
                    for slot in slots.iter_mut() {
                        if let Slot::Child(child) = slot {
                            match child.try_wait() {
                                Ok(None) => has_child = true,
                                Ok(Some(_status)) => {
                                    #[cfg(any(test, feature = "fixture"))]
                                    record_exit_status(child.id(), _status);
                                    *slot = Slot::Free;
                                    #[cfg(any(test, feature = "fixture"))]
                                    {
                                        released = true;
                                    }
                                }
                                Err(_) => {
                                    // Status is unknown (for example ECHILD),
                                    // but retaining this slot would deadlock
                                    // future admissions forever.
                                    #[cfg(test)]
                                    state.unknown_outcomes.fetch_add(1, Ordering::Relaxed);
                                    *slot = Slot::Free;
                                    #[cfg(any(test, feature = "fixture"))]
                                    {
                                        released = true;
                                    }
                                }
                            }
                        }
                    }
                    let has_child_here = slots.iter().any(|slot| matches!(slot, Slot::Child(_)));
                    drop(slots);
                    has_child |= has_child_here;
                    #[cfg(any(test, feature = "fixture"))]
                    if released {
                        state.changed.notify_all();
                    }
                    if !has_child_here {
                        states.swap_remove(index);
                    } else {
                        index += 1;
                    }
                }
            }
            if has_child {
                let _ = self
                    .wake
                    .wait_timeout(guard, Duration::from_millis(5))
                    .unwrap_or_else(|poison| poison.into_inner());
            } else {
                let _guard = self
                    .wake
                    .wait(guard)
                    .unwrap_or_else(|poison| poison.into_inner());
            }
        }
    }
}
static WORKER: OnceLock<Mutex<Option<Arc<Worker>>>> = OnceLock::new();
fn process_worker() -> io::Result<Arc<Worker>> {
    let slot = WORKER.get_or_init(|| Mutex::new(None));
    let mut slot = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(worker) = slot.as_ref() {
        return Ok(worker.clone());
    }
    let worker = Arc::new(Worker {
        states: Mutex::new(Vec::new()),
        wake_lock: Mutex::new(()),
        wake: WakeCondvar::new(),
    });
    let thread_worker = worker.clone();
    std::thread::Builder::new()
        .name("utility-reaper".into())
        .spawn(move || thread_worker.run())?;
    *slot = Some(worker.clone());
    Ok(worker)
}
/// Compose one shared executor at the process edge; clones share admission.
#[derive(Clone, Debug)]
pub struct Reaper(Arc<State>);
#[derive(Debug)]
pub struct Permit {
    state: Arc<State>,
    slot: usize,
    transferred: bool,
}
impl Reaper {
    pub fn new(capacity: usize) -> io::Result<Self> {
        if capacity == 0 || capacity > 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let worker = process_worker()?;
        let state = Arc::new(State {
            slots: Mutex::new((0..capacity).map(|_| Slot::Free).collect()),
            worker: Arc::downgrade(&worker),
            #[cfg(test)]
            unknown_outcomes: AtomicUsize::new(0),
            #[cfg(any(test, feature = "fixture"))]
            paused: AtomicBool::new(false),
            #[cfg(any(test, feature = "fixture"))]
            changed: Condvar::new(),
        });
        Ok(Self(state))
    }
    #[cfg(any(test, feature = "fixture"))]
    pub fn new_paused(capacity: usize) -> io::Result<Self> {
        let reaper = Self::new(capacity)?;
        reaper.0.paused.store(true, Ordering::Release);
        Ok(reaper)
    }
    pub fn reserve(&self) -> Option<Permit> {
        let mut slots = self.0.slots();
        let slot = slots.iter().position(|slot| matches!(slot, Slot::Free))?;
        *slots.get_mut(slot)? = Slot::Reserved;
        Some(Permit {
            state: self.0.clone(),
            slot,
            transferred: false,
        })
    }
    #[cfg(test)]
    pub fn occupied(&self) -> usize {
        self.0
            .slots()
            .iter()
            .filter(|slot| !matches!(slot, Slot::Free))
            .count()
    }
    #[cfg(any(test, feature = "fixture"))]
    pub fn occupied_fixture(&self) -> usize {
        self.0
            .slots()
            .iter()
            .filter(|slot| !matches!(slot, Slot::Free))
            .count()
    }
    #[cfg(any(test, feature = "fixture"))]
    pub fn resume(&self) {
        self.0.paused.store(false, Ordering::Release);
        if let Some(worker) = self.0.worker.upgrade() {
            worker.notify();
        }
    }
    #[cfg(any(test, feature = "fixture"))]
    pub fn wait_for_occupied(&self, maximum: usize, timeout: Duration) -> io::Result<usize> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut slots = self.0.slots();
        loop {
            let occupied = slots
                .iter()
                .filter(|slot| !matches!(slot, Slot::Free))
                .count();
            if occupied <= maximum {
                return Ok(occupied);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            let (next, timed) = self
                .0
                .changed
                .wait_timeout(slots, remaining)
                .unwrap_or_else(|poison| poison.into_inner());
            slots = next;
            if timed.timed_out() {
                let occupied = slots
                    .iter()
                    .filter(|slot| !matches!(slot, Slot::Free))
                    .count();
                if occupied > maximum {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
            }
        }
    }
    #[cfg(test)]
    pub fn unknown_outcomes(&self) -> usize {
        self.0.unknown_outcomes.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    pub fn shares_process_worker(&self, other: &Self) -> bool {
        self.0
            .worker
            .upgrade()
            .zip(other.0.worker.upgrade())
            .is_some_and(|(left, right)| Arc::ptr_eq(&left, &right))
    }
}
impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(worker) = self.0.worker.upgrade() {
            worker.notify();
        }
    }
}
impl Permit {
    pub fn transfer(mut self, child: Child) {
        // slot is reserved before spawn, never resized or released by a peer.
        // Its index cannot fail; match avoids a panic if an invariant regresses.
        let transferred = {
            let mut slots = self.state.slots();
            if let Some(slot) = slots.get_mut(self.slot) {
                *slot = Slot::Child(child);
                true
            } else {
                false
            }
        };
        if transferred {
            self.transferred = true;
        }
        if transferred && let Some(worker) = self.state.worker.upgrade() {
            worker.register(self.state.clone());
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if !self.transferred {
            let freed = {
                let mut slots = self.state.slots();
                if let Some(slot) = slots.get_mut(self.slot) {
                    *slot = Slot::Free;
                    true
                } else {
                    false
                }
            };
            if freed {
                #[cfg(any(test, feature = "fixture"))]
                self.state.changed.notify_all();
            }
        }
    }
}

// Test-only, bounded observations from the actual owning wait path.
#[cfg(any(test, feature = "fixture"))]
static EXIT_STATUSES: Mutex<std::collections::VecDeque<(u32, std::process::ExitStatus)>> =
    Mutex::new(std::collections::VecDeque::new());

#[cfg(any(test, feature = "fixture"))]
fn record_exit_status(pid: u32, status: std::process::ExitStatus) {
    let mut statuses = EXIT_STATUSES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if statuses.len() == 128 {
        statuses.pop_front();
    }
    statuses.push_back((pid, status));
}

#[cfg(any(test, feature = "fixture"))]
pub fn take_exit_status(pid: u32) -> Option<std::process::ExitStatus> {
    let mut statuses = EXIT_STATUSES
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let index = statuses.iter().rposition(|(seen, _)| *seen == pid)?;
    statuses.remove(index).map(|(_, status)| status)
}
