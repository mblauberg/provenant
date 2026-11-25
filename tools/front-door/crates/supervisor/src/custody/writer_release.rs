//! Bounded writer occupancy waits for a release already in progress.
use super::{
    LaunchState, RECORD_LIMIT, Reservation, Result, StopIntentState, read_record, read_stop_intent,
    validate_anchor, writer_admission_guard,
};
use provenant_core::{CancelToken, Clock, WaitBudget};
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode};
use std::ffi::OsStr;
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "fixture"))]
thread_local! {
    pub(crate) static WRITER_LOCK_WAIT_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether a writer slot has a reservation currently being released. A scoped
/// stop intent or the guardian's `Settling` publication marks release, and the
/// guardian keeps `writer.lock` until settlement clears the reservation, so a relaunch in
/// that window is waiting on a release that is already in progress. Unreadable
/// evidence is not treated as a release: the ordinary bounded refusal still
/// applies and no candidate gains custody from it.
pub fn release_in_progress(slot: &Directory) -> bool {
    let Ok(entries) = slot.entries() else {
        return false;
    };
    for (index, name) in entries.enumerate() {
        if index >= 10_000 {
            return false;
        }
        let Ok(name) = name else {
            return false;
        };
        let Ok(metadata) = slot.metadata_at(&name, false) else {
            continue;
        };
        if metadata.kind != provenant_os::fs::FileKind::Directory {
            continue;
        }
        let Ok(child) = slot.open_verified_directory(&name) else {
            continue;
        };
        let Ok(reservation) = read_record::<Reservation>(&child, "reservation.json", RECORD_LIMIT)
        else {
            continue;
        };
        if validate_anchor(&child, &reservation).is_err() {
            continue;
        }
        if reservation.launch_state == LaunchState::Settling {
            return true;
        }
        let (Some(path), Some(requester)) = (&reservation.attempt_path, reservation.requester)
        else {
            continue;
        };
        let Ok(attempt) = Directory::open_anchor(path) else {
            continue;
        };
        let Ok(metadata) = attempt.metadata() else {
            continue;
        };
        if reservation.attempt_device != Some(metadata.device)
            || reservation.attempt_inode != Some(metadata.inode)
        {
            continue;
        }
        if matches!(read_stop_intent(&attempt, reservation.scope), Ok(StopIntentState::Requested(intent)) if intent.requester == requester)
        {
            return true;
        }
    }
    false
}

/// Acquire shared worktree occupancy without keeping the observation gate
/// while another holder owns the lease. Only a release already in progress
/// justifies spending the caller's wait; a foreign live holder still refuses
/// on the first probe.
#[allow(clippy::disallowed_methods)] // The bounded wait must expire even with a stopped caller clock.
pub fn lock_writer_occupancy(
    directory: &Directory,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<LockGuard> {
    lock_writer_mode(directory, LockMode::Shared, budget, cancel, clock, true)
}

/// Acquire exclusive worktree occupancy for removal and other operations that
/// require every active holder to leave.
#[allow(clippy::disallowed_methods)] // The bounded wait must expire even with a stopped caller clock.
pub fn lock_writer_exclusion(
    directory: &Directory,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> Result<LockGuard> {
    lock_writer_mode(directory, LockMode::Exclusive, budget, cancel, clock, false)
}

#[allow(clippy::disallowed_methods)] // The bounded wait must expire even with a stopped caller clock.
fn lock_writer_mode(
    directory: &Directory,
    mode: LockMode,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
    wait_for_release: bool,
) -> Result<LockGuard> {
    let timeout = Duration::from_micros(budget.remaining(clock.mono_now()).as_micros());
    let path = directory.current_path()?.join("writer.lock");
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| FsError::LockDeadlineOverflow { path: path.clone() })?;
    let mut release_waiting = false;
    loop {
        if cancel.is_cancelled() {
            return Err(FsError::Cancelled { path }.into());
        }
        // An observation is not writer custody: contention here is busy,
        // regardless of the caller's permitted wait for the actual writer.
        let observation = writer_admission_guard(directory)?;
        // A nonblocking budget still probes the stable inode once. Keep the
        // caller's cancellation checks before and after that acquisition.
        let attempt = directory.lock(
            OsStr::new("writer.lock"),
            mode,
            WaitBudget::nonblocking(clock.mono_now()),
            cancel,
            clock,
        );
        drop(observation);
        match attempt {
            Ok(lease) => return Ok(lease),
            Err(FsError::LockTimeout { .. }) => {
                // Decide once: a release already in progress is progress to
                // wait out within the caller's budget, while a foreign live
                // holder still refuses on the first probe. Exclusion callers
                // keep their ordinary bounded wait unchanged.
                if wait_for_release && !release_waiting && !release_in_progress(directory) {
                    return Err(FsError::LockTimeout { path, timeout }.into());
                }
                release_waiting = true;
            }
            Err(error) => return Err(error.into()),
        }
        if cancel.is_cancelled() {
            return Err(FsError::Cancelled { path }.into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FsError::LockTimeout { path, timeout }.into());
        }
        #[cfg(any(test, feature = "fixture"))]
        if let Some(hook) = WRITER_LOCK_WAIT_HOOK.with(|hook| hook.borrow_mut().take()) {
            hook();
        }
        provenant_os::sys::wait_pause(remaining.min(Duration::from_millis(5)));
    }
}
