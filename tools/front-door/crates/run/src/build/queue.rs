//! Admission seam. S0 retains the existing bounded slot wait; S2 replaces the
//! polling implementation with persisted priority tickets.

use super::{BuildError, Job, RunContext};
use provenant_config::{BuildOverflow, ResolvedAdmissionClass};
use provenant_core::{CancelToken, Clock, DurationMicros};
use provenant_os::admission::{
    AdmissionSlots, HeldGuard, HostHeldGuard, HostWaitGuard, WaitGuard, mark_held_with_command,
    mark_host_held_with_command, mark_host_waiting_with_command, mark_waiting_with_command,
};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, io::Write, path::Path, time::Duration};

/// The future ticket/admission request consumed by the queue implementation.
pub struct AdmissionRequest<'a> {
    pub state: &'a Path,
    pub class: &'a str,
    pub limit: u64,
    pub job_id: &'a str,
    pub owner: &'a str,
    pub project_key: &'a str,
    pub priority: super::Priority,
    pub aging: Duration,
    pub command_digest: String,
    pub overflow: OverflowPolicy,
    pub affinity: Vec<u64>,
    pub excluded: Vec<u64>,
    pub cancel: &'a CancelToken,
    /// Existing lane marker location, when the shim provides it.
    pub scratch: Option<&'a Path>,
    /// Attempt id attributed to status records.
    pub attempt_id: Option<&'a str>,
    /// Sanitised marker label; raw argv is never persisted here.
    pub command: Option<String>,
    /// The S0 bounded-wait duration from the existing admission class.
    pub wait_seconds: u64,
}

/// Overflow behaviour selected by build.overflow and the active strategy.
pub enum OverflowPolicy {
    Queue,
    Local { after: Duration },
}

/// Admission result. The acquired file lock stays held for the whole job.
pub enum Admitted {
    Held {
        guard: provenant_os::admission::SlotGuard,
        index: u64,
        waited: Duration,
    },
    Overflow {
        waited: Duration,
    },
}

/// Make a request from the frozen build job and its layered admission config.
pub fn request_for_job<'a>(job: &'a Job, context: &'a RunContext) -> AdmissionRequest<'a> {
    let resolved = env_value(&job.env, "PROVENANT_ADMISSION_CONFIG")
        .and_then(|value| serde_json::from_str::<ResolvedAdmissionClass>(value).ok())
        .filter(|config| config.name == "build")
        .unwrap_or_else(|| {
            ResolvedAdmissionClass::resolve(
                "build",
                &context.state,
                Some(&job.project),
                Some(&job.project_key),
            )
        });
    let limit = env_value(&job.env, "PROVENANT_ADMISSION_SLOTS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(resolved.slots);
    let wait_seconds = env_value(&job.env, "PROVENANT_ADMISSION_WAIT_SECONDS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(resolved.wait_seconds);
    let overflow = match context.strategy.port.overflow_policy().value {
        BuildOverflow::Queue => OverflowPolicy::Queue,
        BuildOverflow::Local => OverflowPolicy::Local {
            after: Duration::from_secs(wait_seconds),
        },
        BuildOverflow::Auto if context.strategy.name == super::StrategyName::Slots => {
            OverflowPolicy::Queue
        }
        BuildOverflow::Auto => OverflowPolicy::Local {
            after: Duration::from_secs(wait_seconds),
        },
    };
    let aging = context.strategy.port.queue_aging_duration().value;
    let command_digest = command_digest(job);
    AdmissionRequest {
        state: &context.state,
        class: "build",
        limit,
        job_id: &job.id,
        owner: &job.owner,
        project_key: &job.project_key,
        priority: job.priority,
        aging,
        command_digest,
        overflow,
        affinity: Vec::new(),
        excluded: Vec::new(),
        cancel: &context.cancel,
        scratch: env_path(job, "PROVENANT_ADMISSION_WAIT_DIR"),
        attempt_id: env_value(&job.env, "PROVENANT_ATTEMPT_ID")
            .or_else(|| env_value(&job.env, "PROVENANT_PARENT_ATTEMPT_ID")),
        command: Some("build job".to_owned()),
        wait_seconds,
    }
}

/// Try to acquire immediately, then preserve the current bounded polling
/// behaviour until the priority queue lands in S2. Its 25 ms retry loop mirrors
/// `AdmissionSlots::acquire_within`, while checking the cancellation token that
/// the old API does not accept.
pub fn admit(request: &AdmissionRequest<'_>) -> Result<Admitted, BuildError> {
    if request.cancel.is_cancelled() {
        return Err(BuildError::Cancelled);
    }
    if request.limit == 0 {
        return Ok(Admitted::Overflow {
            waited: Duration::ZERO,
        });
    }
    let slots = match AdmissionSlots::open(request.state, request.class) {
        Ok(slots) => slots,
        Err(_) => {
            let _ = AdmissionSlots::provision(request.state, request.class, request.limit);
            AdmissionSlots::open(request.state, request.class).map_err(|error| {
                BuildError::Unavailable(format!("cannot open admission slots: {error}"))
            })?
        }
    };
    let clock = provenant_os::clock::SystemClock::new();
    let started_micros = clock.mono_now().as_micros();
    match slots.try_acquire(request.limit) {
        Ok(Some(guard)) => {
            return Ok(Admitted::Held {
                index: guard.index(),
                guard,
                waited: Duration::ZERO,
            });
        }
        Ok(None) => {}
        Err(error) => {
            return Err(BuildError::Unavailable(format!(
                "cannot acquire admission slot: {error}"
            )));
        }
    }

    let _waiting = mark_waiting(request).ok();
    let _ = writeln!(
        std::io::stderr().lock(),
        "provenant: waiting for a host {} slot ({} held of {})",
        request.class,
        slots.held(request.limit),
        request.limit
    );
    let timeout = Duration::from_secs(request.wait_seconds);
    loop {
        if request.cancel.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        match slots.try_acquire(request.limit) {
            Ok(Some(guard)) => {
                let waited = Duration::from_micros(
                    clock.mono_now().as_micros().saturating_sub(started_micros),
                );
                drop(_waiting);
                return Ok(Admitted::Held {
                    index: guard.index(),
                    guard,
                    waited,
                });
            }
            Ok(None) => {}
            Err(error) => {
                return Err(BuildError::Unavailable(format!(
                    "cannot acquire admission slot: {error}"
                )));
            }
        }
        let elapsed =
            Duration::from_micros(clock.mono_now().as_micros().saturating_sub(started_micros));
        if elapsed >= timeout {
            return Ok(Admitted::Overflow { waited: elapsed });
        }
        let pause = Duration::from_millis(25).min(timeout.saturating_sub(elapsed));
        let pause_micros = pause.as_micros().min(u128::from(u64::MAX)) as u64;
        if let Ok(deadline) = clock
            .mono_now()
            .checked_add(DurationMicros::from_micros(pause_micros))
        {
            let _ = clock.wait_until(deadline);
        }
    }
}

/// Publish the established held marker for the lifetime of a local job that
/// owns a host admission slot.
pub fn mark_held(job: &Job, context: &RunContext, slot: u64) -> Option<HeldMarker> {
    let pid = i64::from(std::process::id());
    let command = Some("build job".to_owned());
    if let Some(scratch) = env_path(job, "PROVENANT_ADMISSION_WAIT_DIR") {
        mark_held_with_command(
            scratch,
            "build",
            slot,
            pid,
            env_value(&job.env, "PROVENANT_ATTEMPT_ID")
                .or_else(|| env_value(&job.env, "PROVENANT_PARENT_ATTEMPT_ID")),
            command,
        )
        .ok()
        .map(HeldMarker::Lane)
    } else {
        mark_host_held_with_command(&context.state, "build", slot, pid, command)
            .ok()
            .map(HeldMarker::Host)
    }
}

/// Lock-backed status marker retained for the full job duration.
pub enum HeldMarker {
    Lane(HeldGuard),
    Host(HostHeldGuard),
}

#[allow(dead_code, reason = "holding this guard publishes wait status")]
enum WaitingMarker {
    Lane(WaitGuard),
    Host(HostWaitGuard),
}

fn mark_waiting(request: &AdmissionRequest<'_>) -> Result<WaitingMarker, String> {
    let pid = i64::from(std::process::id());
    if let Some(scratch) = request.scratch {
        mark_waiting_with_command(
            scratch,
            request.class,
            pid,
            request.attempt_id,
            request.command.clone(),
        )
        .map(WaitingMarker::Lane)
        .map_err(|error| error.to_string())
    } else {
        mark_host_waiting_with_command(request.state, request.class, pid, request.command.clone())
            .map(WaitingMarker::Host)
            .map_err(|error| error.to_string())
    }
}

fn env_value<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.to_str())
}

fn env_path<'a>(job: &'a Job, key: &str) -> Option<&'a Path> {
    job.env
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| Path::new(value))
}

fn command_digest(job: &Job) -> String {
    let mut digest = Sha256::new();
    for step in &job.steps {
        digest.update(step.cwd_rel.as_os_str().to_string_lossy().as_bytes());
        digest.update([0]);
        for argument in &step.argv {
            digest.update(argument.to_string_lossy().as_bytes());
            digest.update([0]);
        }
        digest.update([0xff]);
    }
    let digest = digest.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
