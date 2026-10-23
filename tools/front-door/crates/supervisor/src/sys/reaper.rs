//! Fixed-capacity utility cleanup. No global state and no destructor signalling.
use std::io;
use std::process::Child;
#[cfg(feature = "fixture")]
use std::sync::Condvar;
#[cfg(feature = "fixture")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
#[cfg(feature = "fixture")]
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
    #[cfg(feature = "fixture")]
    paused: AtomicBool,
    #[cfg(feature = "fixture")]
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
/// Compose one shared executor at the process edge; clones share admission.
#[derive(Clone, Debug)]
pub(crate) struct Reaper(Arc<State>);
#[derive(Debug)]
pub(crate) struct Permit {
    state: Arc<State>,
    slot: usize,
    transferred: bool,
}
impl Reaper {
    pub(crate) fn new(capacity: usize) -> io::Result<Self> {
        if capacity == 0 || capacity > 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let state = Arc::new(State {
            slots: Mutex::new((0..capacity).map(|_| Slot::Free).collect()),
            #[cfg(feature = "fixture")]
            paused: AtomicBool::new(false),
            #[cfg(feature = "fixture")]
            changed: Condvar::new(),
        });
        let worker = state.clone();
        std::thread::Builder::new()
            .name("utility-reaper".into())
            .spawn(move || {
                loop {
                    #[cfg(feature = "fixture")]
                    if worker.paused.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    let empty = {
                        let mut slots = worker.slots();
                        for slot in slots.iter_mut() {
                            if let Slot::Child(child) = slot
                                && matches!(child.try_wait(), Ok(Some(_)))
                            {
                                *slot = Slot::Free;
                            }
                        }
                        #[cfg(feature = "fixture")]
                        worker.changed.notify_all();
                        slots.iter().all(|slot| matches!(slot, Slot::Free))
                    };
                    if empty && Arc::strong_count(&worker) == 1 {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })?;
        Ok(Self(state))
    }
    #[cfg(feature = "fixture")]
    pub(crate) fn new_paused(capacity: usize) -> io::Result<Self> {
        let reaper = Self::new(capacity)?;
        reaper.0.paused.store(true, Ordering::Release);
        Ok(reaper)
    }
    pub(crate) fn reserve(&self) -> Option<Permit> {
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
    pub(crate) fn occupied(&self) -> usize {
        self.0
            .slots()
            .iter()
            .filter(|slot| !matches!(slot, Slot::Free))
            .count()
    }
    #[cfg(feature = "fixture")]
    pub(crate) fn occupied_fixture(&self) -> usize {
        self.0
            .slots()
            .iter()
            .filter(|slot| !matches!(slot, Slot::Free))
            .count()
    }
    #[cfg(feature = "fixture")]
    pub(crate) fn resume(&self) {
        self.0.paused.store(false, Ordering::Release);
    }
    #[cfg(feature = "fixture")]
    pub(crate) fn wait_for_occupied(&self, maximum: usize, timeout: Duration) -> io::Result<usize> {
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
}
impl Permit {
    pub(crate) fn transfer(mut self, child: Child) {
        // slot is reserved before spawn, never resized or released by a peer.
        // Its index cannot fail; match avoids a panic if an invariant regresses.
        if let Some(slot) = self.state.slots().get_mut(self.slot) {
            *slot = Slot::Child(child);
            self.transferred = true;
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if !self.transferred
            && let Some(slot) = self.state.slots().get_mut(self.slot)
        {
            *slot = Slot::Free;
        }
    }
}
