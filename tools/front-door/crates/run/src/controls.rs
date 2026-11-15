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
}

pub(crate) fn steer_route_digest(route: &RouteRecord) -> Result<String> {
    let pool = route
        .pool_selection
        .as_ref()
        .map(|selection| (&selection.pool, &selection.pick));
    Ok(provenant_records::CanonicalV1::record_digest(
        "steer-provider-route",
        1,
        &(
            route.provider.as_deref(),
            route.model.as_deref(),
            route.effort.as_deref(),
            route.account_key.as_deref(),
            route.account_source,
            route.source.as_deref(),
            route.host_native.as_deref(),
            pool,
        ),
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
    /// Queued work ended without reaching a provider.
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

/// Whether a running attempt's guardian and provider are both gone with no
/// exit recorded, so nothing is left to settle it. Read-only; unreadable
/// state counts as not stale. `root` is the execution state root.
pub fn holder_absent(root: &std::path::Path, record: &AttemptRecord) -> bool {
    if !matches!(record.state, AttemptState::Running { .. }) {
        return false;
    }
    let path = root.join(format!("attempts/{}", record.attempt_id));
    let Ok(attempt) = provenant_os::fs::Directory::open_anchor(&path) else {
        return false;
    };
    matches!(
        read_sealed(&attempt, "process-exit.json", RECORD_LIMIT),
        Ok(None)
    ) && custody::holder_gone(&attempt, RunStore::scope(record), &SystemProcessObserver)
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
    /// The supervisor is resuming durable settlement in the background.
    Settling {
        phase: String,
        blocking_reason: Option<String>,
    },
    /// A chair explicitly released settlement while retaining its evidence.
    Abandoned { reason: String },
    /// A durable exit was committed and released.
    Finished,
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
impl std::fmt::Display for Reconciled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => f.write_str("queued"),
            Self::Held => f.write_str("held"),
            Self::DeadlineStop => f.write_str("deadline_stop"),
            Self::Settling {
                phase,
                blocking_reason,
            } => {
                write!(f, "settling ({phase})")?;
                if let Some(reason) = blocking_reason {
                    write!(f, ": {reason}")?;
                }
                Ok(())
            }
            Self::Abandoned { reason } => write!(f, "abandoned: {reason}"),
            Self::Finished => f.write_str("finished"),
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
        }
    }
}

/// How long recovery waits after each of TERM and KILL for an orphan to go.
const ORPHAN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(test)]
mod reboot_tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn missing_terminal_attempt_directory_does_not_abort_reconcile()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        store.commit_lost(RunStore::scope(&record), "fixture", &call)?;
        store.set_temporary_retained(id, true, &call)?;
        let path = context.state_root.as_path().join(format!("attempts/{id}"));
        std::fs::rename(path, temp.path().join("missing-attempt"))?;
        store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        Ok(())
    }

    #[test]
    fn reader_refusal_published_before_recovery_exclusion_is_not_lost()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        let scope = RunStore::scope(&record);
        let attempt = store.attempt_dir(id, false)?;
        let path = context.state_root.as_path().join(format!("attempts/{id}"));
        drop(custody::acquire_reader(
            attempt.try_clone()?,
            scope,
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        let reservation = path.join(id.to_string()).join("reservation.json");
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
        value["launch_state"] = "spawned".into();
        value["boot"] = "darwin:00000000-0000-0000-0000-000000000000".into();
        value["guardian"] = serde_json::json!({
            "boot": "darwin:00000000-0000-0000-0000-000000000000",
            "pid": 123, "native_start": 1, "group_id": 123, "session_id": 123
        });
        let meta = attempt.metadata()?;
        value["attempt_device"] = meta.device.into();
        value["attempt_inode"] = meta.inode.into();
        std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
        crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
        // Simulate the guardian's refusal while it owns writer exclusion, in
        // the precise gap after the first holder check and before acquisition.
        let hook_path = path.clone();
        let hook_reservation = reservation.clone();
        HOLDERLESS_BEFORE_EXCLUSION.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let dir = provenant_os::fs::Directory::open_anchor(&hook_path).unwrap();
                let _lease = dir
                    .try_lock(
                        std::ffi::OsStr::new("writer.lock"),
                        provenant_os::fs::LockMode::Exclusive,
                    )
                    .unwrap();
                crate::artifacts::seal(&dir, "process-exit.json", b"refusal", RECORD_LIMIT)
                    .unwrap();
                let mut current: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&hook_reservation).unwrap()).unwrap();
                current["launch_state"] = "settling".into();
                current["unresolved_cleanup"] = false.into();
                current["census_incomplete"] = false.into();
                std::fs::write(&hook_reservation, serde_json::to_vec(&current).unwrap()).unwrap();
            }))
        });
        assert_eq!(
            store.settle_holderless(&record, &call, &SystemProcessObserver)?,
            None
        );
        assert_eq!(store.record(id, &call)?, record);
        assert!(reservation.exists());
        let recovery =
            custody::acquire_recovery(attempt, scope, call.lock_budget(), &cancel, &clock)?;
        assert_eq!(recovery.observe(), RecoveryState::Absent);
        Ok(())
    }

    #[test]
    fn reboot_settlement_error_propagates() -> std::result::Result<(), Box<dyn std::error::Error>> {
        use provenant_supervisor::census::{
            Census, CensusError, ProcessObservation, ProcessObserver,
        };
        struct Reboot;
        impl ProcessObserver for Reboot {
            fn boot_identity(&self) -> std::result::Result<String, CensusError> {
                Ok("darwin:11111111-1111-1111-1111-111111111111".into())
            }
            fn observe(&self, _: i64) -> ProcessObservation {
                unreachable!()
            }
            fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
                unreachable!()
            }
        }
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let scope = RunStore::scope(&record);
        attempt.atomic_write(std::ffi::OsStr::new("process.json"), ".checkpoint-", &serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "scope": scope, "publication_pending": false,
            "guardian": { "boot": "darwin:00000000-0000-0000-0000-000000000000", "pid": 100, "native_start": 1, "group_id": 100, "session_id": 100 },
            "provider": { "boot": "darwin:00000000-0000-0000-0000-000000000000", "pid": 101, "native_start": 2, "group_id": 101, "session_id": 100 }
        }))?, provenant_os::fs::PublishMode::Replace)?;
        let mut next = record.clone();
        next.generation += 1;
        next.revision += 1;
        store.write(&call, |tx| {
            crate::admission::update_attempt(tx, &record, &next)
        })?;
        let result = store.settle_rebooted(&record, &call, &Reboot);
        assert!(
            matches!(
                result,
                Err(RunError::Validation("process observation scope"))
            ),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn unavailable_reboot_evidence_never_proves_a_reboot() -> TestResult {
        for evidence in [
            RebootEvidence::UnreadableExit,
            RebootEvidence::CorruptReservation,
        ] {
            unavailable_reboot_evidence(evidence)?;
        }
        Ok(())
    }

    #[test]
    fn claim_records_the_runtime_deadline_and_a_host_cap_tightens_it()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for (cap, seconds) in [(0, 3600), (600, 600)] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = crate::tests::input();
            store.create(&run, &call)?;
            let id = run.attempt_id;
            assert_eq!(store.record(id, &call)?.runtime_deadline, None);
            store.mark_starting_with_priority(
                &store.record(id, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                None,
                &crate::limits::deadline_only(cap),
                &call,
            )?;
            let claimed = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let started = claimed.started_at.ok_or("no start")?;
            let expected =
                started.checked_add(provenant_core::DurationMicros::from_seconds(seconds)?)?;
            assert_eq!(claimed.runtime_deadline, Some(expected));
        }
        Ok(())
    }

    #[test]
    fn reap_queues_recovery_for_running_and_cancelled_starting_attempts_only()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let attempt = store.attempt_dir(id, false)?;
        let launcher = attempt.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let spawned = std::cell::Cell::new(0);
        let spawn = |_: &[std::ffi::OsString], _: &std::path::Path| {
            spawned.set(spawned.get() + 1);
            Ok(())
        };
        assert_eq!(
            store.reap_with(&call, spawn)?,
            0,
            "an uncancelled start waits"
        );
        let scope = RunStore::scope(&store.record(id, &call)?);
        let request = serde_json::to_vec(&serde_json::json!({"scope": scope}))?;
        crate::artifacts::seal(&attempt, "cancel.json", &request, RECORD_LIMIT)?;
        assert_eq!(
            store.reap_with(&call, spawn)?,
            1,
            "a cancelled start is reaped"
        );
        assert_eq!(spawned.get(), 1);
        drop(launcher);
        store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
        Ok(())
    }

    fn starting_store() -> std::result::Result<
        (
            tempfile::TempDir,
            RunStore,
            provenant_core::AttemptId,
            AttemptId,
        ),
        Box<dyn std::error::Error>,
    > {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        Ok((temp, store, run.attempt_id, id))
    }

    #[test]
    fn a_live_launcher_fences_unlaunched_reconciliation_until_owner_exits()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, _run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let original = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let launcher = attempt.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;

        assert_eq!(
            store.settle_unlaunched(id, &original, &call)?,
            Reconciled::Held
        );
        assert_eq!(store.record(id, &call)?, original);

        drop(launcher);
        assert_eq!(
            store.settle_unlaunched(id, &original, &call)?,
            Reconciled::Withdrawn
        );
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Rejected,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn cancelling_an_attempt_that_never_launched_settles_it_before_returning()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, _run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Cancelled);
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
        assert_eq!(
            store.cancel(id, "stop", &call)?,
            CancelReceipt::AlreadyTerminal
        );
        Ok(())
    }

    #[test]
    fn a_starting_attempt_whose_launcher_died_is_reaped_and_settles()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let attempt = store.attempt_dir(id, false)?;
        let spawn = |_: &[std::ffi::OsString], _: &std::path::Path| Ok(());
        // A live launcher holds the attempt's writer lock.
        let launcher = attempt.lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        assert_eq!(store.reap_with(&call, spawn)?, 0, "a live launcher waits");
        drop(launcher);
        assert_eq!(
            store.reap_with(&call, spawn)?,
            1,
            "a dead launcher is reaped"
        );
        store.reconcile_with_observer(
            run,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        Ok(())
    }

    #[test]
    fn reaping_settles_a_launcher_lost_before_any_writer_lock_exists()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let attempt = store.attempt_dir(id, false)?;
        assert!(
            matches!(attempt.metadata_at(std::ffi::OsStr::new("writer.lock"), false),
            Err(error) if error.is_not_found())
        );
        let launcher = attempt.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 0);
        drop(launcher);
        assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
        store.reconcile_with_observer(
            run,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Rejected,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn reaping_settles_an_old_start_with_no_launcher_or_writer_lock()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let attempt = store.attempt_dir(id, false)?;
        for name in ["launcher.lock", "writer.lock"] {
            assert!(
                matches!(attempt.metadata_at(std::ffi::OsStr::new(name), false),
                Err(error) if error.is_not_found())
            );
        }
        assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
        store.reconcile_with_observer(
            run,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Rejected,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn a_handed_off_launcher_is_not_reaped_as_an_unlaunched_attempt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, _run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let attempt = store.attempt_dir(id, false)?;
        // The launcher no longer holds writer.lock after handing custody to
        // the guardian. Its durable launch evidence rules out prelaunch death.
        drop(attempt.try_lock(
            std::ffi::OsStr::new("writer.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?);
        crate::artifacts::seal(&attempt, "launch.json", b"{}", RECORD_LIMIT)?;
        assert!(!store.launcher_gone(&store.record(id, &call)?, &attempt, true, &call));
        assert_eq!(
            store.reap_with(&call, |_, _| panic!(
                "handed-off launcher must not be reaped"
            ))?,
            0
        );
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Starting { .. }
        ));
        Ok(())
    }

    #[test]
    fn launcher_death_is_proved_in_its_worktree_fence()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_temp, store, run, id) = starting_store()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let record = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let slot = store
            .writer_registry
            .join("worktrees")
            .join(record.project_id.to_string())
            .join("lane");
        std::fs::create_dir_all(&slot)?;
        let held = provenant_supervisor::fixture::acquire_writer(
            provenant_os::fs::Directory::open_anchor(&slot)?,
            RunStore::scope(&record),
            call.lock_budget(),
            &cancel,
            &clock,
        )?;
        assert_eq!(store.writer_fences()?.len(), 1);
        // The attempt itself has no lock; only its fenced slot can prove life.
        assert!(!store.launcher_gone(&record, &attempt, true, &call));
        assert_eq!(
            store.reap_with(&call, |_, _| panic!("live fence holder must not be reaped"))?,
            0
        );
        drop(held);
        assert!(store.launcher_gone(&record, &attempt, false, &call));
        assert_eq!(store.reap_with(&call, |_, _| Ok(()))?, 1);
        store.reconcile_with_observer(
            run,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Rejected,
                ..
            }
        ));
        assert!(store.writer_fences()?.is_empty());
        Ok(())
    }

    #[test]
    fn a_running_attempt_with_no_guardian_is_stale_and_reaped()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let record = store.record(id, &call)?;
        assert!(holder_absent(context.state_root.as_path(), &record));
        let spawned = std::cell::Cell::new(0);
        let reaped = store.reap_with(&call, |_, _| {
            spawned.set(spawned.get() + 1);
            Ok(())
        })?;
        assert_eq!((reaped, spawned.get()), (1, 1));
        Ok(())
    }

    #[test]
    fn only_a_running_attempt_past_its_deadline_and_grace_is_overdue()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let deadline = provenant_core::Clock::wall_now(&clock)?;
        let mut record: AttemptRecord = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "attempt_id": provenant_core::AttemptId::from_bytes([1; 16]),
            "key": {
                "project": provenant_core::ProjectId::from_bytes([3; 16]),
                "graph": "overdue", "node": "test", "attempt": 1
            },
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "attempt_number": 1, "revision": 1, "generation": 0,
            "requested_route": {"kind":"model", "provider":"p", "model":"m"},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": deadline,
            "state": {"state":"running"},
            "usage": {}, "trace": {},
            "read_only_writable": [], "protected_paths": [],
            "prompt": {"path":{"encoding":"utf8", "path":"prompt.txt"},
                "digest": provenant_records::CanonicalV1::file_digest(b""), "bytes":0, "complete":true, "redacted":false}
        }))?;
        record.runtime_deadline = Some(deadline);
        let overdue = |clock: &provenant_core::TestClock, record: &AttemptRecord| {
            past_runtime_deadline(record, &crate::tests::call(clock, &cancel))
        };
        assert!(
            !overdue(&clock, &record),
            "at the deadline the guardian acts"
        );
        clock.advance(provenant_core::DurationMicros::from_seconds(
            DEADLINE_GRACE_SECONDS - 1,
        )?)?;
        assert!(!overdue(&clock, &record));
        clock.advance(provenant_core::DurationMicros::from_seconds(1)?)?;
        assert!(overdue(&clock, &record));
        record.runtime_deadline = None;
        assert!(!overdue(&clock, &record), "no recorded deadline, no stop");
        record.runtime_deadline = Some(deadline);
        record.state = AttemptState::Starting {
            extra: Default::default(),
        };
        assert!(!overdue(&clock, &record));
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum RebootEvidence {
        UnreadableExit,
        CorruptReservation,
    }

    fn unavailable_reboot_evidence(evidence: RebootEvidence) -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let path = context.state_root.as_path().join(format!("attempts/{id}"));
        let evidence_dir = path.join(id.to_string());
        std::fs::create_dir_all(&evidence_dir)?;
        let reservation = evidence_dir.join("reservation.json");
        let cause = match evidence {
            RebootEvidence::CorruptReservation => {
                std::fs::write(&reservation, b"corrupt reservation")?;
                RebootProofCause::Other
            }
            RebootEvidence::UnreadableExit => {
                std::fs::create_dir(path.join("process-exit.json"))?;
                RebootProofCause::Filesystem
            }
        };
        let before = std::fs::read(&reservation).ok();
        assert!(matches!(store.settle_rebooted(
            &store.record(id, &call)?,
            &call,
            &SystemProcessObserver
        )?, Some(Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { cause: found, .. } })
            if found == cause));
        assert_eq!(store.cancel(id, "stop", &call)?, CancelReceipt::Requested);
        let request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("cancel.json"))?)?;
        assert_eq!(
            request["scope"],
            serde_json::to_value(RunStore::scope(&store.record(id, &call)?))?
        );
        assert!(matches!(store.settle_rebooted(
            &store.record(id, &call)?,
            &call,
            &SystemProcessObserver
        )?, Some(Reconciled::Uncertain { reason: UncertainReason::RebootProofUnavailable { cause: found, .. } })
            if found == cause));
        assert_eq!(std::fs::read(&reservation).ok(), before);
        assert_eq!(
            store.record(id, &call)?.state,
            AttemptState::Running {
                extra: Default::default(),
                child_wait: None,
            }
        );
        Ok(())
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;
    use provenant_confinement::worktrees::{self, WorktreeCreate, WorktreeOrigin, WorktreeOwner};
    use provenant_core::Clock;
    use provenant_os::fixture::run_git;
    use std::path::Path;

    /// Initialise `project` with one empty commit, through the scrubbed fixture Git.
    fn init_base_repository(project: &Path) -> std::io::Result<()> {
        run_git(project, &["init", "-q"])?;
        run_git(project, &["commit", "--allow-empty", "-qm", "Base"])?;
        Ok(())
    }

    #[test]
    fn resume_preserves_explicit_worktrees_and_settled_lane_ownership()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut execution = crate::tests::context(temp.path())?;
        let project = temp.path().join("checkout");
        let home = temp.path().join("home");
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(execution.state_root.as_path().into());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        init_base_repository(&project)?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        for (index, origin) in [
            None,
            Some(WorktreeOrigin::Adopted),
            Some(WorktreeOrigin::Lane),
        ]
        .into_iter()
        .enumerate()
        {
            let mut original = crate::tests::input();
            original.attempt_id = AttemptId::from_bytes([60 + index as u8; 16]);
            original.operation_id = format!("explicit-{index}").parse()?;
            original.attempt_id = AttemptId::from_bytes([70 + index as u8; 16]);
            crate::tests::rekey(&mut original);
            let prior = original.attempt_id;
            let (project_id, info) =
                worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                    let info = worktrees::create(
                        context,
                        WorktreeCreate {
                            branch: format!("explicit-{index}"),
                            base: None,
                        },
                        &cancel,
                    )?;
                    if let Some(origin) = origin {
                        let tree =
                            provenant_confinement::verify_worktree(context.repo, &info.path)?;
                        worktrees::save_owner(
                            &tree,
                            &WorktreeOwner {
                                attempt_id: (origin == WorktreeOrigin::Lane)
                                    .then(|| AttemptId::from_bytes([120; 16])),
                                operation_id: (origin == WorktreeOrigin::Lane)
                                    .then(|| "previous-owner".parse().unwrap()),

                                branch: info
                                    .branch
                                    .clone()
                                    .ok_or(worktrees::WorktreeError::NotFound)?,
                                base: info.head.to_string(),
                                host: provenant_core::HostId::from_bytes([3; 16]),
                                created_at: original.created_at,
                                origin,
                                label: None,
                                reference: None,
                            },
                        )?;
                    }
                    Ok((context.binding.project_id(), info))
                })?;
            execution.project_id = project_id;
            let call = Request {
                clock: &clock,
                cancel: &cancel,
            };
            let store = RunStore::open(&execution, &call)?;
            let writer = WriterRequest {
                extra: Default::default(),
                custody_project: Some(project_id),
                project: project.clone(),
                worktree: info.path.clone(),
            };
            original.request.task.writer = Some(writer.clone());
            original.request.task.head = Some(info.head.to_string());
            original.request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some("graph-writer-lineage".into()),
                node_id: Some(format!("writer-{index}")),
                attempt: Some(1),
                ..Default::default()
            });
            store.create(&original, &call)?;
            let record = store.record(prior, &call)?;
            crate::prepare_writer(&writer, &store.writer_registry, project_id)?.snapshot(
                store.context.state_root.as_path(),
                &store.attempt_dir(prior, false)?,
                RunStore::scope(&record),
            )?;
            if origin == Some(WorktreeOrigin::Lane) {
                let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
                assert!(matches!(
                    store.check_writer_lineage(&record, &launch.worktree, &call),
                    Err(RunError::NotFound)
                ));
                let mut owner_context = execution.clone();
                owner_context.project_id = provenant_core::ProjectId::from_bytes([121; 16]);
                let owner_store = RunStore::open(&owner_context, &call)?;
                let mut owner = crate::tests::input();
                owner.attempt_id = AttemptId::from_bytes([122; 16]);
                owner.operation_id = "previous-owner".parse()?;
                owner.attempt_id = AttemptId::from_bytes([120; 16]);
                crate::tests::rekey(&mut owner);
                owner_store.create(&owner, &call)?;
                assert!(matches!(
                    store.check_writer_lineage(&record, &launch.worktree, &call),
                    Err(RunError::Nonterminal)
                ));
                let owner_id = AttemptId::from_bytes([120; 16]);
                owner_store.withdraw(
                    owner_id,
                    AttemptState::Queued {
                        extra: Default::default(),
                    },
                    Outcome::Cancelled,
                    "fixture",
                    clock.wall_now()?,
                    &call,
                )?;
                assert!(store.writer_owner_terminal(owner_id, &call)?);
            }
            if origin.is_none() {
                let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
                worktrees::save_owner(
                    &launch.worktree,
                    &WorktreeOwner {
                        attempt_id: None,
                        operation_id: None,
                        branch: format!("explicit-{index}"),
                        base: info.head.to_string(),
                        host: provenant_core::HostId::from_bytes([3; 16]),
                        created_at: original.created_at,
                        origin: WorktreeOrigin::Adopted,
                        label: None,
                        reference: None,
                    },
                )?;
                assert!(matches!(
                    crate::writer::seal_writer_ownership(
                        &store.attempt_dir(prior, false)?,
                        &launch.worktree
                    ),
                    Err(RunError::Conflict)
                ));
                std::fs::remove_file(
                    launch
                        .worktree
                        .repo()
                        .private_git_dir()
                        .join("provenant-owner.json"),
                )?;
            }
            if origin == Some(WorktreeOrigin::Lane) {
                let attempt = store.attempt_dir(prior, false)?;
                let snapshot = attempt.current_path()?.join("writer-owned.json");
                let unobserved = crate::db::bytes(&crate::writer::WriterOwnership {
                    extra: Default::default(),
                    schema_version: 1,
                    lane_owned: true,
                    owner: None,
                })?;
                std::fs::write(&snapshot, &unobserved)?;
                let launch = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
                assert!(matches!(
                    crate::writer::seal_writer_ownership(&attempt, &launch.worktree),
                    Err(RunError::Conflict)
                ));
                assert_eq!(std::fs::read(&snapshot)?, unobserved);
                assert!(matches!(
                    store.check_writer_lineage(&record, &launch.worktree, &call),
                    Err(RunError::WriterLineage(
                        "continuation worktree belongs to another lane"
                    ))
                ));
                std::fs::remove_file(snapshot)?;
                crate::writer::seal_writer_ownership(&attempt, &launch.worktree)?;
            }
            std::fs::write(info.path.join("pending.txt"), "keep these edits")?;
            store.withdraw(
                prior,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "fixture",
                clock.wall_now()?,
                &call,
            )?;
            let next = Resume {
                extend: Vec::new(),
                prior,
                expected_generation: 0,
                operation_id: format!("explicit-resume-{index}").parse()?,
                attempt_id: AttemptId::from_bytes([90 + index as u8; 16]),
                prompt: "Continue".into(),
                created_at: clock.wall_now()?,
            };
            let resumed = store.resume_fresh(&next, "no provider session", &call)?;
            // A graph retry has a fresh run and no run-policy retry_of. Its
            // sealed predecessor still protects it against ownership changes
            // between admission and the launch-custody check below.
            let mut graph = crate::tests::input();
            graph.attempt_id = AttemptId::from_bytes([180 + index as u8; 16]);
            graph.operation_id = format!("graph-retry-{index}").parse()?;
            graph.attempt_id = AttemptId::from_bytes([190 + index as u8; 16]);
            graph.request.task = original.request.task.clone();
            let graph_task = &mut graph.request.task;
            graph_task.context.as_mut().ok_or("graph context")?.attempt = Some(3);
            graph_task.source_head = graph_task.head.clone();
            graph_task
                .context
                .as_mut()
                .ok_or("graph context")?
                .writer_predecessor = Some(prior);
            store.create(&graph, &call)?;
            let graph_record = store.record(graph.attempt_id, &call)?;
            assert!(graph_record.retry_of.is_none());
            let tree = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
            store.check_launch_writer_lineage(
                &graph_record,
                &graph.request.task,
                &tree.worktree,
                &call,
            )?;
            let mut other_node = graph.request.task.clone();
            other_node.context.as_mut().ok_or("graph context")?.node_id = Some("unrelated".into());
            assert!(matches!(
                store.check_launch_writer_lineage(
                    &graph_record,
                    &other_node,
                    &tree.worktree,
                    &call
                ),
                Err(RunError::WriterLineage("graph predecessor context"))
            ));
            for field in ["writer", "source", "head"] {
                let mut mismatch = graph.request.task.clone();
                match field {
                    "writer" => {
                        mismatch.writer.as_mut().ok_or("writer")?.worktree =
                            project.join(".worktrees/other");
                    }
                    "source" => mismatch.source_head = Some("0".repeat(40)),
                    "head" => mismatch.head = Some("1".repeat(40)),
                    _ => unreachable!(),
                }
                assert!(
                    matches!(
                        store.check_launch_writer_lineage(
                            &graph_record,
                            &mismatch,
                            &tree.worktree,
                            &call
                        ),
                        Err(RunError::WriterLineage(
                            "graph predecessor writer or source"
                        ))
                    ),
                    "graph predecessor accepted mismatched {field}"
                );
            }
            assert_eq!(
                store
                    .request(resumed.attempt_id, &call)?
                    .task
                    .writer
                    .as_ref(),
                Some(&writer)
            );
            assert_eq!(
                std::fs::read_to_string(info.path.join("pending.txt"))?,
                "keep these edits"
            );
            let tree = crate::prepare_writer(&writer, &store.writer_registry, project_id)?;
            store.check_writer_lineage(
                &store.record(next.attempt_id, &call)?,
                &tree.worktree,
                &call,
            )?;
            worktrees::save_owner(
                &tree.worktree,
                &WorktreeOwner {
                    attempt_id: Some(AttemptId::from_bytes([100; 16])),
                    operation_id: Some("foreign".parse()?),
                    branch: format!("explicit-{index}"),
                    base: info.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: original.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                    reference: None,
                },
            )?;
            assert!(matches!(
                store.check_launch_writer_lineage(
                    &graph_record,
                    &graph.request.task,
                    &tree.worktree,
                    &call
                ),
                Err(RunError::WriterLineage(
                    "continuation worktree belongs to another lane"
                ))
            ));
            assert!(matches!(
                store.check_continuation_writer(next.attempt_id, &call),
                Err(RunError::WriterLineage(
                    "continuation worktree belongs to another lane"
                ))
            ));
        }
        Ok(())
    }

    #[test]
    fn resume_refuses_a_worktree_reused_by_another_lane()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut execution = crate::tests::context(temp.path())?;
        let project = temp.path().join("checkout");
        let home = temp.path().join("home");
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(execution.state_root.as_path().into());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        init_base_repository(&project)?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let mut original = crate::tests::input();
        original.request.task.deliver = Some(WriterDeliveryRequest {
            extra: Default::default(),
            mode: DeliveryMode::Pr,
            remote: "origin".into(),
            branch: "old-lane".into(),
            open_pr: true,
            fast_forward_only: false,
        });
        let prior = original.attempt_id;
        let (project_id, info) =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                let info = worktrees::create(
                    context,
                    WorktreeCreate {
                        branch: "lane".into(),
                        base: None,
                    },
                    &cancel,
                )?;
                let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                worktrees::save_owner(
                    &tree,
                    &WorktreeOwner {
                        attempt_id: Some(prior),
                        operation_id: Some(original.operation_id.clone()),
                        branch: "lane".into(),
                        base: info.head.to_string(),
                        host: provenant_core::HostId::from_bytes([3; 16]),
                        created_at: original.created_at,
                        origin: WorktreeOrigin::Lane,
                        label: None,
                        reference: None,
                    },
                )?;
                Ok((context.binding.project_id(), info))
            })?;
        execution.project_id = project_id;
        let call = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = RunStore::open(&execution, &call)?;
        original.request.task.writer = Some(WriterRequest {
            extra: Default::default(),
            custody_project: Some(project_id),
            project: project.clone(),
            worktree: info.path.clone(),
        });
        store.create(&original, &call)?;
        let launch = crate::prepare_writer(
            original.request.task.writer.as_ref().unwrap(),
            &store.writer_registry,
            project_id,
        )?;
        let mut probe = crate::tests::input();
        probe.request.task.writer = original.request.task.writer.clone();
        probe.attempt_id = AttemptId::from_bytes([70; 16]);
        probe.operation_id = "lineage-probe".parse()?;
        probe.attempt_id = AttemptId::from_bytes([71; 16]);
        crate::tests::rekey(&mut probe);
        store.create(&probe, &call)?;
        let continuation = store.record(probe.attempt_id, &call)?;
        launch.snapshot(
            store.context.state_root.as_path(),
            &store.attempt_dir(continuation.attempt_id, false)?,
            RunStore::scope(&continuation),
        )?;
        let queued = store.record(prior, &call)?;
        assert!(matches!(
            store.check_writer_lineage(&continuation, &launch.worktree, &call),
            Err(RunError::Nonterminal)
        ));
        let mut running = queued.clone();
        running.state = AttemptState::Running {
            extra: Default::default(),
            child_wait: None,
        };
        store.db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(&running)?, prior.to_string()],
        )?;
        assert!(matches!(
            store.check_writer_lineage(&continuation, &launch.worktree, &call),
            Err(RunError::Nonterminal)
        ));
        store.db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(&queued)?, prior.to_string()],
        )?;
        store.withdraw(
            prior,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            clock.wall_now()?,
            &call,
        )?;
        assert!(
            store
                .check_writer_lineage(&continuation, &launch.worktree, &call)?
                .is_some()
        );
        let launch = crate::prepare_writer(
            original.request.task.writer.as_ref().unwrap(),
            &store.writer_registry,
            project_id,
        )?;
        let record = store.record(prior, &call)?;
        launch.snapshot(
            store.context.state_root.as_path(),
            &store.attempt_dir(prior, false)?,
            RunStore::scope(&record),
        )?;
        let owner = worktrees::load_owner(&launch.worktree)?.ok_or("owner")?;
        std::fs::remove_file(
            launch
                .worktree
                .repo()
                .private_git_dir()
                .join("provenant-owner.json"),
        )?;
        assert!(matches!(
            store.check_continuation_writer(prior, &call),
            Err(RunError::WriterLineage(
                "continuation worktree has no lane owner"
            ))
        ));
        worktrees::save_owner(
            &launch.worktree,
            &WorktreeOwner {
                origin: WorktreeOrigin::Adopted,
                ..owner.clone()
            },
        )?;
        assert!(matches!(
            store.check_continuation_writer(prior, &call),
            Err(RunError::WriterLineage(
                "continuation worktree has no lane owner"
            ))
        ));
        worktrees::save_owner(&launch.worktree, &owner)?;
        assert!(store.check_continuation_writer(prior, &call)?.is_some());
        let first = Resume {
            extend: Vec::new(),
            prior,
            expected_generation: 0,
            operation_id: "queued-resume".parse()?,
            attempt_id: AttemptId::from_bytes([31; 16]),
            prompt: "Continue".into(),
            created_at: clock.wall_now()?,
        };
        store.resume_fresh(&first, "fixture", &call)?;
        store.withdraw(
            first.attempt_id,
            AttemptState::Queued {
                extra: Default::default(),
            },
            Outcome::Cancelled,
            "fixture",
            clock.wall_now()?,
            &call,
        )?;
        // Simulate an external replacement. This fixture tests lineage,
        // rather than the host retirement policy used to remove a checkout.
        let path = info.path.to_str().ok_or("fixture worktree path encoding")?;
        run_git(&project, &["worktree", "remove", "--", path])?;
        worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
            let replacement = worktrees::create(
                context,
                WorktreeCreate {
                    branch: "lane".into(),
                    base: None,
                },
                &cancel,
            )?;
            assert_eq!(replacement.path, info.path);
            let tree = provenant_confinement::verify_worktree(context.repo, &replacement.path)?;
            worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(AttemptId::from_bytes([40; 16])),
                    operation_id: Some("foreign".parse().unwrap()),
                    branch: "lane".into(),
                    base: replacement.head.to_string(),
                    host: provenant_core::HostId::from_bytes([3; 16]),
                    created_at: original.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                    reference: None,
                },
            )?;
            let queued = store.record(first.attempt_id, &call).unwrap();
            assert!(matches!(
                store.check_writer_lineage(&queued, &tree, &call),
                Err(RunError::WriterLineage(
                    "continuation worktree belongs to another lane"
                ))
            ));
            Ok(())
        })?;
        let next = Resume {
            extend: Vec::new(),
            prior: first.attempt_id,
            operation_id: "new-resume".parse()?,
            attempt_id: AttemptId::from_bytes([51; 16]),
            ..first
        };
        assert!(matches!(
            store.resume(&next, &call),
            Err(RunError::WriterLineage(
                "continuation worktree belongs to another lane"
            ))
        ));
        assert!(matches!(
            store.resume_fresh(&next, "fixture", &call),
            Err(RunError::WriterLineage(
                "continuation worktree belongs to another lane"
            ))
        ));
        let replacement =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                let replacement = worktrees::create(
                    context,
                    WorktreeCreate {
                        branch: "replacement".into(),
                        base: Some(info.head.to_string()),
                    },
                    &cancel,
                )?;
                let tree = provenant_confinement::verify_worktree(context.repo, &replacement.path)?;
                worktrees::save_owner(
                    &tree,
                    &WorktreeOwner {
                        attempt_id: Some(next.attempt_id),
                        operation_id: Some(next.operation_id.clone()),
                        branch: "replacement".into(),
                        base: replacement.head.to_string(),
                        host: provenant_core::HostId::from_bytes([3; 16]),
                        created_at: next.created_at,
                        origin: WorktreeOrigin::Lane,
                        label: None,
                        reference: None,
                    },
                )?;
                Ok(replacement)
            })?;
        let writer = WriterRequest {
            extra: Default::default(),
            custody_project: Some(project_id),
            project: project.clone(),
            worktree: replacement.path,
        };
        let accepted =
            store.resume_fresh_in(&next, "original worktree reassigned", &writer, &call)?;
        assert_eq!(
            store
                .request(accepted.attempt_id, &call)?
                .task
                .writer
                .as_ref(),
            Some(&writer)
        );
        assert_eq!(
            store
                .request(accepted.attempt_id, &call)?
                .task
                .deliver
                .as_ref()
                .unwrap()
                .branch,
            "replacement"
        );
        assert!(info.path.exists(), "foreign lane must retain its checkout");
        Ok(())
    }
}

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

// A lock timeout can race a terminal commit by another settlement owner.
fn reconcile_failure(error: RunError, terminal: impl FnOnce() -> bool) -> Reconciled {
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
}

impl RunStore {
    /// Persist one idempotent steer for an exact running attempt. A live
    /// interrupt is requested only after the shared adapter stream has
    /// validated a session identity for this exact route and provider process.
    pub fn steer(
        &self,
        id: AttemptId,
        operation_id: OperationId,
        text: &str,
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
        // The accepted text is durable before this best-effort wake. If the
        // session is not ready yet, its decoder will perform the same check
        // when it publishes readiness. If another stop already won, keep the
        // earlier accepted text and let terminal settlement report that outcome.
        let _ = self.request_steer_stop_if_ready(id, call);
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
        let (ready, pending) = self.write(call, |tx| {
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
                return Ok((false, false));
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
            let pending: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM steer_intents WHERE attempt=?1 AND resolved=0)",
                [id.to_string()],
                |row| row.get(0),
            )?;
            Ok((true, pending))
        })?;
        if ready {
            self.persist_live_steer_successor_evidence(id, call)?;
        }
        if ready && pending {
            self.request_steer_stop_if_ready(id, call)?;
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
            ready.session.as_deref(),
            Some(&ready.route_digest),
            identity_conflict,
            false,
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
        session: Option<&str>,
        route_digest: Option<&str>,
        identity_conflict: bool,
        evidence_incomplete: bool,
        call: &Request<'_>,
    ) -> Result<Option<SteerContinuationDisposition>> {
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
            {
                (
                    "not_continued",
                    Some(provenant_records::run::SteeringCause::SessionMismatch),
                )
            } else if session.is_some_and(|session| session != expected_session) {
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

    fn request_steer_stop_if_ready(&self, id: AttemptId, call: &Request<'_>) -> Result<bool> {
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
        let route_digest = steer_route_digest(route)?;
        let source = provenant_os::capture::CaptureSourceIdentity {
            attempt: id.to_string(),
            generation: ready.generation,
            boot: ready.provider_boot,
            pid: ready.provider_pid,
            native_start: ready.provider_start,
        };
        if ready.conflict
            || ready.generation != Self::scope(&record).generation
            || ready.route_digest != route_digest
            || !source_is_live(&source)
        {
            return Ok(false);
        }
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
                    .and_then(provenant_records::run::SteeringCause::from_str)
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
        Ok(Some(provenant_records::run::SteeringStatus {
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
                Err(CustodyError::InvalidRecord) if expected_slot => {
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

    /// Writer exclusion for an attempt, taken without waiting: failure means a
    /// guardian (or an inheritor of its lease) still holds custody.
    fn orphan_recovery(
        &self,
        record: &AttemptRecord,
        attempt: provenant_os::fs::Directory,
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
        custody::acquire_recovery(
            directory,
            Self::scope(record),
            provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
            call.cancel,
            call.clock,
        )
        .map_err(|e| RunError::Io(std::io::Error::other(e.to_string())))
    }
    /// Settle an attempt whose guardian and provider are both gone in this
    /// boot and left no durable exit: nothing remains to import or publish, so
    /// it ends Lost with a reason instead of waiting for a holder that is not
    /// coming. Writer exclusion is taken without waiting, so a live guardian,
    /// or an inheritor of its lease, keeps the attempt open.
    fn settle_holderless(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        observer: &dyn ProcessObserver,
    ) -> Result<Option<Reconciled>> {
        let scope = Self::scope(record);
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        if read_sealed(&attempt, "process-exit.json", RECORD_LIMIT)?.is_some()
            || !custody::holder_gone(&attempt, scope, observer)
        {
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
        let Ok(recovery) = custody::acquire_recovery(
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
            || !custody::holder_gone(&attempt, scope, observer)
            || recovery.observe_with(observer) != RecoveryState::Absent
        {
            return Ok(None);
        }
        self.commit_lost(scope, LostReason::Holderless.message(), call)?;
        recovery
            .release_lost()
            .map_err(|error| RunError::Io(std::io::Error::other(error.to_string())))?;
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
    /// Cancel one attempt. Queued work is withdrawn; started work gets a stop
    /// request. A handoff that has not yet published its launch summary is
    /// retried within the call budget.
    // Cancellation coordinates with a real guardian handoff under a hard cap.
    pub fn cancel(&self, id: AttemptId, reason: &str, call: &Request<'_>) -> Result<CancelReceipt> {
        self.cancel_with_observer(id, reason, call, &SystemProcessObserver)
    }
    pub(crate) fn cancel_with_observer(
        &self,
        id: AttemptId,
        reason: &str,
        call: &Request<'_>,
        _observer: &dyn ProcessObserver,
    ) -> Result<CancelReceipt> {
        loop {
            let record = self.record(id, call)?;
            match record.state {
                AttemptState::Terminal { .. } => {
                    self.record_cancellation(
                        id,
                        record.generation,
                        CancellationDisposition::AcknowledgedIneffective,
                        call,
                    )?;
                    return Ok(CancelReceipt::AlreadyTerminal);
                }
                AttemptState::Queued { .. } => {
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
                        Ok(Some(_)) => return Ok(CancelReceipt::Cancelled),
                        Ok(None) => {
                            let terminal = self.record(id, call)?;
                            self.record_cancellation(
                                id,
                                terminal.generation,
                                CancellationDisposition::AcknowledgedIneffective,
                                call,
                            )?;
                            return Ok(CancelReceipt::AlreadyTerminal);
                        }
                        // A concurrent start took it; it is no longer queued.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                }
                _ => {}
            }
            let dir = self.attempt_dir(id, false)?;
            let mut starting_request = None;
            let unlaunched = if matches!(record.state, AttemptState::Starting { .. }) {
                // Serialize cancellation publication with the launch owner's
                // Starting -> Queued admission-skip transition. A stale read of
                // Starting must not leave a queued attempt with an orphaned stop.
                let publication = dir.lock(
                    std::ffi::OsStr::new("publication.lock"),
                    provenant_os::fs::LockMode::Exclusive,
                    call.lock_budget(),
                    call.cancel,
                    call.clock,
                )?;
                let current = self.record(id, call)?;
                if current.state != record.state {
                    drop(publication);
                    continue;
                }
                let scope = Self::scope(&current);
                let unlaunched = matches!(
                    custody::reattach(dir.try_clone()?, scope),
                    Err(CustodyError::Filesystem(ref error)) if error.is_not_found()
                );
                starting_request = Some(custody::request_stop(
                    dir.try_clone()?,
                    scope,
                    current.verified_owner.unwrap_or(self.context.actor_id),
                    StopReason::ExplicitCancellation,
                ));
                drop(publication);
                unlaunched
            } else {
                false
            };
            if let Some(Err(error)) = starting_request.as_ref() {
                return Ok(CancelReceipt::Unobservable {
                    reason: error.to_string(),
                });
            }
            let stopped = match custody::reattach(dir.try_clone()?, Self::scope(&record)) {
                Ok(handle) => custody::stop(&handle, StopReason::ExplicitCancellation),
                Err(CustodyError::Filesystem(error)) if error.is_not_found() => {
                    if let Some(request) = starting_request {
                        request
                    } else {
                        custody::request_stop(
                            dir,
                            Self::scope(&record),
                            record.verified_owner.unwrap_or(self.context.actor_id),
                            StopReason::ExplicitCancellation,
                        )
                    }
                }
                Err(error) => Err(error),
            };
            if unlaunched && stopped.is_ok() {
                // No guardian exists to act on the request. Settle it here so
                // the queue place and the writer are free before we return.
                if self.settle_cancelled_unlaunched(id, call)? {
                    return Ok(CancelReceipt::Cancelled);
                }
            }
            return Ok(match stopped {
                Ok(StopReceipt::Requested) => CancelReceipt::Requested,
                Ok(StopReceipt::AlreadyRequested) => CancelReceipt::AlreadyRequested,
                Err(error) => CancelReceipt::Unobservable {
                    reason: error.to_string(),
                },
            });
        }
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
        let branch = self.fresh_writer_branch(input, writer)?;
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
        let branch = writer
            .map(|writer| self.fresh_writer_branch(input, writer))
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
    fn fresh_writer_branch(&self, input: &Resume, writer: &WriterRequest) -> Result<String> {
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
    /// Called after writer custody is acquired. Graph retries are separate runs,
    /// so their sealed predecessor must be checked independently of `retry_of`.
    pub(crate) fn check_launch_writer_lineage(
        &self,
        record: &AttemptRecord,
        task: &TaskRequest,
        tree: &provenant_confinement::VerifiedWorktree,
        call: &Request<'_>,
    ) -> Result<()> {
        if record.retry_of.is_some() {
            // A manual continuation follows its immediate run predecessor,
            // whose launch already checked any older graph predecessor.
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
        if context.graph_id.is_none()
            || context.node_id.is_none()
            || previous.context.as_ref().is_none_or(|previous| {
                previous.graph_id != context.graph_id || previous.node_id != context.node_id
            })
        {
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
        if !task
            .writer
            .as_ref()
            .zip(previous_task.writer.as_ref())
            .is_some_and(|(writer, previous)| equivalent_writer(previous, writer))
            || task.source_head.is_none()
            || task.source_head.as_ref() != source
            || task.head.is_none()
            || task.head != head
        {
            return Err(RunError::WriterLineage(
                "graph predecessor writer or source",
            ));
        }
        self.check_writer_lineage(&previous, tree, call)?;
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
        } = options;
        let prior = self.record(input.prior, call)?;
        if !matches!(prior.state, AttemptState::Terminal { .. }) {
            return Err(RunError::Nonterminal);
        }
        let replay = self.operation(&input.operation_id, call)?.is_some();
        if !replay && writer.is_none() {
            self.check_continuation_writer(input.prior, call)?;
        }
        let _retention = if replay || fresh.is_some() {
            None
        } else {
            Some(self.retain_provider_state(input.prior, call)?)
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
            let mut destination = HostDeliveryContext::destination(prior.mode.clone(), branch)?
                .ok_or(RunError::Validation("fresh fallback delivery destination"))?;
            destination.remote = prior.remote.clone();
            destination.open_pr |= prior.open_pr;
            deliver = Some(destination);
        }
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
                    head: task.head.clone(),
                    source_head: task.source_head.clone(),
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

    /// Up to `limit` attempts needing recovery or terminal temporary cleanup in
    /// this project. Nonterminal work takes priority over temporary cleanup.
    pub fn open_attempts(&self, limit: u64, call: &Request<'_>) -> Result<Vec<AttemptId>> {
        call.check()?;
        self.open_attempts_readonly(limit, call)
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
        let fences = match self.writer_fence_listing() {
            Ok(listing) => listing.reservations,
            Err(_) if !runs.is_empty() => Vec::new(),
            Err(error) => return Err(error),
        };
        for fence in fences {
            if !runs.contains(&fence.scope.attempt) && runs.len() < limit as usize {
                runs.push(fence.scope.attempt);
            }
        }
        if runs.len() < limit as usize {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE project=?1 AND terminal=1 AND tmp_retained=1 ORDER BY sequence LIMIT ?2",
            )?;
            let mut rows = query.query(params![self.context.project_id.to_string(), limit])?;
            while runs.len() < limit as usize {
                let Some(row) = rows.next()? else { break };
                let run = AttemptId::parse(&row.get::<_, String>(0)?)
                    .map_err(|_| RunError::Validation("attempt index ID"))?;
                if runs.contains(&run) {
                    continue;
                }
                runs.push(run);
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
        self.reap_with(call, provenant_os::exec::spawn_detached)
    }
    pub(crate) fn reap_with(
        &self,
        call: &Request<'_>,
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
                    AttemptState::Running { .. } => true,
                    AttemptState::Starting { .. } => self
                        .attempt_dir(attempt.attempt_id, false)
                        .is_ok_and(|dir| {
                            read_sealed(&dir, "cancel.json", RECORD_LIMIT)
                                .is_ok_and(|found| found.is_some())
                                || self.launcher_gone(&attempt, &dir, true, call)
                        }),
                    AttemptState::Terminal {
                        outcome: Outcome::Rejected,
                        ..
                    } if attempt.provider_launched == Some(false) => self
                        .request(run, call)
                        .is_ok_and(|request| request.task.writer.is_some()),
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
        let (fences, listing_error) = match self.writer_fence_listing() {
            Ok(listing) => (listing.reservations, None),
            Err(error) => {
                if self.reaction_pending(attempt, call)? && !ids.contains(&attempt.to_string()) {
                    ids.push(attempt.to_string());
                }
                if ids.is_empty() {
                    return Err(error);
                }
                (Vec::new(), Some(error.to_string()))
            }
        };
        for id in self.pending_effect_attempts(attempt, call)? {
            if !ids.contains(&id.to_string()) {
                ids.push(id.to_string());
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
                Reconciled::Uncertain { reason } => {
                    crate::settlement::blocked(&directory, scope, &reason.to_string(), call.clock)
                }
                Reconciled::Settling {
                    blocking_reason: Some(_),
                    ..
                }
                | Reconciled::Abandoned { .. } => Ok(()),
                _ => crate::settlement::clear_blocked(&directory, scope),
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
                self.withdraw(
                    id,
                    AttemptState::Starting {
                        extra: Default::default(),
                    },
                    if cancelled {
                        Outcome::Cancelled
                    } else {
                        Outcome::Rejected
                    },
                    if cancelled {
                        "cancelled before writer launch"
                    } else {
                        "launch interrupted: writer preparation never reached a guardian"
                    },
                    now,
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
        let done = self.withdraw(
            id,
            AttemptState::Starting {
                extra: Default::default(),
            },
            if cancelled {
                Outcome::Cancelled
            } else {
                Outcome::Rejected
            },
            if cancelled {
                "cancelled before launch"
            } else {
                "launch interrupted: never reached a guardian"
            },
            now,
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
        let path = self
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{id}/tmp"));
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
        let pending_writer = record.provider_launched == Some(false)
            && matches!(
                record.state,
                AttemptState::Terminal {
                    outcome: Outcome::Rejected,
                    ..
                }
            )
            && self.request(record.attempt_id, call)?.task.writer.is_some()
            && !record.settlement_receipts.as_ref().is_some_and(|receipts| {
                matches!(receipts.cleanup_custody_release, EffectDisposition::Done)
            });
        // An unreadable reservation cannot establish absence of custody.
        if let Some(issue) = listing.issues.first() {
            let temporary = self.attempt_dir(id, false).and_then(|attempt| {
                attempt
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
            if crate::holder::object_import_refused(&attempt, scope)?.is_some() {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: "Git object import was refused; retain its evidence".into(),
                }));
            }
            if !self.import_release_permitted(record, call)? {
                return Ok(Some(Reconciled::TemporaryRetained {
                    path,
                    reason: "Git import still holds custody".into(),
                }));
            }
            let receipts = self.retry_git_cleanup(record, call)?;
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
                    return Ok(Some(Reconciled::uncertain(
                        UncertainReason::GuardianNotObservable,
                    )));
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
        let attempt = match self.attempt_dir(id, false).and_then(|attempt| {
            attempt.metadata_at(std::ffi::OsStr::new("tmp"), false)?;
            Ok(attempt)
        }) {
            Ok(attempt) => attempt,
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
        // Released refused-import scratch is recovery evidence, even for a
        // launched writer settled after reboot. It is no longer cleanup debt.
        if crate::holder::object_import_refused(&attempt, scope)?.is_some() {
            self.set_temporary_retained(id, false, call)?;
            return Ok(None);
        }
        let remove = || -> Result<()> {
            match attempt.metadata_at(std::ffi::OsStr::new("tmp"), false) {
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
            artifacts::remove_unreserved_terminal_temporary(&attempt)?;
            Ok(())
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
                    path: self
                        .context
                        .state_root
                        .as_path()
                        .join(format!("attempts/{id}/tmp")),
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
                    let recovery = self.orphan_recovery(&record, directory.try_clone()?, call)?;
                    self.finish_recovered_terminal(
                        id, &directory, &exit, recovery, providers, call,
                    )?;
                }
                let current = self.record(id, call)?;
                if !self.import_release_permitted(&current, call)? {
                    return Ok(Some(crate::settlement::settling(&directory, scope)?));
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
                        let Ok(mut recovery) = self.orphan_recovery(record, dir()?, call) else {
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
                    Ok(Observation::Live) if past_runtime_deadline(record, call) => {
                        // The guardian should have stopped the provider at the
                        // deadline itself; ask again through its own channel.
                        Ok(match custody::stop(&handle, StopReason::WallDeadline) {
                            Ok(_) => Reconciled::DeadlineStop,
                            Err(error) => Reconciled::custody_unavailable(&error),
                        })
                    }
                    Ok(Observation::Live | Observation::PublicationPending) => Ok(Reconciled::Held),
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
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn interrupted_writer_preparation_projects_typed_rejection()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let mut run = crate::tests::input();
        run.request.task.writer = Some(WriterRequest {
            extra: Default::default(),
            custody_project: Some(crate::tests::writer_project()),
            project: temp.path().join("checkout"),
            worktree: temp.path().join("checkout/.worktrees/interrupted"),
        });
        store.create(&run, &call)?;
        let id = run.attempt_id;
        let starting = store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let custody = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("interrupted");
        std::fs::create_dir_all(&custody)?;
        // Preparation reserved the writer, then its submitter died before a
        // guardian handoff. No provider preparation refusal was observed.
        drop(provenant_supervisor::fixture::acquire_writer(
            provenant_os::fs::Directory::open_anchor(&custody)?,
            RunStore::scope(&starting),
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found, vec![(id, Reconciled::Withdrawn)]);
        assert!(store.writer_fences()?.is_empty());
        let terminal = store.record(id, &call)?;
        let AttemptState::Terminal {
            terminal_cause,
            terminal_note,
            outcome,
            ..
        } = &terminal.state
        else {
            return Err("interrupted preparation did not settle".into());
        };
        assert_eq!(*outcome, Outcome::Rejected);
        assert_eq!(*terminal_cause, None);
        assert_eq!(*terminal_note, None);
        let projected =
            provenant_records::wire::lane::LaneRow::project(&terminal, Default::default());
        assert_eq!(
            projected.reason.as_deref(),
            Some(Outcome::Rejected.diagnostic_reason())
        );
        Ok(())
    }

    #[test]
    fn reconcile_journal_errors_are_reported_as_uncertain()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for failure in ["clear", "blocked", "listing"] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let registry = temp.path().join("registry");
            std::fs::create_dir(&registry)?;
            if failure == "listing" {
                std::fs::write(registry.join("worktrees"), b"not a directory")?;
            }
            let store = RunStore::open(&context, &call)?.with_writer_registry(&registry);
            let run = crate::tests::input();
            let bad = run.attempt_id;
            store.create(&run, &call)?;
            if failure == "blocked" {
                store.mark_starting(
                    &store.record(bad, &call)?,
                    &crate::tests::route(),
                    &[],
                    &Default::default(),
                    None,
                    &call,
                )?;
                crate::artifacts::seal(
                    &store.attempt_dir(bad, false)?,
                    "launch.json",
                    b"{}",
                    RECORD_LIMIT,
                )?;
            }
            // Make journal I/O fail without relying on disk capacity or permissions.
            std::fs::create_dir(
                context
                    .state_root
                    .as_path()
                    .join(format!("attempts/{bad}/settlement-progress.json")),
            )?;
            let found = store.reconcile_with_observer(
                run.attempt_id,
                &crate::dispatch_tests::Fake::default(),
                &call,
                &SystemProcessObserver,
            )?;
            assert_eq!(found.len(), 1, "{failure}: {found:?}");
            assert!(
                matches!(
                    &found[0],
                    (id, Reconciled::Uncertain {
                        reason: UncertainReason::ReconcileFailed { .. }
                    }) if *id == bad
                ),
                "{failure}: {found:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn reconcile_keeps_unrelated_orphan_fences_outside_its_attempt_scope()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let orphan = AttemptId::from_bytes([7; 16]);
        let slot = context
            .state_root
            .as_path()
            .join("worktrees")
            .join(context.project_id.to_string())
            .join("orphan");
        std::fs::create_dir_all(&slot)?;
        drop(custody::acquire_reader(
            provenant_os::fs::Directory::open_anchor(&slot)?,
            Scope {
                attempt: orphan,
                generation: 1,
            },
            call.lock_budget(),
            &cancel,
            &clock,
        )?);
        assert!(matches!(
            store.record(orphan, &call),
            Err(RunError::NotFound)
        ));
        store.attempt_dir(orphan, true)?;
        assert_eq!(store.writer_fences()?.len(), 1);
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found, vec![(run.attempt_id, Reconciled::Queued)]);
        let queued = store.queue_reconcile_with(run.attempt_id, &call, |_, _| {
            panic!("queued work and an orphan fence need no recovery worker")
        })?;
        assert_eq!(queued, vec![(run.attempt_id, Reconciled::Queued)]);
        assert_eq!(store.writer_fences()?[0].scope.attempt, orphan);
        let orphan_result = store.reconcile_with_observer(
            orphan,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert!(
            matches!(orphan_result.as_slice(), [(id, Reconciled::Uncertain { .. })] if *id == orphan)
        );
        Ok(())
    }

    #[test]
    fn unreadable_writer_listing_still_judges_known_attempts()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::DirBuilderExt;

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let registry = std::fs::canonicalize(temp.path())?.join("registry");
        // claim() also uses this as the private admission-ledger root.
        // Its mode must not depend on the test runner's umask.
        std::fs::DirBuilder::new().mode(0o700).create(&registry)?;
        std::fs::write(registry.join("worktrees"), b"not a directory")?;
        let store = RunStore::open(&crate::tests::context(temp.path())?, &call)?
            .with_writer_registry(&registry);
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found, [(run.attempt_id, Reconciled::Queued)]);
        let id = run.attempt_id;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let directory = store.attempt_dir(id, false)?;
        crate::artifacts::seal(&directory, "stdout.log", b"answer", RECORD_LIMIT)?;
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::artifacts::seal(
            &directory,
            "launch.json",
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "scope": exit.scope, "requester": store.context.actor_id,
            }))?,
            RECORD_LIMIT,
        )?;
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let found = store.reconcile_with_observer(
            run.attempt_id,
            &crate::dispatch_tests::Fake::default(),
            &call,
            &SystemProcessObserver,
        )?;
        assert_eq!(found, [(id, Reconciled::Finished)]);
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
        Ok(())
    }

    #[test]
    fn uncertain_reasons_serialise_by_kind_and_keep_their_text() {
        let retained = Reconciled::TemporaryRetained {
            path: "/tmp".into(),
            reason: TemporaryReason::new(TemporaryReasonKind::ReservationUnreadable, "detail"),
        };
        assert_eq!(serde_json::to_value(retained).unwrap()["reason"], "detail");
        assert_eq!(
            serde_json::to_value(UncertainReason::RebootProofUnavailable {
                error: "proof failed".into(),
                cause: RebootProofCause::Filesystem,
            })
            .unwrap(),
            serde_json::json!({ "kind": "reconcile_failed", "error": "proof failed" })
        );
        let reservation = std::path::PathBuf::from("/state/custody/reservation.json");
        let found = Reconciled::uncertain(UncertainReason::WriterCustodyUnconfirmed {
            reservation: reservation.clone(),
        });
        let value = serde_json::to_value(&found).unwrap();
        assert_eq!(value["state"], "uncertain");
        assert_eq!(value["reason"]["kind"], "writer_custody_unconfirmed");
        assert_eq!(
            value["reason"]["reservation"],
            "/state/custody/reservation.json"
        );
        let Reconciled::Uncertain { reason } = found else {
            unreachable!()
        };
        assert!(matches!(
            reason,
            UncertainReason::WriterCustodyUnconfirmed { .. }
        ));
        let unit = serde_json::to_value(UncertainReason::GuardianNotObservable).unwrap();
        assert_eq!(
            unit,
            serde_json::json!({ "kind": "guardian_not_observable" })
        );
        let custody = Reconciled::custody_unavailable(&"lock is held");
        assert_eq!(
            custody,
            Reconciled::Uncertain {
                reason: UncertainReason::CustodyUnavailable {
                    error: "lock is held".into()
                }
            }
        );
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn steer_replays_are_idempotent_and_natural_settlement_adopts_all_intents() -> TestResult {
        use provenant_supervisor::census::ProcessObservation;

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let mut running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;

        let first_operation = OperationId::parse("steer-first")?;
        let second_operation = OperationId::parse("steer-second")?;
        let first = store.steer(
            id,
            first_operation.clone(),
            "Inspect the failing path.",
            &call,
        )?;
        let second = store.steer(
            id,
            second_operation,
            "Prioritise the smallest repair.",
            &call,
        )?;
        let replay = store.steer(id, first_operation, "Inspect the failing path.", &call)?;
        let message_id = match first {
            SteerReceipt::Queued { message_id } => message_id,
            receipt => return Err(format!("expected initial queue, got {receipt:?}").into()),
        };
        let second_message = match second {
            SteerReceipt::Queued { message_id } => message_id,
            receipt => return Err(format!("expected queued receipt, got {receipt:?}").into()),
        };
        assert_ne!(message_id, second_message);
        assert_eq!(
            replay,
            SteerReceipt::AlreadyQueued { message_id },
            "replaying one operation returns its original message ID"
        );
        let intents = store.steer_intent(id, &call)?;
        assert_eq!(
            intents
                .iter()
                .map(|intent| intent.text.as_str())
                .collect::<Vec<_>>(),
            [
                "Inspect the failing path.",
                "Prioritise the smallest repair."
            ]
        );
        let queued_record = store.record(id, &call)?;
        let queued_status = queued_record.steering.as_ref().ok_or("steering status")?;
        assert_eq!(
            queued_status.state,
            provenant_records::run::SteeringState::Queued
        );
        assert_eq!(queued_status.operations.len(), 2);
        assert!(queued_status.successor.is_none());
        assert!(
            serde_json::to_value(&queued_record)?
                .get("steering")
                .is_none()
        );

        // The provider exits cleanly after intent persistence but before its
        // cancellation request takes effect. Settlement must still adopt it.
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let mut publication = crate::tests::publication();
        publication.session = Some("fixture-session".into());
        running = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
        assert!(matches!(
            running.state,
            AttemptState::Terminal {
                outcome: Outcome::Succeeded,
                terminal_cause: None,
                ..
            }
        ));

        let child = store
            .prepare_steer_continuation(id, &call)?
            .ok_or("natural settlement did not adopt durable steer")?;
        let replayed_child = store
            .prepare_steer_continuation(id, &call)?
            .ok_or("continuation replay lost its child")?;
        assert_eq!(child, replayed_child);
        assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
        let child_prompt = crate::artifacts::read_sealed(
            &store.attempt_dir(child, false)?,
            provenant_records::AttemptFile::Prompt.name(),
            RECORD_LIMIT,
        )?
        .ok_or("continuation prompt")?;
        let expected_prompt = format!(
            "{}Inspect the failing path.\n\nPrioritise the smallest repair.",
            STEER_PROMPT_PREFIX
        );
        assert_eq!(child_prompt, expected_prompt.as_bytes());
        let child_count: i64 = store.db.query_row(
            "SELECT COUNT(*) FROM attempts WHERE continues=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(
            child_count, 1,
            "settlement replay must not create a second child"
        );
        store.record_steer_launch_result(
            id,
            child,
            &[Started::Skipped {
                attempt: child,
                reason: "fixture skip".into(),
            }],
            &call,
        )?;
        let queued_status = store
            .record(id, &call)?
            .steering
            .ok_or("queued steering status")?;
        assert_eq!(
            queued_status.state,
            provenant_records::run::SteeringState::Queued
        );
        assert_eq!(queued_status.successor, Some(child));
        assert_eq!(store.steer_intent(id, &call)?.len(), 2);
        store.record_steer_launch_result(id, child, &[], &call)?;
        store.record_steer_launch_result(
            id,
            child,
            &[Started::Unknown {
                attempt: child,
                error: "fixture uncertain handoff".into(),
            }],
            &call,
        )?;
        assert_eq!(
            store.record(id, &call)?.steering.as_ref().unwrap().state,
            provenant_records::run::SteeringState::Queued
        );
        assert_eq!(store.prepare_steer_continuation(id, &call)?, Some(child));
        let child_count: i64 = store.db.query_row(
            "SELECT COUNT(*) FROM attempts WHERE continues=?1",
            [id.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(child_count, 1, "transient launch receipts reuse one child");
        store.record_steer_launch_result(id, child, &[Started::Launched(child)], &call)?;
        let admitted_status = store
            .record(id, &call)?
            .steering
            .ok_or("admitted steering status")?;
        assert_eq!(
            admitted_status.state,
            provenant_records::run::SteeringState::Queued,
            "launch admission does not prove that the provider resumed its session"
        );
        assert_eq!(admitted_status.successor, Some(child));
        assert_eq!(admitted_status.cause, None);

        let route = crate::tests::route();
        let queued_child = store.record(child, &call)?;
        let starting =
            store.mark_starting(&queued_child, &route, &[], &Default::default(), None, &call)?;
        let expectation = crate::ContinuationExpectation {
            session: "fixture-session".into(),
        };
        let starting = store.record_starting_launch(
            &starting,
            &route,
            Some(&expectation),
            &[],
            &Default::default(),
            None,
            0,
            &Default::default(),
            &call,
        )?;
        assert_eq!(starting.applied_route.as_ref(), Some(&route));
        let saved_expectation = store
            .continuation_expectation(child, &call)?
            .ok_or("continuation expectation missing")?;
        assert_eq!(saved_expectation.session, "fixture-session");
        let child_running = store
            .claim(child, &clock, &call)?
            .ok_or("child not admitted")?;
        let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => {
                provenant_os::capture::CaptureSourceIdentity {
                    attempt: child.to_string(),
                    generation: RunStore::scope(&child_running).generation,
                    boot: fact.identity.boot,
                    pid: fact.identity.pid,
                    native_start: fact.identity.native_start,
                }
            }
            observation => {
                return Err(format!("test process identity unavailable: {observation:?}").into());
            }
        };
        let applied = child_running
            .applied_route
            .as_ref()
            .ok_or("child applied route")?;
        assert!(store.record_provider_session_ready(
            child,
            applied,
            "fixture-session",
            &source,
            &call,
        )?);
        let resumed_status = store
            .record(id, &call)?
            .steering
            .ok_or("resumed steering status")?;
        assert_eq!(
            resumed_status.state,
            provenant_records::run::SteeringState::Resumed
        );
        assert_eq!(resumed_status.successor, Some(child));
        assert_eq!(resumed_status.cause, None);
        let retained = store.steer_ledger(id, &call)?;
        assert_eq!(retained.len(), 2);
        assert!(retained.iter().all(|(_, resolved)| *resolved));

        let mut child_exit = crate::tests::exit();
        child_exit.scope = RunStore::scope(&child_running);
        crate::dispatch_tests::seal_exit(&store, &child_exit)?;
        let mut child_publication = crate::tests::publication();
        child_publication.session = Some("fixture-session".into());
        store.publish_terminal(
            child,
            child_running.revision,
            &child_publication,
            &child_exit,
            &call,
        )?;
        let final_status = store
            .record(id, &call)?
            .steering
            .ok_or("final resumed steering status")?;
        assert_eq!(
            final_status.state,
            provenant_records::run::SteeringState::Resumed
        );
        let (live_ack, terminal_final): (i64, i64) = store.db.query_row(
            "SELECT live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((live_ack, terminal_final), (1, 1));

        store.db.execute(
            "DELETE FROM provider_session_ready WHERE attempt=?1",
            [child.to_string()],
        )?;
        store.db.execute(
            "DELETE FROM continuation_expectations WHERE attempt=?1",
            [child.to_string()],
        )?;
        assert!(
            store.continuation_expectation(child, &call).is_err(),
            "a resumed child cannot be decoded as fresh after its expectation is lost"
        );
        assert_eq!(
            store.record(id, &call)?.steering.unwrap().state,
            provenant_records::run::SteeringState::Resumed,
            "the saved disposition does not depend on retained child proof rows"
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn terminal_proof_revises_live_resume_without_losing_acknowledgement() -> TestResult {
        use provenant_supervisor::census::ProcessObservation;

        #[derive(Clone, Copy)]
        enum Case {
            Refused,
            MissingSession,
            MismatchedSession,
            LiveThenTerminalMismatch,
            LiveThenTerminalIncomplete,
            AccountMismatch,
        }
        for case in [
            Case::Refused,
            Case::MissingSession,
            Case::MismatchedSession,
            Case::LiveThenTerminalMismatch,
            Case::LiveThenTerminalIncomplete,
            Case::AccountMismatch,
        ] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = crate::tests::input();
            let parent = run.attempt_id;
            let mut route = crate::tests::route();
            route.account_key = Some("fixture-account".into());
            store.create(&run, &call)?;
            store.mark_starting(
                &store.record(parent, &call)?,
                &route,
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let parent_running = store
                .claim(parent, &clock, &call)?
                .ok_or("parent not admitted")?;
            store.steer(
                parent,
                OperationId::parse("steer-proof-boundary")?,
                "Continue from the bound provider session.",
                &call,
            )?;
            let mut parent_exit = crate::tests::exit();
            parent_exit.scope = RunStore::scope(&parent_running);
            crate::dispatch_tests::seal_exit(&store, &parent_exit)?;
            let mut parent_publication = crate::tests::publication();
            parent_publication.session = Some("fixture-session".into());
            parent_publication.resolved_route = Some(route.clone());
            parent_publication.applied_route = Some(route.clone());
            parent_publication.observed_route = Some(route.clone());
            store.publish_terminal(
                parent,
                parent_running.revision,
                &parent_publication,
                &parent_exit,
                &call,
            )?;
            let child = store
                .prepare_steer_continuation(parent, &call)?
                .ok_or("steer successor missing")?;

            if matches!(case, Case::Refused) {
                let queued = store.record(child, &call)?;
                store.withdraw(
                    child,
                    queued.state.clone(),
                    Outcome::Rejected,
                    "fixture preparation refusal",
                    WallTime::from_micros(300).unwrap(),
                    &call,
                )?;
                store.record_steer_launch_result(
                    parent,
                    child,
                    &[Started::Refused {
                        attempt: child,
                        reason: "fixture preparation refusal".into(),
                        kind: RefusalKind::Preparation,
                    }],
                    &call,
                )?;
                let status = store
                    .record(parent, &call)?
                    .steering
                    .ok_or("refused steering status")?;
                assert_eq!(
                    status.state,
                    provenant_records::run::SteeringState::NotContinued
                );
                assert_eq!(status.successor, Some(child));
                assert_eq!(
                    status.cause,
                    Some(provenant_records::run::SteeringCause::ContinuationRefused)
                );
            } else {
                let child_route = if matches!(case, Case::AccountMismatch) {
                    let mut changed = route.clone();
                    changed.account_key = Some("other-account".into());
                    changed
                } else {
                    route.clone()
                };
                let starting = store.mark_starting(
                    &store.record(child, &call)?,
                    &child_route,
                    &[],
                    &Default::default(),
                    None,
                    &call,
                )?;
                let expectation = crate::ContinuationExpectation {
                    session: "fixture-session".into(),
                };
                let launch = store.record_starting_launch(
                    &starting,
                    &child_route,
                    Some(&expectation),
                    &[],
                    &Default::default(),
                    None,
                    0,
                    &Default::default(),
                    &call,
                );
                if matches!(case, Case::AccountMismatch) {
                    assert!(matches!(launch, Err(RunError::SteerSessionMismatch)));
                    let status = store
                        .record(parent, &call)?
                        .steering
                        .ok_or("account-mismatch steering status")?;
                    assert_eq!(status.state, provenant_records::run::SteeringState::Queued);
                    assert_eq!(status.successor, Some(child));
                    assert_eq!(status.cause, None);
                    assert!(store.continuation_expectation(child, &call).is_err());
                    continue;
                }
                let starting = launch?;
                let child_running = store
                    .claim(child, &clock, &call)?
                    .ok_or("child not admitted")?;
                store.record_steer_launch_result(
                    parent,
                    child,
                    &[Started::Launched(child)],
                    &call,
                )?;
                assert_eq!(
                    store.record(parent, &call)?.steering.unwrap().state,
                    provenant_records::run::SteeringState::Queued
                );
                let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                    ProcessObservation::Present(fact) if !fact.zombie => {
                        provenant_os::capture::CaptureSourceIdentity {
                            attempt: child.to_string(),
                            generation: RunStore::scope(&child_running).generation,
                            boot: fact.identity.boot,
                            pid: fact.identity.pid,
                            native_start: fact.identity.native_start,
                        }
                    }
                    observation => {
                        return Err(
                            format!("test process identity unavailable: {observation:?}").into(),
                        );
                    }
                };
                let applied = child_running
                    .applied_route
                    .as_ref()
                    .ok_or("child applied route")?;
                let live_ack = matches!(
                    case,
                    Case::LiveThenTerminalMismatch | Case::LiveThenTerminalIncomplete
                );
                if live_ack {
                    assert!(store.record_provider_session_ready(
                        child,
                        applied,
                        "fixture-session",
                        &source,
                        &call,
                    )?);
                    assert_eq!(
                        store.record(parent, &call)?.steering.unwrap().state,
                        provenant_records::run::SteeringState::Resumed,
                        "a verified live session acknowledgement is reported promptly"
                    );
                    let (saved_live_ack, saved_terminal_final): (i64, i64) = store.db.query_row(
                        "SELECT live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
                        [parent.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    assert_eq!((saved_live_ack, saved_terminal_final), (1, 0));
                }
                let mut exit = crate::tests::exit();
                exit.scope = RunStore::scope(&child_running);
                let mut publication = crate::tests::publication();
                publication.resolved_route = Some(child_route.clone());
                publication.applied_route = Some(child_route.clone());
                publication.observed_route = Some(child_route.clone());
                let (expected_state, expected_cause) = match case {
                    Case::MissingSession => {
                        exit.stop_reason = Some(StopReason::Steered);
                        exit.cancel_delivered_before_exit = Some(true);
                        exit.provider_identity_eof_fragment = true;
                        publication.outcome = Outcome::Cancelled;
                        publication.terminal_cause = Some(TerminalCause::Steered);
                        publication.result = "continued attempt stopped before session proof";
                        publication.session = None;
                        (
                            provenant_records::run::SteeringState::NotContinued,
                            provenant_records::run::SteeringCause::SessionProofIncomplete,
                        )
                    }
                    Case::MismatchedSession => {
                        exit.provider_identity_conflict = true;
                        publication.outcome = Outcome::Failed;
                        publication.terminal_cause = Some(TerminalCause::IdentityMismatch);
                        publication.result = "provider session did not match its continuation";
                        publication.complete = false;
                        publication.session = Some("different-session".into());
                        (
                            provenant_records::run::SteeringState::NotContinued,
                            provenant_records::run::SteeringCause::SessionMismatch,
                        )
                    }
                    Case::LiveThenTerminalMismatch => {
                        exit.provider_identity_conflict = true;
                        publication.outcome = Outcome::Failed;
                        publication.terminal_cause = Some(TerminalCause::IdentityMismatch);
                        publication.result = "terminal session contradicted live acknowledgement";
                        publication.complete = false;
                        publication.session = Some("different-session".into());
                        (
                            provenant_records::run::SteeringState::NotContinued,
                            provenant_records::run::SteeringCause::SessionMismatch,
                        )
                    }
                    Case::LiveThenTerminalIncomplete => {
                        exit.provider_identity_evidence_incomplete = true;
                        publication.outcome = Outcome::Unsettled;
                        publication.reason =
                            Some("terminal provider evidence was incomplete".into());
                        publication.result = "terminal provider evidence was incomplete";
                        publication.complete = false;
                        publication.session = None;
                        (
                            provenant_records::run::SteeringState::NotContinued,
                            provenant_records::run::SteeringCause::SessionProofIncomplete,
                        )
                    }
                    Case::AccountMismatch => unreachable!(),
                    Case::Refused => unreachable!(),
                };
                crate::dispatch_tests::seal_exit(&store, &exit)?;
                store.publish_terminal(
                    child,
                    child_running.revision,
                    &publication,
                    &exit,
                    &call,
                )?;
                let status = store
                    .record(parent, &call)?
                    .steering
                    .ok_or("terminal steering status")?;
                assert_eq!(status.state, expected_state);
                assert_eq!(status.successor, Some(child));
                assert_eq!(status.cause, Some(expected_cause));
                let (saved_live_ack, saved_terminal_final): (i64, i64) = store.db.query_row(
                    "SELECT live_ack,terminal_final FROM steer_continuations WHERE attempt=?1",
                    [parent.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(saved_live_ack, if live_ack { 1_i64 } else { 0_i64 });
                assert_eq!(saved_terminal_final, 1);
                assert_eq!(store.prepare_steer_continuation(parent, &call)?, None);
                assert!(
                    store
                        .steer_ledger(parent, &call)?
                        .iter()
                        .any(|(intent, resolved)| intent.text
                            == "Continue from the bound provider session."
                            && *resolved)
                );
                assert!(starting.applied_route.is_some());
            }
        }
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn sealed_live_identity_conflict_fences_a_queued_successor() -> TestResult {
        for (conflict, incomplete, outcome, cause) in [
            (
                true,
                false,
                Outcome::Failed,
                Some(TerminalCause::IdentityMismatch),
            ),
            (false, true, Outcome::Unsettled, None),
        ] {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = crate::tests::input();
            let id = run.attempt_id;
            store.create(&run, &call)?;
            store.mark_starting(
                &store.record(id, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let operation = OperationId::parse("steer-identity-conflict")?;
            store.steer(id, operation, "Keep the existing session.", &call)?;

            let attempt = store.attempt_dir(id, false)?;
            crate::artifacts::seal(
                &attempt,
                "stdout.log",
                b"apparently successful",
                CAPTURE_LIMIT,
            )?;
            let mut exit = crate::tests::exit();
            exit.scope = RunStore::scope(&running);
            exit.provider_identity_conflict = conflict;
            exit.provider_identity_evidence_incomplete = incomplete;
            crate::dispatch_tests::seal_exit(&store, &exit)?;
            let fake = crate::dispatch_tests::Fake::with_observed(
                Some(crate::tests::route()),
                Some(Outcome::Succeeded),
            );
            let finished = store.finish(id, &attempt, &exit, &fake, &call)?;
            assert!(matches!(
                finished.state,
                AttemptState::Terminal {
                    outcome: actual_outcome,
                    terminal_cause: actual_cause,
                    ..
                } if actual_outcome == outcome && actual_cause == cause
            ));
            assert_eq!(store.prepare_steer_continuation(id, &call)?, None);
            assert_eq!(store.record(id, &call)?.superseded_by, None);
            let ledger = store.steer_ledger(id, &call)?;
            assert_eq!(ledger.len(), 1);
            assert_eq!(ledger[0].0.text.as_str(), "Keep the existing session.");
            assert!(ledger[0].1, "a non-continuable stop resolves the intent");
            assert_eq!(
                store
                    .steering_status(id, &call)?
                    .ok_or("steering status missing")?
                    .state,
                provenant_records::run::SteeringState::NotContinued
            );
            assert_eq!(
                store
                    .steering_status(id, &call)?
                    .ok_or("steering status missing")?
                    .cause,
                Some(provenant_records::run::SteeringCause::TerminalStop)
            );
        }
        Ok(())
    }

    fn seal_provider_identity_for_exit(
        attempt: &provenant_os::fs::Directory,
        scope: Scope,
        source: &provenant_os::capture::CaptureSourceIdentity,
    ) -> TestResult {
        let metadata = attempt.metadata()?;
        let identity = serde_json::json!({
            "boot": source.boot.clone(),
            "pid": source.pid,
            "native_start": source.native_start,
            "group_id": source.pid,
            "session_id": source.pid,
        });
        let reservation = serde_json::json!({
            "schema_version": 1,
            "scope": scope,
            "launch_state": "spawned",
            "attempt_device": metadata.device,
            "attempt_inode": metadata.inode,
            "reservation_device": metadata.device,
            "reservation_inode": metadata.inode,
            "binding": { "kind": "read_only" },
            "boot": source.boot.clone(),
            "guardian": identity.clone(),
            "provider": identity,
            "unresolved_cleanup": false,
            "descendants": [],
            "census_incomplete": false,
            "omitted_descendants": 0,
        });
        attempt
            .atomic_write(
                std::ffi::OsStr::new("reservation.json"),
                ".provider-identity-",
                &serde_json::to_vec(&reservation)?,
                provenant_os::fs::PublishMode::NoReplace,
            )?
            .require_durable()?;
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn eof_fragment_only_continues_after_a_confirmed_intentional_stop() -> TestResult {
        use provenant_supervisor::census::ProcessObservation;

        let cases = [
            (
                Some(StopReason::Steered),
                Some(true),
                false,
                false,
                Outcome::Cancelled,
                Some(TerminalCause::Steered),
                true,
            ),
            (
                Some(StopReason::ExplicitCancellation),
                Some(true),
                false,
                false,
                Outcome::Cancelled,
                None,
                false,
            ),
            (
                None,
                Some(false),
                false,
                false,
                Outcome::Unsettled,
                None,
                false,
            ),
            (
                Some(StopReason::Steered),
                Some(false),
                false,
                false,
                Outcome::Unsettled,
                None,
                false,
            ),
            (
                Some(StopReason::Steered),
                Some(true),
                false,
                true,
                Outcome::Unsettled,
                None,
                false,
            ),
            (
                Some(StopReason::Steered),
                Some(true),
                true,
                false,
                Outcome::Failed,
                Some(TerminalCause::IdentityMismatch),
                false,
            ),
        ];
        for (index, (reason, delivered, conflict, incomplete, outcome, cause, resumes)) in
            cases.into_iter().enumerate()
        {
            let temp = tempfile::tempdir()?;
            let clock = crate::tests::clock();
            let cancel = provenant_core::CancelToken::new();
            let call = crate::tests::call(&clock, &cancel);
            let context = crate::tests::context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = crate::tests::input();
            let id = run.attempt_id;
            store.create(&run, &call)?;
            store.mark_starting(
                &store.record(id, &call)?,
                &crate::tests::route(),
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
            let operation = OperationId::parse(&format!("steer-eof-{index}"))?;
            assert!(matches!(
                store.steer(id, operation, "Continue from the validated session.", &call)?,
                SteerReceipt::Queued { .. }
            ));
            let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
                ProcessObservation::Present(fact) if !fact.zombie => {
                    provenant_os::capture::CaptureSourceIdentity {
                        attempt: id.to_string(),
                        generation: RunStore::scope(&running).generation,
                        boot: fact.identity.boot,
                        pid: fact.identity.pid,
                        native_start: fact.identity.native_start,
                    }
                }
                observation => {
                    return Err(
                        format!("test process identity unavailable: {observation:?}").into(),
                    );
                }
            };
            let route = running
                .applied_route
                .as_ref()
                .ok_or("applied route")?
                .clone();
            assert!(store.record_provider_session_ready(
                id,
                &route,
                "live-session",
                &source,
                &call,
            )?);
            if conflict {
                assert!(store.record_provider_identity_conflict(id, &route, &source, &call)?);
            }

            let attempt = store.attempt_dir(id, false)?;
            seal_provider_identity_for_exit(&attempt, RunStore::scope(&running), &source)?;
            crate::artifacts::seal(
                &attempt,
                "stdout.log",
                b"{\"event\":\"init\",\"conversation_id\":\"live-session\",\"init\":{\"model\":\"expected-model\"}}\n{\"event\":",
                CAPTURE_LIMIT,
            )?;
            let mut exit = crate::tests::exit();
            exit.scope = RunStore::scope(&running);
            exit.stop_reason = reason;
            exit.cancel_delivered_before_exit = delivered;
            exit.provider_identity_conflict = conflict;
            exit.provider_identity_evidence_incomplete = incomplete;
            exit.provider_identity_eof_fragment = true;
            exit.terminal_observed = true;
            exit.settlement_evidence = custody::SettlementEvidence::DarwinCensusAndCustodyLease;
            crate::dispatch_tests::seal_exit(&store, &exit)?;
            let mut fake =
                crate::dispatch_tests::Fake::with_observed(Some(route), Some(Outcome::Succeeded));
            fake.session = Some("live-session".into());
            let finished = store.finish(id, &attempt, &exit, &fake, &call)?;
            assert!(matches!(
                finished.state,
                AttemptState::Terminal {
                    outcome: actual_outcome,
                    terminal_cause: actual_cause,
                    ..
                } if actual_outcome == outcome && actual_cause == cause
            ));

            let child = store.prepare_steer_continuation(id, &call)?;
            assert_eq!(child.is_some(), resumes);
            if let Some(child) = child {
                assert_eq!(store.record(id, &call)?.superseded_by, Some(child));
                assert_eq!(
                    store
                        .steering_status(id, &call)?
                        .ok_or("steering status")?
                        .state,
                    provenant_records::run::SteeringState::Resumed
                );
            } else {
                assert_eq!(store.record(id, &call)?.superseded_by, None);
            }
        }
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn steer_session_readiness_binds_scope_process_and_immutable_route_identity() -> TestResult {
        use provenant_supervisor::census::ProcessObservation;

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => {
                provenant_os::capture::CaptureSourceIdentity {
                    attempt: id.to_string(),
                    generation: RunStore::scope(&running).generation,
                    boot: fact.identity.boot,
                    pid: fact.identity.pid,
                    native_start: fact.identity.native_start,
                }
            }
            observation => {
                return Err(format!("test process identity unavailable: {observation:?}").into());
            }
        };
        let route = running
            .applied_route
            .as_ref()
            .ok_or("applied route")?
            .clone();
        assert!(store.record_provider_session_ready(
            id,
            &route,
            "fixture-live-session",
            &source,
            &call,
        )?);
        let mut warning_only_change = route.clone();
        warning_only_change.availability_warning = Some("later warning".into());
        assert!(store.record_provider_session_ready(
            id,
            &warning_only_change,
            "fixture-live-session",
            &source,
            &call,
        )?);
        let ready = store
            .steer_session_ready(id, &call)?
            .ok_or("ready session missing")?;
        assert_eq!(ready.session.as_deref(), Some("fixture-live-session"));
        assert_eq!(ready.generation, source.generation);
        assert!(!ready.conflict);
        assert!(!ready.terminal_confirmed);
        assert!(store.record_provider_session_ready(
            id,
            &route,
            "different-live-session",
            &source,
            &call,
        )?);
        assert!(
            store
                .steer_session_ready(id, &call)?
                .is_some_and(|ready| ready.conflict)
        );

        let mut other_account = route.clone();
        other_account.account_key = Some("different-account".into());
        assert!(!store.record_provider_session_ready(
            id,
            &other_account,
            "fixture-live-session",
            &source,
            &call,
        )?);
        let mut other_process = source.clone();
        other_process.native_start = other_process.native_start.saturating_add(1);
        assert!(!store.record_provider_session_ready(
            id,
            &route,
            "fixture-live-session",
            &other_process,
            &call,
        )?);
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn steer_session_disagreement_keeps_accepted_text_and_refuses_fresh_fallback() -> TestResult {
        use provenant_supervisor::census::ProcessObservation;

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let mut running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let operation = OperationId::parse("steer-session-disagreement")?;
        let accepted = store.steer(id, operation, "Keep this input.", &call)?;
        assert!(matches!(accepted, SteerReceipt::Queued { .. }));
        let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => {
                provenant_os::capture::CaptureSourceIdentity {
                    attempt: id.to_string(),
                    generation: RunStore::scope(&running).generation,
                    boot: fact.identity.boot,
                    pid: fact.identity.pid,
                    native_start: fact.identity.native_start,
                }
            }
            observation => {
                return Err(format!("test process identity unavailable: {observation:?}").into());
            }
        };
        let route = running
            .applied_route
            .as_ref()
            .ok_or("applied route")?
            .clone();
        assert!(store.record_provider_session_ready(id, &route, "live-session", &source, &call,)?);
        assert!(store.record_provider_identity_conflict(id, &route, &source, &call)?);
        let terminal_provider = provenant_supervisor::census::ProcessIdentity {
            boot: source.boot.clone(),
            pid: source.pid,
            native_start: source.native_start,
            group_id: source.pid,
            session_id: source.pid,
        };
        store.confirm_steer_terminal_process(
            id,
            RunStore::scope(&running),
            Some(&terminal_provider),
            &call,
        )?;
        assert!(
            store
                .steer_session_ready(id, &call)?
                .is_some_and(|ready| { ready.conflict && ready.terminal_confirmed })
        );
        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let mut publication = crate::tests::publication();
        publication.session = Some("live-session".into());
        running = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
        assert!(matches!(running.state, AttemptState::Terminal { .. }));
        assert!(matches!(
            store.prepare_steer_continuation(id, &call),
            Err(RunError::SteerSessionMismatch)
        ));
        store.record_steer_not_continued(
            id,
            provenant_records::run::SteeringCause::SessionMismatch,
            &call,
        )?;
        let ledger = store.steer_ledger(id, &call)?;
        assert_eq!(ledger[0].0.text, "Keep this input.");
        assert!(
            ledger[0].1,
            "the original private text remains durably stored"
        );
        let status = store.record(id, &call)?.steering.ok_or("steering status")?;
        assert_eq!(
            status.state,
            provenant_records::run::SteeringState::NotContinued
        );
        assert_eq!(
            status.cause,
            Some(provenant_records::run::SteeringCause::SessionMismatch)
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn first_frame_identity_mismatch_without_session_event_fences_natural_steer() -> TestResult {
        use provenant_supervisor::census::ProcessObservation;

        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        let mut running = store.claim(id, &clock, &call)?.ok_or("not admitted")?;
        let accepted = store.steer(
            id,
            OperationId::parse("steer-first-frame-mismatch")?,
            "Keep the accepted work, then verify its provider identity.",
            &call,
        )?;
        assert!(matches!(accepted, SteerReceipt::Queued { .. }));
        let source = match SystemProcessObserver.observe(i64::from(std::process::id())) {
            ProcessObservation::Present(fact) if !fact.zombie => {
                provenant_os::capture::CaptureSourceIdentity {
                    attempt: id.to_string(),
                    generation: RunStore::scope(&running).generation,
                    boot: fact.identity.boot,
                    pid: fact.identity.pid,
                    native_start: fact.identity.native_start,
                }
            }
            observation => {
                return Err(format!("test process identity unavailable: {observation:?}").into());
            }
        };
        let route = running
            .applied_route
            .as_ref()
            .ok_or("applied route")?
            .clone();
        // Agy may reject its first init frame on Model after decoding Session;
        // the adapter then returns neither event. The host still records a
        // route/process-bound fence for that exact admitted candidate.
        assert!(store.record_provider_identity_conflict(id, &route, &source, &call)?);
        let ready = store
            .steer_session_ready(id, &call)?
            .ok_or("identity conflict row missing")?;
        assert!(ready.conflict);
        assert!(ready.session.is_none());
        assert!(!ready.terminal_confirmed);

        let mut exit = crate::tests::exit();
        exit.scope = RunStore::scope(&running);
        crate::dispatch_tests::seal_exit(&store, &exit)?;
        let mut publication = crate::tests::publication();
        publication.session = Some("session-from-final-decoder".into());
        running = store.publish_terminal(id, running.revision, &publication, &exit, &call)?;
        assert!(matches!(running.state, AttemptState::Terminal { .. }));
        assert!(matches!(
            store.prepare_steer_continuation(id, &call),
            Err(RunError::SteerSessionMismatch)
        ));
        assert_eq!(
            store.steer_intent(id, &call)?[0].text,
            "Keep the accepted work, then verify its provider identity."
        );
        Ok(())
    }

    // Spec: docs/specs/fabric.md#session-ready-steering
    #[test]
    fn steer_stop_conflict_preserves_each_durable_intent_and_receipt() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        store.mark_starting(
            &store.record(id, &call)?,
            &crate::tests::route(),
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.claim(id, &clock, &call)?.ok_or("not admitted")?;

        let accepted_operation = OperationId::parse("steer-accepted")?;
        let accepted = store.steer(
            id,
            accepted_operation.clone(),
            "Keep this instruction.",
            &call,
        )?;
        let accepted_message = match accepted {
            SteerReceipt::Queued { message_id } => message_id,
            receipt => return Err(format!("expected accepted receipt, got {receipt:?}").into()),
        };
        assert!(matches!(
            store.cancel(id, "owner cancellation", &call)?,
            CancelReceipt::Requested
        ));
        assert_eq!(
            store.steer(id, accepted_operation, "Keep this instruction.", &call,)?,
            SteerReceipt::AlreadyQueued {
                message_id: accepted_message
            }
        );
        let later_message = match store.steer(
            id,
            OperationId::parse("steer-after-stop")?,
            "Keep this second instruction too.",
            &call,
        )? {
            SteerReceipt::Queued { message_id } => message_id,
            receipt => return Err(format!("expected durable queue, got {receipt:?}").into()),
        };
        assert_ne!(later_message, accepted_message);
        let intents = store.steer_intent(id, &call)?;
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].text, "Keep this instruction.");
        assert_eq!(intents[1].text, "Keep this second instruction too.");
        Ok(())
    }
}
