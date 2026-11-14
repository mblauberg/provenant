//! The format-2 custody contract. Advisory ownership is never a writer lease.
use crate::ExtraFields;
use provenant_core::{ActorId, AttemptId, HostId, ProjectId, WallTime};
use serde::{Deserialize, Serialize};

/// Identity of the submitting chair, shared by dispatch and custody.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chair {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub host: HostId,
    pub actor: ActorId,
    pub session: Option<String>,
    pub process: Option<HolderProcess>,
}

impl PartialEq for Chair {
    fn eq(&self, other: &Self) -> bool {
        (self.host, self.actor, &self.session) == (other.host, other.actor, &other.session)
    }
}
impl Eq for Chair {}
/// Native process identity used only for advisory presence observations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderProcess {
    pub boot: String,
    pub pid: i64,
    pub native_start: u64,
}

macro_rules! subject_kinds {
    ($($variant:ident => ($name:literal, $path:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum SubjectKind { $(#[serde(rename = $name)] $variant),+ }
        impl SubjectKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn is_path(self) -> bool { match self { $(Self::$variant => $path),+ } }
            pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
        }
        impl std::str::FromStr for SubjectKind {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value { $($name => Ok(Self::$variant),)+ _ => Err(format!("unknown custody subject kind: {value}")) }
            }
        }
    }
}
subject_kinds! {
    Graph => ("graph", false), Attempt => ("attempt", false), Branch => ("branch", false), Worktree => ("worktree", true),
    Pr => ("pr", false), Issue => ("issue", false), Path => ("path", true), CloudKey => ("cloud_key", false),
}
impl std::fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: String,
}
impl Subject {
    pub fn new(kind: SubjectKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}
impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}
impl std::str::FromStr for Subject {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, id) = value.split_once(':').ok_or("expected KIND:ID")?;
        if id.is_empty() || id.len() > 4096 || id.chars().any(char::is_control) {
            return Err("invalid custody subject id".into());
        }
        Ok(Self::new(kind.parse()?, id))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Holder {
    Chair {
        chair: Chair,
    },
    Attempt {
        attempt: AttemptId,
        chair: Option<Chair>,
    },
}
impl Holder {
    pub fn chair(&self) -> Option<&Chair> {
        match self {
            Self::Chair { chair } => Some(chair),
            Self::Attempt { chair, .. } => chair.as_ref(),
        }
    }
    pub fn actor(&self) -> Option<ActorId> {
        self.chair().map(|chair| chair.actor)
    }
    pub fn belongs_to(&self, chair: &Chair) -> bool {
        self.chair().is_some_and(|owner| {
            owner.host == chair.host && owner.actor == chair.actor && owner.session == chair.session
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Provenant,
    Adopted,
    External,
}

/// What was proved, independently of how it was proved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingPredicate {
    Subsumed,
    Landed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LandingSource {
    RetainedRef {
        reference: String,
        commit: provenant_core::GitOid,
    },
    TreeEquivalent {
        head: provenant_core::GitOid,
        target: provenant_core::GitOid,
        tree: provenant_core::GitOid,
    },
    IntegrationTarget {
        reference: String,
        commit: provenant_core::GitOid,
    },
    ForgeMerged {
        pr: String,
        head: provenant_core::GitOid,
        merge_commit: Option<provenant_core::GitOid>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedEvidence {
    pub predicate: LandingPredicate,
    pub source: LandingSource,
}

macro_rules! removal_uncertainties {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum RemovalUncertainty {
            $(#[serde(rename = $wire)] $variant),+
        }
        impl RemovalUncertainty {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
}
removal_uncertainties! {
    ShallowClone => "shallow_clone",
    ConflictedMergeTree => "conflicted_merge_tree",
    OfflineForge => "offline_forge",
    RewrittenDefaultBranch => "rewritten_default_branch",
    UnprovenLanding => "unproven_landing",
    MissingCommit => "missing_commit",
}

/// The outcome of considering one subject and tip for removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemovalOutcome {
    Removed { evidence: LandedEvidence },
    Kept { cause: RemovalUncertainty },
}

/// Evidence emitted by collection for a removed or uncertainty-retained subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalRecord {
    pub project: ProjectId,
    pub subject: Subject,
    pub tip: provenant_core::GitOid,
    pub outcome: RemovalOutcome,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CustodyState {
    Active,
    Landed {
        evidence: LandedEvidence,
    },
    /// Points at `landing::RetirementRecord::id`, which owns the decision and step outcomes.
    Retired {
        retirement_id: String,
    },
    Released,
}
impl CustodyState {
    pub const fn kind(&self) -> StateKind {
        match self {
            Self::Active => StateKind::Active,
            Self::Landed { .. } => StateKind::Landed,
            Self::Retired { .. } => StateKind::Retired,
            Self::Released => StateKind::Released,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    Active,
    Landed,
    Retired,
    Released,
}
impl StateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Landed => "landed",
            Self::Retired => "retired",
            Self::Released => "released",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyRecord {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub schema_version: u64,
    pub subject: Subject,
    pub holder: Holder,
    pub project: ProjectId,
    pub origin: Origin,
    pub state: CustodyState,
    pub last_activity: WallTime,
    /// Related artefacts acquired together; landing follows branch/worktree links.
    pub links: Vec<Subject>,
    /// Host-sealed dispatch request digest for a graph association.
    pub provenance: Option<provenant_core::Digest>,
}
/// One query contract for status, session digests and retention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustodyQuery {
    pub after: u64,
    pub limit: u64,
    pub project: Option<ProjectId>,
    pub state: Option<StateKind>,
    pub holder: Option<Holder>,
    /// Includes lane items nested under this chair, across projects.
    pub chair: Option<Chair>,
}

impl Default for CustodyQuery {
    fn default() -> Self {
        Self {
            after: 0,
            limit: 100,
            project: None,
            state: None,
            holder: None,
            chair: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyPage {
    pub items: Vec<CustodyRecord>,
    pub next: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyChange {
    pub record: CustodyRecord,
    /// An active prior holder is retained in the event and exposed as a warning.
    pub previous_holder: Option<Holder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderPresence {
    Live,
    Unattended,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyView {
    pub record: CustodyRecord,
    pub presence: HolderPresence,
    pub mine: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyStatus {
    pub items: Vec<CustodyView>,
    pub next: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removal_records_round_trip_every_predicate_source_and_uncertainty() {
        let oid = provenant_core::GitOid::parse(&"a".repeat(40)).unwrap();
        let target = provenant_core::GitOid::parse(&"b".repeat(40)).unwrap();
        let project = ProjectId::from_bytes([7; 16]);
        let sources = [
            LandingSource::RetainedRef {
                reference: "refs/heads/feature".into(),
                commit: oid,
            },
            LandingSource::TreeEquivalent {
                head: oid,
                target,
                tree: oid,
            },
            LandingSource::IntegrationTarget {
                reference: "refs/remotes/origin/main".into(),
                commit: target,
            },
            LandingSource::ForgeMerged {
                pr: "42".into(),
                head: oid,
                merge_commit: Some(target),
            },
        ];
        for predicate in [LandingPredicate::Subsumed, LandingPredicate::Landed] {
            for source in &sources {
                let record = RemovalRecord {
                    project,
                    subject: Subject::new(SubjectKind::Worktree, "feature"),
                    tip: oid,
                    outcome: RemovalOutcome::Removed {
                        evidence: LandedEvidence {
                            predicate,
                            source: source.clone(),
                        },
                    },
                };
                let encoded = serde_json::to_vec(&record).unwrap();
                let decoded: RemovalRecord = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(decoded, record);
            }
        }
        for &cause in RemovalUncertainty::ALL {
            let record = RemovalRecord {
                project,
                subject: Subject::new(SubjectKind::Worktree, "feature"),
                tip: oid,
                outcome: RemovalOutcome::Kept { cause },
            };
            let encoded = serde_json::to_vec(&record).unwrap();
            let decoded: RemovalRecord = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn every_declared_subject_kind_round_trips() {
        for &kind in SubjectKind::ALL {
            assert_eq!(kind.as_str().parse::<SubjectKind>().unwrap(), kind);
            let subject = Subject::new(kind, "item:with:colons");
            assert_eq!(subject.to_string().parse::<Subject>().unwrap(), subject);
            assert_eq!(
                serde_json::from_value::<Subject>(serde_json::to_value(&subject).unwrap()).unwrap(),
                subject
            );
        }
    }
}
