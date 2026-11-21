//! Scoped process-group signal forwarding used by the build runtime.

use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicI32, Ordering},
};

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];
const GROUP_CAPACITY: usize = 64;
static GROUPS: [AtomicI32; GROUP_CAPACITY] = [const { AtomicI32::new(0) }; GROUP_CAPACITY];
static GROUP_SIGNALS: [AtomicI32; GROUP_CAPACITY] = [const { AtomicI32::new(0) }; GROUP_CAPACITY];

struct State {
    active: usize,
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            active: 0,
            previous: Vec::new(),
        })
    })
}

extern "C" fn handle(signal: libc::c_int) {
    for index in 0..GROUP_CAPACITY {
        let group = GROUPS[index].load(Ordering::SeqCst);
        if group != 0 {
            GROUP_SIGNALS[index]
                .compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
        }
        if group > 0 {
            // SAFETY: kill is async-signal-safe and the target is a live
            // process-group id published by the owner before its child runs.
            unsafe { libc::kill(-group, signal) };
        }
    }
}

/// Signal forwarding registration bound to one private process group.
pub(crate) struct Forwarder {
    index: usize,
}

impl Forwarder {
    pub(crate) fn install() -> std::io::Result<Self> {
        let mut state = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active == 0 {
            let mut previous = Vec::new();
            for signal in SIGNALS {
                // SAFETY: zero is a valid initial sigaction structure and all
                // pointers passed to sigaction refer to live local values.
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = handle as extern "C" fn(libc::c_int) as usize;
                // SAFETY: sa_mask is a live field of the action above.
                unsafe { libc::sigemptyset(&mut action.sa_mask) };
                // SAFETY: old receives the prior action for this signal.
                let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
                // SAFETY: sigaction reads action and writes old for a valid signal.
                if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
                    let error = std::io::Error::last_os_error();
                    for (restored_signal, restored_action) in previous.iter().rev() {
                        // SAFETY: restores actions saved by the calls above.
                        unsafe {
                            libc::sigaction(*restored_signal, restored_action, std::ptr::null_mut())
                        };
                    }
                    return Err(error);
                }
                previous.push((signal, old));
            }
            state.previous = previous;
        }
        let Some(index) = GROUPS
            .iter()
            .position(|group| group.load(Ordering::SeqCst) == 0)
        else {
            if state.active == 0 {
                restore(&mut state);
            }
            return Err(std::io::Error::other(
                "build signal forwarding capacity reached",
            ));
        };
        GROUP_SIGNALS[index].store(0, Ordering::SeqCst);
        GROUPS[index].store(-1, Ordering::SeqCst);
        state.active += 1;
        Ok(Self { index })
    }

    /// Bind the forwarding slot to the process-group leader just spawned.
    pub(crate) fn child(&self, pgid: i32) {
        GROUPS[self.index].store(pgid, Ordering::SeqCst);
        let signal = GROUP_SIGNALS[self.index].load(Ordering::SeqCst);
        if signal != 0 {
            // SAFETY: the group id was returned by the child just spawned for
            // this slot and the signal came from the scoped forwarding hook.
            unsafe { libc::kill(-pgid, signal) };
        }
    }

    pub(crate) fn signal_received(&self) -> bool {
        self.signal() != 0
    }

    pub(crate) fn signal(&self) -> i32 {
        GROUP_SIGNALS[self.index].load(Ordering::SeqCst)
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        GROUPS[self.index].store(0, Ordering::SeqCst);
        GROUP_SIGNALS[self.index].store(0, Ordering::SeqCst);
        let mut state = state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            restore(&mut state);
        }
    }
}

fn restore(state: &mut State) {
    for (signal, action) in state.previous.drain(..).rev() {
        // SAFETY: this restores a sigaction captured when forwarding was first
        // installed; the null output pointer is permitted when old is ignored.
        unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
    }
}
