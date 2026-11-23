//! Stagger steered-lane restarts so a batch of steers cannot race the shared
//! launch lock. The pure scheduling rule is kept separate from the state-dir
//! gate so the concurrency bound can be tested without host timing.
use super::*;
use provenant_records::warning::{Warning, warn};
use std::ffi::OsStr;

const LOCK: &str = "steer-restart.lock";
const LAST: &str = "steer-restart.last";

#[derive(serde::Serialize, serde::Deserialize)]
struct LastRestart {
    boot: String,
    host_us: u64,
}

/// Earliest start, in monotonic microseconds, for the next restart given the
/// last restart's claimed start and the configured spacing. Consecutive starts
/// are at least `spacing_ms` apart, which bounds how many restarts may launch
/// in any window. `None` means there is no prior restart, so the first is free.
pub(crate) fn restart_start_us(now_us: u64, last_us: Option<u64>, spacing_ms: u64) -> u64 {
    match last_us {
        Some(last) if spacing_ms > 0 => last
            .saturating_add(spacing_ms.saturating_mul(1_000))
            .max(now_us),
        _ => now_us,
    }
}

/// How long the next restart must wait before its slot opens.
pub(crate) fn restart_wait_us(now_us: u64, last_us: Option<u64>, spacing_ms: u64) -> u64 {
    restart_start_us(now_us, last_us, spacing_ms).saturating_sub(now_us)
}

fn fail(message: impl std::fmt::Display) -> Fail {
    Fail::new(
        Status::Error,
        ErrorKind::Io,
        format!("steer restart pacing: {message}"),
    )
}

/// Serialise and space steer restarts through a shared state-dir gate. Hold the
/// exclusive lock while claiming the next slot, wait until that slot opens, then
/// record the claimed start so the following restart is spaced behind it. Call
/// this before handing a steered successor to the launch lock. Returns the wait
/// actually applied in microseconds. `spacing_ms == 0` disables staggering.
pub fn pace_steer_restart(
    state: &Path,
    spacing_ms: u64,
    call: &run::Request<'_>,
) -> Result<u64, Fail> {
    if spacing_ms == 0 {
        return Ok(0);
    }
    let dir = provenant_os::fs::Directory::open_anchor(state).map_err(fail)?;
    let guard = dir
        .lock(
            OsStr::new(LOCK),
            provenant_os::fs::LockMode::Exclusive,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )
        .map_err(fail)?;
    let boot = call.clock.boot_identity().map_err(fail)?;
    let now_us = call.clock.host_monotonic_micros().map_err(fail)?;
    let last_us = match dir.read_bounded(OsStr::new(LAST), 256) {
        Ok(bytes) => match serde_json::from_slice::<LastRestart>(&bytes) {
            Ok(last) => (last.boot == boot).then_some(last.host_us),
            Err(_) => {
                warn(Warning::new(
                    provenant_records::warning::STEER_PACING_RECORD_INVALID,
                    "ignoring invalid steer restart pacing record",
                ));
                None
            }
        },
        Err(error) if error.is_not_found() => None,
        Err(error) => return Err(fail(error)),
    };
    let wait_us = restart_wait_us(now_us, last_us, spacing_ms);
    if wait_us > 0 {
        let deadline = provenant_core::MonoTime::from_micros(
            call.clock.mono_now().as_micros().saturating_add(wait_us),
        );
        call.clock.wait_until(deadline).map_err(fail)?;
    }
    let record = LastRestart {
        boot,
        host_us: call.clock.host_monotonic_micros().map_err(fail)?,
    };
    let bytes = serde_json::to_vec(&record).map_err(fail)?;
    dir.atomic_write(
        OsStr::new(LAST),
        "steer-restart",
        &bytes,
        provenant_os::fs::PublishMode::Replace,
    )
    .map_err(fail)?;
    drop(guard);
    Ok(wait_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_process_clocks_share_pacing_only_within_a_boot() {
        use provenant_core::{Clock, MonoTime, TimeError, WallTime};
        use std::sync::atomic::{AtomicU64, Ordering};

        struct ProcessClock<'a> {
            local: AtomicU64,
            host: &'a AtomicU64,
            boot: &'static str,
        }
        impl Clock for ProcessClock<'_> {
            fn wall_now(&self) -> Result<WallTime, TimeError> {
                Err(TimeError::HostClock)
            }
            fn mono_now(&self) -> MonoTime {
                MonoTime::from_micros(self.local.load(Ordering::SeqCst))
            }
            fn wait_until(&self, deadline: MonoTime) -> Result<(), TimeError> {
                let old = self.local.swap(deadline.as_micros(), Ordering::SeqCst);
                self.host
                    .fetch_add(deadline.as_micros().saturating_sub(old), Ordering::SeqCst);
                Ok(())
            }
            fn host_monotonic_micros(&self) -> Result<u64, TimeError> {
                Ok(self.host.load(Ordering::SeqCst))
            }
            fn boot_identity(&self) -> Result<String, TimeError> {
                Ok(self.boot.to_owned())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let host = AtomicU64::new(1_500_000_000);
        let first = ProcessClock {
            local: AtomicU64::new(1_200_000_000),
            host: &host,
            boot: "boot-a",
        };
        let second = ProcessClock {
            local: AtomicU64::new(100_000),
            host: &host,
            boot: "boot-a",
        };
        let reboot = ProcessClock {
            local: AtomicU64::new(0),
            host: &host,
            boot: "boot-b",
        };
        let cancel = provenant_core::CancelToken::new();
        let pace = |clock: &dyn Clock| {
            pace_steer_restart(
                dir.path(),
                2_000,
                &run::Request {
                    clock,
                    cancel: &cancel,
                },
            )
            .unwrap()
        };
        assert_eq!(pace(&first), 0);
        assert_eq!(pace(&second), 2_000_000);
        assert_eq!(pace(&reboot), 0);
        std::fs::write(dir.path().join(LAST), b"not a valid pacing record").unwrap();
        assert_eq!(pace(&reboot), 0);
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn stagger_bounds_concurrent_restarts() {
        let spacing_ms = 2_000;
        let mut starts = Vec::new();
        let mut last = None;
        for _ in 0..8 {
            // Eight lanes steered together all arrive at the same instant.
            let start = restart_start_us(0, last, spacing_ms);
            if let Some(previous) = starts.last() {
                assert!(
                    start.saturating_sub(*previous) >= spacing_ms * 1_000,
                    "restarts must be spaced by the configured window: {starts:?}"
                );
            }
            starts.push(start);
            last = Some(start);
        }
        assert_eq!(
            starts,
            vec![
                0, 2_000_000, 4_000_000, 6_000_000, 8_000_000, 10_000_000, 12_000_000, 14_000_000,
            ]
        );
    }

    #[test]
    fn zero_spacing_never_waits_and_late_arrivals_are_free() {
        assert_eq!(restart_wait_us(500, Some(0), 0), 0);
        // The spacing window has already elapsed, so no wait is needed.
        assert_eq!(restart_wait_us(3_000_000, Some(0), 2_000), 0);
        assert_eq!(restart_wait_us(500_000, Some(0), 2_000), 1_500_000);
    }
}
