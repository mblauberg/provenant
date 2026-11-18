//! Shared, versioned lane interfaces. Admission and projection owners apply these contracts.
use std::collections::{BTreeMap, BTreeSet};

use provenant_core::{AttemptId, ProjectId};
use serde::{Deserialize, Serialize};

use crate::{
    ExtraFields, LimitSource, PathWire, RecordHeader, WorkKey,
    handoff::{Handoff, WriteScopeReport},
};

/// Resolved restrictions within the parent's authority. Paths are relative to
/// the same host-verified root; admission must resolve links and other boundary
/// authority independently before using this comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarrowedAuthority {
    pub routes: BTreeSet<String>,
    /// Each budget is a ceiling; an absent key is unbounded.
    pub budgets: BTreeMap<String, u64>,
    pub paths: Vec<PathWire>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl NarrowedAuthority {
    /// Compare resolved restrictions. This does not grant authority or replace
    /// host validation of roots, links, credentials and inherited boundaries.
    pub fn is_within(&self, parent: &Self) -> bool {
        self.extra.is_empty()
            && parent.extra.is_empty()
            && self.routes.is_subset(&parent.routes)
            && parent
                .budgets
                .iter()
                .all(|(key, ceiling)| self.budgets.get(key).is_some_and(|value| value <= ceiling))
            && self.paths.iter().all(|path| {
                let Ok(path) = path.to_relative_path() else {
                    return false;
                };
                parent.paths.iter().any(|root| {
                    root.to_relative_path()
                        .is_ok_and(|root| path.as_path().starts_with(root.as_path()))
                })
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    ParentWorktree,
    ManagedWorktree { branch: BranchChoice },
    ParentOwnedPath { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchChoice {
    New { name: String },
    Existing { name: String },
}

/// Resolved configuration always records its source, including the off default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: LimitSource,
}
impl<T: Default> Default for Sourced<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            source: LimitSource::Default,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitChoice {
    OwnPaths,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildLaunchRequest {
    pub parent_attempt: AttemptId,
    pub authority: NarrowedAuthority,
    pub destination: Destination,
    #[serde(default)]
    pub path_claim: Sourced<Option<Vec<PathWire>>>,
    #[serde(default)]
    pub commit: Sourced<Option<CommitChoice>>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// A cursor is scoped to a project stream. It is the store's sequence, not a
/// wall clock or an offset into one of the source journals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCursor {
    pub project: ProjectId,
    pub seq: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    LaneLifecycle,
    Note,
    CheckRun,
    CheckSuite,
    WorkflowRun,
    PullRequest,
    IssueComment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationLineage {
    pub attempt: Option<AttemptId>,
    pub parent_attempt: Option<AttemptId>,
    pub work: Option<WorkKey>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// Identity is stable across redelivery; cursor orders events within a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationEvent {
    pub header: RecordHeader,
    pub id: String,
    pub cursor: ObservationCursor,
    pub lineage: ObservationLineage,
    pub kind: ObservationKind,
    pub coverage: Coverage,
    /// Source-specific, normalised data (forge repository, PR, head and check
    /// identifiers where available); unknown values remain absent or null.
    pub detail: serde_json::Value,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed<T> {
    pub source: String,
    pub completeness: Coverage,
    /// None is unknown, never an assertion that nothing occurred.
    pub value: Option<T>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub command: Option<String>,
    pub exit_code: Option<i64>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFacts {
    pub base: Option<String>,
    pub head: Option<String>,
    pub commits: Vec<String>,
    pub diffstat: Option<String>,
    /// In a shared worktree this describes the worktree, not child authorship.
    pub shared_worktree: bool,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    pub commands: Observed<Vec<Observed<CommandReceipt>>>,
    pub git: Observed<GitFacts>,
    pub ci: Observed<serde_json::Value>,
    pub scope: Observed<WriteScopeReport>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneClaims {
    pub handoff: Option<Handoff>,
    /// Explicitly labelled claims; absent handoff is not an empty verification.
    pub verify: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// The run owner constructs and bounds this projection before publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultProjection {
    pub header: RecordHeader,
    pub attempt: AttemptId,
    pub host_facts: HostFacts,
    pub lane_claims: LaneClaims,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// Maximum encoded projection size, shared with existing run result readers.
pub const RESULT_PROJECTION_MAX_BYTES: u64 = crate::run::RESULT_LIMIT;

impl ResultProjection {
    /// Decode untrusted projection bytes with the standard bounded record codec.
    pub fn decode(reader: impl std::io::Read) -> Result<Self, crate::DecodeError> {
        crate::decode(
            reader,
            crate::Limits {
                max_bytes: RESULT_PROJECTION_MAX_BYTES,
                ..crate::Limits::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::RelativePath;
    use serde_json::json;

    fn path(value: &str) -> PathWire {
        PathWire::from_relative(&RelativePath::parse(value).unwrap()).unwrap()
    }
    fn authority() -> NarrowedAuthority {
        NarrowedAuthority {
            routes: ["provider/model".into()].into(),
            budgets: [("tokens".into(), 100)].into(),
            paths: vec![path("src")],
            extra: Default::default(),
        }
    }
    fn attempt() -> AttemptId {
        AttemptId::from_bytes([2; 16])
    }
    fn unknown<T>() -> Observed<T> {
        Observed {
            source: "unavailable".into(),
            completeness: Coverage::Unknown,
            value: None,
            extra: Default::default(),
        }
    }

    // Spec: docs/specs/lane-contracts.md#launch-request
    #[test]
    fn contract_authority_never_widens() {
        let parent = authority();
        let mut child = parent.clone();
        child.paths = vec![path("src/nested")];
        child.budgets.insert("tokens".into(), 50);
        assert!(child.is_within(&parent));
        let mut wider = child.clone();
        wider.routes.insert("other".into());
        assert!(!wider.is_within(&parent));
        wider = child.clone();
        wider.budgets.remove("tokens");
        assert!(!wider.is_within(&parent));
        wider.budgets.insert("tokens".into(), 101);
        assert!(!wider.is_within(&parent));
        wider = child;
        wider.paths = vec![path("src-other")];
        assert!(!wider.is_within(&parent));
        let mut future = parent.clone();
        future.extra.insert("new_authority".into(), json!(true));
        assert!(!future.is_within(&parent));
        assert!(!parent.is_within(&future));
    }

    // Spec: docs/specs/lane-contracts.md#launch-request
    #[test]
    fn contract_launch_roundtrip_defaults_and_unknown_fields() {
        let request = ChildLaunchRequest {
            parent_attempt: attempt(),
            authority: authority(),
            destination: Destination::ManagedWorktree {
                branch: BranchChoice::Existing {
                    name: "feature".into(),
                },
            },
            path_claim: Sourced::default(),
            commit: Sourced::default(),
            extra: Default::default(),
        };
        let mut wire = serde_json::to_value(&request).unwrap();
        wire.as_object_mut().unwrap().remove("path_claim");
        wire.as_object_mut().unwrap().remove("commit");
        wire["future"] = json!({"value": true});
        let decoded: ChildLaunchRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.path_claim, Sourced::default());
        assert_eq!(decoded.commit, Sourced::default());
        assert_eq!(decoded.extra["future"], json!({"value": true}));
        assert_eq!(
            serde_json::from_slice::<ChildLaunchRequest>(&serde_json::to_vec(&decoded).unwrap())
                .unwrap(),
            decoded
        );
        for destination in [
            Destination::ParentWorktree,
            Destination::ParentOwnedPath {
                path: "/owned".into(),
            },
            Destination::ManagedWorktree {
                branch: BranchChoice::New { name: "new".into() },
            },
        ] {
            assert_eq!(
                serde_json::from_value::<Destination>(serde_json::to_value(&destination).unwrap())
                    .unwrap(),
                destination
            );
        }
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn contract_observation_roundtrip_and_unknown_fields() {
        for kind in [
            ObservationKind::LaneLifecycle,
            ObservationKind::Note,
            ObservationKind::CheckRun,
            ObservationKind::CheckSuite,
            ObservationKind::WorkflowRun,
            ObservationKind::PullRequest,
            ObservationKind::IssueComment,
        ] {
            let event = ObservationEvent {
                header: RecordHeader::V1,
                id: "stable-id".into(),
                cursor: ObservationCursor {
                    project: ProjectId::from_bytes([1; 16]),
                    seq: 42,
                },
                lineage: ObservationLineage {
                    attempt: Some(attempt()),
                    parent_attempt: None,
                    work: None,
                    extra: Default::default(),
                },
                kind,
                coverage: Coverage::Partial,
                detail: json!({"repo":"org/repo"}),
                extra: Default::default(),
            };
            let mut value = serde_json::to_value(&event).unwrap();
            value["future"] = json!(1);
            let decoded: ObservationEvent = serde_json::from_value(value).unwrap();
            assert_eq!(decoded.extra["future"], json!(1));
            assert_eq!(
                serde_json::from_slice::<ObservationEvent>(&serde_json::to_vec(&decoded).unwrap())
                    .unwrap(),
                decoded
            );
        }
    }

    // Spec: docs/specs/lane-contracts.md#result-projection
    #[test]
    fn contract_result_roundtrip_unknown_and_bounded() {
        let projection = ResultProjection {
            header: RecordHeader::V1,
            attempt: attempt(),
            host_facts: HostFacts {
                commands: unknown(),
                git: unknown(),
                ci: unknown(),
                scope: unknown(),
                extra: Default::default(),
            },
            lane_claims: LaneClaims {
                handoff: None,
                verify: None,
                extra: Default::default(),
            },
            extra: Default::default(),
        };
        let mut value = serde_json::to_value(&projection).unwrap();
        value["host_facts"]["future"] = json!(true);
        let bytes = serde_json::to_vec(&value).unwrap();
        let decoded = ResultProjection::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.host_facts.extra["future"], json!(true));
        assert_eq!(
            ResultProjection::decode(serde_json::to_vec(&decoded).unwrap().as_slice()).unwrap(),
            decoded
        );
        let mut oversized = value;
        oversized["padding"] = json!("x".repeat(RESULT_PROJECTION_MAX_BYTES as usize));
        assert!(
            ResultProjection::decode(serde_json::to_vec(&oversized).unwrap().as_slice()).is_err()
        );
    }
}
