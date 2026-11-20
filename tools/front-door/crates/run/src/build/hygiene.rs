//! Slot reset and maintenance seam; S4 supplies reset and garbage collection.

use super::{
    BuildError, JobOutcome,
    slots::{SlotPaths, SlotRecord},
};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::SystemTime};

/// Pre-job action selected by the clock guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreJob {
    Proceed,
    LightReset,
    FullReset,
}

/// Clock guard stub. S4 implements rollback detection.
pub fn before_job(_record: &mut SlotRecord, _now_unix_ms: u64) -> PreJob {
    PreJob::Proceed
}

/// Reset ladder stub. S4 implements the infrastructure-failure ladder.
pub fn after_job(
    _record: &mut SlotRecord,
    _outcome: &JobOutcome,
    _now_unix_ms: u64,
) -> super::Reset {
    super::Reset::None
}

/// Remove untracked and ignored changes from a slot checkout. S0 is a no-op.
pub fn light_reset(_slot: &SlotPaths) -> Result<(), BuildError> {
    Ok(())
}

/// Recreate the cache and target for a slot. S0 is a no-op.
pub fn full_reset(_slot: &SlotPaths) -> Result<(), BuildError> {
    Ok(())
}

/// Result of slot and store garbage collection.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub removed_slots: u64,
    pub removed_bytes: u64,
    pub warnings: Vec<String>,
}

/// Remove stale unlocked slot state. S0 returns an empty report.
pub fn gc(_home: &Path, _state: &Path, _limit: u64, _now: SystemTime) -> GcReport {
    GcReport::default()
}

/// One observed slot row for the status command.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotStatus {
    pub project_key: String,
    pub index: u64,
    pub allocated_bytes: u64,
    pub health: String,
    pub last_job: Option<super::slots::SlotJobMark>,
}

/// Read slot records and allocated sizes. S0 returns no rows.
pub fn status(_home: &Path, _state: &Path) -> Vec<SlotStatus> {
    Vec::new()
}

/// Result of a maintenance/prewarm request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainOutcome {
    pub ran: bool,
    pub warning: Option<String>,
}

/// Run one maintenance job when policy allows it. S0 does no work.
pub fn maintain(_state: &Path, _home: &Path, _project: &Path, _now: SystemTime) -> MaintainOutcome {
    MaintainOutcome::default()
}
