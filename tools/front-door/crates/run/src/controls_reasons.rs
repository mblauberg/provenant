use super::*;

/// The identity of a stored attempt record that no longer decodes. This is
/// evidence for a host-side typed warning, never a substitution for the bytes:
/// the bytes stay in the run database and a copy is sealed under `quarantine/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnreadableSource {
    pub attempt: AttemptId,
    pub path: std::path::PathBuf,
    pub schema_version: Option<u64>,
    pub error: String,
}

impl std::fmt::Display for UnreadableSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} attempt {}", self.path.display(), self.attempt)?;
        if let Some(version) = self.schema_version {
            write!(f, " (schema version {version})")?;
        }
        write!(f, ": {}", self.error)
    }
}

/// A decode failure plus the original bytes, held together so recovery can
/// preserve the record before it releases the lease it fenced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadableAttempt {
    pub source: UnreadableSource,
    pub(crate) raw: Vec<u8>,
}

/// The one typed warning for an undecodable attempt record. Its subject carries
/// the source path, schema and error; admission refusals stay fixed text.
pub fn unreadable_record_warning(source: &UnreadableSource, released: bool) -> Warning {
    use provenant_records::warning::{
        UNREADABLE_RECORD_HELD, UNREADABLE_RECORD_RELEASED, detected_source,
    };
    let code = if released {
        UNREADABLE_RECORD_RELEASED
    } else {
        UNREADABLE_RECORD_HELD
    };
    Warning::new(
        code,
        format!(
            "stored record for attempt {} could not be decoded; {}",
            source.attempt,
            if released {
                "its writer lease was released after liveness was disproved and the bytes were preserved"
            } else {
                "its writer lease is retained because liveness could not be disproved"
            }
        ),
        detected_source(code),
    )
    .subject(source.to_string())
    .repair("inspect the preserved copy under quarantine/, then rerun reconcile")
}

pub(super) fn attributed_orphan_warning(message: String, attempt: impl ToString) -> Warning {
    let code = warning::ORPHAN_PROCESS_ATTRIBUTED;
    Warning::new(code, message, warning::detected_source(code))
        .subject(attempt.to_string())
        .once_per_change()
}

pub(super) fn reservation_issue_blocks_report(
    reservation: &std::path::Path,
    issue: &std::path::Path,
) -> bool {
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
            Self::UnreadableReleased { source, locks } => write!(
                f,
                "unreadable_record_released {source}; writer leases {}",
                locks
                    .iter()
                    .map(|lock| lock.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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
    pub(super) fn new(kind: TemporaryReasonKind, text: impl Into<String>) -> Self {
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
    /// The record is unreadable and no liveness disproof was possible, so the
    /// writer lease stays fenced and the evidence is retained.
    UnreadableLivenessUnknown { source: UnreadableSource },
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
    pub(super) fn from_error(error: &RunError) -> Self {
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
            Self::UnreadableLivenessUnknown { source } => {
                write!(
                    f,
                    "unreadable attempt record still fenced: {source}; liveness not disproved"
                )
            }
        }
    }
}
