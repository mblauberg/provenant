//! Fixed-capacity utility cleanup. No global state and no destructor signalling.
use std::io;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug)]
enum Slot {
    Free,
    Reserved,
    Child(Child),
}
#[derive(Debug)]
struct State {
    slots: Mutex<Vec<Slot>>,
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
        });
        let worker = state.clone();
        std::thread::Builder::new()
            .name("utility-reaper".into())
            .spawn(move || loop {
                let empty = {
                    let mut slots = worker.slots();
                    for slot in slots.iter_mut() {
                        // Fair nonblocking observations, errors retain ownership.
                        if let Slot::Child(child) = slot {
                            if matches!(child.try_wait(), Ok(Some(_))) {
                                *slot = Slot::Free;
                            }
                        }
                    }
                    slots.iter().all(|slot| matches!(slot, Slot::Free))
                };
                if empty && Arc::strong_count(&worker) == 1 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            })?;
        Ok(Self(state))
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
        if !self.transferred {
            if let Some(slot) = self.state.slots().get_mut(self.slot) {
                *slot = Slot::Free;
            }
        }
    }
}
