//! Explicit controls: cancel, resume and reconcile. None of them guesses at
//! process state; uncertain custody stays fenced and is reported as such.
use crate::{artifacts::read_sealed, launch::Providers, *};
use provenant_core::{AttemptId, MessageId, OperationId, WallTime};
use provenant_records::custody::Scope;
use provenant_supervisor::{
    census::{ProcessObserver, SystemProcessObserver},
    custody::{self, CustodyError, Observation, RecoveryState, StopReceipt},
    machine::StopReason,
};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

pub(crate) const STEER_PROMPT_PREFIX: &str =
    "Continue the existing task and provider session, then apply these steering instructions:\n\n";
pub(crate) const STEER_TEXT_SEPARATOR: &str = "\n\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SteerSessionReady {
    pub generation: u64,
    pub route_digest: String,
    pub session: Option<String>,
    pub provider_boot: String,
    pub provider_pid: i64,
    pub provider_start: u64,
    pub conflict: bool,
    pub terminal_confirmed: bool,
}

struct ProviderSessionReadyRow {
    generation: i64,
    route_digest: String,
    session: Option<String>,
    provider_boot: String,
    provider_pid: i64,
    provider_start: i64,
    identity_conflict: i64,
    terminal_confirmed: i64,
}

fn provider_session_ready_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ProviderSessionReadyRow> {
    Ok(ProviderSessionReadyRow {
        generation: row.get(0)?,
        route_digest: row.get(1)?,
        session: row.get(2)?,
        provider_boot: row.get(3)?,
        provider_pid: row.get(4)?,
        provider_start: row.get(5)?,
        identity_conflict: row.get(6)?,
        terminal_confirmed: row.get(7)?,
    })
}

pub(crate) struct SteerSuccessorProof<'a> {
    pub session: Option<&'a str>,
    pub route_digest: Option<&'a str>,
    pub identity_conflict: bool,
    pub evidence_incomplete: bool,
}

#[derive(PartialEq, Eq)]
struct SteerContinuationOutcome {
    state: String,
    cause: Option<String>,
    successor: Option<String>,
    live_ack: bool,
    terminal_final: bool,
}

pub(crate) struct SteerContinuationDisposition {
    pub parent: AttemptId,
    pub successor: AttemptId,
    pub state: &'static str,
    pub cause: Option<provenant_records::run::SteeringCause>,
}

#[derive(Default)]
struct ResumeOptions<'a> {
    fresh: Option<&'a str>,
    writer: Option<(&'a WriterRequest, &'a str)>,
    providers: Option<&'a dyn Providers>,
    budget_remaining: Option<u64>,
    graph_context: Option<&'a provenant_records::run::WorkContext>,
    parent_publication_locked: bool,
    /// Re-pinned head for a retained-writer graph continuation whose worktree
    /// advanced through its own commits after the predecessor was sealed. The
    /// original source pin is kept; only the successor's predecessor head moves.
    retained_head: Option<provenant_core::GitOid>,
}

/// Digest of the session identity only; route-selection history (source,
/// account source, pool selection) is deliberately excluded.
pub(crate) fn steer_route_digest(route: &RouteRecord) -> Result<String> {
    Ok(provenant_records::CanonicalV1::record_digest(
        "steer-provider-route",
        2,
        &route.session_identity(),
    )?
    .to_string())
}

fn source_is_live(source: &provenant_os::capture::CaptureSourceIdentity) -> bool {
    use provenant_supervisor::census::ProcessObservation;
    matches!(
        SystemProcessObserver.observe(source.pid),
        ProcessObservation::Present(fact)
            if !fact.zombie
                && fact.identity.boot == source.boot
                && fact.identity.pid == source.pid
                && fact.identity.native_start == source.native_start
    )
}

#[cfg(test)]
thread_local! {
    pub(crate) static HOLDERLESS_BEFORE_EXCLUSION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static REBOOT_TEMPORARY_CLEANUP: std::cell::RefCell<Option<std::result::Result<Option<Reconciled>, String>>> = const { std::cell::RefCell::new(None) };
}

/// What a cancel request achieved. Requested does not prove exit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CancelReceipt {
    /// The stop ended the attempt without a live provider to act on it.
    Cancelled,
    AlreadyTerminal,
    Requested,
    AlreadyRequested,
    /// No launch summary or custody to address; reconcile first.
    Unobservable {
        reason: String,
    },
}

/// Durable steering input accepted for an exact running attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SteerIntent {
    pub schema_version: u64,
    pub attempt: AttemptId,
    pub operation_id: OperationId,
    pub message_id: MessageId,
    pub text: String,
    pub created_at: WallTime,
}

/// Persisted steering intent. A queued receipt confirms durable text only; the
/// provider is interrupted after live session readiness and settlement owns
/// the continuation outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SteerReceipt {
    Queued { message_id: MessageId },
    AlreadyQueued { message_id: MessageId },
    AlreadyTerminal { message_id: MessageId },
}
/// Merge this retry's limit raises over the task's typed extensions.
fn extensions(
    task: &TaskRequest,
    raises: &[(String, u64)],
) -> std::collections::BTreeMap<String, u64> {
    let mut all = task.limit_extensions.clone();
    all.extend(raises.iter().cloned());
    all
}

/// A continuation of one terminal attempt with new input, as a new run.
#[derive(Clone)]
pub struct Resume {
    pub prior: AttemptId,
    pub expected_generation: u64,
    pub operation_id: OperationId,
    pub attempt_id: AttemptId,
    pub prompt: String,
    pub created_at: WallTime,
    /// Limit raises (`retry --extend`), by limit name.
    pub extend: Vec<(String, u64)>,
}
/// How long past its runtime deadline a live guardian is left to act on its own
/// before `reconcile` asks it to stop.
const DEADLINE_GRACE_SECONDS: u64 = 120;

fn past_runtime_deadline(record: &AttemptRecord, call: &Request<'_>) -> bool {
    call.clock
        .wall_now()
        .is_ok_and(|now| overdue_at(record, now))
}

pub(crate) fn record_steer_outcome(
    tx: &rusqlite::Connection,
    id: AttemptId,
    state: &str,
    cause: Option<provenant_records::run::SteeringCause>,
    successor: Option<AttemptId>,
) -> Result<()> {
    if !matches!(state, "resumed" | "not_continued") || (state == "resumed") != cause.is_none() {
        return Err(RunError::Validation("steer final outcome shape"));
    }
    let has_intent: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM steer_intents WHERE attempt=?1)",
        [id.to_string()],
        |row| row.get(0),
    )?;
    if !has_intent {
        return Err(RunError::NotFound);
    }
    let cause = cause.map(provenant_records::run::SteeringCause::as_str);
    let successor = successor.map(|successor| successor.to_string());
    let existing: Option<SteerContinuationOutcome> = tx
        .query_row(
            "SELECT state,cause,successor,live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
            [id.to_string()],
            |row| {
                Ok(SteerContinuationOutcome {
                    state: row.get(0)?,
                    cause: row.get(1)?,
                    successor: row.get(2)?,
                    live_ack: row.get::<_, i64>(3)? != 0,
                    terminal_final: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?;
    let expected = SteerContinuationOutcome {
        state: state.to_owned(),
        cause: cause.map(str::to_owned),
        successor: successor.clone(),
        live_ack: false,
        terminal_final: true,
    };
    match existing {
        Some(existing)
            if existing.terminal_final
                && existing.state == expected.state
                && existing.cause == expected.cause
                && existing.successor == expected.successor => {}
        Some(existing)
            if !existing.terminal_final
                && existing.successor.as_deref() == successor.as_deref() =>
        {
            tx.execute(
                "UPDATE steer_continuations SET state=?1,cause=?2,terminal_final=1 WHERE attempt=?3 AND terminal_final=0",
                params![state, cause, id.to_string()],
            )?;
        }
        Some(_) => return Err(RunError::Conflict),
        None => {
            tx.execute(
                "INSERT INTO steer_continuations(attempt,state,cause,successor,live_ack,terminal_final) VALUES (?1,?2,?3,?4,0,1)",
                params![id.to_string(), state, cause, successor],
            )?;
        }
    }
    tx.execute(
        "UPDATE steer_intents SET resolved=1 WHERE attempt=?1 AND resolved=0",
        [id.to_string()],
    )?;
    Ok(())
}

pub(crate) fn record_steer_disposition(
    tx: &rusqlite::Connection,
    disposition: &SteerContinuationDisposition,
) -> Result<()> {
    let successor = disposition.successor.to_string();
    let existing: Option<SteerContinuationOutcome> = tx
        .query_row(
            "SELECT state,cause,successor,live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
            [disposition.parent.to_string()],
            |row| {
                Ok(SteerContinuationOutcome {
                    state: row.get(0)?,
                    cause: row.get(1)?,
                    successor: row.get(2)?,
                    live_ack: row.get::<_, i64>(3)? != 0,
                    terminal_final: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing.terminal_final {
            let cause = disposition
                .cause
                .map(provenant_records::run::SteeringCause::as_str);
            if existing.state == disposition.state
                && existing.cause.as_deref() == cause
                && existing.successor.as_deref() == Some(successor.as_str())
            {
                return Ok(());
            }
            return Err(RunError::Conflict);
        }
        if existing.successor.as_deref() != Some(successor.as_str()) {
            return Err(RunError::Conflict);
        }
    }
    record_steer_outcome(
        tx,
        disposition.parent,
        disposition.state,
        disposition.cause,
        Some(disposition.successor),
    )
}

/// Publish the live acknowledgement promptly without treating it as the
/// immutable terminal disposition. A later final record may revise the state.
fn record_steer_live_disposition(
    tx: &rusqlite::Connection,
    disposition: &SteerContinuationDisposition,
) -> Result<()> {
    if !matches!(disposition.state, "resumed" | "not_continued")
        || (disposition.state == "resumed") != disposition.cause.is_none()
    {
        return Err(RunError::Validation("live steer evidence shape"));
    }
    let has_intent: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM steer_intents WHERE attempt=?1)",
        [disposition.parent.to_string()],
        |row| row.get(0),
    )?;
    if !has_intent {
        return Err(RunError::NotFound);
    }
    let successor = disposition.successor.to_string();
    let existing: Option<SteerContinuationOutcome> = tx
        .query_row(
            "SELECT state,cause,successor,live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
            [disposition.parent.to_string()],
            |row| {
                Ok(SteerContinuationOutcome {
                    state: row.get(0)?,
                    cause: row.get(1)?,
                    successor: row.get(2)?,
                    live_ack: row.get::<_, i64>(3)? != 0,
                    terminal_final: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing.successor.as_deref() != Some(successor.as_str()) {
            return Err(RunError::Conflict);
        }
        if existing.terminal_final {
            return Ok(());
        }
        if existing.state == "not_continued" {
            return Ok(());
        }
        let live_ack = existing.live_ack || disposition.state == "resumed";
        tx.execute(
            "UPDATE steer_continuations SET state=?1,cause=?2,live_ack=?3 WHERE attempt=?4 AND terminal_final=0",
            params![
                disposition.state,
                disposition.cause.map(provenant_records::run::SteeringCause::as_str),
                if live_ack { 1_i64 } else { 0_i64 },
                disposition.parent.to_string()
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO steer_continuations(attempt,state,cause,successor,live_ack,terminal_final) VALUES (?1,?2,?3,?4,?5,0)",
            params![
                disposition.parent.to_string(),
                disposition.state,
                disposition.cause.map(provenant_records::run::SteeringCause::as_str),
                successor,
                if disposition.state == "resumed" {
                    1_i64
                } else {
                    0_i64
                }
            ],
        )?;
    }
    tx.execute(
        "UPDATE steer_intents SET resolved=1 WHERE attempt=?1 AND resolved=0",
        [disposition.parent.to_string()],
    )?;
    Ok(())
}

fn record_steer_admission(
    tx: &rusqlite::Connection,
    id: AttemptId,
    successor: AttemptId,
) -> Result<()> {
    let has_intent: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM steer_intents WHERE attempt=?1)",
        [id.to_string()],
        |row| row.get(0),
    )?;
    if !has_intent {
        return Err(RunError::NotFound);
    }
    let successor = successor.to_string();
    let existing: Option<SteerContinuationOutcome> = tx
        .query_row(
            "SELECT state,cause,successor,live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
            [id.to_string()],
            |row| {
                Ok(SteerContinuationOutcome {
                    state: row.get(0)?,
                    cause: row.get(1)?,
                    successor: row.get(2)?,
                    live_ack: row.get::<_, i64>(3)? != 0,
                    terminal_final: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?;
    match existing {
        Some(existing) if existing.successor.as_deref() == Some(successor.as_str()) => {
            if existing.state == "admitted"
                || matches!(existing.state.as_str(), "resumed" | "not_continued")
            {
                Ok(())
            } else {
                Err(RunError::Validation("steer admission state"))
            }
        }
        Some(_) => Err(RunError::Conflict),
        None => {
            tx.execute(
                "INSERT INTO steer_continuations(attempt,state,cause,successor,live_ack,terminal_final) VALUES (?1,'admitted',NULL,?2,0,0)",
                params![id.to_string(), successor],
            )?;
            Ok(())
        }
    }
}

/// Read-only presence projection for one durable Running attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptPresence {
    Live,
    Unattended,
    Unknown,
    Settling,
}

/// Observe process presence from the attempt's scoped supervisor checkpoint.
/// A published exit means settlement is pending; missing or unreadable proof
/// never fabricates a live process.
pub fn attempt_presence(root: &std::path::Path, record: &AttemptRecord) -> Option<AttemptPresence> {
    attempt_presence_with_observer(root, record, &SystemProcessObserver)
}

/// Share one bounded host census across a status page containing multiple
/// attempts. The caller supplies a read-only native snapshot from this host.
pub fn attempt_presence_with_census(
    root: &std::path::Path,
    record: &AttemptRecord,
    census: &provenant_supervisor::census::Census,
) -> Option<AttemptPresence> {
    attempt_presence_with_observer_and_census(root, record, &SystemProcessObserver, census)
}

/// Preserve attempt-scoped exit evidence when a command's shared host census
/// failed. This path checks the sealed exit without starting another census;
/// every remaining Running attempt stays unknown.
pub fn attempt_presence_with_unavailable_census(
    root: &std::path::Path,
    record: &AttemptRecord,
) -> Option<AttemptPresence> {
    attempt_presence_inner(root, record, &SystemProcessObserver, None, true)
}

pub(crate) fn attempt_presence_with_observer(
    root: &std::path::Path,
    record: &AttemptRecord,
    observer: &dyn ProcessObserver,
) -> Option<AttemptPresence> {
    attempt_presence_inner(root, record, observer, None, false)
}

pub(crate) fn attempt_presence_with_observer_and_census(
    root: &std::path::Path,
    record: &AttemptRecord,
    observer: &dyn ProcessObserver,
    census: &provenant_supervisor::census::Census,
) -> Option<AttemptPresence> {
    attempt_presence_inner(root, record, observer, Some(census), false)
}

fn attempt_presence_without_census(
    root: &std::path::Path,
    record: &AttemptRecord,
    observer: &dyn ProcessObserver,
) -> Option<AttemptPresence> {
    attempt_presence_inner(root, record, observer, None, true)
}

fn attempt_presence_inner(
    root: &std::path::Path,
    record: &AttemptRecord,
    observer: &dyn ProcessObserver,
    census: Option<&provenant_supervisor::census::Census>,
    census_unavailable: bool,
) -> Option<AttemptPresence> {
    if !matches!(record.state, AttemptState::Running { .. }) {
        return None;
    }
    let path = root.join(format!("attempts/{}", record.attempt_id));
    let Ok(attempt) = provenant_os::fs::Directory::open_anchor(&path) else {
        return Some(AttemptPresence::Unknown);
    };
    match read_sealed(&attempt, "process-exit.json", RECORD_LIMIT) {
        Ok(Some(_)) => return Some(AttemptPresence::Settling),
        Ok(None) => {}
        Err(_) => return Some(AttemptPresence::Unknown),
    }
    let holder = if let Some(census) = census {
        custody::observe_holder_with_census(&attempt, RunStore::scope(record), observer, census)
    } else if census_unavailable {
        custody::HolderPresence::Unknown
    } else {
        custody::observe_holder(&attempt, RunStore::scope(record), observer)
    };
    Some(match holder {
        custody::HolderPresence::Live => AttemptPresence::Live,
        custody::HolderPresence::Gone => AttemptPresence::Unattended,
        custody::HolderPresence::Unknown => AttemptPresence::Unknown,
    })
}

/// Read-only release diagnostics. The committed outcome remains authoritative;
/// unresolved descendants explain why its writer reservation is still held,
/// and the retention record names the guardian holding it and for how long.
pub fn release_progress(
    root: &std::path::Path,
    record: &AttemptRecord,
    census: Option<&provenant_supervisor::census::Census>,
    now: Option<WallTime>,
) -> Option<String> {
    if !matches!(record.state, AttemptState::Terminal { .. }) {
        return None;
    }
    let attempt = provenant_os::fs::Directory::open_anchor(
        &root.join(format!("attempts/{}", record.attempt_id)),
    )
    .ok()?;
    if read_sealed(&attempt, "release.json", RECORD_LIMIT)
        .ok()?
        .is_some()
    {
        return None;
    }
    let raw = read_sealed(&attempt, "process-exit.json", RECORD_LIMIT).ok()??;
    #[derive(serde::Deserialize)]
    struct Cleanup {
        scope: Scope,
        unresolved_cleanup: bool,
        unresolved_pids: Vec<i64>,
        #[serde(default)]
        unresolved_identities: Vec<provenant_supervisor::census::ProcessIdentity>,
        #[serde(default)]
        observed_holders: Vec<ObservedHolder>,
        #[serde(default)]
        retention: Option<custody::Retention>,
    }
    #[derive(serde::Deserialize)]
    struct ObservedHolder {
        pid: i64,
        #[serde(default)]
        command: String,
        resource: String,
        #[serde(default)]
        ownership_proven: bool,
    }
    let exit: Cleanup = crate::db::decode(&raw).ok()?;
    if exit.scope != RunStore::scope(record) {
        return None;
    }
    if !exit.unresolved_cleanup {
        return None;
    }
    let names = exit
        .unresolved_pids
        .iter()
        .take(16)
        .map(|pid| {
            let name = census
                .and_then(|census| match census.process(*pid) {
                    provenant_supervisor::census::ProcessLookup::Present(fact)
                        if exit.unresolved_identities.contains(&fact.identity) =>
                    {
                        fact.command.as_deref()
                    }
                    _ => None,
                })
                .map(|name| provenant_core::bounded_line(name, 128));
            name.map_or_else(
                || format!("pid {pid}"),
                |name| format!("{name} (pid {pid})"),
            )
        })
        .collect::<Vec<_>>();
    // Descriptor occupancy names a contended resource but never proves its
    // owner. Say so explicitly; only recorded descendants may be signalled.
    let holders = exit
        .observed_holders
        .iter()
        .take(8)
        .map(|observed| {
            let command = if observed.command.is_empty() {
                "unknown"
            } else {
                observed.command.as_str()
            };
            format!(
                "pid {} ({command}) on {}{}",
                observed.pid,
                provenant_core::bounded_line(&observed.resource, 128),
                if observed.ownership_proven {
                    ""
                } else {
                    ", ownership unproven"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let held = if holders.is_empty() {
        String::new()
    } else {
        format!("; held resources: {holders}")
    };
    // One command ends every survivor this attempt proved; unproven holders
    // stay named so the owner can close the service themselves.
    let remedy = if exit.unresolved_pids.is_empty() && holders.is_empty() {
        String::new()
    } else {
        "; run `provenant reconcile <lane>` to end proven survivors".to_owned()
    };
    let retention = exit.retention.map(|retention| {
        let guardian = custody::GuardianLiveness::observe(census, &retention.guardian);
        format!("; {}", retention.describe(guardian, now))
    });
    Some(format!(
        "releasing ({} survivors: {}){}{}{}",
        exit.unresolved_pids.len(),
        if names.is_empty() {
            "census pending".into()
        } else {
            names.join(", ")
        },
        held,
        remedy,
        retention.unwrap_or_default(),
    ))
}

/// Whether a running attempt is past its runtime deadline plus the grace.
pub fn overdue_at(record: &AttemptRecord, now: WallTime) -> bool {
    let (AttemptState::Running { .. }, Some(deadline)) = (&record.state, record.runtime_deadline)
    else {
        return false;
    };
    provenant_core::DurationMicros::from_seconds(DEADLINE_GRACE_SECONDS)
        .and_then(|grace| deadline.checked_add(grace))
        .is_ok_and(|limit| now >= limit)
}

/// The silence threshold recorded on this attempt, or the compiled default for
/// records written before the limit existed. Zero disables the report.
pub fn stall_after_seconds(record: &AttemptRecord) -> u64 {
    record
        .limits
        .as_ref()
        .and_then(|limits| limits.stall_after_seconds.as_ref())
        .map_or(provenant_records::DEFAULT_STALL_AFTER_SECONDS, |limit| {
            limit.value
        })
}

/// Read-only stall fact for a running, live attempt: the threshold and the
/// last output time, or the start time if the provider has emitted nothing.
/// A queued attempt is never measured as a running provider.
pub fn stall_fact(record: &AttemptRecord, now: WallTime) -> Option<(u64, WallTime)> {
    if !matches!(record.state, AttemptState::Running { .. }) {
        return None;
    }
    let last = record.last_output_at.or(record.started_at)?;
    let threshold = stall_after_seconds(record);
    // `as_micros` is i64; a clock that moved backwards has not gone silent, so
    // clamp the age at zero before widening it for the u64 threshold.
    let elapsed = now.as_micros().saturating_sub(last.as_micros()).max(0) as u128;
    (threshold > 0 && elapsed >= u128::from(threshold) * 1_000_000).then_some((threshold, last))
}

/// Whether a live running attempt has exceeded its silence threshold. `live`
/// is the host's presence observation: an unattended or unknown writer is a
/// custody problem, not a silent provider.
pub fn stalled_at(record: &AttemptRecord, now: WallTime, live: bool) -> bool {
    live && stall_fact(record, now).is_some()
}

/// Whether a running attempt has reboot-confirmed guardian/provider absence
/// with no exit recorded. Same-boot absence remains unknown because its last
/// checkpoint may miss an escaped child. Read-only; `root` is the execution
/// state root.
pub fn holder_absent(root: &std::path::Path, record: &AttemptRecord) -> bool {
    attempt_presence(root, record) == Some(AttemptPresence::Unattended)
}

fn running_attempt_reapable(presence: Option<AttemptPresence>) -> bool {
    matches!(
        presence,
        Some(AttemptPresence::Unattended | AttemptPresence::Settling | AttemptPresence::Unknown)
    )
}

/// What reconcile found for one attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reconciled {
    /// Not yet handed off; `start` launches it.
    Queued,
    /// A live guardian still holds custody.
    Held,
    /// A live guardian outlived its attempt's runtime deadline, so a stop at
    /// the deadline was requested of it. It settles the attempt as a timeout.
    DeadlineStop,
    /// A silent provider was stopped after the opt-in stall policy warned.
    /// Settlement records the policy-specific typed provider-stall cause.
    StallStop {
        policy: provenant_records::StallPolicy,
    },
    /// The supervisor is resuming durable settlement in the background.
    Settling {
        phase: String,
        held_reason: Option<String>,
    },
    /// A chair explicitly released settlement while retaining its evidence.
    Abandoned { reason: String },
    /// A durable exit was committed and released.
    Finished,
    /// Reconcile removed a provably stale writer reservation and released its
    /// worktree lock. The path identifies the stable lock slot it inspected.
    WriterLeaseReleased { locks: Vec<std::path::PathBuf> },
    /// Leftover temporary data was removed from a terminal attempt.
    TemporaryRemoved,
    /// The attempt is terminal, but temporary data could not be removed.
    TemporaryRetained {
        path: std::path::PathBuf,
        reason: TemporaryReason,
    },
    /// Launch never reached a guardian; ended as rejected.
    Withdrawn,
    /// Cleanup ended with these descendants possibly alive (for example one
    /// that left the process group); custody stays fenced until they exit.
    Escaped { pids: Vec<i64> },
    /// Custody cannot be proven either way; it stays fenced.
    Uncertain { reason: UncertainReason },
}
fn reservation_issue_blocks_report(reservation: &std::path::Path, issue: &std::path::Path) -> bool {
    reservation.starts_with(issue)
        || reservation
            .parent()
            .and_then(std::path::Path::parent)
            .is_some_and(|slot| issue == slot.join("reservation.json"))
}

impl std::fmt::Display for Reconciled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => f.write_str("queued"),
            Self::Held => f.write_str("held"),
            Self::DeadlineStop => f.write_str("deadline_stop"),
            Self::StallStop { policy } => write!(f, "stall_stop ({})", policy.as_str()),
            Self::Settling { phase, held_reason } => {
                write!(f, "settling ({phase})")?;
                if let Some(reason) = held_reason {
                    write!(f, ": {reason}")?;
                }
                Ok(())
            }
            Self::Abandoned { reason } => write!(f, "abandoned: {reason}"),
            Self::Finished => f.write_str("finished"),
            Self::WriterLeaseReleased { locks } => write!(
                f,
                "writer_lease_released {}",
                locks
                    .iter()
                    .map(|lock| lock.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::TemporaryRemoved => f.write_str("temporary_removed"),
            Self::TemporaryRetained { path, reason } => {
                write!(f, "temporary_retained {}: {reason}", path.display())
            }
            Self::Withdrawn => f.write_str("withdrawn"),
            Self::Escaped { pids } => write!(
                f,
                "escaped: {}",
                pids.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Uncertain { reason } => write!(f, "uncertain: {reason}"),
        }
    }
}
/// Stable reason for retaining terminal temporary data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporaryReasonKind {
    Error,
    ReservationUnreadable,
    ReservationRetained,
    NotPrivate,
}

/// A typed reason that remains a string in reconcile output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TemporaryReason {
    text: String,
    #[serde(skip)]
    pub kind: TemporaryReasonKind,
}
impl TemporaryReason {
    fn new(kind: TemporaryReasonKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}
impl From<String> for TemporaryReason {
    fn from(text: String) -> Self {
        Self::new(TemporaryReasonKind::Error, text)
    }
}
impl From<&str> for TemporaryReason {
    fn from(text: &str) -> Self {
        Self::from(text.to_owned())
    }
}
impl std::ops::Deref for TemporaryReason {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}
impl std::fmt::Display for TemporaryReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Why custody could not be proven. The text of an underlying error rides
/// along as `error`; the kind is what a caller matches on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UncertainReason {
    /// Reconciling the attempt failed before it could be settled.
    ReconcileFailed { error: String },
    /// Reboot evidence could not be established; the wire kind is unchanged.
    #[serde(rename = "reconcile_failed")]
    RebootProofUnavailable {
        error: String,
        #[serde(skip)]
        cause: RebootProofCause,
    },
    /// Custody could not be reattached, acquired or observed.
    CustodyUnavailable { error: String },
    /// A running attempt has no launch summary.
    NoLaunchSummary,
    /// The launch custody is neither held nor provably absent.
    CustodyNotProvablyAbsent,
    /// A process exit exists although no launch was recorded.
    ExitWithoutLaunchSummary,
    /// The guardian is neither alive nor finished as far as can be seen.
    GuardianNotObservable,
    /// A recorded supervisor is stuck in uninterruptible sleep.
    HostStalled,
    /// A launch was handed to a writer whose custody is unconfirmed.
    WriterCustodyUnconfirmed { reservation: std::path::PathBuf },
    /// A writer reservation outlived the attempt's terminal commit.
    WriterReservationRetained { reservation: std::path::PathBuf },
    /// Another holder kept the writer lock past settlement's bounded wait.
    /// Settlement retries; contention never consumes the recovery budget.
    WriterLockBusy {
        lock: std::path::PathBuf,
        holder: String,
        holder_live: bool,
    },
}
/// The source of an unavailable reboot proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebootProofCause {
    Filesystem,
    Other,
}

/// Causes of an attempt lost during custody recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LostReason {
    Holderless,
    RemovedWorktree,
    StaleReservation,
}
impl LostReason {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Holderless => {
                "no live holder: the guardian and provider ended before the attempt settled"
            }
            Self::RemovedWorktree => {
                "machine rebooted before settlement and the writer worktree was removed"
            }
            Self::StaleReservation => {
                "writer reservation was gone or stale and the recorded process tree predates this boot"
            }
        }
    }
}
impl RebootProofCause {
    fn from_error(error: &RunError) -> Self {
        if matches!(error, RunError::Filesystem(_)) {
            Self::Filesystem
        } else {
            Self::Other
        }
    }
}
impl std::fmt::Display for UncertainReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReconcileFailed { error }
            | Self::RebootProofUnavailable { error, .. }
            | Self::CustodyUnavailable { error } => f.write_str(error),
            Self::NoLaunchSummary => f.write_str("running attempt has no launch summary"),
            Self::CustodyNotProvablyAbsent => f.write_str("launch custody is not provably absent"),
            Self::ExitWithoutLaunchSummary => f.write_str("exit exists without a launch summary"),
            Self::GuardianNotObservable => f.write_str("guardian is not observable"),
            Self::HostStalled => {
                f.write_str("host_stalled: supervisor is in uninterruptible sleep")
            }
            Self::WriterCustodyUnconfirmed { reservation } => write!(
                f,
                "writer custody is unconfirmed; reservation retained at {}; chair recovery required",
                reservation.display()
            ),
            Self::WriterReservationRetained { reservation } => write!(
                f,
                "writer reservation retained after terminal commit: {}; chair recovery required",
                reservation.display()
            ),
            Self::WriterLockBusy { lock, holder, .. } => write!(
                f,
                "waiting for writer lock {}, held by {holder}",
                lock.display()
            ),
        }
    }
}

/// How long recovery waits after each of TERM and KILL for an orphan to go.
const ORPHAN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
#[path = "controls_reboot_tests.rs"]
mod reboot_tests;

#[cfg(test)]
#[path = "controls_lineage_tests.rs"]
mod lineage_tests;

impl Reconciled {
    fn uncertain(reason: UncertainReason) -> Self {
        Self::Uncertain { reason }
    }
    fn custody_unavailable(error: &dyn std::fmt::Display) -> Self {
        Self::uncertain(UncertainReason::CustodyUnavailable {
            error: error.to_string(),
        })
    }
}

// Recovery is idempotent and the next reconciliation can retry a busy store.
// SQLite retains its single bounded integrity wait; no outer retry ladder.
pub(crate) fn retry_reconcile<T>(
    _db: &rusqlite::Connection,
    call: &Request<'_>,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    call.check()?;
    action()
}

/// Map a failed writer-exclusion acquisition. Contention, at the lock or its
/// observation gate, is typed and names the recorded holder; anything else
/// stays an I/O failure.
pub(crate) fn writer_exclusion_error(
    store: &RunStore,
    slot: &provenant_os::fs::Directory,
    own: Scope,
    error: CustodyError,
    call: &Request<'_>,
) -> RunError {
    let lock = match error {
        CustodyError::Filesystem(provenant_os::fs::FsError::LockTimeout { path, .. }) => path,
        CustodyError::Busy => slot
            .current_path()
            .map_or_else(|_| "writer.lock".into(), |path| path.join("writer.lock")),
        other => return RunError::Io(std::io::Error::other(other.to_string())),
    };
    let (holder, holder_live) = writer_lock_holder(store, slot, own, call);
    RunError::WriterExclusionBusy {
        lock,
        holder,
        holder_live,
    }
}

/// Name the attempts whose reservations occupy a writer slot, for status, and
/// whether any holder may still release the lock. A flock has no owner record,
/// so the slot's reservations are the evidence: a reserved attempt may still
/// release it while its guardian is live or unobservable, or while its
/// settlement owner is live (a settlement worker can hold the lock through a
/// long import after the guardian exits). An unidentified holder, or one whose
/// settlement owner cannot be read, keeps the wait going. The caller's own
/// reservation is labelled; the caller is its settlement owner.
fn writer_lock_holder(
    store: &RunStore,
    slot: &provenant_os::fs::Directory,
    own: Scope,
    call: &Request<'_>,
) -> (String, bool) {
    let Ok(listing) = custody::list_writer_reservations_in_slot(slot) else {
        return (
            "an unidentified holder (slot reservations unreadable)".into(),
            true,
        );
    };
    if listing.reservations.is_empty() {
        return (
            "an unidentified holder (no reservation recorded in the slot)".into(),
            true,
        );
    }
    let mut holder_live = false;
    let holder = listing
        .reservations
        .iter()
        .map(|reservation| {
            let own_reservation = reservation.scope == own;
            let (may_release, liveness) =
                reservation_may_release(own_reservation, reservation.live, || {
                    store
                        .attempt_dir(reservation.scope.attempt, false)
                        .ok()
                        .and_then(|attempt| {
                            crate::settlement::settlement_owner_live(
                                &attempt,
                                reservation.scope,
                                call.clock,
                            )
                        })
                });
            holder_live |= may_release;
            let whose = if own_reservation {
                "this attempt's own reservation"
            } else {
                "attempt"
            };
            format!(
                "{whose} {} ({liveness}, reserved {} ago)",
                reservation.scope.attempt,
                reservation.age.as_deref().unwrap_or("an unknown time")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    (holder, holder_live)
}

/// Whether one reserved attempt may still release a writer lock, and its
/// liveness for status. A live or unobservable guardian may; so may another
/// attempt's live or unreadable settlement owner. The waiter is its own
/// reservation's settlement owner, so only its guardian counts there.
fn reservation_may_release(
    own: bool,
    guardian: Option<bool>,
    settlement_owner: impl FnOnce() -> Option<bool>,
) -> (bool, &'static str) {
    match guardian {
        Some(true) => (true, "guardian live"),
        None => (true, "guardian unobservable"),
        Some(false) if own => (false, "guardian exited"),
        Some(false) => match settlement_owner() {
            Some(true) => (true, "guardian exited, settlement owner live"),
            Some(false) => (false, "guardian exited, no settlement owner"),
            None => (true, "guardian exited, settlement owner unknown"),
        },
    }
}

// A lock timeout can race a terminal commit by another settlement owner.
// Writer contention that did not race one is a wait, never a failure.
fn reconcile_failure(error: RunError, terminal: impl FnOnce() -> bool) -> Reconciled {
    if let RunError::WriterExclusionBusy {
        lock,
        holder,
        holder_live,
    } = error
    {
        return if terminal() {
            Reconciled::Finished
        } else {
            Reconciled::uncertain(UncertainReason::WriterLockBusy {
                lock,
                holder,
                holder_live,
            })
        };
    }
    let timed_out = match &error {
        RunError::Contention
        | RunError::Filesystem(provenant_os::fs::FsError::LockTimeout { .. }) => true,
        // Custody wraps some filesystem errors as I/O errors at its boundary.
        RunError::Io(error) => error.to_string().starts_with("lock acquisition timed out"),
        _ => false,
    };
    if timed_out && terminal() {
        Reconciled::Finished
    } else {
        Reconciled::uncertain(UncertainReason::ReconcileFailed {
            error: error.to_string(),
        })
    }
}

#[test]
fn lock_timeout_reports_a_concurrently_settled_attempt() {
    let timeout = || {
        RunError::Filesystem(provenant_os::fs::FsError::LockTimeout {
            path: "writer.lock".into(),
            timeout: std::time::Duration::from_secs(1),
        })
    };
    assert_eq!(reconcile_failure(timeout(), || true), Reconciled::Finished);
    let wrapped = RunError::Io(std::io::Error::other(
        provenant_os::fs::FsError::LockTimeout {
            path: "writer.lock".into(),
            timeout: std::time::Duration::from_secs(1),
        },
    ));
    assert_eq!(reconcile_failure(wrapped, || true), Reconciled::Finished);
    assert!(matches!(
        reconcile_failure(timeout(), || false),
        Reconciled::Uncertain { .. }
    ));
    assert!(matches!(
        reconcile_failure(RunError::Validation("other failure"), || true),
        Reconciled::Uncertain { .. }
    ));
    let busy = || RunError::WriterExclusionBusy {
        lock: "writer.lock".into(),
        holder: "attempt X (guardian live, reserved 2s ago)".into(),
        holder_live: true,
    };
    assert_eq!(reconcile_failure(busy(), || true), Reconciled::Finished);
    assert_eq!(
        reconcile_failure(busy(), || false),
        Reconciled::Uncertain {
            reason: UncertainReason::WriterLockBusy {
                lock: "writer.lock".into(),
                holder: "attempt X (guardian live, reserved 2s ago)".into(),
                holder_live: true,
            }
        }
    );
}

impl RunStore {
    /// Apply Run's policy after warning. The guardian and recovery can retry
    /// an interrupted action; steering and custody requests are idempotent.
    pub(crate) fn apply_stall_policy(
        &self,
        record: &AttemptRecord,
        handle: &provenant_supervisor::custody::CustodyHandle,
        call: &Request<'_>,
    ) -> Result<Option<Reconciled>> {
        use provenant_records::StallPolicy;
        use provenant_records::warning::{self, Warning};
        let limit = record
            .limits
            .as_ref()
            .and_then(|limits| limits.stall_after_seconds.clone());
        let policy = record
            .limits
            .as_ref()
            .map_or(StallPolicy::Report, |limits| limits.stall_policy);
        if limit.is_none() {
            return Ok(None);
        }
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        const MARKER: &str = "stall-action.json";
        if let Some(raw) = read_sealed(&attempt, MARKER, RECORD_LIMIT)? {
            let marker: serde_json::Value = serde_json::from_slice(&raw)
                .map_err(|_| RunError::Validation("stall action marker"))?;
            if matches!(marker["policy"].as_str(), Some("retry" | "fail")) {
                warning::warn(
                    Warning::new(
                        warning::PROVIDER_STALLED,
                        "retrying the recorded stall stop request",
                    )
                    .subject(record.attempt_id.to_string())
                    .once_per_change(),
                );
                custody::stop_if_unrequested(handle, StopReason::SupervisorSignal)
                    .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
                return Ok(Some(Reconciled::StallStop { policy }));
            }
            return Ok(None);
        }
        let now = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let Some((threshold, last)) = stall_fact(record, now) else {
            return Ok(None);
        };
        let marker = serde_json::json!({
            "schema_version": 1,
            "policy": policy.as_str(),
            "threshold_seconds": threshold,
            "last_event_at": last,
            "source": limit.as_ref().map(|limit| limit.source),
        });
        let source = provenant_records::provenance::Source {
            layer: limit
                .as_ref()
                .map_or(provenant_records::Layer::Default, |limit| limit.source),
            origin: Some(format!(
                "providers.{}.stall_after_seconds",
                record
                    .applied_route
                    .as_ref()
                    .and_then(|route| route.provider.as_deref())
                    .unwrap_or("unknown")
            )),
        };
        let message = format!(
            "PROVIDER_STALLED: provider silent for at least {threshold}s since {last}; policy {}",
            policy.as_str()
        );
        // Guardian stdio is detached. Publish the warning for status before
        // requesting an action, rather than relying on its stderr sink.
        if read_sealed(&attempt, "stall-warning.json", RECORD_LIMIT)?.is_none() {
            crate::artifacts::seal(
                &attempt,
                "stall-warning.json",
                &crate::db::bytes(&message)?,
                RECORD_LIMIT,
            )?;
        }
        warning::warn(
            Warning::new(warning::PROVIDER_STALLED, message)
                .subject(record.attempt_id.to_string())
                .source(source)
                .once_per_change(),
        );
        match policy {
            StallPolicy::Report => Ok(None),
            StallPolicy::Steer => {
                // The operation must keep identical text across an interrupted
                // reconcile, even if further provider output was observed.
                let text = "The provider has gone silent. Continue the task now; if you cannot, report the blocker.";
                let operation = OperationId::parse(&format!("stall-{}", record.attempt_id))
                    .map_err(|_| RunError::Validation("stall steer operation"))?;
                self.steer(record.attempt_id, operation, text, call)?;
                crate::artifacts::seal(
                    &attempt,
                    MARKER,
                    &crate::db::bytes(&marker)?,
                    RECORD_LIMIT,
                )?;
                Ok(Some(Reconciled::Held))
            }
            StallPolicy::Retry | StallPolicy::Fail => {
                crate::artifacts::seal(
                    &attempt,
                    MARKER,
                    &crate::db::bytes(&marker)?,
                    RECORD_LIMIT,
                )?;
                custody::stop_if_unrequested(handle, StopReason::SupervisorSignal)
                    .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
                Ok(Some(Reconciled::StallStop { policy }))
            }
        }
    }

    /// Persist one idempotent steer for an exact running attempt. The default
    /// delivery queues the turn at the next boundary without interrupting this
    /// attempt. `steer_now` asks for the session-preserving restart instead.
    pub fn steer(
        &self,
        id: AttemptId,
        operation_id: OperationId,
        text: &str,
        call: &Request<'_>,
    ) -> Result<SteerReceipt> {
        self.steer_inner(id, operation_id, text, false, call)
    }

    /// Persist a steer and interrupt through custody once the session identity
    /// is durable, so settlement resumes the same session with the text. The
    /// text is durable before any stop is requested, so a custody race cannot
    /// lose the accepted instruction.
    pub fn steer_now(
        &self,
        id: AttemptId,
        operation_id: OperationId,
        text: &str,
        call: &Request<'_>,
    ) -> Result<SteerReceipt> {
        self.steer_inner(id, operation_id, text, true, call)
    }

    fn steer_inner(
        &self,
        id: AttemptId,
        operation_id: OperationId,
        text: &str,
        now: bool,
        call: &Request<'_>,
    ) -> Result<SteerReceipt> {
        if text.trim().is_empty() || text.len() > provenant_records::control::CONTROL_TEXT_BYTES {
            return Err(RunError::Validation("steering text"));
        }
        let at = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let digest = provenant_records::CanonicalV1::record_digest(
            "steer_intent",
            1,
            &(id, &operation_id, text),
        )?;
        let message_digest = provenant_records::CanonicalV1::record_digest(
            "steer_message",
            1,
            &(id, &operation_id),
        )?;
        let mut message_bytes = [0; 16];
        message_bytes.copy_from_slice(&message_digest.as_bytes()[..16]);
        let expected = SteerIntent {
            schema_version: 1,
            attempt: id,
            operation_id: operation_id.clone(),
            message_id: MessageId::from_bytes(message_bytes),
            text: text.to_owned(),
            created_at: at,
        };
        let encoded = crate::db::bytes(&expected)?;
        let (intent, terminal, already_queued, inserted) = self.write(call, |tx| {
            let raw: Vec<u8> = tx
                .query_row(
                    "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                    params![id.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            let record = crate::db::decode_attempt(&raw)?;
            let saved: Option<(String, String, Vec<u8>)> = tx
                .query_row(
                    "SELECT attempt,digest,record FROM steer_intents WHERE operation=?1",
                    [operation_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((saved_attempt, saved_digest, saved_record)) = saved {
                let saved: SteerIntent = crate::db::decode_column(&saved_record)?;
                if saved_attempt != id.to_string()
                    || saved.attempt != id
                    || saved.operation_id != operation_id
                    || saved_digest != digest.to_string()
                    || saved.text != text
                    || saved.message_id != expected.message_id
                {
                    return Err(RunError::OperationConflict);
                }
                return Ok((
                    saved,
                    matches!(record.state, AttemptState::Terminal { .. }),
                    true,
                    false,
                ));
            }
            if matches!(record.state, AttemptState::Terminal { .. }) {
                return Err(RunError::Nonterminal);
            }
            if !matches!(record.state, AttemptState::Running { .. }) {
                return Err(RunError::Nonterminal);
            }
            let mut total_bytes = 0usize;
            let mut ledger_bytes = 0usize;
            let mut count = 0usize;
            let mut query =
                tx.prepare("SELECT record FROM steer_intents WHERE attempt=?1 ORDER BY sequence")?;
            let rows = query.query_map([id.to_string()], |row| row.get::<_, Vec<u8>>(0))?;
            for row in rows {
                let raw = row?;
                ledger_bytes = ledger_bytes.saturating_add(raw.len());
                let existing: SteerIntent = crate::db::decode_column(&raw)?;
                total_bytes = total_bytes.saturating_add(existing.text.len());
                count = count.saturating_add(1);
            }
            let prompt_bytes = STEER_PROMPT_PREFIX
                .len()
                .saturating_add(total_bytes)
                .saturating_add(text.len())
                .saturating_add(count.saturating_mul(STEER_TEXT_SEPARATOR.len()));
            let ledger_limit = usize::try_from(RECORD_LIMIT).unwrap_or(usize::MAX);
            if prompt_bytes > ledger_limit
                || ledger_bytes.saturating_add(encoded.len()) > ledger_limit
            {
                return Err(RunError::Validation(
                    "steering queue reached the run prompt limit; retry after the successor starts",
                ));
            }
            tx.execute(
                "INSERT INTO steer_intents(attempt,operation,digest,record) VALUES (?1,?2,?3,?4)",
                params![
                    id.to_string(),
                    operation_id.to_string(),
                    digest.to_string(),
                    encoded
                ],
            )?;
            Ok((expected.clone(), false, false, true))
        })?;
        if terminal {
            return Ok(SteerReceipt::AlreadyTerminal {
                message_id: intent.message_id,
            });
        }

        let record = self.record(id, call)?;
        if matches!(record.state, AttemptState::Terminal { .. }) {
            return Ok(if inserted {
                SteerReceipt::Queued {
                    message_id: intent.message_id,
                }
            } else {
                SteerReceipt::AlreadyTerminal {
                    message_id: intent.message_id,
                }
            });
        }
        // The accepted text is durable before any interrupt. Only `--now`
        // requests a `Steered` stop, and only once the session identity is
        // durable; the default path leaves the attempt running for settlement
        // to deliver the queued turn at its next boundary.
        if now {
            let _ = self.request_steer_stop_if_ready(id, call);
        }
        Ok(if already_queued {
            SteerReceipt::AlreadyQueued {
                message_id: intent.message_id,
            }
        } else {
            SteerReceipt::Queued {
                message_id: intent.message_id,
            }
        })
    }

    /// Persist the first session identity emitted by the admitted adapter's
    /// validated stream. The source identity is passed by the supervisor from
    /// the spawned child before it reads stdout, so reservation-file timing
    /// cannot lose the first session frame.
    pub fn record_provider_session_ready(
        &self,
        id: AttemptId,
        route: &RouteRecord,
        session: &str,
        source: &provenant_os::capture::CaptureSourceIdentity,
        call: &Request<'_>,
    ) -> Result<bool> {
        if session.is_empty()
            || session.len() > 4096
            || session.chars().any(char::is_control)
            || source.boot.is_empty()
            || source.boot.len() > 256
            || source.pid <= 0
            || source.native_start == 0
        {
            return Err(RunError::Validation("provider session readiness"));
        }
        let route_digest = steer_route_digest(route)?;
        if route.provider.is_none() {
            return Err(RunError::Validation("provider session route"));
        }
        let admitted = self.record(id, call)?;
        if !matches!(admitted.state, AttemptState::Running { .. })
            || source.attempt != id.to_string()
            || source.generation != Self::scope(&admitted).generation
            || admitted.applied_route.as_ref().is_none_or(|applied| {
                steer_route_digest(applied).ok().as_deref() != Some(route_digest.as_str())
            })
        {
            return Ok(false);
        }
        let previous_ready = self.steer_session_ready(id, call)?;
        let same_bound_process = previous_ready.as_ref().is_some_and(|ready| {
            ready.provider_boot == source.boot
                && ready.provider_pid == source.pid
                && ready.provider_start == source.native_start
        });
        if !same_bound_process && !source_is_live(source) {
            return Ok(false);
        }
        let ready = self.write(call, |tx| {
            let raw: Vec<u8> = tx
                .query_row(
                    "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                    params![id.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            let record = crate::db::decode_attempt(&raw)?;
            if !matches!(record.state, AttemptState::Running { .. })
                || Self::scope(&record).generation != source.generation
                || record
                    .applied_route
                    .as_ref()
                    .is_none_or(|applied| {
                        steer_route_digest(applied).ok().as_deref() != Some(route_digest.as_str())
                    })
            {
                return Ok(false);
            }
            let scope = Self::scope(&record);
            let saved: Option<ProviderSessionReadyRow> = tx
                .query_row(
                    "SELECT generation,route_digest,session,provider_boot,provider_pid,provider_start,identity_conflict,terminal_confirmed FROM provider_session_ready WHERE attempt=?1",
                    [id.to_string()],
                    provider_session_ready_row,
                )
                .optional()?;
            if let Some(saved) = saved {
                let conflict = saved.generation != i64::try_from(scope.generation).unwrap_or(i64::MAX)
                    || saved.route_digest != route_digest
                    || saved.session.as_deref() != Some(session)
                    || saved.provider_boot != source.boot
                    || saved.provider_pid != source.pid
                    || saved.provider_start != i64::try_from(source.native_start).unwrap_or(i64::MAX);
                if conflict && saved.identity_conflict == 0 {
                    tx.execute(
                        "UPDATE provider_session_ready SET identity_conflict=1 WHERE attempt=?1",
                        [id.to_string()],
                    )?;
                }
            } else {
                tx.execute(
                    "INSERT INTO provider_session_ready(attempt,generation,route_digest,session,provider_boot,provider_pid,provider_start,identity_conflict,terminal_confirmed) VALUES (?1,?2,?3,?4,?5,?6,?7,0,0)",
                    params![
                        id.to_string(),
                        i64::try_from(scope.generation).map_err(|_| RunError::Conflict)?,
                        route_digest,
                        session,
                        source.boot,
                        source.pid,
                        i64::try_from(source.native_start).map_err(|_| RunError::Conflict)?
                    ],
                )?;
            }
            Ok(true)
        })?;
        if ready {
            self.persist_live_steer_successor_evidence(id, call)?;
        }
        Ok(ready)
    }

    /// Permanently fence continuation when the validated adapter stream
    /// contradicts the model or session bound to this live provider process.
    /// The route and process identity must still match the admitted candidate.
    pub fn record_provider_identity_conflict(
        &self,
        id: AttemptId,
        route: &RouteRecord,
        source: &provenant_os::capture::CaptureSourceIdentity,
        call: &Request<'_>,
    ) -> Result<bool> {
        if source.boot.is_empty()
            || source.boot.len() > 256
            || source.pid <= 0
            || source.native_start == 0
        {
            return Err(RunError::Validation("provider identity conflict source"));
        }
        let route_digest = steer_route_digest(route)?;
        if route.provider.is_none() {
            return Err(RunError::Validation("provider identity conflict route"));
        }
        let admitted = self.record(id, call)?;
        if !matches!(admitted.state, AttemptState::Running { .. })
            || source.attempt != id.to_string()
            || source.generation != Self::scope(&admitted).generation
            || admitted.applied_route.as_ref().is_none_or(|applied| {
                steer_route_digest(applied).ok().as_deref() != Some(route_digest.as_str())
            })
        {
            return Ok(false);
        }
        let recorded = self.write(call, |tx| {
            let raw: Vec<u8> = tx
                .query_row(
                    "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                    params![id.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            let record = crate::db::decode_attempt(&raw)?;
            if !matches!(record.state, AttemptState::Running { .. })
                || Self::scope(&record).generation != source.generation
                || record
                    .applied_route
                    .as_ref()
                    .is_none_or(|applied| {
                        steer_route_digest(applied).ok().as_deref()
                            != Some(route_digest.as_str())
                    })
            {
                return Ok(false);
            }
            let changed = tx.execute(
                "UPDATE provider_session_ready SET identity_conflict=1 WHERE attempt=?1",
                [id.to_string()],
            )?;
            if changed == 1 {
                return Ok(true);
            }
            // The adapter can reject the first frame before it returns the
            // session event in that same frame. Retain a conflict-only row
            // anyway so natural settlement cannot adopt a contradictory stream.
            tx.execute(
                "INSERT INTO provider_session_ready(attempt,generation,route_digest,session,provider_boot,provider_pid,provider_start,identity_conflict,terminal_confirmed) VALUES (?1,?2,?3,NULL,?4,?5,?6,1,0)",
                params![
                    id.to_string(),
                    i64::try_from(source.generation).map_err(|_| RunError::Conflict)?,
                    route_digest,
                    source.boot,
                    source.pid,
                    i64::try_from(source.native_start).map_err(|_| RunError::Conflict)?,
                ],
            )?;
            Ok(true)
        })?;
        if recorded {
            self.persist_live_steer_successor_evidence(id, call)?;
        }
        Ok(recorded)
    }

    pub(crate) fn steer_session_ready(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<SteerSessionReady>> {
        call.check()?;
        let saved: Option<ProviderSessionReadyRow> = self
            .db
            .query_row(
                "SELECT generation,route_digest,session,provider_boot,provider_pid,provider_start,identity_conflict,terminal_confirmed FROM provider_session_ready WHERE attempt=?1",
                [id.to_string()],
                provider_session_ready_row,
            )
            .optional()?;
        let Some(saved) = saved else {
            return Ok(None);
        };
        Ok(Some(SteerSessionReady {
            generation: u64::try_from(saved.generation)
                .map_err(|_| RunError::Validation("provider session generation"))?,
            route_digest: saved.route_digest,
            session: saved.session,
            provider_boot: saved.provider_boot,
            provider_pid: saved.provider_pid,
            provider_start: u64::try_from(saved.provider_start)
                .map_err(|_| RunError::Validation("provider session process start"))?,
            conflict: saved.identity_conflict != 0,
            terminal_confirmed: saved.terminal_confirmed != 0,
        }))
    }

    /// Read the sealed session expectation for a resumed provider invocation.
    /// The expected identity is private Run state and is valid only when it
    /// still matches both the child's applied route and its predecessor.
    pub fn continuation_expectation(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<crate::ContinuationExpectation>> {
        let record = self.record(id, call)?;
        let saved: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT session,route_digest FROM continuation_expectations WHERE attempt=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((session, digest)) = saved else {
            if record.retry_of.is_some() && record.continuation.as_deref() != Some("fresh") {
                return Err(RunError::Validation(
                    "same-session continuation expectation missing",
                ));
            }
            return Ok(None);
        };
        if record.retry_of.is_none() || record.continuation.is_some() {
            return Err(RunError::Validation(
                "unexpected continuation session expectation",
            ));
        }
        let applied = record.applied_route.as_ref().ok_or(RunError::Validation(
            "continuation expectation route missing",
        ))?;
        if crate::controls::steer_route_digest(applied)? != digest {
            return Err(RunError::Validation(
                "continuation expectation route mismatch",
            ));
        }
        let parent_id = record.retry_of.ok_or(RunError::Validation(
            "continuation expectation parent missing",
        ))?;
        let parent = self.record(parent_id, call)?;
        let parent_session = parent.session.as_deref().ok_or(RunError::Validation(
            "continuation expectation session missing",
        ))?;
        let parent_route = parent.applied_route.as_ref().ok_or(RunError::Validation(
            "continuation expectation parent route missing",
        ))?;
        if parent_session != session || crate::controls::steer_route_digest(parent_route)? != digest
        {
            return Err(RunError::Validation(
                "continuation expectation lineage mismatch",
            ));
        }
        Ok(Some(crate::ContinuationExpectation { session }))
    }

    pub(crate) fn steer_parent_for_successor(
        &self,
        successor: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<AttemptId>> {
        call.check()?;
        let link: Option<(Option<String>, String)> = self
            .db
            .query_row(
                "SELECT continues,operation FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                params![successor.to_string(), self.context.project_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((Some(parent), operation)) = link else {
            return Ok(None);
        };
        let parent = AttemptId::parse(&parent)
            .map_err(|_| RunError::Validation("steer parent attempt ID"))?;
        if operation != format!("graph-steer-{parent}") {
            return Ok(None);
        }
        let has_intent: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM steer_intents WHERE attempt=?1)",
            [parent.to_string()],
            |row| row.get(0),
        )?;
        Ok(has_intent.then_some(parent))
    }

    fn persist_live_steer_successor_evidence(
        &self,
        successor: AttemptId,
        call: &Request<'_>,
    ) -> Result<()> {
        let Some(ready) = self.steer_session_ready(successor, call)? else {
            return Ok(());
        };
        let child = self.record(successor, call)?;
        let child_route_digest = child
            .applied_route
            .as_ref()
            .map(crate::controls::steer_route_digest)
            .transpose()?;
        let identity_conflict = ready.conflict
            || ready.generation != child.generation
            || child_route_digest.as_deref() != Some(ready.route_digest.as_str());
        let disposition = self.steer_successor_disposition(
            successor,
            &child,
            SteerSuccessorProof {
                session: ready.session.as_deref(),
                route_digest: Some(&ready.route_digest),
                identity_conflict,
                evidence_incomplete: false,
            },
            call,
        )?;
        if let Some(disposition) = disposition {
            self.persist_live_steer_successor_disposition(&disposition, call)?;
        }
        Ok(())
    }

    pub(crate) fn steer_successor_disposition(
        &self,
        successor: AttemptId,
        child: &AttemptRecord,
        proof: SteerSuccessorProof<'_>,
        call: &Request<'_>,
    ) -> Result<Option<SteerContinuationDisposition>> {
        let SteerSuccessorProof {
            session,
            route_digest,
            identity_conflict,
            evidence_incomplete,
        } = proof;
        let Some(parent_id) = self.steer_parent_for_successor(successor, call)? else {
            return Ok(None);
        };
        if child.attempt_id != successor || child.retry_of != Some(parent_id) {
            return Err(RunError::Validation("steer successor lineage"));
        }
        let parent = self.record(parent_id, call)?;
        let expected: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT session,route_digest FROM continuation_expectations WHERE attempt=?1",
                [successor.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let parent_route_digest = parent
            .applied_route
            .as_ref()
            .map(crate::controls::steer_route_digest)
            .transpose()?;
        let child_route_digest = child
            .applied_route
            .as_ref()
            .map(crate::controls::steer_route_digest)
            .transpose()?;
        let route_is_missing =
            parent_route_digest.is_none() || child_route_digest.is_none() || route_digest.is_none();

        let (state, cause) = if identity_conflict {
            (
                "not_continued",
                Some(provenant_records::run::SteeringCause::SessionMismatch),
            )
        } else if let Some((expected_session, expected_route)) = expected {
            let Some(parent_session) = parent.session.as_deref() else {
                return Ok(Some(SteerContinuationDisposition {
                    parent: parent_id,
                    successor,
                    state: "not_continued",
                    cause: Some(provenant_records::run::SteeringCause::SessionUnavailable),
                }));
            };
            if route_is_missing {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionProofIncomplete),
                )
            } else if expected_session != parent_session
                || parent_route_digest.as_deref() != Some(expected_route.as_str())
                || child_route_digest.as_deref() != Some(expected_route.as_str())
                || route_digest != Some(expected_route.as_str())
                || session.is_some_and(|session| session != expected_session)
            {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionMismatch),
                )
            } else if evidence_incomplete {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionProofIncomplete),
                )
            } else if session.is_none() {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionUnavailable),
                )
            } else {
                ("resumed", None)
            }
        } else if self.request(successor, call)?.fresh_continuation.is_some() {
            // A fresh fallback resumes the task in a new session. It still
            // requires the original route and an observed conflict-free child.
            if route_is_missing || evidence_incomplete || session.is_none() {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionProofIncomplete),
                )
            } else if parent_route_digest != child_route_digest
                || route_digest != child_route_digest.as_deref()
            {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionMismatch),
                )
            } else {
                ("resumed", None)
            }
        } else {
            (
                "not_continued",
                Some(provenant_records::run::SteeringCause::SessionProofIncomplete),
            )
        };
        Ok(Some(SteerContinuationDisposition {
            parent: parent_id,
            successor,
            state,
            cause,
        }))
    }

    fn persist_live_steer_successor_disposition(
        &self,
        disposition: &SteerContinuationDisposition,
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| record_steer_live_disposition(tx, disposition))
    }

    pub(crate) fn confirm_steer_terminal_process(
        &self,
        id: AttemptId,
        scope: Scope,
        provider: Option<&provenant_supervisor::census::ProcessIdentity>,
        call: &Request<'_>,
    ) -> Result<()> {
        let Some(ready) = self.steer_session_ready(id, call)? else {
            return Ok(());
        };
        let matches = ready.generation == scope.generation
            && provider.is_some_and(|provider| {
                ready.provider_boot == provider.boot
                    && ready.provider_pid == provider.pid
                    && ready.provider_start == provider.native_start
            });
        self.write(call, |tx| {
            tx.execute(
                "UPDATE provider_session_ready SET identity_conflict=CASE WHEN ?1=1 THEN identity_conflict ELSE 1 END, terminal_confirmed=1 WHERE attempt=?2",
                params![if matches { 1_i64 } else { 0_i64 }, id.to_string()],
            )?;
            Ok(())
        })
    }

    /// Request a `Steered` stop only when the running attempt holds a durable,
    /// validated session for its exact route. Interrupting without one would
    /// cancel the attempt and strand the accepted text, so this is the gate for
    /// the `--now` restart path; queued delivery never calls it.
    fn request_steer_stop_if_ready(&self, id: AttemptId, call: &Request<'_>) -> Result<bool> {
        if !self.steer_session_available(id, call)? {
            return Ok(false);
        }
        let record = self.record(id, call)?;
        let directory = self.attempt_dir(id, false)?;
        let stopped = match custody::reattach(directory.try_clone()?, Self::scope(&record)) {
            Ok(handle) => custody::stop(&handle, StopReason::Steered),
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => custody::request_stop(
                directory,
                Self::scope(&record),
                record.verified_owner.unwrap_or(self.context.actor_id),
                StopReason::Steered,
            ),
            Err(error) => Err(error),
        };
        match stopped {
            Ok(StopReceipt::Requested | StopReceipt::AlreadyRequested)
            | Err(CustodyError::StopConflict) => Ok(true),
            Err(error) => Err(RunError::Io(std::io::Error::other(error.to_string()))),
        }
    }

    /// Whether this running attempt holds a validated, live provider session
    /// bound to its exact route (provider, model, effort, account and
    /// host-native lane). For queued delivery this is evidence for how
    /// settlement delivers the turn, not a gate on acceptance; for `--now` it
    /// is the condition for requesting the interrupt.
    pub fn steer_session_available(&self, id: AttemptId, call: &Request<'_>) -> Result<bool> {
        let record = self.record(id, call)?;
        if !matches!(record.state, AttemptState::Running { .. }) {
            return Ok(false);
        }
        let Some(route) = record.applied_route.as_ref() else {
            return Ok(false);
        };
        let Some(ready) = self.steer_session_ready(id, call)? else {
            return Ok(false);
        };
        if ready.conflict || ready.session.is_none() {
            return Ok(false);
        }
        let route_digest = steer_route_digest(route)?;
        if ready.generation != Self::scope(&record).generation || ready.route_digest != route_digest
        {
            return Ok(false);
        }
        let source = provenant_os::capture::CaptureSourceIdentity {
            attempt: id.to_string(),
            generation: ready.generation,
            boot: ready.provider_boot,
            pid: ready.provider_pid,
            native_start: ready.provider_start,
        };
        Ok(source_is_live(&source))
    }

    /// Read and verify the complete private steering ledger. The caller may
    /// consume text for a continuation or project only its safe identifiers.
    fn steer_ledger(&self, id: AttemptId, call: &Request<'_>) -> Result<Vec<(SteerIntent, bool)>> {
        call.check()?;
        let mut query = self.db.prepare(
            "SELECT operation,digest,record,resolved FROM steer_intents WHERE attempt=?1 ORDER BY sequence",
        )?;
        let rows = query.query_map([id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut intents = Vec::new();
        for row in rows {
            let (operation, digest, raw, resolved) = row?;
            let intent: SteerIntent = crate::db::decode_column(&raw)?;
            let expected = provenant_records::CanonicalV1::record_digest(
                "steer_intent",
                1,
                &(id, &intent.operation_id, &intent.text),
            )?;
            let message_digest = provenant_records::CanonicalV1::record_digest(
                "steer_message",
                1,
                &(id, &intent.operation_id),
            )?;
            let mut message_bytes = [0; 16];
            message_bytes.copy_from_slice(&message_digest.as_bytes()[..16]);
            if intent.schema_version != 1
                || intent.attempt != id
                || operation != intent.operation_id.as_str()
                || digest != expected.to_string()
                || intent.message_id != MessageId::from_bytes(message_bytes)
                || !matches!(resolved, 0 | 1)
            {
                return Err(RunError::Validation("steering intent identity"));
            }
            intents.push((intent, resolved == 1));
        }
        call.check()?;
        Ok(intents)
    }

    /// Read the accepted steering input for a run. The row digest and both
    /// identity keys are checked before returning provider-authored text.
    pub fn steer_intent(&self, id: AttemptId, call: &Request<'_>) -> Result<Vec<SteerIntent>> {
        Ok(self
            .steer_ledger(id, call)?
            .into_iter()
            .filter_map(|(intent, resolved)| (!resolved).then_some(intent))
            .collect())
    }

    /// Project the ledger without exposing its steering text. A launcher
    /// receipt records a queued successor; only validated session evidence or
    /// a terminal disposition resolves it.
    pub(crate) fn steering_status(
        &self,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<provenant_records::run::SteeringStatus>> {
        let ledger = self.steer_ledger(id, call)?;
        if ledger.is_empty() {
            return Ok(None);
        }
        let pending = ledger.iter().any(|(_, resolved)| !resolved);
        let pending_successor = if pending {
            let operation = format!("graph-steer-{id}");
            self.db
                .query_row(
                    "SELECT id FROM attempts WHERE continues=?1 AND operation=?2 AND refused=0",
                    params![id.to_string(), operation],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|successor| {
                    AttemptId::parse(&successor)
                        .map_err(|_| RunError::Validation("steer successor ID"))
                })
                .transpose()?
        } else {
            None
        };
        let outcome: Option<SteerContinuationOutcome> = self
            .db
            .query_row(
                "SELECT state,cause,successor,live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
                [id.to_string()],
                |row| {
                    Ok(SteerContinuationOutcome {
                        state: row.get(0)?,
                        cause: row.get(1)?,
                        successor: row.get(2)?,
                        live_ack: row.get::<_, i64>(3)? != 0,
                        terminal_final: row.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()?;
        let (state, cause, successor) = match (pending, outcome) {
            (true, None) => (
                provenant_records::run::SteeringState::Queued,
                None,
                pending_successor,
            ),
            (true, Some(outcome)) if outcome.state == "admitted" => {
                let Some(successor) = outcome.successor else {
                    return Err(RunError::Validation("admitted steer successor"));
                };
                if outcome.cause.is_some() || outcome.live_ack || outcome.terminal_final {
                    return Err(RunError::Validation("admitted steer cause"));
                }
                (
                    provenant_records::run::SteeringState::Queued,
                    None,
                    Some(
                        AttemptId::parse(&successor)
                            .map_err(|_| RunError::Validation("steer successor ID"))?,
                    ),
                )
            }
            (true, Some(_)) => return Err(RunError::Validation("pending steer outcome")),
            (false, Some(outcome)) if outcome.state == "resumed" => {
                let Some(successor) = outcome.successor else {
                    return Err(RunError::Validation("resumed steer successor"));
                };
                if outcome.cause.is_some() || (!outcome.live_ack && !outcome.terminal_final) {
                    return Err(RunError::Validation("resumed steer cause"));
                }
                (
                    provenant_records::run::SteeringState::Resumed,
                    None,
                    Some(
                        AttemptId::parse(&successor)
                            .map_err(|_| RunError::Validation("steer successor ID"))?,
                    ),
                )
            }
            (false, Some(outcome)) if outcome.state == "not_continued" => {
                let cause = outcome
                    .cause
                    .as_deref()
                    .and_then(provenant_records::run::SteeringCause::from_code)
                    .ok_or(RunError::Validation("steer refusal cause"))?;
                (
                    provenant_records::run::SteeringState::NotContinued,
                    Some(cause),
                    outcome
                        .successor
                        .map(|successor| {
                            AttemptId::parse(&successor)
                                .map_err(|_| RunError::Validation("steer successor ID"))
                        })
                        .transpose()?,
                )
            }
            (false, Some(_)) => return Err(RunError::Validation("steer outcome state")),
            (false, None) => return Err(RunError::Validation("steer outcome missing")),
        };
        let fresh_session = successor
            .map(|successor| {
                self.request(successor, call)
                    .map(|request| request.fresh_continuation.is_some())
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Some(provenant_records::run::SteeringStatus {
            fresh_session,
            state,
            operations: ledger
                .into_iter()
                .map(|(intent, _)| provenant_records::run::SteeringOperation {
                    operation_id: intent.operation_id,
                    message_id: intent.message_id,
                })
                .collect(),
            successor,
            cause,
        }))
    }

    /// Record launcher admission separately from a validated session
    /// acknowledgement. Admission alone leaves the intent queued.
    fn record_steer_admitted(
        &self,
        id: AttemptId,
        successor: AttemptId,
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| {
            let operation = format!("graph-steer-{id}");
            let admitted: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND continues=?2 AND operation=?3 AND refused=0)",
                params![successor.to_string(), id.to_string(), operation],
                |row| row.get(0),
            )?;
            if !admitted {
                return Err(RunError::Validation("steer successor admission"));
            }
            record_steer_admission(tx, id, successor)
        })
    }

    /// Apply exactly one launch receipt for the prepared continuation. A
    /// missing, skipped, blocked or uncertain receipt leaves the intent queued.
    pub fn record_steer_launch_result(
        &self,
        id: AttemptId,
        successor: AttemptId,
        started: &[Started],
        call: &Request<'_>,
    ) -> Result<()> {
        let mut matching = started
            .iter()
            .filter(|receipt| started_attempt(receipt) == successor);
        let Some(receipt) = matching.next() else {
            return Ok(());
        };
        if matching.next().is_some() {
            return Ok(());
        }
        match receipt {
            Started::Launched(_) => {
                self.record_steer_admitted(id, successor, call)?;
                self.persist_live_steer_successor_evidence(successor, call)
            }
            Started::Refused { .. } => self.record_steer_refused(id, successor, call),
            Started::Skipped { .. } | Started::Unknown { .. } | Started::Blocked { .. } => Ok(()),
        }
    }

    /// Retain the private accepted text while recording a safe final cause.
    /// The outcome and resolution commit together for replay safety.
    pub fn record_steer_not_continued(
        &self,
        id: AttemptId,
        cause: provenant_records::run::SteeringCause,
        call: &Request<'_>,
    ) -> Result<()> {
        self.write(call, |tx| {
            let raw: Vec<u8> = tx
                .query_row(
                    "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                    params![id.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(RunError::NotFound)?;
            let parent = crate::db::decode_attempt(&raw)?;
            if !matches!(parent.state, AttemptState::Terminal { .. }) {
                return Err(RunError::Nonterminal);
            }
            let operation = format!("graph-steer-{id}");
            let successor: Option<String> = tx
                .query_row(
                    "SELECT id FROM attempts WHERE continues=?1 AND operation=?2 AND refused=0",
                    params![id.to_string(), operation],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(successor) = successor.as_deref() {
                let raw: Vec<u8> = tx.query_row(
                    "SELECT record FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
                    params![successor, self.context.project_id.to_string()],
                    |row| row.get(0),
                )?;
                let successor = crate::db::decode_attempt(&raw)?;
                if !matches!(successor.state, AttemptState::Terminal { .. })
                    || successor.provider_launched != Some(false)
                {
                    return Err(RunError::Conflict);
                }
            }
            let successor = successor
                .map(|successor| {
                    AttemptId::parse(&successor)
                        .map_err(|_| RunError::Validation("steer successor ID"))
                })
                .transpose()?;
            record_steer_outcome(tx, id, "not_continued", Some(cause), successor)
        })
    }

    fn record_steer_refused(
        &self,
        id: AttemptId,
        successor: AttemptId,
        call: &Request<'_>,
    ) -> Result<()> {
        call.check()?;
        let operation = format!("graph-steer-{id}");
        let belongs: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND continues=?2 AND operation=?3 AND refused=0)",
            params![successor.to_string(), id.to_string(), operation],
            |row| row.get(0),
        )?;
        if !belongs {
            return Err(RunError::Validation("steer refusal receipt"));
        }
        let record = self.record(successor, call)?;
        if !matches!(record.state, AttemptState::Terminal { .. })
            || record.provider_launched != Some(false)
        {
            return Err(RunError::Validation("steer refusal outcome"));
        }
        self.record_steer_not_continued(
            id,
            provenant_records::run::SteeringCause::ContinuationRefused,
            call,
        )
    }

    /// A previous boot ended every member of this attempt's process tree.
    /// Keep both exclusions until the canonical commit precedes fence release.
    fn settle_rebooted(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        if matches!(record.state, AttemptState::Queued { .. }) {
            return Ok(None);
        }
        if matches!(record.state, AttemptState::Terminal { .. })
            && matches!(self.attempt_dir(record.attempt_id, false), Err(RunError::Filesystem(error)) if error.is_not_found())
        {
            // Terminal cleanup handles the missing directory and any writer fence.
            return Ok(None);
        }
        if Self::needs_lost_settlement(record)
            && let Some(result) = self.settle_unreserved_loss(record, call, observer, true)?
        {
            return Ok(Some(result));
        }
        // Collection failures are uncertain; settlement errors still propagate.
        let collect = || -> Result<_> {
            let attempt = self.attempt_dir(record.attempt_id, false)?;
            if !matches!(record.state, AttemptState::Terminal { .. })
                && read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
            {
                // A durable exit retains its canonical completion path and facts.
                return Ok(None);
            }
            // Recovery needs the persisted binding, not a fresh Git inspection of
            // the worktree the interrupted writer left behind.
            let request = self.request(record.attempt_id, call)?;
            let writer = request.task.writer.as_ref();
            let (directory, expected_slot) = match writer {
                Some(writer) => {
                    let expected = self.writer_slot(writer)?;
                    let readable = self
                        .writer_fences()?
                        .into_iter()
                        .find(|fence| fence.scope == Self::scope(record));
                    let slot = match readable {
                        Some(fence) => fence
                            .custody_directory()
                            .ok_or_else(|| {
                                RunError::unsafe_path(
                                    fence.reservation.clone(),
                                    PathCheck::Containment,
                                )
                            })?
                            .to_path_buf(),
                        None => expected.clone(),
                    };
                    match provenant_os::fs::Directory::open_anchor(&slot) {
                        Ok(directory) => (directory, slot == expected),
                        Err(error) if error.is_not_found() => return Ok(None),
                        Err(error) => return Err(error.into()),
                    }
                }
                None => (attempt.try_clone()?, true),
            };
            let recovery = match custody::acquire_recovery(
                directory.try_clone()?,
                Self::scope(record),
                provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                call.cancel,
                call.clock,
            ) {
                Ok(recovery) => recovery,
                Err(CustodyError::Filesystem(error))
                    if error.is_not_found()
                        || matches!(error, provenant_os::fs::FsError::LockTimeout { .. }) =>
                {
                    return Ok(None);
                }
                Err(error) if error.is_invalid_record() && expected_slot => {
                    #[cfg(test)]
                    let recovered = provenant_supervisor::fixture::acquire_reboot_recovery_with(
                        directory,
                        &attempt,
                        Self::scope(record),
                        provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                        call.cancel,
                        call.clock,
                        observer,
                    );
                    #[cfg(not(test))]
                    let recovered = custody::acquire_reboot_recovery(
                        directory,
                        &attempt,
                        Self::scope(record),
                        provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                        call.cancel,
                        call.clock,
                    );
                    recovered.map_err(|error| RunError::Io(std::io::Error::other(format!(
                        "reservation cannot be verified after reboot: {error}; retain custody and inspect the reservation and boot identity before chair recovery"
                    ))))?
                }
                Err(error) => return Err(RunError::Io(std::io::Error::other(error.to_string()))),
            };
            #[cfg(test)]
            let proof = provenant_supervisor::fixture::after_reboot_with(
                recovery,
                &attempt,
                Self::scope(record),
                observer,
            );
            #[cfg(not(test))]
            let proof = recovery.after_reboot(&attempt, Self::scope(record));
            let proof =
                proof.map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
            Ok(proof.map(|proof| (attempt, proof)))
        };
        let (attempt, proof) = match collect() {
            Ok(Some(collected)) => collected,
            Ok(None) => return Ok(None),
            Err(error) => {
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::RebootProofUnavailable {
                        cause: RebootProofCause::from_error(&error),
                        error: format!("reboot proof unavailable: {error}"),
                    },
                )));
            }
        };
        if !Self::needs_lost_settlement(record) {
            let mut recovery = proof.into_recovery();
            recovery
                .prepare_settlement()
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
            self.settle_failed_import(record, &attempt, &recovery, call)?;
            self.settle_recovered(&attempt, recovery)?;
            // Recheck custody after release before removing lock-named payloads.
            let cleanup = self.reconcile_terminal_temporary(record, call, observer);
            #[cfg(test)]
            REBOOT_TEMPORARY_CLEANUP.with(|result| {
                *result.borrow_mut() = Some(cleanup.as_ref().cloned().map_err(ToString::to_string));
            });
            let _ = cleanup;
        } else {
            // Lost without exit evidence deliberately preserves scratch.
            let request = self.request(record.attempt_id, call)?;
            let missing_worktree = request
                .task
                .writer
                .as_ref()
                .is_some_and(crate::writer::worktree_removed);
            if missing_worktree {
                self.commit_lost(proof.scope(), LostReason::RemovedWorktree.message(), call)?;
            } else {
                self.commit_reboot_lost(&proof, call)?;
            }
            proof
                .release()
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
        }
        if matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Lost,
                exit_code: None,
                ..
            }
        ) && record.exit_evidence.is_none()
        {
            self.set_temporary_retained(record.attempt_id, false, call)?;
        }
        Ok(Some(Reconciled::Finished))
    }
    fn needs_lost_settlement(record: &AttemptRecord) -> bool {
        !matches!(record.state, AttemptState::Terminal { .. })
            || (matches!(
                record.state,
                AttemptState::Terminal {
                    outcome: Outcome::Lost,
                    exit_code: None,
                    ..
                }
            ) && record.exit_evidence.is_none())
    }

    /// Writer exclusion for an attempt within `budget`. A live attempt probes
    /// once: a held lock means its guardian (or an inheritor of its lease)
    /// still holds custody. Settlement of an exited attempt waits a bounded
    /// time; contention past it is `WriterExclusionBusy`, naming the holder.
    fn orphan_recovery(
        &self,
        record: &AttemptRecord,
        attempt: provenant_os::fs::Directory,
        budget: provenant_core::WaitBudget,
        call: &Request<'_>,
    ) -> Result<custody::Recovery> {
        let request = self.request(record.attempt_id, call)?;
        let task = request.task;
        let failed_import = record.settlement_receipts.as_ref().is_some_and(|receipts| {
            matches!(receipts.git_import, EffectDisposition::Failed { .. })
        });
        let retained = if failed_import {
            self.writer_fences()?
                .into_iter()
                .find(|fence| fence.scope == Self::scope(record))
        } else {
            None
        };
        let directory = match retained
            .as_ref()
            .and_then(|fence| fence.custody_directory())
        {
            // Finalising a failed import needs its recorded custody binding,
            // not a fresh Git inspection of a possibly damaged worktree.
            Some(path) => provenant_os::fs::Directory::open_anchor(path)?,
            None => match &task.writer {
                Some(writer) => self.writer_custody_directory(record, writer)?,
                None => attempt,
            },
        };
        let slot = directory.try_clone()?;
        custody::acquire_recovery(
            directory,
            Self::scope(record),
            budget,
            call.cancel,
            call.clock,
        )
        .map_err(|error| writer_exclusion_error(self, &slot, Self::scope(record), error, call))
    }
    /// A durable provider exit survives guardian loss; only cleanup is recovered.
    fn settle_proven_guardian_exit(
        &self,
        record: &AttemptRecord,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        use provenant_confinement::worktrees::WorkerObserver;
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_none() {
            return Ok(None);
        }
        let scope = Self::scope(record);
        let request = self.request(record.attempt_id, call)?;
        let directory = match request.task.writer.as_ref() {
            Some(writer) => {
                match provenant_os::fs::Directory::open_anchor(&self.writer_slot(writer)?) {
                    Ok(directory) => directory,
                    Err(error) if error.is_not_found() => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
            }
            None => attempt.try_clone()?,
        };
        let Ok(recovery) = custody::acquire_recovery(
            directory,
            scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        ) else {
            return Ok(None);
        };
        let workers = provenant_confinement::worktrees::SystemWorkerObserver;
        let proof = recovery
            .after_guardian_loss(&attempt, observer, || {
                let Some(writer) = request.task.writer.as_ref() else {
                    return Ok(true);
                };
                workers.invalidate();
                Ok(provenant_confinement::worktrees::observe_writer_tree(
                    &workers,
                    &writer.worktree,
                )
                .0
                .is_available())
            })
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        let Ok(proof) = proof else {
            return Ok(None);
        };
        let handle = custody::reattach(attempt.try_clone()?, scope)
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        let Observation::Exited(exit) =
            custody::observe(&handle, call.lock_budget(), call.cancel, call.clock)
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?
        else {
            return Ok(None);
        };
        crate::artifacts::seal(
            &attempt,
            "guardian-loss.json",
            &crate::db::bytes(&serde_json::json!({
                "schema_version": 1, "scope": scope,
                "warning": "guardian lost on this boot; original free locks, writer lease and empty worktree census prove cleanup; provider outcome retained"
            }))?,
            RECORD_LIMIT,
        )?;
        let recovery = proof
            .into_recovery()
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        if self.finish_recovered_terminal(
            record.attempt_id,
            &attempt,
            &exit,
            recovery,
            providers,
            call,
        )? {
            Ok(Some(Reconciled::Finished))
        } else {
            Ok(Some(crate::settlement::settling(
                &attempt, scope, call.clock,
            )?))
        }
    }
    /// Settle a missing exit only under native death proof and writer exclusion.
    /// Writer exclusion is taken without waiting, so an inheritor of the
    /// lease still keeps the attempt open.
    fn settle_holderless(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        self.settle_holderless_with_workers(
            record,
            call,
            observer,
            &provenant_confinement::worktrees::SystemWorkerObserver,
        )
    }
    fn settle_holderless_with_workers(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
        workers: &dyn provenant_confinement::worktrees::WorkerObserver,
    ) -> Result<Option<Reconciled>> {
        let scope = Self::scope(record);
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
            return Ok(None);
        }
        // Name the slot from the recorded binding; a lost writer's repository
        // need not be readable for its custody to be settled.
        let request = self.request(record.attempt_id, call)?;
        let writer = request.task.writer.as_ref();
        let directory = match writer {
            Some(writer) => {
                let slot = self.writer_slot(writer)?;
                match provenant_os::fs::Directory::open_anchor(&slot) {
                    Ok(directory) => directory,
                    Err(error) if error.is_not_found() => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
            }
            None => attempt,
        };
        #[cfg(test)]
        HOLDERLESS_BEFORE_EXCLUSION.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let Ok(mut recovery) = custody::acquire_recovery(
            directory,
            scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        ) else {
            return Ok(None);
        };
        // A reader can publish its exit and release exclusion after the first
        // check, so both proofs must still hold under exclusion.
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        if recovery.scope() != scope
            || read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
        {
            return Ok(None);
        }
        // A lane killed before its guardian could publish an exit can leave
        // its recorded tree alive and holding the heavy lock. End only what
        // the attempt recorded; a live guardian keeps the fence and falls
        // through to the normal proof path.
        match recovery.end_recorded_orphans(observer, ORPHAN_PATIENCE) {
            Ok(report) if !report.survivors.is_empty() => {
                return Ok(Some(Reconciled::Escaped {
                    pids: report
                        .survivors
                        .iter()
                        .map(|identity| identity.pid)
                        .collect(),
                }));
            }
            Ok(_) | Err(CustodyError::Fenced) => {}
            Err(error) => return Ok(Some(Reconciled::custody_unavailable(&error))),
        }
        if custody::holder_gone(&attempt, scope, observer)
            && recovery.observe_with(observer) == RecoveryState::Absent
        {
            self.commit_lost(scope, LostReason::Holderless.message(), call)?;
            recovery
                .release_lost()
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        } else {
            let proof = recovery
                .after_guardian_loss(&attempt, observer, || {
                    let Some(writer) = writer else {
                        return Ok(true);
                    };
                    workers.invalidate();
                    Ok(provenant_confinement::worktrees::observe_writer_tree(
                        workers,
                        &writer.worktree,
                    )
                    .0
                    .is_available())
                })
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
            let Ok(proof) = proof else {
                return Ok(None);
            };
            self.commit_lost(scope,
                "warning: guardian lost on this boot; original guardian and custody locks, writer lease and empty worktree census prove the attempt gone; committed work retained",
                call)?;
            proof
                .release()
                .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        }
        self.set_temporary_retained(record.attempt_id, false, call)?;
        Ok(Some(Reconciled::Finished))
    }
    /// Settle an unreserved attempt only with proof its process tree is gone.
    fn settle_unreserved_loss(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
        proof_errors_uncertain: bool,
    ) -> Result<Option<Reconciled>> {
        let collect = || -> Result<_> {
            let attempt = self.attempt_dir(record.attempt_id, false)?;
            // The slot is named from the worktree path, so it needs no Git access.
            let request = self.request(record.attempt_id, call)?;
            let writer = request.task.writer.as_ref();
            let slot = writer.map(|writer| self.writer_slot(writer)).transpose()?;
            let custody = match &slot {
                Some(slot) => match provenant_os::fs::Directory::open_anchor(slot) {
                    Ok(directory) => directory,
                    Err(error) if error.is_not_found() => return Ok(None),
                    Err(error) => return Err(error.into()),
                },
                None => attempt.try_clone()?,
            };
            let observation = custody::writer_admission_guard(&custody)
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
            let _lease = match custody.try_lock(
                std::ffi::OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            ) {
                Ok(lease) => lease,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            drop(observation);
            let stale = match custody::reservation_present(&custody, Self::scope(record)) {
                Ok(false) => false,
                Ok(true) => {
                    let Some(slot) = &slot else {
                        return Ok(None);
                    };
                    let path = slot
                        .join(record.attempt_id.to_string())
                        .join("reservation.json");
                    let scope = Self::scope(record);
                    if !self.writer_fences()?.iter().any(|fence| {
                        fence.reservation == path
                            && fence.scope.attempt == scope.attempt
                            && fence.scope.attempt == scope.attempt
                            && fence.scope.generation < scope.generation
                    }) {
                        return Ok(None);
                    }
                    true
                }
                Err(error) => return Err(error.into()),
            };
            if !stale && matches!(record.state, AttemptState::Terminal { .. }) {
                return Ok(None);
            }
            if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
                return Ok(None);
            }
            if !custody::checkpoint_before_reboot(&attempt, Self::scope(record), observer) {
                return Ok(None);
            }
            let missing_worktree = writer.is_some_and(crate::writer::worktree_removed);
            Ok(Some((custody, _lease, stale, missing_worktree)))
        };
        let (custody, _lease, stale, missing_worktree) = match collect() {
            Ok(Some(proof)) => proof,
            Ok(None) => return Ok(None),
            Err(error) if proof_errors_uncertain => {
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::RebootProofUnavailable {
                        cause: RebootProofCause::from_error(&error),
                        error: format!("reboot proof unavailable: {error}"),
                    },
                )));
            }
            Err(error) => return Err(error),
        };
        self.commit_lost(
            Self::scope(record),
            if missing_worktree {
                LostReason::RemovedWorktree.message()
            } else {
                LostReason::StaleReservation.message()
            },
            call,
        )?;
        if stale {
            let evidence = custody::attempt_evidence(&custody, Self::scope(record))?;
            evidence.unlink(std::ffi::OsStr::new("reservation.json"))?;
            evidence.sync()?;
        }
        self.set_temporary_retained(record.attempt_id, false, call)?;
        Ok(Some(Reconciled::Finished))
    }
    pub(crate) fn scope(record: &AttemptRecord) -> Scope {
        Scope {
            attempt: record.attempt_id,
            generation: record.generation,
        }
    }
    /// Settle a Running attempt whose sealed provider exit makes status report
    /// settling. The provider is gone, so a stop request cannot reach it; when
    /// no holder can act on the stop and no driver owns settlement, the stop
    /// commits the attempt terminal from the provider's own captured exit. A
    /// live holder keeps the ordinary stop path and settles the attempt itself.
    fn cancel_vanished(
        &self,
        record: &AttemptRecord,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<Option<CancelReceipt>> {
        let id = record.attempt_id;
        if !matches!(&record.state, AttemptState::Running { .. }) {
            return Ok(None);
        }
        let scope = Self::scope(record);
        let attempt = self.attempt_dir(id, false)?;
        let Ok(Some(raw)) = read_sealed(&attempt, "process-exit.json", RECORD_LIMIT) else {
            return Ok(None);
        };
        // Only the exit sealed for this generation proves this provider is gone.
        let Ok(exit) = crate::db::decode::<provenant_supervisor::custody::StoredExit>(&raw) else {
            return Ok(None);
        };
        if exit.schema_version != 1
            || exit.scope != scope
            || (exit.termination.is_none() && !exit.never_spawned)
        {
            return Ok(None);
        }
        use provenant_supervisor::custody::HolderPresence;
        if custody::observe_holder(&attempt, scope, &SystemProcessObserver) == HolderPresence::Live
            || crate::settlement::settlement_owner_live(&attempt, scope, call.clock) == Some(true)
        {
            return Ok(None);
        }
        // Hold the same lease as settlement while publishing the decision.
        // A pending terminal intent already belongs to recovery.
        let Ok(_driver) = attempt.try_lock(
            std::ffi::OsStr::new("settlement-driver.lock"),
            provenant_os::fs::LockMode::Exclusive,
        ) else {
            return Ok(None);
        };
        if read_sealed(&attempt, "terminal-pending.json", TERMINAL_RECORD_LIMIT)?.is_some() {
            return Ok(None);
        }
        // The owner request is durable evidence, not proof of delivery.
        if custody::request_stop(
            attempt,
            scope,
            record.verified_owner.unwrap_or(self.context.actor_id),
            StopReason::ExplicitCancellation,
        )
        .is_err()
        {
            return Ok(None);
        }
        self.commit_stopped_after_exit(scope, reason, &exit, call)?;
        Ok(Some(CancelReceipt::Cancelled))
    }
    /// Cancel one attempt. Queued work is withdrawn; started work gets a stop
    /// request. A handoff that has not yet published its launch summary is
    /// retried within the call budget.
    // Cancellation coordinates with a real guardian handoff under a hard cap.
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        self.cancel_inner(id, reason, call, false)
            .map(|(_, receipt)| receipt)
    }
    /// Cancel the current host-owned continuation of a graph target. The
    /// returned attempt is the exact validated leaf whose stop was accepted.
    pub fn cancel_graph(
        &self,
        id: AttemptId,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<(AttemptId, CancelReceipt)> {
        self.cancel_inner(id, reason, call, true)
    }
    fn cancel_inner(
        &self,
        mut id: AttemptId,
        reason: &str,
        call: &Request<'_>,
        follow_graph_successors: bool,
    ) -> Result<(AttemptId, CancelReceipt)> {
        let mut pending_steer_parent = None;
        loop {
            let record = self.record(id, call)?;
            if let AttemptState::Queued { .. } = record.state {
                let now = call
                    .clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?;
                match self.withdraw(
                    id,
                    AttemptState::Queued {
                        extra: Default::default(),
                    },
                    Outcome::Cancelled,
                    reason,
                    now,
                    call,
                ) {
                    Ok(Some(_)) => {
                        return self.finish_graph_cancel(
                            id,
                            pending_steer_parent,
                            CancelReceipt::Cancelled,
                            call,
                        );
                    }
                    // The expected queued state lost. Re-read under the
                    // publication fence so a terminal steer cannot accept
                    // this cancel without preserving its continuation veto.
                    Ok(None) => continue,
                    // A concurrent start took it; it is no longer queued.
                    Err(RunError::Conflict) => continue,
                    Err(error) => return Err(error),
                }
            }
            let dir = self.attempt_dir(id, false)?;
            // Terminal publication, continuation admission, and every started
            // cancellation use this same fence. Re-read after acquiring it:
            // a stale Running snapshot must not publish a stop behind an
            // already admitted continuation.
            let publication = dir.lock(
                std::ffi::OsStr::new("publication.lock"),
                provenant_os::fs::LockMode::Exclusive,
                call.lock_budget(),
                call.cancel,
                call.clock,
            )?;
            let current = self.record(id, call)?;
            if current.state != record.state
                || matches!(&current.state, AttemptState::Queued { .. })
            {
                drop(publication);
                continue;
            }
            if follow_graph_successors && let Some(successor) = current.superseded_by {
                pending_steer_parent = self
                    .validate_graph_cancel_successor(&current, successor, call)?
                    .then_some(id);
                drop(publication);
                id = successor;
                continue;
            }
            if follow_graph_successors
                && let Some(receipt) = self.cancel_vanished(&current, reason, call)?
            {
                drop(publication);
                let committed = self.record(id, call)?;
                self.settle_cleanup(&committed, None, call);
                return self.finish_graph_cancel(id, pending_steer_parent, receipt, call);
            }
            if matches!(&current.state, AttemptState::Terminal { .. }) {
                // This is an owner intent only. It remains distinct from the
                // guardian's stop reason and signal-delivery evidence; the
                // locked continuation gate will observe it and refuse a child.
                let steer_queued = current.superseded_by.is_none()
                    && current.steering.as_ref().is_some_and(|steering| {
                        steering.state == provenant_records::run::SteeringState::Queued
                    });
                if steer_queued {
                    if let Err(error) = custody::request_stop(
                        dir.try_clone()?,
                        Self::scope(&current),
                        current.verified_owner.unwrap_or(self.context.actor_id),
                        StopReason::ExplicitCancellation,
                    ) {
                        drop(publication);
                        return Ok((
                            id,
                            CancelReceipt::Unobservable {
                                reason: error.to_string(),
                            },
                        ));
                    }
                    // Resolve the steering now, under the fence that orders
                    // continuation admission: a continuation still waiting
                    // for the steered writer's custody release must not leave
                    // the stopped node queued behind it.
                    self.record_steer_not_continued(
                        id,
                        provenant_records::run::SteeringCause::TerminalStop,
                        call,
                    )?;
                }
                self.record_cancellation(
                    id,
                    current.generation,
                    CancellationDisposition::AcknowledgedIneffective,
                    call,
                )?;
                drop(publication);
                return self.finish_graph_cancel(
                    id,
                    pending_steer_parent,
                    CancelReceipt::AlreadyTerminal,
                    call,
                );
            }
            let scope = Self::scope(&current);
            let starting = matches!(&current.state, AttemptState::Starting { .. });
            let mut starting_request = None;
            let unlaunched = if starting {
                matches!(
                    custody::reattach(dir.try_clone()?, scope),
                    Err(CustodyError::Filesystem(ref error)) if error.is_not_found()
                )
            } else {
                false
            };
            if starting {
                // Serialize the Starting handoff request with the launch
                // owner's admission-skip transition.
                starting_request = Some(custody::request_stop(
                    dir.try_clone()?,
                    scope,
                    current.verified_owner.unwrap_or(self.context.actor_id),
                    StopReason::ExplicitCancellation,
                ));
            }
            if let Some(Err(error)) = starting_request.as_ref() {
                drop(publication);
                return Ok((
                    id,
                    CancelReceipt::Unobservable {
                        reason: error.to_string(),
                    },
                ));
            }
            let stopped = match custody::reattach(dir.try_clone()?, scope) {
                Ok(handle) => custody::stop(&handle, StopReason::ExplicitCancellation),
                Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                    if let Some(request) = starting_request {
                        request
                    } else {
                        custody::request_stop(
                            dir,
                            scope,
                            current.verified_owner.unwrap_or(self.context.actor_id),
                            StopReason::ExplicitCancellation,
                        )
                    }
                }
                Err(error) => Err(error),
            };
            drop(publication);
            if unlaunched && stopped.is_ok() {
                // No guardian exists to act on the request. Settle it here so
                // the queue place and the writer are free before we return.
                if self.settle_cancelled_unlaunched(id, call)? {
                    return self.finish_graph_cancel(
                        id,
                        pending_steer_parent,
                        CancelReceipt::Cancelled,
                        call,
                    );
                }
            }
            let receipt = match stopped {
                Ok(StopReceipt::Requested) => CancelReceipt::Requested,
                Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
                Err(error) => CancelReceipt::Unobservable {
                    reason: error.to_string(),
                },
            };
            return self.finish_graph_cancel(id, pending_steer_parent, receipt, call);
        }
    }
    fn validate_graph_cancel_successor(
        &self,
        parent: &AttemptRecord,
        successor_id: AttemptId,
        call: &Request<'_>,
    ) -> Result<bool> {
        let successor = self.record(successor_id, call)?;
        let operation = self.attempt_operation(successor_id, call)?;
        let graph_steer = format!("graph-steer-{}", parent.attempt_id);
        let budget_wrap_up = format!("budget-wrap-up-{}", parent.attempt_id);
        let operation_id = operation.operation_id.as_str();
        if successor.project_id != parent.project_id
            || successor.retry_of != Some(parent.attempt_id)
            || successor.key.graph() != parent.key.graph()
            || successor.key.node() != parent.key.node()
            || successor.key.attempt() != parent.key.attempt().saturating_add(1)
            || operation.kind != provenant_records::run::OperationKind::Resume
            || operation.project_id != parent.project_id
            || (operation_id != graph_steer && operation_id != budget_wrap_up)
        {
            return Err(RunError::Conflict);
        }
        Ok(operation_id == graph_steer)
    }
    fn finish_graph_cancel(
        &self,
        cancelled: AttemptId,
        pending_steer_parent: Option<AttemptId>,
        receipt: CancelReceipt,
        call: &Request<'_>,
    ) -> Result<(AttemptId, CancelReceipt)> {
        if let Some(parent) = pending_steer_parent {
            let child = self.record(cancelled, call)?;
            let parent_record = self.record(parent, call)?;
            if matches!(&child.state, AttemptState::Terminal { .. })
                && child.provider_launched == Some(false)
                && parent_record.steering.as_ref().is_some_and(|steering| {
                    steering.state == provenant_records::run::SteeringState::Queued
                })
            {
                self.record_steer_not_continued(
                    parent,
                    provenant_records::run::SteeringCause::TerminalStop,
                    call,
                )?;
            }
        }
        Ok((cancelled, receipt))
    }
    /// Settle a cancelled attempt whose provider never launched. A launcher
    /// that is still alive watches for the request and unwinds its own
    /// preparation, so retry briefly before leaving the request standing.
    fn settle_cancelled_unlaunched(&self, id: AttemptId, call: &Request<'_>) -> Result<bool> {
        const ATTEMPTS: u32 = 40;
        for attempt in 0..ATTEMPTS {
            let record = self.record(id, call)?;
            if matches!(record.state, AttemptState::Terminal { .. }) {
                return Ok(true);
            }
            if !matches!(record.state, AttemptState::Starting { .. }) {
                return Ok(false);
            }
            // `settle_unlaunched` waits on a held lock; a live launcher is
            // asked to unwind instead, so cancel stays prompt.
            let dir = self.attempt_dir(id, false)?;
            if self.launcher_gone(&record, &dir, true, call)
                && matches!(
                    self.settle_unlaunched(id, &record, call)?,
                    Reconciled::Withdrawn | Reconciled::Finished
                )
            {
                return Ok(true);
            }
            if attempt + 1 < ATTEMPTS {
                provenant_core::Pause::wait(
                    &provenant_core::ThreadPause,
                    provenant_core::DurationMicros::from_micros(100_000),
                    call.cancel,
                );
            }
        }
        Ok(matches!(
            self.record(id, call)?.state,
            AttemptState::Terminal { .. }
        ))
    }

    /// Accept a continuation run. Each terminal attempt can be continued
    /// once; `expected_generation` is 0 before that and 1 after. The route,
    /// access and timeout come from the prior task, never from the caller.
    pub fn resume(&self, input: &Resume, call: &Request<'_>) -> Result<AttemptRecord> {
        self.resume_mode(input, call, ResumeOptions::default())
            .map(|(run, _)| run)
    }
    /// Same-session graph continuation whose work key advances exactly once.
    pub fn resume_graph(
        &self,
        input: &Resume,
        context: &provenant_records::run::WorkContext,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                graph_context: Some(context),
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }
    /// Fresh graph continuation when the caller already holds the predecessor
    /// publication fence. It reuses the exact graph work key without provider
    /// retention or a second lock acquisition.
    pub(crate) fn resume_graph_fresh_with_parent_publication_lock(
        &self,
        input: &Resume,
        context: &provenant_records::run::WorkContext,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                fresh: Some(reason),
                graph_context: Some(context),
                parent_publication_locked: true,
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }

    /// Same-session graph continuation when the caller already holds the
    /// predecessor's exclusive publication fence. That fence itself protects
    /// provider state from retention, so this path must not reacquire it.
    pub(crate) fn resume_graph_with_parent_publication_lock(
        &self,
        input: &Resume,
        context: &provenant_records::run::WorkContext,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                graph_context: Some(context),
                parent_publication_locked: true,
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }
    /// Retained-writer graph continuation whose predecessor head was re-pinned
    /// to a descendant produced by the same writer. `fresh` carries the reason
    /// when no provider session survives a steering turn.
    pub(crate) fn resume_repinned_with_parent_publication_lock(
        &self,
        input: &Resume,
        context: &provenant_records::run::WorkContext,
        head: provenant_core::GitOid,
        fresh: Option<&str>,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                fresh,
                graph_context: Some(context),
                parent_publication_locked: true,
                retained_head: Some(head),
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }
    /// Same lineage, route, authority and writer, without provider continuation.
    pub fn resume_fresh(
        &self,
        input: &Resume,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                fresh: Some(reason),
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }
    /// A fresh fallback may replace a vanished or reassigned writer only with
    /// a new worktree owned by this continuation.
    pub fn resume_fresh_in(
        &self,
        input: &Resume,
        reason: &str,
        writer: &WriterRequest,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        let policy = self.resume_policy(input.prior, call)?;
        let branch = self.fresh_writer_branch(input, writer, &policy)?;
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                fresh: Some(reason),
                writer: Some((writer, &branch)),
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }
    /// Prepare against the canonical continuation request before recording it.
    /// Replays retain their accepted IDs and use the ordinary launch path.
    pub fn resume_fresh_prepared(
        &self,
        input: &Resume,
        reason: &str,
        writer: Option<&WriterRequest>,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<(AttemptRecord, Option<PreparedStart>)> {
        let policy = self.resume_policy(input.prior, call)?;
        let branch = writer
            .map(|writer| self.fresh_writer_branch(input, writer, &policy))
            .transpose()?;
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                fresh: Some(reason),
                writer: writer.zip(branch.as_deref()),
                providers: Some(providers),
                ..Default::default()
            },
        )
    }
    /// Budget wrap-up is a same-session continuation with a single remaining
    /// allowance; its sealed profile prevents another wrap-up continuation.
    pub(crate) fn resume_budget_wrap_up(
        &self,
        input: &Resume,
        remaining: u64,
        call: &Request<'_>,
    ) -> Result<AttemptRecord> {
        self.resume_mode(
            input,
            call,
            ResumeOptions {
                budget_remaining: Some(remaining),
                ..Default::default()
            },
        )
        .map(|(run, _)| run)
    }
    pub(crate) fn resume_policy(
        &self,
        prior: AttemptId,
        call: &Request<'_>,
    ) -> Result<provenant_records::policy::Policy> {
        let request = self.request(prior, call)?;
        Ok(crate::launch_policy(
            self.writer_registry(),
            &self.context.project_id.to_string(),
            &request.task,
        ))
    }

    fn fresh_writer_branch(
        &self,
        input: &Resume,
        writer: &WriterRequest,
        _policy: &provenant_records::policy::Policy,
    ) -> Result<String> {
        let launch = crate::prepare_writer_with_facts(
            writer,
            &self.writer_registry,
            self.context.project_id,
            &self.repo_facts,
        )?;
        let owner = provenant_confinement::worktrees::load_owner(&launch.worktree)?.ok_or(
            RunError::Validation("fresh fallback needs an owned worktree"),
        )?;
        if owner.origin != provenant_confinement::worktrees::WorktreeOrigin::Lane
            || owner.operation_id.as_ref() != Some(&input.operation_id)
            || owner.attempt_id != Some(input.attempt_id)
        {
            return Err(RunError::Validation(
                "fresh fallback worktree has another owner",
            ));
        }
        Ok(owner.branch)
    }
    /// Admission checks ownership as well as the path. A later handoff checks
    /// again under custody so prune and reuse cannot redirect a queued resume.
    pub fn check_continuation_writer(
        &self,
        prior: AttemptId,
        call: &Request<'_>,
    ) -> Result<Option<provenant_confinement::worktrees::WorktreeOwner>> {
        let prior = self.record(prior, call)?;
        let request = self.request(prior.attempt_id, call)?;
        let task = request.task;
        let Some(writer) = &task.writer else {
            return Ok(None);
        };
        let launch = crate::prepare_writer_with_facts(
            writer,
            &self.writer_registry,
            prior.project_id,
            &self.repo_facts,
        )?;
        self.check_writer_lineage(&prior, &launch.worktree, call)
    }
    /// Whether a successor head is the writer's own descendant of its pinned
    /// head. Only a lane-owned writer may advance the pin this way; the git
    /// probe is bounded to the verified worktree and reads no untrusted input.
    fn repinned_head_descends(
        tree: &provenant_confinement::VerifiedWorktree,
        pinned: provenant_core::GitOid,
        head: provenant_core::GitOid,
    ) -> bool {
        let Some(git) = provenant_os::git::git_executable() else {
            return false;
        };
        let repo = tree.repo();
        crate::objects::command(
            &git,
            repo.common_git_dir(),
            repo.root_path(),
            &[
                "merge-base".into(),
                "--is-ancestor".into(),
                pinned.to_string().into(),
                head.to_string().into(),
            ],
            None,
        )
        .is_ok()
    }
    /// Called after writer custody is acquired. Graph retries are separate runs,
    /// so their sealed predecessor must be checked independently of `retry_of`.
    pub(crate) fn check_launch_writer_lineage(
        &self,
        record: &AttemptRecord,
        task: &TaskRequest,
        tree: &provenant_confinement::VerifiedWorktree,
        call: &Request<'_>,
    ) -> Result<()> {
        if record.retry_of.is_some()
            && task
                .context
                .as_ref()
                .and_then(|context| context.writer_predecessor)
                != record.retry_of
        {
            // Manual continuations without a graph predecessor follow the
            // immediate run's owner chain. Graph continuations also recheck
            // their sealed source and head below.
            return self.check_writer_lineage(record, tree, call).map(|_| ());
        }
        let Some(context) = task.context.as_ref() else {
            return Ok(());
        };
        let Some(prior) = context.writer_predecessor else {
            return Ok(());
        };
        let previous = self.record(prior, call)?;
        if !matches!(previous.state, AttemptState::Terminal { .. })
            || !self.custody_released(&previous, call)?
        {
            return Err(RunError::Nonterminal);
        }
        let previous_context = previous.context.as_ref();
        let same_node = context.graph_id.is_some()
            && context.node_id.is_some()
            && previous_context.is_some_and(|previous| {
                previous.graph_id == context.graph_id && previous.node_id == context.node_id
            });
        // A new graph can adopt a retained writer worktree whose lane owner
        // belongs to a finished attempt in an earlier graph. It inherits the
        // worktree and the writer's own commits, not the earlier graph's source
        // and head pins: those describe the old graph, not the adopted tree.
        // Writer identity, terminal custody and the lane-owner chain below stay
        // authoritative; ordinary same-node retries keep their pin check.
        let adopted_prior_graph = context.graph_id.is_some()
            && context.node_id.is_some()
            && previous_context.is_some_and(|previous| {
                previous.graph_id.is_some()
                    && previous.node_id.is_some()
                    && previous.graph_id != context.graph_id
            });
        if !same_node && !adopted_prior_graph {
            return Err(RunError::WriterLineage("graph predecessor context"));
        }
        let request = self.request(previous.attempt_id, call)?;
        let previous_task = request.task;
        let source = previous_task
            .source_head
            .as_ref()
            .or(previous_task.head.as_ref());
        let head = previous
            .head
            .map(|head| head.to_string())
            .or_else(|| previous_task.head.clone());
        let owner = self.check_writer_lineage(&previous, tree, call)?;
        if !task
            .writer
            .as_ref()
            .zip(previous_task.writer.as_ref())
            .is_some_and(|(writer, previous)| equivalent_writer(previous, writer))
            || (!adopted_prior_graph
                && (task.source_head.is_none()
                    || task.source_head.as_ref() != source
                    || task.head.is_none()))
        {
            return Err(RunError::WriterLineage(
                "graph predecessor writer or source",
            ));
        }
        // A retained writer may commit after its predecessor was sealed. The
        // continuation re-pins to that descendant under the verified custody of
        // the same lane-owned writer; a moved head from anywhere else still
        // fails this check. An adopted prior graph inherits the writer's own
        // commits, not the earlier graph's pins, so it keeps its inherited head.
        let head_matches = adopted_prior_graph
            || task.head == head
            || owner.is_some_and(|owner| {
                owner.origin == provenant_confinement::worktrees::WorktreeOrigin::Lane
            }) && head
                .as_deref()
                .and_then(|pinned| provenant_core::GitOid::parse(pinned).ok())
                .zip(
                    task.head
                        .as_deref()
                        .and_then(|current| provenant_core::GitOid::parse(current).ok()),
                )
                .is_some_and(|(pinned, current)| {
                    Self::repinned_head_descends(tree, pinned, current)
                });
        if !head_matches {
            return Err(RunError::WriterLineage(
                "graph predecessor writer or source",
            ));
        }
        Ok(())
    }
    pub(crate) fn check_writer_lineage(
        &self,
        record: &AttemptRecord,
        tree: &provenant_confinement::VerifiedWorktree,
        call: &Request<'_>,
    ) -> Result<Option<provenant_confinement::worktrees::WorktreeOwner>> {
        use provenant_confinement::worktrees::{WorktreeOrigin, load_owner};
        let owner = load_owner(tree)?;
        let lane = owner
            .as_ref()
            .filter(|owner| owner.origin == WorktreeOrigin::Lane);
        let mut current = record.clone();
        let mut seen = std::collections::HashSet::new();
        let mut was_owned = false;
        let mut initial_owner = None;
        loop {
            if !seen.insert(current.attempt_id) || seen.len() > 128 {
                return Err(RunError::WriterLineage("continuation parent chain"));
            }
            if lane.is_some_and(|owner| {
                owner.attempt_id == Some(current.attempt_id)
                    || self
                        .attempt_operation(current.attempt_id, call)
                        .is_ok_and(|operation| {
                            owner.operation_id.as_ref() == Some(&operation.operation_id)
                        })
            }) {
                let owner_id =
                    lane.and_then(|owner| owner.attempt_id)
                        .ok_or(RunError::WriterLineage(
                            "continuation owner attempt missing",
                        ))?;
                if owner_id != record.attempt_id && !self.writer_owner_terminal(owner_id, call)? {
                    return Err(RunError::Nonterminal);
                }
                return Ok(owner);
            }
            let directory = self.attempt_dir(current.attempt_id, false)?;
            if let Some(raw) = read_sealed(&directory, "writer-owned.json", crate::RECORD_LIMIT)? {
                // Recorded under custody before launch. A later owner removal or
                // adoption cannot turn an owned lane into an explicit tree.
                let ownership = crate::db::decode::<crate::writer::WriterOwnership>(&raw)?;
                was_owned |= ownership.lane_owned;
                if ownership.lane_owned
                    && ownership
                        .owner
                        .as_ref()
                        .is_some_and(|saved| Some(saved) == lane)
                {
                    let owner_id =
                        lane.and_then(|owner| owner.attempt_id)
                            .ok_or(RunError::WriterLineage(
                                "continuation owner attempt missing",
                            ))?;
                    if !self.writer_owner_terminal(owner_id, call)? {
                        return Err(RunError::Nonterminal);
                    }
                    return Ok(owner);
                }
                let snapshot = ownership;
                was_owned |= snapshot.lane_owned;
                if current.retry_of.is_none() {
                    initial_owner = snapshot.owner;
                }
            }
            let Some(parent) = current.retry_of else {
                break;
            };
            current = self.record(parent, call)?;
        }
        if lane.is_some() && owner == initial_owner {
            return Ok(owner);
        }
        if lane.is_some() {
            return Err(RunError::WriterLineage(
                "continuation worktree belongs to another lane",
            ));
        }
        if was_owned {
            return Err(RunError::WriterLineage(
                "continuation worktree has no lane owner",
            ));
        }
        // Explicit unowned and adopted trees retain their existing lifecycle.
        Ok(owner)
    }
    fn resume_mode(
        &self,
        input: &Resume,
        call: &Request<'_>,
        options: ResumeOptions<'_>,
    ) -> Result<(AttemptRecord, Option<PreparedStart>)> {
        let ResumeOptions {
            fresh,
            writer,
            providers,
            budget_remaining,
            graph_context,
            parent_publication_locked,
            retained_head,
        } = options;
        let prior = self.record(input.prior, call)?;
        if !matches!(prior.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Nonterminal);
        }
        let replay = self.operation(&input.operation_id, call)?.is_some();
        if !replay && writer.is_none() {
            self.check_continuation_writer(input.prior, call)?;
        }
        let policy = self.resume_policy(prior.attempt_id, call)?;
        let _retention = if replay || fresh.is_some() {
            None
        } else if parent_publication_locked {
            self.ensure_provider_state_not_expired(input.prior, &policy)?;
            None
        } else {
            Some(self.retain_provider_state(input.prior, call, &policy)?)
        };
        let request = self.request(prior.attempt_id, call)?;
        let task = &request.task;
        let context = match (graph_context, task.context.as_ref()) {
            (Some(next), Some(previous)) => {
                let next_attempt = previous.attempt.and_then(|attempt| attempt.checked_add(1));
                if next.graph_id != previous.graph_id
                    || next.node_id != previous.node_id
                    || next.attempt != next_attempt
                {
                    return Err(RunError::Validation("graph continuation work key"));
                }
                Some(next.clone())
            }
            (Some(_), None) => {
                return Err(RunError::Validation("graph continuation context missing"));
            }
            (None, _) => budget_remaining
                .and(task.context.clone())
                .map(|mut context| {
                    // A run-internal resume is not a new graph launch ordinal.
                    context.attempt = None;
                    context
                }),
        };
        let mut deliver = task.deliver.clone();
        if let Some((_, branch)) = writer
            && let Some(prior) = &deliver
            && matches!(prior.mode, DeliveryMode::Push | DeliveryMode::Pr)
        {
            // The recorded delivery carries the owner's forge remote; reuse it
            // instead of resolving a second context that could disagree.
            // Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
            let mut destination = HostDeliveryContext::destination_for_remote(
                prior.mode.clone(),
                branch,
                &prior.remote,
            )?
            .ok_or(RunError::Validation("fresh fallback delivery destination"))?;
            destination.remote = prior.remote.clone();
            destination.open_pr |= prior.open_pr;
            deliver = Some(destination);
        }
        // A retained writer continues from the predecessor's sealed terminal
        // head, which may include commits made while it was running. Preserve
        // the original source pin separately so launch preparation can prove
        // that this exact head descends from the authorised source. A fresh
        // replacement worktree keeps the caller's source/head fields.
        let (head, source_head) = if let Some(head) = retained_head {
            (
                Some(head.to_string()),
                task.source_head.clone().or_else(|| task.head.clone()),
            )
        } else if writer.is_none() && task.writer.is_some() {
            (
                prior
                    .head
                    .as_ref()
                    .map(ToString::to_string)
                    .or_else(|| task.head.clone()),
                task.source_head.clone().or_else(|| task.head.clone()),
            )
        } else {
            (task.head.clone(), task.source_head.clone())
        };
        let prepared = PreparedRun {
            attempt_id: input.attempt_id,
            operation_id: input.operation_id.clone(),
            request: CanonicalRequest {
                extra: Default::default(),
                chair: request.chair.clone(),
                fresh_continuation: fresh.map(str::to_owned),
                schema_version: 1,
                task: TaskRequest {
                    context,
                    extra: Default::default(),
                    limit_extensions: extensions(task, &input.extend),
                    build_heavy: task.build_heavy,
                    title: None,
                    base: task.base.clone(),
                    fixture: task.fixture.clone(),
                    head,
                    source_head,
                    base_ref: task.base_ref.clone(),
                    profile: {
                        let mut profile = task.profile.clone();
                        if let Some(remaining) = budget_remaining {
                            profile.budget_wrap_up =
                                Some(provenant_records::run::BudgetWrapUp { remaining });
                        }
                        profile
                    },
                    prompt_digest: provenant_records::CanonicalV1::file_digest(
                        input.prompt.as_bytes(),
                    ),
                    route: prior.requested_route.clone(),
                    account: task.account.clone(),
                    authority_digest: prior.authority_digest,
                    writer: writer
                        .map(|(writer, _)| writer.clone())
                        .or_else(|| task.writer.clone()),
                    deliver,
                    provider_timeout_seconds: task.provider_timeout_seconds,
                    delivery_id: prior.delivery_id,
                    access: task.access.clone(),
                },
                parent: Some(input.prior),
                lane_depth: request.lane_depth,
                parent_attempt: request.parent_attempt,
            },
            prompt: input.prompt.clone(),
            created_at: input.created_at,
        };
        // A replay, prepared or accepted, is answered by `create`, which
        // compares the request digest and adopts a prepared operation's IDs.
        if !replay {
            let generation: i64 = {
                self.db.query_row(
                    "SELECT COUNT(*) FROM attempts WHERE continues=?1 AND refused=0",
                    [input.prior.to_string()],
                    |r| r.get(0),
                )?
            };
            if u64::try_from(generation).ok() != Some(input.expected_generation) {
                return Err(RunError::GenerationConflict {
                    current_generation: u64::try_from(generation)
                        .map_err(|_| RunError::Conflict)?,
                });
            }
        }
        let mut ready = if replay {
            None
        } else {
            providers
                .map(|providers| self.prepare_fresh_start(&prepared, providers, call))
                .transpose()?
        };
        let created = self.create(&prepared, call);
        if let Some(ready) = &mut ready {
            let recorded = match &created {
                Ok(run) => run.attempt_id == prepared.attempt_id,
                // Publication may fail after reserving the operation. Keep
                // its staging for recovery, including an unreadable index.
                Err(_) => match self.operation_attempt(&input.operation_id, call) {
                    Ok(attempt) => attempt == prepared.attempt_id,
                    Err(RunError::NotFound) => false,
                    Err(_) => true,
                },
            };
            if recorded {
                self.retain_prepared_start(ready, prepared.attempt_id);
            }
        }
        let run = created?;
        self.contribution_diagnostic(
            run.attempt_id,
            self.inherit_contribution(
                input.prior,
                run.attempt_id,
                writer.map(|(_, branch)| branch),
                call,
            ),
        );
        if run.attempt_id != prepared.attempt_id {
            ready = None;
        }
        Ok((run, ready))
    }

    /// Up to `limit` attempts needing recovery in this project. Retained scratch
    /// is collected separately after its work has finished.
    pub fn open_attempts(&self, limit: u64, call: &Request<'_>) -> Result<Vec<AttemptId>> {
        call.check()?;
        self.open_attempts_readonly(limit, call)
    }

    pub(crate) fn writer_issue_candidate(
        &self,
        issue: &provenant_supervisor::custody::ReservationIssue,
        call: &Request<'_>,
    ) -> Option<(AttemptId, std::path::PathBuf)> {
        if issue.reservation.file_name() != Some(std::ffi::OsStr::new("reservation.json")) {
            return None;
        }
        let parent = issue.reservation.parent()?;
        let name = parent.file_name()?.to_str()?;
        let id = AttemptId::parse(name).ok()?;
        if id.to_string() != name {
            return None;
        }
        let expected_slot = self.attempt_writer_slot(id, call).ok()??;
        (parent.parent() == Some(expected_slot.as_path())).then(|| (id, issue.reservation.clone()))
    }

    /// Inspect pending recovery without mutating Run state.
    pub fn open_attempts_readonly(&self, limit: u64, call: &Request<'_>) -> Result<Vec<AttemptId>> {
        call.check()?;
        let limit = crate::db::sql_int(limit)?;
        let ids: Vec<String> = {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE project=?1 AND terminal=0 ORDER BY sequence LIMIT ?2",
            )?;
            let rows = query
                .query_map(params![self.context.project_id.to_string(), limit], |r| {
                    r.get(0)
                })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut runs = ids
            .iter()
            .map(|id| AttemptId::parse(id).map_err(|_| RunError::Validation("attempt index ID")))
            .collect::<Result<Vec<_>>>()?;
        let (fences, issues) = match self.writer_fence_listing() {
            Ok(listing) => (listing.reservations, listing.issues),
            Err(_) if !runs.is_empty() => (Vec::new(), Vec::new()),
            Err(error) => return Err(error),
        };
        for fence in fences {
            if !runs.contains(&fence.scope.attempt) && runs.len() < limit as usize {
                runs.push(fence.scope.attempt);
            }
        }
        for issue in issues {
            if let Some((id, _)) = self.writer_issue_candidate(&issue, call)
                && !runs.contains(&id)
                && runs.len() < limit as usize
            {
                runs.push(id);
            }
        }
        // A closed run whose reactions are still owed needs a drain too.
        if runs.len() < limit as usize {
            let owed: Vec<String> = {
                let mut query = self.db.prepare(
                    "SELECT attempt FROM reactions WHERE project=?1 AND pending=1 ORDER BY rowid LIMIT ?2",
                )?;
                let rows = query
                    .query_map(params![self.context.project_id.to_string(), limit], |r| {
                        r.get(0)
                    })?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            for id in owed {
                let run =
                    AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
                if !runs.contains(&run) && runs.len() < limit as usize {
                    runs.push(run);
                }
            }
        }
        // Retry terminal settlement effects without treating every retained
        // scratch directory as actionable recovery work.
        if runs.len() < limit as usize {
            let mut query = self.db.prepare(
                "SELECT a.id,r.record FROM attempts a JOIN settlement_receipts r ON r.attempt=a.id WHERE a.project=?1 AND a.terminal=1 ORDER BY a.sequence",
            )?;
            let mut rows = query.query([self.context.project_id.to_string()])?;
            while runs.len() < limit as usize {
                call.check()?;
                let Some(row) = rows.next()? else { break };
                let id = AttemptId::parse(&row.get::<_, String>(0)?)
                    .map_err(|_| RunError::Validation("attempt index ID"))?;
                if !runs.contains(&id)
                    && self
                        .effects_pending(id, &row.get::<_, Vec<u8>>(1)?, call)
                        .unwrap_or(true)
                {
                    runs.push(id);
                }
            }
        }
        Ok(runs)
    }

    /// A `starting` attempt with no launch evidence whose launcher no longer
    /// holds custody. A live launcher holds its liveness lock before Starting,
    /// then holds writer custody until launch. Any doubt answers false;
    /// settlement re-proves custody before withdrawing.
    fn launcher_gone(
        &self,
        record: &AttemptRecord,
        dir: &provenant_os::fs::Directory,
        missing_lock_is_gone: bool,
        call: &Request<'_>,
    ) -> bool {
        // New launchers hold this from before Starting until handoff returns.
        // A missing file also covers attempts abandoned by older launchers.
        match dir.try_lock_existing(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        ) {
            Ok(_guard) => {}
            Err(error) if error.is_not_found() => {}
            Err(_) => return false,
        }
        let scope = Self::scope(record);
        let launched = |name: &str| {
            !matches!(
                dir.metadata_at(std::ffi::OsStr::new(name), false),
                Err(ref error) if error.is_not_found()
            )
        };
        if launched("launch.json") || launched("process-exit.json") {
            return false;
        }
        match self.writer_fences() {
            Ok(fences) => {
                if let Some(fence) = fences.into_iter().find(|fence| fence.scope == scope) {
                    let Some(held) = fence.custody_directory() else {
                        return false;
                    };
                    let Ok(anchor) = provenant_os::fs::Directory::open_anchor(held) else {
                        return false;
                    };
                    return custody::acquire_recovery(
                        anchor,
                        scope,
                        provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                        call.cancel,
                        call.clock,
                    )
                    .is_ok();
                }
            }
            Err(_) => return false,
        }
        let Ok(_observation) = custody::writer_admission_guard(dir) else {
            return false;
        };
        match dir.try_lock_existing(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        ) {
            Ok(_) => true,
            // No lock file yet: the launcher may not have reached custody.
            // A caller about to take the lock itself may proceed; the lock
            // then keeps a late launcher out.
            Err(error) => missing_lock_is_gone && error.is_not_found(),
        }
    }
    /// Queue recovery for running attempts, cancelled or abandoned starts,
    /// and rejected writers with deferred cleanup. Recovery rechecks custody;
    /// queuing is best effort and leaves live holders alone.
    pub fn reap(&self, call: &Request<'_>) -> Result<usize> {
        let observer = SystemProcessObserver;
        let census = observer
            .census(provenant_supervisor::census::MAX_CENSUS_ENTRIES)
            .ok();
        self.reap_with_census_and_spawn(
            call,
            &observer,
            census.as_ref(),
            provenant_os::exec::spawn_detached,
        )
    }
    /// Queue recovery using one bounded host census shared by every attempt.
    /// A failed or incomplete census cannot prove a writer unattended;
    /// durable exit evidence still permits settlement recovery.
    pub fn reap_with_census(
        &self,
        call: &Request<'_>,
        census: &provenant_supervisor::census::Census,
    ) -> Result<usize> {
        self.reap_with_census_and_spawn(
            call,
            &SystemProcessObserver,
            Some(census),
            provenant_os::exec::spawn_detached,
        )
    }
    /// Continue independent launcher/temporary recovery when host census is
    /// unavailable. Without durable exit evidence, Running attempts remain
    /// Unknown and are not queued.
    pub fn reap_without_census(&self, call: &Request<'_>) -> Result<usize> {
        self.reap_with_census_and_spawn(
            call,
            &SystemProcessObserver,
            None,
            provenant_os::exec::spawn_detached,
        )
    }
    #[cfg(test)]
    pub(crate) fn reap_with(
        &self,
        call: &Request<'_>,
        spawn: impl Fn(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
    ) -> Result<usize> {
        let observer = SystemProcessObserver;
        let census = observer
            .census(provenant_supervisor::census::MAX_CENSUS_ENTRIES)
            .ok();
        self.reap_with_census_and_spawn(call, &observer, census.as_ref(), spawn)
    }
    pub(crate) fn reap_with_census_and_spawn(
        &self,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
        census: Option<&provenant_supervisor::census::Census>,
        spawn: impl Fn(
            &[std::ffi::OsString],
            &std::path::Path,
        ) -> std::result::Result<(), provenant_os::exec::ExecError>,
    ) -> Result<usize> {
        let runs: Vec<String> = {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE project=?1 AND (terminal=0 OR tmp_retained=1)",
            )?;
            let rows =
                query.query_map(params![self.context.project_id.to_string()], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut queued = 0;
        for run in runs {
            let Ok(run) = AttemptId::parse(&run) else {
                continue;
            };
            let ids: Vec<String> = {
                let mut query = self.db.prepare(
                    "SELECT id FROM attempts WHERE id=?1 AND project=?2 AND (terminal=0 OR tmp_retained=1)",
                )?;
                let rows = query.query_map(
                    params![run.to_string(), self.context.project_id.to_string()],
                    |row| row.get(0),
                )?;
                rows.collect::<std::result::Result<_, _>>()?
            };
            let reapable = ids
                .iter()
                .filter_map(|id| {
                    AttemptId::parse(id)
                        .ok()
                        .and_then(|id| self.record(id, call).ok())
                })
                .any(|attempt| match attempt.state {
                    AttemptState::Running { .. } => running_attempt_reapable(census.map_or_else(
                        || {
                            attempt_presence_without_census(
                                self.context.state_root.as_path(),
                                &attempt,
                                observer,
                            )
                        },
                        |census| {
                            attempt_presence_with_observer_and_census(
                                self.context.state_root.as_path(),
                                &attempt,
                                observer,
                                census,
                            )
                        },
                    )),
                    AttemptState::Starting { .. } => self
                        .attempt_dir(attempt.attempt_id, false)
                        .is_ok_and(|dir| {
                            crate::store::StopMarker::read(&dir, Self::scope(&attempt))
                                .is_ok_and(|marker| marker != crate::store::StopMarker::Absent)
                                || self.launcher_gone(&attempt, &dir, true, call)
                        }),
                    AttemptState::Terminal {
                        outcome: Outcome::Rejected,
                        ..
                    } if attempt.provider_launched == Some(false) => self
                        .request(run, call)
                        .is_ok_and(|request| request.task.writer.is_some()),
                    AttemptState::Terminal { .. } => {
                        !self
                            .import_release_permitted(&attempt, call)
                            .unwrap_or(false)
                            || !self.custody_released(&attempt, call).unwrap_or(false)
                    }
                    _ => false,
                });
            if reapable && self.queue_reconcile_with(run, call, &spawn).is_ok() {
                queued += 1;
            }
        }
        Ok(queued)
    }
    /// Check every attempt of `run` against supervisor custody.
    /// A durable exit is committed and released; a launch that provably never
    /// reached a guardian is withdrawn. Terminal temporary data is retried
    /// only after reservations are released; removal failures are reported.
    pub fn reconcile(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        let _ = providers;
        self.queue_reconcile(attempt, call)
    }
    /// Execute the supervisor recovery entry with fixture-owned providers.
    /// Installed builds expose only the asynchronous public control.
    #[cfg(feature = "fixture")]
    pub fn reconcile_for_fixture(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        self.reconcile_with_observer(attempt, providers, call, &SystemProcessObserver)
    }
    pub(crate) fn reconcile_with_observer(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Vec<(AttemptId, Reconciled)>> {
        call.check()?;
        let mut ids: Vec<String> = {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE id=?1 AND project=?2 AND (terminal=0 OR tmp_retained=1) ORDER BY sequence")?;
            let rows = query.query_map(
                params![attempt.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let (fences, issues, listing_error) = match self.writer_fence_listing() {
            Ok(listing) => (listing.reservations, listing.issues, None),
            Err(error) => {
                if self.reaction_pending(attempt, call)? && !ids.contains(&attempt.to_string()) {
                    ids.push(attempt.to_string());
                }
                if ids.is_empty() {
                    return Err(error);
                }
                (Vec::new(), Vec::new(), Some(error.to_string()))
            }
        };
        for id in self.pending_effect_attempts(attempt, call)? {
            if !ids.contains(&id.to_string()) {
                ids.push(id.to_string());
            }
        }
        let mut prior_fences: Vec<_> = fences
            .iter()
            .map(|fence| (fence.scope.attempt, fence.reservation.clone()))
            .collect();
        // Reboot recovery can validate records whose old device numbers make
        // ordinary listing fail. Their paths are candidates for reporting,
        // never authority to release: recovery still proves that separately.
        for issue in issues {
            if let Some((id, reservation)) = self.writer_issue_candidate(&issue, call) {
                prior_fences.push((id, reservation));
                if id == attempt && !ids.contains(&id.to_string()) {
                    ids.push(id.to_string());
                }
            }
        }
        for fence in fences {
            if fence.scope.attempt == attempt && !ids.contains(&fence.scope.attempt.to_string()) {
                ids.push(fence.scope.attempt.to_string());
            }
        }
        let mut found = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let mut state = match retry_reconcile(&self.db, call, || {
                self.reconcile_attempt(id, providers, call, observer)
            }) {
                Ok(state) => state,
                Err(error) => Some(reconcile_failure(error, || {
                    self.reconcile_is_settled(attempt, id, call)
                        .unwrap_or(false)
                })),
            };
            if let Some(error) = &listing_error
                && let Some(
                    uncertain @ (Reconciled::Uncertain { .. }
                    | Reconciled::TemporaryRetained { .. }),
                ) = &state
            {
                state = Some(Reconciled::uncertain(UncertainReason::ReconcileFailed {
                    error: format!("{uncertain}; writer fence listing failed: {error}"),
                }));
            }
            if let Some(state) = state {
                found.push((id, self.track_reconcile_progress(id, state, call)));
            }
        }
        if let Ok(remaining) = self.writer_fence_listing() {
            for (id, state) in &mut found {
                if !matches!(
                    state,
                    Reconciled::Finished | Reconciled::Withdrawn | Reconciled::TemporaryRemoved
                ) {
                    continue;
                }
                let mut locks = Vec::new();
                for (attempt, reservation) in &prior_fences {
                    if *attempt != *id
                        || remaining
                            .reservations
                            .iter()
                            .any(|fence| &fence.reservation == reservation)
                        || remaining.issues.iter().any(|issue| {
                            reservation_issue_blocks_report(reservation, &issue.reservation)
                        })
                    {
                        continue;
                    }
                    if let Some(slot) = reservation.parent().and_then(std::path::Path::parent) {
                        let lock = slot.join("writer.lock");
                        if !locks.contains(&lock) {
                            locks.push(lock);
                        }
                    }
                }
                if !locks.is_empty() {
                    *state = Reconciled::WriterLeaseReleased { locks };
                }
            }
        }
        // A peer may have completed an earlier busy record while this pass
        // judged the remaining records. Report its final state, not that error.
        found.retain(|(id, state)| {
            !matches!(
                state,
                Reconciled::Uncertain { .. } | Reconciled::Settling { .. }
            ) || !self
                .reconcile_is_settled(attempt, *id, call)
                .unwrap_or(false)
        });
        Ok(found)
    }

    pub(crate) fn reconcile_is_settled(
        &self,
        attempt: AttemptId,
        id: AttemptId,
        call: &Request<'_>,
    ) -> Result<bool> {
        let record = self.record(id, call)?;
        let temporary: bool = self.db.query_row(
            "SELECT tmp_retained FROM attempts WHERE id=?1 AND project=?2",
            params![id.to_string(), self.context.project_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(matches!(record.state, AttemptState::Terminal { .. })
            && !temporary
            && self.import_release_permitted(&record, call)?
            && self.custody_released(&record, call)?
            && !self.pending_effect_attempts(attempt, call)?.contains(&id))
    }
    /// Progress is advisory: a broken attempt must not hide its peers' results.
    fn track_reconcile_progress(
        &self,
        id: AttemptId,
        state: Reconciled,
        call: &Request<'_>,
    ) -> Reconciled {
        if matches!(
            &state,
            Reconciled::Uncertain {
                reason: UncertainReason::HostStalled
            }
        ) {
            // A transient host stall must not consume recovery attempts.
            return state;
        }
        let progress = (|| {
            let scope = Self::scope(&self.record(id, call)?);
            let directory = self.attempt_dir(id, false)?;
            match &state {
                // Contention names its holder for status without consuming
                // the recovery budget; the holder is still progressing.
                Reconciled::Uncertain {
                    reason: reason @ UncertainReason::WriterLockBusy { holder_live, .. },
                } => crate::settlement::waiting(
                    &directory,
                    scope,
                    &reason.to_string(),
                    *holder_live,
                    call.clock,
                ),
                Reconciled::Uncertain { reason } => crate::settlement::record_recovery_failure(
                    &directory,
                    scope,
                    &reason.to_string(),
                    call.clock,
                ),
                Reconciled::Settling {
                    held_reason: Some(_),
                    ..
                }
                | Reconciled::Abandoned { .. } => Ok(()),
                _ => crate::settlement::clear_held(&directory, scope),
            }
        })();
        if let Err(error) = progress {
            let outcome = match &state {
                Reconciled::Uncertain { reason } => reason.to_string(),
                _ => state.to_string(),
            };
            return Reconciled::uncertain(UncertainReason::ReconcileFailed {
                error: format!("{outcome}; settlement progress update failed: {error}"),
            });
        }
        state
    }
    fn withdraw_prepared_writer(&self, fence: &WriterFence, call: &Request<'_>) -> Result<bool> {
        let directory = fence.custody_directory().ok_or_else(|| {
            RunError::unsafe_path(fence.reservation.clone(), PathCheck::Containment)
        })?;
        let recovery = custody::acquire_recovery(
            provenant_os::fs::Directory::open_anchor(directory)?,
            fence.scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        );
        Ok(match recovery {
            Ok(recovery) if recovery.scope() == fence.scope => recovery.withdraw_prepared().is_ok(),
            _ => false,
        })
    }
    fn settle_never_spawned_writer(
        &self,
        record: &AttemptRecord,
        fence: &WriterFence,
        call: &Request<'_>,
    ) -> Result<Option<Reconciled>> {
        let scope = Self::scope(record);
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        if fence.scope != scope
            || read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
        {
            return Ok(None);
        }
        let directory = fence.custody_directory().ok_or_else(|| {
            RunError::unsafe_path(fence.reservation.clone(), PathCheck::Containment)
        })?;
        let recovery = custody::acquire_recovery(
            provenant_os::fs::Directory::open_anchor(directory)?,
            scope,
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        );
        let Ok(recovery) = recovery else {
            return Ok(None);
        };
        if !recovery
            .never_spawned(&attempt)
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?
        {
            return Ok(None);
        }
        // No canonical exit exists to import the writer's private head.
        // Lost-without-exit retains it through both cleanup and GC. Repeating
        // this transaction also finishes a release interrupted after commit.
        self.commit_lost(
            scope,
            "launch interrupted: provider never spawned; private evidence retained",
            call,
        )?;
        recovery
            .release_lost()
            .map_err(|error| RunError::Io(std::io::Error::other(error)))?;
        self.set_temporary_retained(record.attempt_id, false, call)?;
        Ok(Some(Reconciled::Finished))
    }
    /// Settle an attempt that has no launch summary only after acquiring the
    /// launcher's liveness lock. A live owner may still be preparing a provider;
    /// keep that owner ahead of reconciliation, then re-read under the lock.
    fn settle_unlaunched(
        &self,
        id: AttemptId,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> Result<Reconciled> {
        if !matches!(record.state, AttemptState::Starting { .. }) {
            return Ok(Reconciled::uncertain(UncertainReason::NoLaunchSummary));
        }
        let attempt = self.attempt_dir(id, false)?;
        let _launcher = match attempt.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        ) {
            Ok(guard) => guard,
            Err(provenant_os::fs::FsError::LockTimeout { .. }) => {
                return Ok(Reconciled::Held);
            }
            Err(error) => return Ok(Reconciled::custody_unavailable(&error)),
        };
        let current = self.record(id, call)?;
        if !matches!(current.state, AttemptState::Starting { .. })
            || current.generation != record.generation
            || current.key != record.key
        {
            return Ok(Reconciled::Held);
        }
        let record = &current;
        let scope = Self::scope(record);
        let cancelled = crate::store::cancel_requested(&attempt, scope);
        let dir = || self.attempt_dir(id, false);
        if let Some(fence) = self
            .writer_fences()?
            .into_iter()
            .find(|fence| fence.scope == scope)
        {
            if self.withdraw_prepared_writer(&fence, call)? {
                let now = call
                    .clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?;
                self.withdraw_with_preparation_failure(
                    id,
                    crate::store::Withdrawal {
                        expected: AttemptState::Starting {
                            extra: Default::default(),
                        },
                        outcome: if cancelled {
                            Outcome::Cancelled
                        } else {
                            Outcome::Rejected
                        },
                        reason: if cancelled {
                            "cancelled before writer launch"
                        } else {
                            "launch interrupted: writer preparation never reached a guardian"
                        },
                        finished_at: now,
                        failure: None,
                        cause: if cancelled {
                            None
                        } else {
                            Some(CauseKind::LaunchHandoffRefused)
                        },
                        terminal_note: None,
                    },
                    call,
                )?;
                return Ok(Reconciled::Withdrawn);
            }
            if let Some(state) = self.settle_never_spawned_writer(record, &fence, call)? {
                return Ok(state);
            }
            return Ok(Reconciled::uncertain(
                UncertainReason::WriterCustodyUnconfirmed {
                    reservation: fence.reservation.clone(),
                },
            ));
        }
        // The submitter died before reserving: the held lease proves no
        // handoff began and keeps a late reservation out.
        let attempt = dir()?;
        let unreserved =
            custody::prove_unreserved(&attempt, scope, call.lock_budget(), call.cancel, call.clock)
                .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
        let _recovery = match unreserved {
            Some(lease) => Held::Unreserved(lease),
            None => {
                let recovery = match custody::acquire_recovery(
                    dir()?,
                    scope,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                ) {
                    Ok(recovery) => recovery,
                    Err(error) => return Ok(Reconciled::custody_unavailable(&error)),
                };
                if recovery.scope() != scope || recovery.observe() != RecoveryState::Absent {
                    return Ok(Reconciled::uncertain(
                        UncertainReason::CustodyNotProvablyAbsent,
                    ));
                }
                if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some() {
                    return Ok(Reconciled::uncertain(
                        UncertainReason::ExitWithoutLaunchSummary,
                    ));
                }
                Held::Reserved(Box::new(recovery))
            }
        };
        let now = call
            .clock
            .wall_now()
            .map_err(|_| RunError::Validation("wall clock"))?;
        let done = self.withdraw_with_preparation_failure(
            id,
            crate::store::Withdrawal {
                expected: AttemptState::Starting {
                    extra: Default::default(),
                },
                outcome: if cancelled {
                    Outcome::Cancelled
                } else {
                    Outcome::Rejected
                },
                reason: if cancelled {
                    "cancelled before launch"
                } else {
                    "launch interrupted: never reached a guardian"
                },
                finished_at: now,
                failure: None,
                cause: if cancelled {
                    None
                } else {
                    Some(CauseKind::LaunchHandoffRefused)
                },
                terminal_note: None,
            },
            call,
        )?;
        Ok(match done {
            Some(_) => Reconciled::Withdrawn,
            None => Reconciled::Finished,
        })
    }
    fn reconcile_terminal_temporary(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        let directory = self.attempt_dir(record.attempt_id, false)?;
        if crate::settlement::queued_abandonment_pending(&directory, record)? {
            return Ok(None);
        }
        if let Some(state) = self.settle_rebooted(record, call, observer)? {
            return Ok(Some(state));
        }
        let id = record.attempt_id;
        let scope = Self::scope(record);
        let path = crate::plan::scratch_root(self.context.state_root.as_path(), id).join("tmp");
        let listing = match self.writer_fence_listing() {
            Ok(listing) => listing,
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string().into(),
                }));
            }
        };
        let retained = listing
            .reservations
            .into_iter()
            .find(|fence| fence.scope.attempt == record.attempt_id && fence.scope.attempt == id);
        match retained {
            Some(fence)
                if fence.scope == scope
                    && record.provider_launched == Some(false)
                    && record.exit_evidence.is_none()
                    && self.withdraw_prepared_writer(&fence, call)? =>
            {
                // Another retained reservation may name this attempt.
                // Retry temporary cleanup on the next scan.
                return Ok(Some(Reconciled::Withdrawn));
            }
            Some(fence) => {
                if Self::needs_lost_settlement(record)
                    && let Some(state) = self.settle_never_spawned_writer(record, &fence, call)?
                {
                    return Ok(Some(state));
                }
                return Ok(Some(Reconciled::uncertain(
                    UncertainReason::WriterReservationRetained {
                        reservation: fence.reservation.clone(),
                    },
                )));
            }
            None => {}
        }
        // Any terminal writer whose cleanup is not done: a refusal defers it,
        // and a path that found the writer lock busy left it, launched or not.
        // A refused import ends automatic cleanup instead: its Git pins and
        // scratch are recovery evidence, never cleanup debt.
        let pending_writer = matches!(record.state, AttemptState::Terminal { .. })
            && self.request(record.attempt_id, call)?.task.writer.is_some()
            && !record.settlement_receipts.as_ref().is_some_and(|receipts| {
                matches!(receipts.cleanup_custody_release, EffectDisposition::Done)
            })
            && crate::holder::object_import_refused(&directory, scope)?.is_none();
        // An unreadable reservation cannot establish absence of custody.
        if let Some(issue) = listing.issues.first() {
            let temporary = self.attempt_scratch_dir(id, false).and_then(|scratch| {
                scratch
                    .metadata_at(std::ffi::OsStr::new("tmp"), false)
                    .map_err(Into::into)
            });
            if !pending_writer
                && matches!(temporary, Err(RunError::Filesystem(ref error)) if error.is_not_found())
            {
                self.set_temporary_retained(id, false, call)?;
                return Ok(None);
            }
            return Ok(Some(Reconciled::TemporaryRetained {
                path,
                reason: TemporaryReason::new(
                    TemporaryReasonKind::ReservationUnreadable,
                    format!(
                        "writer reservation unreadable at {}: {}",
                        issue.reservation.display(),
                        issue.reason
                    ),
                ),
            }));
        }
        // Starting writer refusals defer Git cleanup so launch can report the
        // terminal result without waiting for Git. Keep the retained marker
        // until the independently retried effect actually succeeds.
        if pending_writer {
            let attempt = self.attempt_dir(id, false)?;
            if !self.import_release_permitted(record, call)? {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: "Git import still holds custody".into(),
                }));
            }
            // A busy writer lock leaves the cleanup pending, naming its holder.
            let receipts = match self.retry_git_cleanup(record, call) {
                Err(error) if error.is_writer_contention() => {
                    return Ok(Some(Reconciled::TemporaryRetained {
                        path,
                        reason: error.to_string().into(),
                    }));
                }
                receipts => receipts?,
            };
            if !matches!(receipts.cleanup_custody_release, EffectDisposition::Done) {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: format!(
                        "Git cleanup remains pending: {:?}",
                        receipts.cleanup_custody_release
                    )
                    .into(),
                }));
            }
            let _ = crate::retention::prune_caches(
                &attempt,
                record,
                false,
                &mut crate::ProviderStateGc::default(),
                &|| true,
            );
        }
        if matches!(
            record.state,
            AttemptState::Terminal {
                outcome: Outcome::Lost,
                exit_code: None,
                ..
            }
        ) && record.exit_evidence.is_none()
        {
            // The retry marker covers release, including reader reservations.
            // Once released, preserved scratch must never enter normal cleanup.
            match self.attempt_dir(id, false).and_then(|attempt| {
                let evidence = custody::attempt_evidence(&attempt, scope)?;
                read_sealed(&evidence, "reservation.json", RECORD_LIMIT)
            }) {
                Ok(Some(_)) => {
                    // A read-only review holds no Git work: a reservation its
                    // departed guardian left cannot fence anything, so it is
                    // not uncertain custody (#3844).
                    if !self.terminal_read_only_without_git_work(record, call, scope)
                        || !self.custody_released(record, call)?
                    {
                        return Ok(Some(Reconciled::uncertain(
                            UncertainReason::GuardianNotObservable,
                        )));
                    }
                }
                Ok(None) => {}
                Err(RunError::Filesystem(error)) if error.is_not_found() => {}
                Err(error) => {
                    return Ok(Some(Reconciled::TemporaryRetained {
                        path,
                        reason: error.to_string().into(),
                    }));
                }
            }
            self.set_temporary_retained(id, false, call)?;
            return Ok(None);
        }
        let scratch = match self.attempt_scratch_dir(id, false).and_then(|scratch| {
            match scratch.metadata_at(std::ffi::OsStr::new("tmp"), false) {
                // Already collected: prune the emptied per-attempt root too.
                Err(error) if error.is_not_found() => {
                    crate::retention::collect_empty_scratch_root(&scratch)?;
                    Err(error.into())
                }
                Err(error) => Err(error.into()),
                Ok(_) => Ok(scratch),
            }
        }) {
            Ok(scratch) => scratch,
            Err(RunError::Filesystem(error)) if error.is_not_found() => {
                self.set_temporary_retained(id, false, call)?;
                return Ok(None);
            }
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string().into(),
                }));
            }
        };
        let attempt = match self.attempt_dir(id, false) {
            Ok(attempt) => attempt,
            Err(error) => {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: error.to_string().into(),
                }));
            }
        };
        // Released refused-import scratch is recovery evidence, launched or
        // not, even when settled after reboot. It is never cleanup debt.
        if crate::holder::object_import_refused(&attempt, scope)?.is_some() {
            self.set_temporary_retained(id, false, call)?;
            return Ok(None);
        }
        let request = self.request(id, call)?;
        if crate::retention::scratch_owner(record, &request) != crate::ScratchOwner::Finished {
            // Release does not finish the work. A follow-up may still need scratch.
            return Ok(None);
        }
        let _publication = attempt.try_lock_existing(
            std::ffi::OsStr::new("publication.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        if !self.custody_released(record, call)? || self.scratch_in_use(id)? {
            return Ok(None);
        }
        let remove = || -> Result<()> {
            match scratch.metadata_at(std::ffi::OsStr::new("tmp"), false) {
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            // A read-only guardian reserves the attempt itself. Hold
            // its exclusion lock and refuse any retained reservation.
            let observation = custody::writer_admission_guard(&attempt)
                .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
            let _lease = attempt.try_lock(
                std::ffi::OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )?;
            drop(observation);
            match custody::reservation_present(&attempt, scope) {
                Ok(true) => return Err(RunError::AttemptReservationRetained),
                Ok(false) => {}
                Err(error) => return Err(error.into()),
            }
            artifacts::remove_unreserved_terminal_temporary(
                &scratch,
                &path,
                &|| !call.cancel.is_cancelled(),
                &mut 0,
            )?;
            crate::retention::collect_empty_scratch_root(&scratch)
        };
        Ok(Some(match remove() {
            Ok(()) => {
                self.set_temporary_retained(id, false, call)?;
                Reconciled::TemporaryRemoved
            }
            Err(error) => Reconciled::TemporaryRetained {
                path,
                reason: TemporaryReason::new(
                    match error {
                        RunError::AttemptReservationRetained => {
                            TemporaryReasonKind::ReservationRetained
                        }
                        RunError::Filesystem(provenant_os::fs::FsError::NotPrivate { .. }) => {
                            TemporaryReasonKind::NotPrivate
                        }
                        _ => TemporaryReasonKind::Error,
                    },
                    error.to_string(),
                ),
            },
        }))
    }

    fn reconcile_attempt(
        &self,
        id: AttemptId,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        let record = self.record(id, call)?;
        let directory = self.attempt_dir(id, false)?;
        if let Some(state) = crate::settlement::abandoned(&directory, Self::scope(&record))? {
            return Ok(Some(state));
        }
        if !matches!(record.state, AttemptState::Queued { .. })
            && self
                .attempt_dir(id, false)
                .is_ok_and(|attempt| self.supervisor_stalled(&attempt, Self::scope(&record), call))
        {
            return Ok(Some(Reconciled::uncertain(UncertainReason::HostStalled)));
        }
        if matches!(record.state, AttemptState::Terminal { .. }) {
            if !self.release_admission(&record, call) {
                self.set_temporary_retained(id, true, call)?;
                return Ok(Some(Reconciled::TemporaryRetained {
                    path: crate::plan::scratch_root(self.context.state_root.as_path(), id)
                        .join("tmp"),
                    reason: TemporaryReason::new(
                        TemporaryReasonKind::Error,
                        "repository admission release remains pending",
                    ),
                }));
            }
            if !self.import_release_permitted(&record, call)?
                && let Some(state) = self.settle_rebooted(&record, call, observer)?
            {
                return Ok(Some(state));
            }
            let directory = self.attempt_dir(id, false)?;
            if let Some(state) =
                self.settle_proven_guardian_exit(&record, providers, call, observer)?
            {
                return Ok(Some(state));
            }
            let scope = Self::scope(&record);
            let effects_retained = read_sealed(&directory, "terminal-effects.json", RECORD_LIMIT)?
                .is_some()
                && record.exit_evidence.is_some()
                && (custody::reservation_present(&directory, scope)?
                    || self
                        .writer_fences()?
                        .iter()
                        .any(|fence| fence.scope == scope));
            // Retained custody can only be reattached through the launch
            // summary; an attempt that never sealed one has nothing to recover.
            let reattachable = effects_retained
                && directory
                    .metadata_at(std::ffi::OsStr::new("launch.json"), false)
                    .is_ok();
            if !self.import_release_permitted(&record, call)?
                || self.retained_import_fence(Self::scope(&record))?
                || reattachable
            {
                let handle = custody::reattach(directory.try_clone()?, Self::scope(&record))
                    .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
                if let Observation::Exited(exit) =
                    custody::observe(&handle, call.lock_budget(), call.cancel, call.clock)
                        .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?
                {
                    // The provider exited. The settlement worker waits boundedly
                    // for another holder of its writer lock; contention is never
                    // counted as failure.
                    let recovery = self.orphan_recovery(
                        &record,
                        directory.try_clone()?,
                        self.writer_lock_wait(call),
                        call,
                    )?;
                    self.finish_recovered_terminal(
                        id, &directory, &exit, recovery, providers, call,
                    )?;
                }
                let current = self.record(id, call)?;
                if !self.import_release_permitted(&current, call)? {
                    return Ok(Some(crate::settlement::settling(
                        &directory, scope, call.clock,
                    )?));
                }
            }
            let current = self.record(id, call)?;
            self.reconcile_terminal_effects(&current, call)?;
            if crate::holder::object_import_refused(&directory, scope)?.is_some()
                && self.custody_released(&current, call)?
            {
                self.set_temporary_retained(id, false, call)?;
                return Ok(Some(Reconciled::Finished));
            }
            return self.reconcile_terminal_temporary(&current, call, observer);
        }
        self.reconcile_live_attempt(&record, providers, call, observer)
            .map(Some)
    }

    fn reconcile_live_attempt(
        &self,
        record: &AttemptRecord,
        providers: &dyn Providers,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Reconciled> {
        let id = record.attempt_id;
        let scope = Self::scope(record);
        if matches!(record.state, AttemptState::Queued { .. }) {
            return Ok(Reconciled::Queued);
        }
        if let Some(state) = self.settle_proven_guardian_exit(record, providers, call, observer)? {
            return Ok(state);
        }
        if let Some(state) = self.settle_rebooted(record, call, observer)? {
            return Ok(state);
        }
        let dir = || self.attempt_dir(id, false);
        match custody::reattach(dir()?, scope) {
            Ok(handle) => {
                let budget = provenant_core::WaitBudget::cli(call.clock.mono_now(), 1)
                    .map_err(|_| RunError::Validation("observation budget"))?;
                match custody::observe(&handle, budget, call.cancel, call.clock) {
                    Ok(Observation::Exited(exit))
                        if exit.unresolved_cleanup && !exit.unresolved_pids.is_empty() =>
                    {
                        let still_named = || {
                            if custody::any_pid_alive(&exit.unresolved_pids) {
                                Reconciled::Escaped {
                                    pids: exit.unresolved_pids.clone(),
                                }
                            } else {
                                // A live guardian republishes the settled exit;
                                // the next pass commits it.
                                Reconciled::Held
                            }
                        };
                        // The writer lock is free only when no guardian remains
                        // to settle this exit, so recovery ends the orphans.
                        let probe = provenant_core::WaitBudget::nonblocking(call.clock.mono_now());
                        let Ok(mut recovery) = self.orphan_recovery(record, dir()?, probe, call)
                        else {
                            return Ok(still_named());
                        };
                        match recovery.end_orphans(&dir()?, ORPHAN_PATIENCE) {
                            Ok(report) if report.survivors.is_empty() => {
                                drop(recovery);
                                // The exit is settled: commit it as any other.
                                self.reconcile_attempt(id, providers, call, observer)
                                    .map(|state| state.unwrap_or(Reconciled::Finished))
                            }
                            Ok(report) => Ok(Reconciled::Escaped {
                                pids: report.survivors.iter().map(|s| s.pid).collect(),
                            }),
                            Err(CustodyError::Fenced) => Ok(still_named()),
                            Err(error) => Ok(Reconciled::custody_unavailable(&error)),
                        }
                    }
                    Ok(Observation::Exited(exit)) => {
                        let attempt = dir()?;
                        let finished = self.finish(id, &attempt, &exit, providers, call)?;
                        if !self.import_release_permitted(&finished, call)? {
                            // The first finish may just have sealed an import
                            // failure. Settle its receipt in this same pass.
                            return self
                                .reconcile_attempt(id, providers, call, observer)
                                .map(|state| state.unwrap_or(Reconciled::Finished));
                        }
                        custody::publish_release(&attempt, scope)
                            .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))?;
                        Ok(Reconciled::Finished)
                    }
                    Ok(Observation::Live) => {
                        // Recovery may repeat the guardian holder's idempotent
                        // policy action after warning.
                        if let Some(state) = self.apply_stall_policy(record, &handle, call)? {
                            return Ok(state);
                        }
                        if past_runtime_deadline(record, call) {
                            // The guardian should have stopped the provider at
                            // the deadline itself; ask again through its own
                            // channel.
                            Ok(match custody::stop(&handle, StopReason::WallDeadline) {
                                Ok(_) => Reconciled::DeadlineStop,
                                Err(error) => Reconciled::custody_unavailable(&error),
                            })
                        } else {
                            Ok(Reconciled::Held)
                        }
                    }
                    Ok(Observation::PublicationPending) => Ok(Reconciled::Held),
                    Ok(Observation::Unknown) => {
                        let settled =
                            match self.settle_unreserved_loss(record, call, observer, false)? {
                                Some(state) => Some(state),
                                None => self.settle_holderless(record, call, observer)?,
                            };
                        Ok(settled.unwrap_or_else(|| {
                            Reconciled::uncertain(UncertainReason::GuardianNotObservable)
                        }))
                    }
                    Err(CustodyError::Filesystem(
                        error @ provenant_os::fs::FsError::LockTimeout { .. },
                    )) => Ok(reconcile_failure(RunError::Filesystem(error), || {
                        self.reconcile_is_settled(record.attempt_id, id, call)
                            .unwrap_or(false)
                    })),
                    Err(error) => Ok(Reconciled::custody_unavailable(&error)),
                }
            }
            // No launch summary: the handoff never confirmed. Only a
            // reservation that provably never spawned may be withdrawn.
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                self.settle_unlaunched(id, record, call)
            }
            Err(error) => Ok(Reconciled::custody_unavailable(&error)),
        }
    }
}

fn started_attempt(started: &Started) -> AttemptId {
    match started {
        Started::Launched(attempt)
        | Started::Refused { attempt, .. }
        | Started::Skipped { attempt, .. }
        | Started::Unknown { attempt, .. }
        | Started::Blocked { attempt, .. } => *attempt,
    }
}
/// Writer exclusion held across a withdrawal.
#[expect(dead_code, reason = "held only for its lock")]
enum Held {
    Unreserved(custody::Unreserved),
    Reserved(Box<custody::Recovery>),
}

#[cfg(test)]
#[path = "controls_tests.rs"]
mod tests;
