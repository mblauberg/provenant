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
    Graph => ("graph", false), Attempt => ("attempt", false), Branch => ("branch", false), Ref => ("ref", false), Worktree => ("worktree", true),
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

/// Evidence accepted for a destructive removal. Explicit discard is an
/// authority choice, not a claim that the subject landed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemovalEvidence {
    Landing { evidence: LandedEvidence },
    ExplicitDiscard,
}

/// Where a reference lives. Remote delivery is pinned to the push URL bound by
/// the host; callers never infer it from the remote name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RefEndpoint {
    Local,
    Remote { remote: String, push_url: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefOrigin {
    Created,
    Adopted,
}

/// Endpoint-qualified identity for one logical reference in a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RefEndpointIdentity {
    Local,
    Remote {
        remote: String,
        push_url_digest: provenant_core::Digest,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefIdentity {
    pub repository: ProjectId,
    pub endpoint: RefEndpointIdentity,
    pub reference: String,
}

impl RefIdentity {
    fn custody_subject(&self, generation: u64) -> Result<Subject, crate::CanonicalError> {
        let id =
            crate::CanonicalV1::record_digest("custody-ref-generation", 2, &(self, generation))?
                .to_string();
        Ok(Subject::new(SubjectKind::Ref, id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefOwnership {
    pub repository: ProjectId,
    pub endpoint: RefEndpoint,
    pub reference: String,
    pub generation: u64,
    pub origin: RefOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefGeneration {
    pub identity: RefIdentity,
    pub generation: u64,
    pub origin: RefOrigin,
}

impl RefGeneration {
    pub fn custody_subject(&self) -> Result<Subject, crate::CanonicalError> {
        self.identity.custody_subject(self.generation)
    }

    pub fn matches_ownership(
        &self,
        ownership: &RefOwnership,
    ) -> Result<bool, crate::CanonicalError> {
        let observed = ownership.generation_identity()?;
        Ok(self == &observed)
    }
}

impl RefOwnership {
    /// Logical branch custody is an advisory owner source only; endpoint and
    /// generation identity for retirement comes from `custody_subject`.
    pub fn logical_branch_subject(&self) -> Option<Subject> {
        let id = self.reference.strip_prefix("refs/heads/")?;
        if id.is_empty() || id.len() > 4096 || id.chars().any(char::is_control) {
            return None;
        }
        Some(Subject::new(SubjectKind::Branch, id))
    }

    pub fn identity(&self) -> Result<RefIdentity, crate::CanonicalError> {
        let endpoint = match &self.endpoint {
            RefEndpoint::Local => RefEndpointIdentity::Local,
            RefEndpoint::Remote { remote, push_url } => RefEndpointIdentity::Remote {
                remote: remote.clone(),
                push_url_digest: crate::CanonicalV1::record_digest(
                    "custody-ref-push-url",
                    2,
                    &(self.repository, remote, push_url),
                )?,
            },
        };
        Ok(RefIdentity {
            repository: self.repository,
            endpoint,
            reference: self.reference.clone(),
        })
    }

    pub fn generation_identity(&self) -> Result<RefGeneration, crate::CanonicalError> {
        Ok(RefGeneration {
            identity: self.identity()?,
            generation: self.generation,
            origin: self.origin,
        })
    }

    /// Stable custody identity for one observed generation. Remote URL text
    /// stays in the live ref ledger and never enters the custody record or key.
    pub fn custody_subject(&self) -> Result<Subject, crate::CanonicalError> {
        self.generation_identity()?.custody_subject()
    }
}

/// The actor's durable decision. Explicit discard is distinct from a landing
/// claim and remains fenced by the pinned reference generation and tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetirementDecision {
    Discard {
        authority: ActorId,
        evidence: RemovalEvidence,
    },
}
impl RetirementDecision {
    pub fn evidence(&self) -> &RemovalEvidence {
        match self {
            Self::Discard { evidence, .. } => evidence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RetirementStep {
    Worktree { subject: Subject },
    Reference,
}
impl RetirementStep {
    pub fn for_subjects(subjects: &[Subject]) -> Vec<Self> {
        subjects
            .iter()
            .cloned()
            .map(|subject| Self::Worktree { subject })
            .chain(std::iter::once(Self::Reference))
            .collect()
    }

    pub fn prerequisites_complete(&self, outcomes: &[RetirementStepOutcome]) -> bool {
        match self {
            Self::Worktree { .. } => true,
            Self::Reference => outcomes
                .iter()
                .filter(|outcome| matches!(&outcome.step, Self::Worktree { .. }))
                .all(|outcome| matches!(&outcome.state, RetirementStepState::Completed { .. })),
        }
    }

    /// Worktrees can be discarded once their exact tip is subsumed; deleting
    /// the reference itself requires a current landed proof.
    pub fn accepts_completion_evidence(
        &self,
        intent: &RemovalEvidence,
        evidence: &RemovalEvidence,
    ) -> bool {
        match evidence {
            RemovalEvidence::ExplicitDiscard => {
                matches!(intent, RemovalEvidence::ExplicitDiscard)
            }
            RemovalEvidence::Landing { evidence } => match self {
                Self::Worktree { .. } => matches!(
                    evidence.predicate,
                    LandingPredicate::Subsumed | LandingPredicate::Landed
                ),
                Self::Reference => evidence.predicate == LandingPredicate::Landed,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RetirementStepState {
    Pending,
    Completed { evidence: RemovalEvidence },
    Refused { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementStepOutcome {
    pub step: RetirementStep,
    pub state: RetirementStepState,
}

/// Custody-owned intent and resumable outcomes for one reference generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementRecord {
    pub schema_version: u32,
    pub id: String,
    pub project: ProjectId,
    /// The endpoint-qualified reference generation being retired.
    pub subject: Subject,
    /// Additional custody subjects retired by this same worktree operation.
    pub related_subjects: Vec<Subject>,
    pub reference: RefGeneration,
    pub expected_tip: provenant_core::GitOid,
    pub decision: RetirementDecision,
    pub revision: u64,
    pub outcomes: Vec<RetirementStepOutcome>,
}

impl RetirementRecord {
    fn normalize_subjects(subjects: &[Subject]) -> Vec<Subject> {
        let mut subjects = subjects.to_vec();
        subjects.sort_by(|left, right| {
            (left.kind.as_str(), &left.id).cmp(&(right.kind.as_str(), &right.id))
        });
        subjects.dedup();
        subjects
    }

    pub fn identity(
        project: ProjectId,
        related_subjects: &[Subject],
        reference: &RefGeneration,
        expected_tip: provenant_core::GitOid,
        decision: &RetirementDecision,
    ) -> Result<String, crate::CanonicalError> {
        let subject = reference.custody_subject()?;
        let related_subjects = Self::normalize_subjects(related_subjects);
        Ok(crate::CanonicalV1::record_digest(
            "custody-retirement",
            2,
            &(
                project,
                subject,
                related_subjects,
                reference,
                expected_tip,
                decision,
            ),
        )?
        .to_string())
    }

    pub fn plan(
        project: ProjectId,
        related_subjects: Vec<Subject>,
        reference: RefOwnership,
        expected_tip: provenant_core::GitOid,
        decision: RetirementDecision,
    ) -> Result<Self, crate::CanonicalError> {
        let reference = reference.generation_identity()?;
        let subject = reference.custody_subject()?;
        let related_subjects = Self::normalize_subjects(&related_subjects);
        let id = Self::identity(
            project,
            &related_subjects,
            &reference,
            expected_tip,
            &decision,
        )?;
        let outcomes = RetirementStep::for_subjects(&related_subjects)
            .into_iter()
            .map(|step| RetirementStepOutcome {
                step,
                state: RetirementStepState::Pending,
            })
            .collect();
        Ok(Self {
            schema_version: 2,
            id,
            project,
            subject,
            related_subjects,
            reference,
            expected_tip,
            decision,
            revision: 1,
            outcomes,
        })
    }

    fn projection(&self, subject: &Subject) -> RemovalRecord {
        let projected_step = if subject == &self.subject {
            RetirementStep::Reference
        } else {
            RetirementStep::Worktree {
                subject: subject.clone(),
            }
        };
        let state = self
            .outcomes
            .iter()
            .find(|outcome| outcome.step == projected_step)
            .map(|outcome| &outcome.state);
        let outcome = match (&self.decision, state) {
            (
                RetirementDecision::Discard { .. },
                Some(RetirementStepState::Completed { evidence }),
            ) => RemovalOutcome::Removed {
                evidence: evidence.clone(),
                retirement_id: self.id.clone(),
            },
            (RetirementDecision::Discard { .. }, _) => RemovalOutcome::Kept {
                cause: RemovalUncertainty::RetirementIncomplete,
            },
        };
        RemovalRecord {
            project: self.project,
            subject: subject.clone(),
            tip: self.expected_tip,
            outcome,
        }
    }

    /// Project each affected custody subject into the existing GC report.
    pub fn removals(&self) -> Vec<RemovalRecord> {
        std::iter::once(self.projection(&self.subject))
            .chain(
                self.related_subjects
                    .iter()
                    .map(|subject| self.projection(subject)),
            )
            .collect()
    }

    pub fn removal(&self) -> RemovalRecord {
        self.projection(&self.subject)
    }

    pub fn owns_subject(&self, subject: &Subject) -> bool {
        &self.subject == subject || self.related_subjects.contains(subject)
    }
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
    RetirementIncomplete => "retirement_incomplete",
}

/// The outcome of considering one subject and tip for removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemovalOutcome {
    Removed {
        evidence: RemovalEvidence,
        retirement_id: String,
    },
    Kept {
        cause: RemovalUncertainty,
    },
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
    /// Points at this module's durable custody-owned retirement record.
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
    fn ref_custody_subject_binds_endpoint_reference_and_generation_without_exposing_url() {
        let project = ProjectId::from_bytes([9; 16]);
        let local = RefOwnership {
            repository: project,
            endpoint: RefEndpoint::Local,
            reference: "refs/heads/topic".into(),
            generation: 1,
            origin: RefOrigin::Created,
        };
        let remote = RefOwnership {
            endpoint: RefEndpoint::Remote {
                remote: "origin".into(),
                push_url: "ssh://example.invalid/team/repo.git".into(),
            },
            ..local.clone()
        };
        let another_generation = RefOwnership {
            generation: 2,
            ..remote.clone()
        };
        let another_remote = RefOwnership {
            endpoint: RefEndpoint::Remote {
                remote: "backup".into(),
                push_url: "ssh://backup.invalid/team/repo.git".into(),
            },
            ..remote.clone()
        };
        let local_subject = local.custody_subject().unwrap();
        let remote_subject = remote.custody_subject().unwrap();
        assert_eq!(local_subject.kind, SubjectKind::Ref);
        assert_eq!(
            local.logical_branch_subject(),
            remote.logical_branch_subject()
        );
        assert_ne!(local_subject, remote_subject);
        assert_ne!(
            remote_subject,
            another_generation.custody_subject().unwrap()
        );
        assert_ne!(remote_subject, another_remote.custody_subject().unwrap());
        let RefEndpoint::Remote { push_url, .. } = &remote.endpoint else {
            unreachable!()
        };
        assert!(!remote_subject.id.contains(&push_url));
        assert!(
            !serde_json::to_string(&remote.generation_identity().unwrap())
                .unwrap()
                .contains(&push_url)
        );
    }

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
                        evidence: RemovalEvidence::Landing {
                            evidence: LandedEvidence {
                                predicate,
                                source: source.clone(),
                            },
                        },
                        retirement_id: "retirement-test".into(),
                    },
                };
                let encoded = serde_json::to_vec(&record).unwrap();
                let decoded: RemovalRecord = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(decoded, record);
                let mut without_owner = serde_json::to_value(&record).unwrap();
                without_owner["outcome"]
                    .as_object_mut()
                    .unwrap()
                    .remove("retirement_id");
                assert!(serde_json::from_value::<RemovalRecord>(without_owner).is_err());
            }
        }
        let explicit = RemovalRecord {
            project,
            subject: Subject::new(SubjectKind::Worktree, "feature"),
            tip: oid,
            outcome: RemovalOutcome::Removed {
                evidence: RemovalEvidence::ExplicitDiscard,
                retirement_id: "retirement-explicit".into(),
            },
        };
        let encoded = serde_json::to_vec(&explicit).unwrap();
        assert_eq!(
            serde_json::from_slice::<RemovalRecord>(&encoded).unwrap(),
            explicit
        );
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
