//! Shared graph state vocabulary used by runtime records and peer projections.
use crate::run::CauseKind;
use serde::{Deserialize, Serialize};

/// Stable identity for one graph route attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptKey {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_attempt_ref: Option<String>,
    pub graph_id: String,
    pub node_id: String,
    /// One-based WorkKey attempt identity in the journal's project namespace.
    /// A continuation advances this number without consuming another route.
    pub attempt: u32,
    /// Zero-based position in that node's ordered route list.
    pub route_index: u16,
}

impl AttemptKey {
    pub fn work_key(
        &self,
        project: provenant_core::ProjectId,
    ) -> Result<crate::WorkKey, crate::work_key::WorkKeyError> {
        crate::WorkKey::new(project, &self.graph_id, &self.node_id, self.attempt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Blocked,
    Skipped,
}

impl NodeState {
    pub fn settled(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Starting,
    Running,
    Unresolved,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphAttemptOutcome {
    Succeeded,
    TaskFailed,
    /// A settled provider stall explicitly requested the next route.
    ProviderStalledRetry,
    DeclaredStop,
    StoppedNoWork,
    OtherFailure,
    ReviewRequired,
    AuthRequired,
    UsageLimited,
    AdapterUnavailable,
    PreparationRefused(CauseKind),
    DeadlineExceeded,
    Unclassified,
}

impl GraphAttemptOutcome {
    /// The advance code settlement owns, if this outcome has one. The two
    /// declared-stop outcomes are the only settlement-derived codes; every
    /// other advance code comes from the owner or a launch refusal. Keeping the
    /// spellings here gives the graph one source for them (docs/specs/run.md
    /// lists the same two codes).
    pub const fn advance_code(self) -> Option<&'static str> {
        match self {
            Self::DeclaredStop => Some("declared_stop"),
            Self::StoppedNoWork => Some("stopped_no_work"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOutcome {
    Succeeded,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Pending,
    Complete,
    Cancelled,
    Paused,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn advance_codes_are_the_documented_settlement_codes() {
        let spec = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../docs/specs/run.md"),
        )
        .unwrap();
        let list = spec
            .split_once("The settlement-derived advance codes are ")
            .unwrap()
            .1
            .lines()
            .next()
            .unwrap();
        let documented: BTreeSet<&str> = list
            .split_whitespace()
            .filter_map(|word| word.strip_prefix('`').and_then(|w| w.strip_suffix('`')))
            .collect();
        let owned: BTreeSet<&str> = [
            GraphAttemptOutcome::DeclaredStop,
            GraphAttemptOutcome::StoppedNoWork,
        ]
        .into_iter()
        .filter_map(GraphAttemptOutcome::advance_code)
        .collect();
        assert_eq!(documented, owned);
        assert_eq!(GraphAttemptOutcome::Succeeded.advance_code(), None);
    }
}
