//! Slot records and S0 execution stub. S1 owns slot materialisation.

use super::{Captured, Exception, ExecPlace, ExitStatus, Job, JobOutcome, RunContext, SlotChanges};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted slot record, schema 1.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRecord {
    pub schema: u32,
    pub project_key: String,
    pub index: u64,
    pub created_unix_ms: u64,
    pub last_job: Option<SlotJobMark>,
    pub prepare_digest: Option<String>,
    pub health: SlotHealth,
    pub last_canary_unix_ms: Option<u64>,
}

/// Last completed job associated with a slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotJobMark {
    pub snapshot: String,
    pub base: Option<String>,
    pub owner: String,
    pub finished_unix_ms: u64,
}

/// Slot health and quarantine history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotHealth {
    pub infra_failures_unix_ms: Vec<u64>,
    pub state: SlotState,
    pub quarantine_reason: Option<String>,
}

/// Slot health state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    #[default]
    Ok,
    Quarantined,
}

/// Paths inside one project's slot cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotPaths {
    pub root: PathBuf,
    pub repo: PathBuf,
    pub target: PathBuf,
    pub tmp: PathBuf,
    pub record: PathBuf,
    pub job: PathBuf,
    pub lock: PathBuf,
}

impl SlotPaths {
    /// Resolve all files belonging to one cache slot.
    pub fn new(home: &std::path::Path, project_key: &str, index: u64) -> Self {
        let root = super::paths::slot_root(home, project_key, index);
        Self {
            repo: root.join("repo"),
            target: root.join("target"),
            tmp: root.join("tmp"),
            record: root.join("slot.json"),
            job: root.join("job.json"),
            lock: root.join("lock"),
            root,
        }
    }
}

/// Successful slot execution. S1 fills these values after snapshot and sync.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlotExecution {
    pub snapshot: Option<String>,
    pub prev_snapshot: Option<String>,
    pub slot_changes: Option<SlotChanges>,
    pub exit: Option<ExitStatus>,
    pub step_exits: Vec<Option<ExitStatus>>,
    pub captured: Option<Captured>,
}

/// Execute a job in the selected slot. S0 falls back locally until S1 lands.
pub fn execute(_job: &Job, _context: &RunContext, _index: u64) -> Result<SlotExecution, Exception> {
    Err(Exception::PoolUnavailable)
}

/// Slot/local placement convenience for the ledger.
pub fn local_outcome(outcome: &mut JobOutcome) {
    outcome.exec = ExecPlace::Local;
}
