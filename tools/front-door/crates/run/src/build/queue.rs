//! Admission seam. S0 retains the existing bounded slot wait; S2 replaces the
//! polling implementation with persisted priority tickets.

use super::{BuildError, Job, RunContext};
use provenant_config::{BuildOverflow, ResolvedAdmissionClass};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::admission::{
    AdmissionSlots, HeldGuard, HostHeldGuard, HostWaitGuard, WaitGuard, mark_held_with_command,
    mark_host_held_with_command, mark_host_waiting_with_command, mark_waiting_with_command,
};
use provenant_os::clock::SystemClock;
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode, LockRefusal, PublishMode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

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
        affinity: affinity(job, context, limit),
        excluded: if context.strategy.name == super::StrategyName::Slots {
            (0..limit)
                .filter(|index| {
                    let path =
                        super::slots::SlotPaths::new(&context.home, &job.project_key, *index)
                            .record;
                    std::fs::read(path)
                        .ok()
                        .and_then(|data| {
                            serde_json::from_slice::<super::slots::SlotRecord>(&data).ok()
                        })
                        .is_some_and(|record| {
                            record.health.state == super::slots::SlotState::Quarantined
                        })
                })
                .collect()
        } else {
            Vec::new()
        },
        cancel: &context.cancel,
        scratch: env_path(job, "PROVENANT_ADMISSION_WAIT_DIR"),
        attempt_id: env_value(&job.env, "PROVENANT_ATTEMPT_ID")
            .or_else(|| env_value(&job.env, "PROVENANT_PARENT_ATTEMPT_ID")),
        command: Some("build job".to_owned()),
        wait_seconds,
    }
}

/// Persisted waiter. The exclusive lock on its inode proves liveness.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Ticket {
    schema: u32,
    id: String,
    owner: String,
    project_key: String,
    priority: super::Priority,
    aging_ms: u64,
    enqueued_unix_ms: u64,
    pid: u32,
    start_time: u64,
    command_digest: String,
}

fn affinity(job: &Job, context: &RunContext, limit: u64) -> Vec<u64> {
    let mut indices: Vec<_> = (0..limit).collect();
    indices.sort_by_key(|index| {
        let path = super::slots::SlotPaths::new(&context.home, &job.project_key, *index).record;
        let record = std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice::<super::slots::SlotRecord>(&data).ok());
        let mark = record.and_then(|record| record.last_job);
        let same_owner = mark.as_ref().is_some_and(|mark| mark.owner == job.owner);
        let same_base = mark
            .as_ref()
            .and_then(|mark| mark.base.as_ref())
            .zip(job.verify_base.as_ref())
            .is_some_and(|(base, expected)| base == &expected.commit);
        (
            !same_owner,
            !same_base,
            mark.map_or(0, |mark| mark.finished_unix_ms),
            *index,
        )
    });
    indices
}

struct TicketLease {
    json_path: PathBuf,
    lock_path: PathBuf,
    _guard: LockGuard,
}
impl Drop for TicketLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.json_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn unavailable(error: impl std::fmt::Display) -> BuildError {
    BuildError::Unavailable(error.to_string())
}

/// A queue entry (ticket lock or slot) that departs during a scan can surface
/// as three equivalent outcomes: `NotFound` when it vanished before a stat or
/// open, `LinkCount` when the unlink landed between the surrounding stats, and
/// `RaceRefused` when the path was unlinked or replaced after the descriptor
/// was opened and its inode re-validated. All three mean the listed entry is
/// gone; none is a real I/O failure.
fn entry_departed(error: &FsError) -> bool {
    error.is_not_found()
        || matches!(
            error,
            FsError::RaceRefused { .. }
                | FsError::LockRefused {
                    reason: LockRefusal::LinkCount,
                    ..
                }
        )
}

/// Try to take one slot, treating a slot file that vanished under the scan as
/// an unavailable slot rather than a host failure. The slot's owner has
/// departed; the scan moves on to the remaining slots.
fn try_slot(
    slots: &AdmissionSlots,
    order: &[u64],
) -> Result<Option<provenant_os::admission::SlotGuard>, BuildError> {
    match slots.try_acquire_ordered(order) {
        Ok(guard) => Ok(guard),
        Err(error) if entry_departed(&error) => Ok(None),
        Err(error) => Err(unavailable(error)),
    }
}

fn ticket_key(ticket: &Ticket, now: u64) -> (u64, u64, &str) {
    let level = match ticket.priority {
        super::Priority::Interactive => 0u64,
        super::Priority::Test => 1,
        super::Priority::Train => 2,
        super::Priority::Prewarm => 3,
    };
    let age = now.saturating_sub(ticket.enqueued_unix_ms) / ticket.aging_ms.max(1);
    (
        level.saturating_sub(age),
        ticket.enqueued_unix_ms,
        &ticket.id,
    )
}

/// Scan the queue directory for live tickets.
///
/// `after_list` is an injectable seam run after an entry has been listed but
/// before its lock is opened. Production supplies a no-op; tests use it to
/// remove an entry between the two steps without relying on timing.
fn live_tickets_with(
    dir: &Directory,
    path: &Path,
    mut after_list: impl FnMut(&OsStr) -> std::io::Result<()>,
) -> Result<Vec<Ticket>, BuildError> {
    let mut tickets = Vec::new();
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        // The whole queue directory departed; the scan has nothing to report.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(tickets),
        Err(error) => return Err(unavailable(error)),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            // A listed entry vanished before it could be examined; skip it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(unavailable(error)),
        };
        let name = entry.file_name();
        if Path::new(&name).extension() != Some(OsStr::new("lock")) {
            continue;
        }
        after_list(&name).map_err(unavailable)?;
        match dir.try_lock_existing(&name, LockMode::Exclusive) {
            Ok(_guard) => {
                // The waiter is gone: its lock was free, so reap both files.
                let json = entry.path().with_extension("json");
                let _ = std::fs::remove_file(&json);
                let _ = std::fs::remove_file(entry.path());
            }
            Err(FsError::LockTimeout { .. }) => {
                let json = entry.path().with_extension("json");
                match std::fs::read(&json) {
                    Ok(bytes) => {
                        let ticket: Ticket = serde_json::from_slice(&bytes).map_err(unavailable)?;
                        tickets.push(ticket);
                    }
                    // Its JSON departed with the entry; skip rather than fail.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(unavailable(error)),
                }
            }
            // Every outcome of a vanished entry is a departure, including a
            // stale-inode refusal after `try_lock_existing` revalidated the path.
            Err(error) if entry_departed(&error) => {}
            // Only a real I/O failure fails the scan.
            Err(error) => return Err(unavailable(error)),
        }
    }
    Ok(tickets)
}

fn live_tickets(dir: &Directory, path: &Path) -> Result<Vec<Ticket>, BuildError> {
    live_tickets_with(dir, path, |_| Ok(()))
}

fn check_cancel(
    request: &AdmissionRequest<'_>,
    signals: &provenant_os::exec::BuildSignalWait,
) -> Result<(), BuildError> {
    if let Some(signal) = signals.signal() {
        request.cancel.cancel();
        return Err(BuildError::CancelledSignal(signal));
    }
    if request.cancel.is_cancelled() {
        return Err(BuildError::Cancelled);
    }
    Ok(())
}

/// Admit immediately if possible, otherwise join the host-wide priority queue.
#[allow(clippy::disallowed_methods)]
pub fn admit(request: &AdmissionRequest<'_>) -> Result<Admitted, BuildError> {
    let signals = provenant_os::exec::BuildSignalWait::install().map_err(unavailable)?;
    check_cancel(request, &signals)?;
    if request.limit == 0 {
        return Ok(Admitted::Overflow {
            waited: Duration::ZERO,
        });
    }
    let slots = match AdmissionSlots::open(request.state, request.class) {
        Ok(slots) => slots,
        Err(provenant_os::admission::AdmissionError::Fs(error)) if error.is_not_found() => {
            AdmissionSlots::provision(request.state, request.class, request.limit)
                .map_err(unavailable)?;
            AdmissionSlots::open(request.state, request.class).map_err(unavailable)?
        }
        Err(error) => return Err(unavailable(error)),
    };
    let mut order = Vec::new();
    for index in request.affinity.iter().copied().chain(0..request.limit) {
        if index < request.limit && !request.excluded.contains(&index) && !order.contains(&index) {
            order.push(index);
        }
    }
    if order.is_empty() {
        return Err(unavailable("no eligible admission slots"));
    }
    let started = Instant::now();
    if let Some(guard) = try_slot(&slots, &order)? {
        check_cancel(request, &signals)?;
        return Ok(Admitted::Held {
            index: guard.index(),
            guard,
            waited: Duration::ZERO,
        });
    }
    if request.job_id.is_empty()
        || !request
            .job_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        || request.class.is_empty()
        || !request
            .class
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(unavailable("invalid queue ticket identity"));
    }
    let queue_path = request.state.join("build/queue").join(request.class);
    std::fs::create_dir_all(&queue_path).map_err(unavailable)?;
    let dir = Directory::open_anchor(&queue_path).map_err(unavailable)?;
    let ticket = Ticket {
        schema: 1,
        id: request.job_id.to_owned(),
        owner: request.owner.to_owned(),
        project_key: request.project_key.to_owned(),
        priority: request.priority,
        aging_ms: request.aging.as_millis().min(u128::from(u64::MAX)) as u64,
        enqueued_unix_ms: provenant_os::clock::now_unix_millis(),
        pid: std::process::id(),
        start_time: super::process_marker()
            .and_then(|s| s.split_once(':').and_then(|(_, start)| start.parse().ok()))
            .unwrap_or(0),
        command_digest: request.command_digest.clone(),
    };
    let lock_name = format!("{}.lock", request.job_id);
    let ticket_name = format!("{}.json", request.job_id);
    // The separate sibling lock carries liveness: a waiter that dies releases
    // it, and any other waiter reaps both files. Lock names may not be renamed,
    // so the ticket bytes live in the adjacent JSON file.
    // A reaper in another participant's scan can unlink this freshly created
    // lock in the window before our flock lands. That lock never became ours;
    // recreate it. The probe can also hold the lock briefly while it decides
    // the entry is live, but our own ticket is never a stale entry: wait out
    // that transient hold instead of refusing admission on a zero-timeout
    // acquisition. Bound both so a persistent refusal still surfaces.
    const TICKET_LOCK_GRACE: DurationMicros = DurationMicros::from_micros(1_000_000);
    let clock = SystemClock::new();
    let mut departed = 0;
    let guard = loop {
        match dir.lock(
            OsStr::new(&lock_name),
            LockMode::Exclusive,
            WaitBudget::up_to(clock.mono_now(), TICKET_LOCK_GRACE),
            request.cancel,
            &clock,
        ) {
            Ok(guard) => break guard,
            Err(error) if entry_departed(&error) && departed < 8 => departed += 1,
            Err(error) => return Err(unavailable(error)),
        }
    };
    let bytes = serde_json::to_vec(&ticket).map_err(unavailable)?;
    dir.atomic_write(
        OsStr::new(&ticket_name),
        ".ticket-",
        &bytes,
        PublishMode::NoReplace,
    )
    .map_err(unavailable)?;
    let _lease = TicketLease {
        json_path: queue_path.join(&ticket_name),
        lock_path: queue_path.join(&lock_name),
        _guard: guard,
    };
    let _waiting = mark_waiting(request).ok();
    let _ = writeln!(
        std::io::stderr().lock(),
        "provenant: waiting for a host {} slot ({} held of {})",
        request.class,
        slots.held(request.limit),
        request.limit
    );
    let mut round = 0u64;
    loop {
        check_cancel(request, &signals)?;
        let elapsed = started.elapsed();
        if let OverflowPolicy::Local { after } = request.overflow
            && elapsed >= after
        {
            return Ok(Admitted::Overflow { waited: elapsed });
        }
        let now = provenant_os::clock::now_unix_millis();
        let mut tickets = live_tickets(&dir, &queue_path)?;
        if tickets.iter().any(|other| {
            other.owner == ticket.owner
                && other.priority == ticket.priority
                && other.command_digest == ticket.command_digest
                && (other.enqueued_unix_ms, &other.id) > (ticket.enqueued_unix_ms, &ticket.id)
        }) {
            return Err(BuildError::Superseded);
        }
        tickets.sort_by(|a, b| ticket_key(a, now).cmp(&ticket_key(b, now)));
        let rank = tickets
            .iter()
            .position(|other| other.id == ticket.id)
            .ok_or_else(|| unavailable("queue ticket disappeared"))?;
        let mut free = 0;
        for index in &order {
            if try_slot(&slots, &[*index])?.is_some() {
                free += 1;
            }
        }
        if rank < free
            && let Some(guard) = try_slot(&slots, &order)?
        {
            check_cancel(request, &signals)?;
            return Ok(Admitted::Held {
                index: guard.index(),
                guard,
                waited: started.elapsed(),
            });
        }
        round = round.wrapping_add(1);
        let jitter = (u64::from(std::process::id()).wrapping_add(round.wrapping_mul(17))) % 41;
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(Duration::from_millis(80 + jitter));
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
        digest.update(step.cwd_rel.as_os_str().as_encoded_bytes());
        digest.update([0]);
        for argument in &step.argv {
            digest.update(argument.as_encoded_bytes());
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

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn request<'a>(
        state: &'a Path,
        id: &'a str,
        owner: &'a str,
        digest: &str,
        cancel: &'a CancelToken,
    ) -> AdmissionRequest<'a> {
        AdmissionRequest {
            state,
            class: "build",
            limit: 1,
            job_id: id,
            owner,
            project_key: "project",
            priority: super::super::Priority::Interactive,
            aging: Duration::from_secs(120),
            command_digest: digest.into(),
            overflow: OverflowPolicy::Queue,
            affinity: vec![],
            excluded: vec![],
            cancel,
            scratch: None,
            attempt_id: None,
            command: None,
            wait_seconds: 1,
        }
    }
    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline, "ticket was not created");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn ticket(id: &str, priority: super::super::Priority, time: u64) -> Ticket {
        Ticket {
            schema: 1,
            id: id.into(),
            owner: "owner".into(),
            project_key: "p".into(),
            priority,
            aging_ms: 100,
            enqueued_unix_ms: time,
            pid: 1,
            start_time: 1,
            command_digest: id.into(),
        }
    }
    #[test]
    fn priority_and_aging_order_tickets() {
        use super::super::Priority::*;
        let old = ticket("old", Prewarm, 100);
        let newer = ticket("new", Interactive, 400);
        assert!(ticket_key(&newer, 400) < ticket_key(&old, 300));
        assert!(ticket_key(&old, 400) < ticket_key(&newer, 400));
        let train = ticket("train", Train, 400);
        let test = ticket("test", Test, 400);
        assert!(ticket_key(&test, 400) < ticket_key(&train, 400));
    }
    #[test]
    fn free_slot_is_acquired_without_ticket_and_respects_affinity() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 2).unwrap();
        let cancel = CancelToken::new();
        let mut req = request(state.path(), "free", "owner", "a", &cancel);
        req.limit = 2;
        req.affinity = vec![1];
        let Admitted::Held { index, waited, .. } = admit(&req).unwrap() else {
            panic!("overflow")
        };
        assert_eq!(index, 1);
        assert_eq!(waited, Duration::ZERO);
        assert!(!state.path().join("build/queue/build/free.json").exists());
    }
    #[test]
    fn local_overflow_respects_timeout_and_excluded_slots() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 2).unwrap();
        let held = AdmissionSlots::open(state.path(), "build")
            .unwrap()
            .try_acquire_ordered(&[0])
            .unwrap()
            .unwrap();
        let cancel = CancelToken::new();
        let mut req = request(state.path(), "overflow", "owner", "a", &cancel);
        req.limit = 2;
        req.excluded = vec![1];
        req.overflow = OverflowPolicy::Local {
            after: Duration::ZERO,
        };
        assert!(matches!(admit(&req), Ok(Admitted::Overflow { .. })));
        assert!(
            !state
                .path()
                .join("build/queue/build/overflow.json")
                .exists()
        );
        drop(held);
    }
    #[test]
    fn queued_higher_priority_acquires_first() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 1).unwrap();
        let held = AdmissionSlots::open(state.path(), "build")
            .unwrap()
            .try_acquire(1)
            .unwrap()
            .unwrap();
        let path = state.path().join("build/queue/build");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            for (id, priority) in [
                ("low", super::super::Priority::Train),
                ("high", super::super::Priority::Interactive),
            ] {
                let tx = tx.clone();
                let state = state.path();
                scope.spawn(move || {
                    let cancel = CancelToken::new();
                    let mut req = request(state, id, id, id, &cancel);
                    req.priority = priority;
                    let guard = admit(&req).unwrap();
                    tx.send(id).unwrap();
                    std::thread::sleep(Duration::from_millis(150));
                    drop(guard);
                });
                wait_for(&path.join(format!("{id}.json")));
            }
            drop(held);
            assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), "high");
            assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), "low");
        });
    }
    #[test]
    fn dead_ticket_is_reaped_and_cancel_removes_live_ticket() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 1).unwrap();
        let slots = AdmissionSlots::open(state.path(), "build").unwrap();
        let held = slots.try_acquire(1).unwrap().unwrap();
        let path = state.path().join("build/queue/build");
        std::fs::create_dir_all(&path).unwrap();
        let dead = ticket("dead", super::super::Priority::Interactive, 1);
        std::fs::write(path.join("dead.json"), serde_json::to_vec(&dead).unwrap()).unwrap();
        // Liveness is carried by the sibling lock; an unlocked ticket is stale.
        std::fs::write(path.join("dead.lock"), b"").unwrap();
        let cancel = CancelToken::new();
        std::thread::scope(|scope| {
            let handle =
                scope.spawn(|| admit(&request(state.path(), "live", "owner", "a", &cancel)));
            wait_for(&path.join("live.json"));
            let deadline = Instant::now() + Duration::from_secs(10);
            while path.join("dead.json").exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            cancel.cancel();
            assert!(matches!(handle.join().unwrap(), Err(BuildError::Cancelled)));
        });
        assert!(!path.join("live.json").exists());
        drop(held);
    }
    #[test]
    fn only_identical_newer_command_supersedes() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 1).unwrap();
        let slots = AdmissionSlots::open(state.path(), "build").unwrap();
        let held = slots.try_acquire(1).unwrap().unwrap();
        let path = state.path().join("build/queue/build");
        let a = CancelToken::new();
        let b = CancelToken::new();
        let c = CancelToken::new();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| admit(&request(state.path(), "a", "owner", "same", &a)));
            wait_for(&path.join("a.json"));
            let different = scope.spawn(|| {
                matches!(
                    admit(&request(state.path(), "b", "owner", "different", &b)),
                    Ok(Admitted::Held { .. })
                )
            });
            wait_for(&path.join("b.json"));
            let newer = scope.spawn(|| {
                matches!(
                    admit(&request(state.path(), "c", "owner", "same", &c)),
                    Ok(Admitted::Held { .. })
                )
            });
            wait_for(&path.join("c.json"));
            assert!(matches!(first.join().unwrap(), Err(BuildError::Superseded)));
            drop(held);
            assert!(different.join().unwrap());
            assert!(newer.join().unwrap());
        });
    }
    #[test]
    fn entry_vanishing_between_listing_and_open_is_skipped() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("build/queue/build");
        std::fs::create_dir_all(&path).unwrap();
        let dir = Directory::open_anchor(&path).unwrap();
        // A departed participant left its lock behind. The seam removes it
        // after listing but before the probe opens it, deterministically.
        std::fs::write(path.join("vanished.lock"), b"").unwrap();
        let mut removed = false;
        let tickets = live_tickets_with(&dir, &path, |name| {
            if name == "vanished.lock" {
                std::fs::remove_file(path.join(name)).unwrap();
                removed = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(removed);
        assert!(tickets.is_empty());
        assert!(!path.join("vanished.lock").exists());
    }
    #[test]
    fn unlinked_ticket_lock_is_skipped_not_reported_unavailable() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("build/queue/build");
        std::fs::create_dir_all(&path).unwrap();
        let dir = Directory::open_anchor(&path).unwrap();
        let lock = path.join("departed.lock");
        std::fs::write(&lock, b"").unwrap();
        let hook_lock = lock.clone();
        // After the waiter takes the flock, the holder unlinks its lock and a
        // newcomer creates a fresh inode under the same name. The waiter must
        // refuse the stale inode, and the scan must treat that entry as gone.
        provenant_os::fixture::on_existing_lock_acquire(move || {
            std::fs::remove_file(&hook_lock).unwrap();
            std::fs::write(&hook_lock, b"").unwrap();
        });
        let tickets = live_tickets(&dir, &path).unwrap();
        assert!(tickets.is_empty());
        assert!(lock.exists());
    }
    #[test]
    fn link_count_refusal_from_departing_entry_is_skipped() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("build/queue/build");
        std::fs::create_dir_all(&path).unwrap();
        let dir = Directory::open_anchor(&path).unwrap();
        let lock = path.join("hardlinked.lock");
        let target = path.join("elsewhere");
        std::fs::write(&lock, b"").unwrap();
        std::fs::write(&target, b"").unwrap();
        let hook_lock = lock.clone();
        let hook_target = target.clone();
        let tickets = live_tickets_with(&dir, &path, move |name| {
            if name == "hardlinked.lock" {
                // The listed lock departs and is replaced by an entry with a
                // link count other than one, which the fs layer refuses.
                std::fs::remove_file(&hook_lock).unwrap();
                std::fs::hard_link(&hook_target, &hook_lock).unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert!(tickets.is_empty());
    }
    #[test]
    fn real_scan_io_error_surfaces() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("build/queue/build");
        std::fs::create_dir_all(&path).unwrap();
        let dir = Directory::open_anchor(&path).unwrap();
        std::fs::write(path.join("present.lock"), b"").unwrap();
        let result = live_tickets_with(&dir, &path, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected scan failure",
            ))
        });
        assert!(matches!(result, Err(BuildError::Unavailable(_))));
    }
    #[test]
    fn ticket_lock_reaped_before_flock_is_recreated() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 1).unwrap();
        let held = AdmissionSlots::open(state.path(), "build")
            .unwrap()
            .try_acquire(1)
            .unwrap()
            .unwrap();
        let path = state.path().join("build/queue/build");
        let cancel = CancelToken::new();
        let lock = path.join("reaped.lock");
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let hook_lock = lock.clone();
                let mut fired = false;
                provenant_os::fixture::on_owner_lock_acquire(move |name| {
                    if name == "reaped.lock" && !fired {
                        fired = true;
                        // A concurrent reaper unlinks our brand-new ticket lock
                        // before our flock lands, so the first acquisition is
                        // stale and the waiter has to recreate the lock.
                        let _ = std::fs::remove_file(&hook_lock);
                    }
                });
                admit(&request(state.path(), "reaped", "owner", "a", &cancel))
            });
            wait_for(&path.join("reaped.json"));
            cancel.cancel();
            assert!(matches!(handle.join().unwrap(), Err(BuildError::Cancelled)));
        });
        drop(held);
    }
    #[test]
    fn probe_holding_ticket_lock_does_not_refuse_admission() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 1).unwrap();
        let held_slot = AdmissionSlots::open(state.path(), "build")
            .unwrap()
            .try_acquire(1)
            .unwrap()
            .unwrap();
        let path = state.path().join("build/queue/build");
        std::fs::create_dir_all(&path).unwrap();
        let dir = Directory::open_anchor(&path).unwrap();
        // A scan can win the lock in the window before the waiter's flock
        // lands. It holds that lock only while it proves the entry live or
        // reaps it, so the waiter must wait rather than report a timeout.
        let probe = dir
            .try_lock(OsStr::new("contended.lock"), LockMode::Exclusive)
            .unwrap();
        let lock_path = path.join("contended.lock");
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let cancel = CancelToken::new();
        std::thread::scope(|scope| {
            let handle = scope.spawn(move || {
                provenant_os::fixture::on_owner_lock_wait(move |name| {
                    if name == OsStr::new("contended.lock") {
                        let _ = waiting_tx.send(());
                    }
                });
                let admitted = matches!(
                    admit(&request(state.path(), "contended", "owner", "a", &cancel)),
                    Ok(Admitted::Held { .. })
                );
                result_tx.send(admitted).unwrap();
            });
            // The waiter reaches the lock only after the probe already holds it.
            waiting_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            // The probe reaps the entry and releases the lock; the waiter then
            // recreates its own lock and proceeds.
            std::fs::remove_file(&lock_path).unwrap();
            drop(probe);
            drop(held_slot);
            assert!(result_rx.recv_timeout(Duration::from_secs(10)).unwrap());
            handle.join().unwrap();
        });
    }
    #[test]
    fn concurrent_holders_never_exceed_limit() {
        let state = tempfile::tempdir().unwrap();
        AdmissionSlots::provision(state.path(), "build", 2).unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for n in 0..8 {
                let active = Arc::clone(&active);
                let state = state.path();
                scope.spawn(move || {
                    let id = format!("job-{n}");
                    let cancel = CancelToken::new();
                    let mut req = request(state, &id, &id, &id, &cancel);
                    req.limit = 2;
                    let hold = admit(&req).unwrap();
                    assert!(matches!(hold, Admitted::Held { .. }));
                    assert!(active.fetch_add(1, Ordering::SeqCst) < 2);
                    std::thread::sleep(Duration::from_millis(30));
                    active.fetch_sub(1, Ordering::SeqCst);
                    drop(hold);
                });
            }
        });
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
