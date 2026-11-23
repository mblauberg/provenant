//! Shared, versioned lane interfaces. Admission and projection owners apply these contracts.
use std::collections::{BTreeMap, BTreeSet};

use provenant_core::{AttemptId, ProjectId};
use serde::{Deserialize, Serialize};

use crate::{
    ExtraFields, Layer, PathWire, RecordHeader, WorkKey,
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
    pub source: Layer,
}
impl<T: Default> Default for Sourced<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            source: Layer::Default,
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
    GraphCheckpoint,
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

impl CommandReceipt {
    /// An absent exit code is unknown; it is never inferred from the outcome.
    pub fn exit_code_text(&self) -> String {
        self.exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_owned())
    }
}

impl<T> Observed<T> {
    /// The source and completeness of an observation, for human rendering.
    pub fn observed_label(&self) -> String {
        let completeness = match &self.completeness {
            Coverage::Complete => "complete",
            Coverage::Partial => "partial",
            Coverage::Unknown => "unknown",
        };
        format!("source {}; {completeness}", self.source)
    }
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
    /// The single producer: host facts are always placed before lane claims so
    /// a reader cannot mistake a lane assertion for something the host saw.
    pub fn new(
        header: RecordHeader,
        attempt: AttemptId,
        host_facts: HostFacts,
        lane_claims: LaneClaims,
    ) -> Self {
        Self {
            header,
            attempt,
            host_facts,
            lane_claims,
            extra: ExtraFields::default(),
        }
    }

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

    /// Render the projection for a person. What the harness observed comes
    /// first; the lane's handoff and verify follow, labelled as claims. An
    /// unobserved fact stays unknown rather than being inferred.
    pub fn render(&self) -> String {
        let mut out = String::from("host facts (observed):\n");
        let commands = &self.host_facts.commands;
        out.push_str(&format!("- commands: {}\n", commands.observed_label()));
        match &commands.value {
            Some(receipts) => {
                for receipt in receipts {
                    let command = receipt
                        .value
                        .as_ref()
                        .and_then(|value| value.command.as_deref())
                        .unwrap_or("unknown");
                    let exit = receipt
                        .value
                        .as_ref()
                        .map(CommandReceipt::exit_code_text)
                        .unwrap_or_else(|| "unknown".to_owned());
                    out.push_str(&format!(
                        "  - {command}: exit {exit}, {}\n",
                        receipt.observed_label()
                    ));
                }
            }
            None => out.push_str("  - none observed; none inferred\n"),
        }
        let git = &self.host_facts.git;
        match &git.value {
            Some(facts) => out.push_str(&format!(
                "- git: base {}, head {}, {} commit(s), diffstat {}, {}; {}\n",
                facts.base.as_deref().unwrap_or("unknown"),
                facts.head.as_deref().unwrap_or("unknown"),
                facts.commits.len(),
                facts.diffstat.as_deref().unwrap_or("unknown"),
                if facts.shared_worktree {
                    "shared worktree"
                } else {
                    "own worktree"
                },
                git.observed_label()
            )),
            None => out.push_str(&format!("- git: unknown; {}\n", git.observed_label())),
        }
        let ci = &self.host_facts.ci;
        out.push_str(&format!(
            "- ci: {}; {}\n",
            json_summary(&ci.value),
            ci.observed_label()
        ));
        let scope = &self.host_facts.scope;
        let outside = scope
            .value
            .as_ref()
            .map(|report| report.out_of_scope_count())
            .map(|count| count.to_string())
            .unwrap_or_else(|| "unknown".to_owned());
        out.push_str(&format!(
            "- files outside scope: {outside}; {}\n",
            scope.observed_label()
        ));

        out.push_str("lane claims (unverified):\n");
        match &self.lane_claims.handoff {
            Some(handoff) => out.push_str(&format!(
                "- handoff: {} ({})\n",
                bounded(&handoff.note),
                handoff.status.as_str()
            )),
            None => out.push_str("- handoff: none\n"),
        }
        match &self.lane_claims.verify {
            Some(verify) => {
                for line in verify {
                    out.push_str(&format!("  - verify: {}\n", bounded(line)));
                }
            }
            None => out.push_str("- verify: unknown\n"),
        }
        out
    }
}

/// A single-line, bounded rendering of a JSON observation, without secrets.
fn json_summary(value: &Option<serde_json::Value>) -> String {
    match value {
        None => "unknown".to_owned(),
        Some(serde_json::Value::Null) => "unknown".to_owned(),
        Some(serde_json::Value::String(text)) => bounded(text),
        Some(value) => bounded(&value.to_string()),
    }
}

/// Keep one rendered field on one bounded line.
fn bounded(text: &str) -> String {
    let single = text.replace(['\n', '\r'], " ");
    if single.len() <= 256 {
        return single;
    }
    let mut end = 256;
    while end > 0 && !single.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &single[..end])
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
            ObservationKind::GraphCheckpoint,
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

    // Spec: docs/specs/lane-contracts.md#result-projection
    #[test]
    fn result_projection_renders_host_facts_before_lane_claims() {
        let projection = ResultProjection::new(
            RecordHeader::V1,
            attempt(),
            HostFacts {
                commands: unknown(),
                git: unknown(),
                ci: unknown(),
                scope: unknown(),
                extra: Default::default(),
            },
            LaneClaims {
                handoff: None,
                verify: Some(vec!["ran targeted tests".into()]),
                extra: Default::default(),
            },
        );
        let rendered = projection.render();
        let facts = rendered.find("host facts").expect("facts header");
        let claims = rendered.find("lane claims").expect("claims header");
        assert!(facts < claims, "{rendered}");
        let wire = serde_json::to_string(&projection).unwrap();
        assert!(
            wire.find("host_facts").unwrap() < wire.find("lane_claims").unwrap(),
            "{wire}"
        );
    }

    // Spec: docs/specs/lane-contracts.md#result-projection
    #[test]
    fn result_projection_unknown_exit_code_is_unknown() {
        let receipt = CommandReceipt {
            command: Some("cargo test".into()),
            exit_code: None,
            extra: Default::default(),
        };
        assert_eq!(receipt.exit_code_text(), "unknown");
        let commands = Observed {
            source: "adapter-capture".into(),
            completeness: Coverage::Partial,
            value: Some(vec![Observed {
                source: "adapter-capture".into(),
                completeness: Coverage::Complete,
                value: Some(receipt),
                extra: Default::default(),
            }]),
            extra: Default::default(),
        };
        let projection = ResultProjection::new(
            RecordHeader::V1,
            attempt(),
            HostFacts {
                commands,
                git: unknown(),
                ci: unknown(),
                scope: unknown(),
                extra: Default::default(),
            },
            LaneClaims {
                handoff: None,
                verify: None,
                extra: Default::default(),
            },
        );
        let rendered = projection.render();
        assert!(rendered.contains("exit unknown"), "{rendered}");
        assert!(!rendered.contains("exit 0"), "{rendered}");
        assert!(rendered.contains("source adapter-capture"), "{rendered}");
        assert!(rendered.contains("partial"), "{rendered}");
    }
}
