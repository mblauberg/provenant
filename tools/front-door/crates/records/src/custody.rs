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
        reference: String,
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

/// A durable pointer to the archive captured before an explicit discard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveProof {
    /// Archive directory id under the project's declared discard archive root.
    pub id: String,
    pub tip: provenant_core::GitOid,
}
impl ArchiveProof {
    pub fn valid_for(&self, expected_tip: provenant_core::GitOid) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 255
            && self.id != "."
            && self.id != ".."
            && !self.id.chars().any(|ch| ch == '/' || ch == '\\')
            && !self.id.chars().any(char::is_control)
            && self.tip == expected_tip
    }
}

/// Evidence accepted for a destructive removal. Explicit discard is an
/// authority choice, not a claim that the subject landed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemovalEvidence {
    Landing {
        evidence: LandedEvidence,
    },
    /// The reference generation is discarded by authority. Any associated
    /// worktree must have an archive proof before removal begins.
    ExplicitDiscard {
        archive: Option<ArchiveProof>,
    },
    /// A clean idle worktree is removed while its reference remains available.
    IdleRetirement {
        tip: provenant_core::GitOid,
    },
}

/// Where a reference lives. Remote delivery is pinned to the push URL bound by
/// the host; callers never infer it from the remote name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RefEndpoint {
    Local,
    Remote {
        remote: String,
        push_url: String,
    },
    /// A detached private checkout's actual HEAD, distinguished by its
    /// canonical worktree path and its RefLedger generation.
    Worktree {
        path: String,
    },
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
    Worktree {
        path_digest: provenant_core::Digest,
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
        if matches!(self.endpoint, RefEndpoint::Worktree { .. }) {
            return None;
        }
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
            RefEndpoint::Worktree { path } => {
                if self.reference != "HEAD" || !valid_worktree_path(path) {
                    return Err(crate::CanonicalError::InvalidKind);
                }
                RefEndpointIdentity::Worktree {
                    path_digest: crate::CanonicalV1::record_digest(
                        "custody-worktree-head",
                        2,
                        &(self.repository, path),
                    )?,
                }
            }
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

fn valid_worktree_path(path: &str) -> bool {
    use std::path::Component;
    path.len() <= 4096
        && !path.is_empty()
        && !path.chars().any(char::is_control)
        && !path.contains("//")
        && path
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
        && std::path::Path::new(path).is_absolute()
        && std::path::Path::new(path)
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
        && std::path::Path::new(path)
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

/// The actor's durable decision. Explicit discard is distinct from a landing
/// claim and remains fenced by the pinned reference generation and tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetirementDecision {
    /// Remove the reference and its associated worktrees after current landing
    /// evidence or a previously captured explicit-discard archive.
    Discard {
        authority: ActorId,
        evidence: RemovalEvidence,
    },
    /// Remove only clean, idle worktrees while preserving the local reference.
    RetireWorktree {
        authority: ActorId,
        evidence: RemovalEvidence,
    },
}
impl RetirementDecision {
    pub fn authority(&self) -> ActorId {
        match self {
            Self::Discard { authority, .. } | Self::RetireWorktree { authority, .. } => *authority,
        }
    }

    pub fn evidence(&self) -> &RemovalEvidence {
        match self {
            Self::Discard { evidence, .. } | Self::RetireWorktree { evidence, .. } => evidence,
        }
    }

    pub fn removes_reference(&self) -> bool {
        matches!(self, Self::Discard { .. })
    }

    /// Whether a request can resume this recorded action for the same pinned
    /// generation. Callers must validate both decisions against the record's
    /// related subjects and expected tip; this comparison deliberately ignores
    /// a refreshed authority and refreshed Landing evidence, but keeps the
    /// action and evidence family fixed.
    pub fn permits_replay(&self, requested: &Self) -> bool {
        match (self, requested) {
            (
                Self::Discard {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
                Self::Discard {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
            )
            | (
                Self::RetireWorktree {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
                Self::RetireWorktree {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
            ) => true,
            (
                Self::Discard {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: left },
                    ..
                },
                Self::Discard {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: right },
                    ..
                },
            )
            | (
                Self::RetireWorktree {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: left },
                    ..
                },
                Self::RetireWorktree {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: right },
                    ..
                },
            ) => matches!((left, right), (None, None) | (Some(_), Some(_))),
            (
                Self::RetireWorktree {
                    evidence: RemovalEvidence::IdleRetirement { tip: left },
                    ..
                },
                Self::RetireWorktree {
                    evidence: RemovalEvidence::IdleRetirement { tip: right },
                    ..
                },
            ) => left == right,
            _ => false,
        }
    }

    pub fn valid_for(
        &self,
        related_subjects: &[Subject],
        expected_tip: provenant_core::GitOid,
    ) -> bool {
        match self {
            Self::Discard {
                evidence: RemovalEvidence::Landing { evidence },
                ..
            } => valid_landing_evidence(evidence, expected_tip, true),
            Self::Discard {
                evidence: RemovalEvidence::ExplicitDiscard { archive },
                ..
            } => match archive {
                Some(archive) => archive.valid_for(expected_tip),
                None => related_subjects.is_empty(),
            },
            Self::Discard {
                evidence: RemovalEvidence::IdleRetirement { .. },
                ..
            } => false,
            Self::RetireWorktree {
                evidence: RemovalEvidence::Landing { evidence },
                ..
            } => {
                !related_subjects.is_empty()
                    && valid_landing_evidence(evidence, expected_tip, false)
            }
            Self::RetireWorktree {
                evidence: RemovalEvidence::IdleRetirement { tip },
                ..
            } => !related_subjects.is_empty() && *tip == expected_tip,
            Self::RetireWorktree {
                evidence:
                    RemovalEvidence::ExplicitDiscard {
                        archive: Some(archive),
                    },
                ..
            } => !related_subjects.is_empty() && archive.valid_for(expected_tip),
            Self::RetireWorktree { .. } => false,
        }
    }
}

fn valid_landing_evidence(
    evidence: &LandedEvidence,
    expected_tip: provenant_core::GitOid,
    reference_must_be_landed: bool,
) -> bool {
    match (&evidence.predicate, &evidence.source) {
        (LandingPredicate::Subsumed, LandingSource::RetainedRef { reference, .. })
            if !reference_must_be_landed =>
        {
            valid_ref_name(reference)
        }
        (
            LandingPredicate::Subsumed | LandingPredicate::Landed,
            LandingSource::TreeEquivalent {
                reference, head, ..
            },
        ) if *head == expected_tip && !reference_must_be_landed => valid_ref_name(reference),
        (LandingPredicate::Landed, LandingSource::IntegrationTarget { reference, .. }) => {
            valid_ref_name(reference)
        }
        (LandingPredicate::Landed, LandingSource::ForgeMerged { pr, head, .. }) => {
            !pr.is_empty() && *head == expected_tip
        }
        _ => false,
    }
}

fn valid_ref_name(reference: &str) -> bool {
    reference.starts_with("refs/")
        && reference.len() <= 4096
        && !reference.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RetirementStep {
    Worktree { subject: Subject },
    Reference,
}
impl RetirementStep {
    pub fn for_decision(subjects: &[Subject], decision: &RetirementDecision) -> Vec<Self> {
        let worktrees = subjects
            .iter()
            .cloned()
            .map(|subject| Self::Worktree { subject });
        if decision.removes_reference() {
            worktrees.chain(std::iter::once(Self::Reference)).collect()
        } else {
            worktrees.collect()
        }
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
    fn accepts_completion_evidence(
        &self,
        intent: &RemovalEvidence,
        evidence: &RemovalEvidence,
    ) -> bool {
        match (intent, evidence, self) {
            (
                RemovalEvidence::ExplicitDiscard {
                    archive: Some(expected),
                },
                RemovalEvidence::ExplicitDiscard {
                    archive: Some(actual),
                },
                Self::Worktree { .. } | Self::Reference,
            ) => expected.valid_for(expected.tip) && actual.valid_for(expected.tip),
            (
                RemovalEvidence::ExplicitDiscard { archive: None },
                RemovalEvidence::ExplicitDiscard { archive: None },
                Self::Reference,
            ) => true,
            (
                RemovalEvidence::IdleRetirement { tip: expected },
                RemovalEvidence::IdleRetirement { tip: actual },
                Self::Worktree { .. },
            ) => expected == actual,
            (
                RemovalEvidence::Landing { .. },
                RemovalEvidence::Landing { evidence },
                Self::Worktree { .. },
            ) => matches!(
                evidence.predicate,
                LandingPredicate::Subsumed | LandingPredicate::Landed
            ),
            (
                RemovalEvidence::Landing { .. },
                RemovalEvidence::Landing { evidence },
                Self::Reference,
            ) => evidence.predicate == LandingPredicate::Landed,
            _ => false,
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
        let outcomes = RetirementStep::for_decision(&related_subjects, &decision)
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
        let projected_step = if subject == &self.subject && self.decision.removes_reference() {
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
        let outcome = match state {
            Some(RetirementStepState::Completed { evidence }) => RemovalOutcome::Removed {
                evidence: evidence.clone(),
                retirement_id: self.id.clone(),
            },
            _ => RemovalOutcome::Kept {
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
        self.subjects()
            .map(|subject| self.projection(subject))
            .collect()
    }

    /// Validate a step proof against this immutable decision and pinned tip.
    /// Refreshed archive IDs are valid when they remain safe and name the same
    /// expected tip; confinement verifies the captured archive before mutation.
    pub fn accepts_completion_evidence(
        &self,
        step: &RetirementStep,
        evidence: &RemovalEvidence,
    ) -> bool {
        if !self.outcomes.iter().any(|outcome| &outcome.step == step)
            || !step.accepts_completion_evidence(self.decision.evidence(), evidence)
        {
            return false;
        }
        let completion = match step {
            RetirementStep::Worktree { .. } => RetirementDecision::RetireWorktree {
                authority: self.decision.authority(),
                evidence: evidence.clone(),
            },
            RetirementStep::Reference => RetirementDecision::Discard {
                authority: self.decision.authority(),
                evidence: evidence.clone(),
            },
        };
        completion.valid_for(&self.related_subjects, self.expected_tip)
    }

    pub fn removal(&self) -> RemovalRecord {
        self.subjects()
            .next()
            .map(|subject| self.projection(subject))
            .unwrap_or_else(|| self.projection(&self.subject))
    }

    pub fn subjects(&self) -> impl Iterator<Item = &Subject> {
        std::iter::once(&self.subject)
            .filter(move |_| self.decision.removes_reference())
            .chain(self.related_subjects.iter())
    }

    pub fn valid_evidence(&self) -> bool {
        if !self
            .decision
            .valid_for(&self.related_subjects, self.expected_tip)
        {
            return false;
        }
        if self.outcomes.iter().any(|outcome| {
            matches!(
                &outcome.state,
                RetirementStepState::Completed { evidence }
                    if !self.accepts_completion_evidence(&outcome.step, evidence)
            )
        }) {
            return false;
        }
        match &self.reference.identity.endpoint {
            RefEndpointIdentity::Worktree { path_digest } => {
                let [worktree] = self.related_subjects.as_slice() else {
                    return false;
                };
                let expected = crate::CanonicalV1::record_digest(
                    "custody-worktree-head",
                    2,
                    &(self.reference.identity.repository, &worktree.id),
                );
                self.reference.identity.reference == "HEAD"
                    && self.reference.generation > 0
                    && worktree.kind == SubjectKind::Worktree
                    && valid_worktree_path(&worktree.id)
                    && matches!(&self.decision, RetirementDecision::RetireWorktree { .. })
                    && expected.is_ok_and(|expected| &expected == path_digest)
            }
            RefEndpointIdentity::Local => {
                let refs_valid = self.reference.identity.reference.starts_with("refs/")
                    && self.reference.identity.reference.len() <= 4096
                    && !self
                        .reference
                        .identity
                        .reference
                        .chars()
                        .any(char::is_control);
                let related_worktrees = !self.related_subjects.is_empty()
                    && self
                        .related_subjects
                        .iter()
                        .all(|subject| subject.kind == SubjectKind::Worktree);
                self.reference.generation > 0
                    && refs_valid
                    && match &self.decision {
                        RetirementDecision::Discard { .. } => true,
                        RetirementDecision::RetireWorktree { .. } => related_worktrees,
                    }
            }
            RefEndpointIdentity::Remote { .. } => {
                self.reference.generation > 0
                    && self.reference.identity.reference.starts_with("refs/")
                    && self.reference.identity.reference.len() <= 4096
                    && !self
                        .reference
                        .identity
                        .reference
                        .chars()
                        .any(char::is_control)
                    && matches!(&self.decision, RetirementDecision::Discard { .. })
            }
        }
    }

    pub fn owns_subject(&self, subject: &Subject) -> bool {
        self.subjects().any(|owned| owned == subject)
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
    CustodyUnknown => "custody_unknown",
    CensusUnknown => "census_unknown",
    RunRecordsUnknown => "run_records_unknown",
    Busy => "busy",
    DirtyWorktree => "dirty_worktree",
    LockedOrUnavailable => "locked_or_unavailable",
    ObservationFailed => "observation_failed",
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
        let detached = RefOwnership {
            endpoint: RefEndpoint::Worktree {
                path: "/fixture/detached".into(),
            },
            reference: "HEAD".into(),
            ..local.clone()
        };
        let detached_identity = detached.identity().unwrap();
        assert!(matches!(
            detached_identity.endpoint,
            RefEndpointIdentity::Worktree { .. }
        ));
        assert!(detached.logical_branch_subject().is_none());
        assert!(
            RefOwnership {
                endpoint: RefEndpoint::Worktree {
                    path: "/fixture/../escape".into(),
                },
                ..detached
            }
            .identity()
            .is_err()
        );
    }

    #[test]
    fn retirement_records_follow_one_declared_decision_and_archive_identity() {
        let project = ProjectId::from_bytes([12; 16]);
        let tip = provenant_core::GitOid::parse(&"c3".repeat(20)).unwrap();
        let reference = RefOwnership {
            repository: project,
            endpoint: RefEndpoint::Local,
            reference: "refs/heads/idle".into(),
            generation: 4,
            origin: RefOrigin::Created,
        };
        let worktree = Subject::new(SubjectKind::Worktree, "/fixture/idle");
        let idle = RetirementDecision::RetireWorktree {
            authority: ActorId::from_bytes([4; 16]),
            evidence: RemovalEvidence::IdleRetirement { tip },
        };
        let mut record = RetirementRecord::plan(
            project,
            vec![worktree.clone()],
            reference.clone(),
            tip,
            idle,
        )
        .unwrap();
        assert!(record.valid_evidence());
        assert_eq!(record.outcomes.len(), 1);
        assert_eq!(
            record.subjects().cloned().collect::<Vec<_>>(),
            [worktree.clone()]
        );
        assert_eq!(record.removals().len(), 1);
        assert_eq!(record.removal().subject, worktree);
        let completion = record.decision.evidence().clone();
        record.outcomes[0].state = RetirementStepState::Completed {
            evidence: completion.clone(),
        };
        assert!(matches!(
            record.removal().outcome,
            RemovalOutcome::Removed { .. }
        ));
        assert!(record.accepts_completion_evidence(&record.outcomes[0].step, &completion));

        let subsumed = RetirementRecord::plan(
            project,
            vec![Subject::new(SubjectKind::Worktree, "/fixture/subsumed")],
            reference.clone(),
            tip,
            RetirementDecision::RetireWorktree {
                authority: ActorId::from_bytes([4; 16]),
                evidence: RemovalEvidence::Landing {
                    evidence: LandedEvidence {
                        predicate: LandingPredicate::Subsumed,
                        source: LandingSource::RetainedRef {
                            reference: "refs/heads/retained".into(),
                            commit: tip,
                        },
                    },
                },
            },
        )
        .unwrap();
        assert!(subsumed.valid_evidence());
        assert_eq!(subsumed.outcomes.len(), 1);
        assert_eq!(subsumed.subjects().count(), 1);
        let refreshed_subsumed = RetirementDecision::RetireWorktree {
            authority: ActorId::from_bytes([5; 16]),
            evidence: RemovalEvidence::Landing {
                evidence: LandedEvidence {
                    predicate: LandingPredicate::Subsumed,
                    source: LandingSource::RetainedRef {
                        reference: "refs/heads/updated-base".into(),
                        commit: tip,
                    },
                },
            },
        };
        assert!(subsumed.decision.permits_replay(&refreshed_subsumed));
        assert!(refreshed_subsumed.valid_for(&subsumed.related_subjects, tip));

        let archive = ArchiveProof {
            id: "archive-123".into(),
            tip,
        };
        let explicit = RetirementDecision::Discard {
            authority: ActorId::from_bytes([4; 16]),
            evidence: RemovalEvidence::ExplicitDiscard {
                archive: Some(archive.clone()),
            },
        };
        let explicit_record = RetirementRecord::plan(
            project,
            vec![Subject::new(SubjectKind::Worktree, "/fixture/idle")],
            reference.clone(),
            tip,
            explicit.clone(),
        )
        .unwrap();
        assert!(explicit_record.valid_evidence());
        assert_eq!(explicit_record.outcomes.len(), 2);
        let refreshed_archive = RetirementDecision::Discard {
            authority: ActorId::from_bytes([5; 16]),
            evidence: RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    id: "archive-retry".into(),
                    tip,
                }),
            },
        };
        assert!(explicit.permits_replay(&refreshed_archive));
        assert!(refreshed_archive.valid_for(&explicit_record.related_subjects, tip));
        let worktree_step = RetirementStep::Worktree {
            subject: explicit_record.related_subjects[0].clone(),
        };
        assert!(
            explicit_record
                .accepts_completion_evidence(&worktree_step, refreshed_archive.evidence(),)
        );
        let wrong_tip_archive = RetirementDecision::Discard {
            authority: ActorId::from_bytes([5; 16]),
            evidence: RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    id: "archive-wrong-tip".into(),
                    tip: provenant_core::GitOid::parse(&"d4".repeat(20)).unwrap(),
                }),
            },
        };
        assert!(explicit.permits_replay(&wrong_tip_archive));
        assert!(!wrong_tip_archive.valid_for(&explicit_record.related_subjects, tip));
        assert!(
            !explicit_record
                .accepts_completion_evidence(&worktree_step, wrong_tip_archive.evidence(),)
        );
        assert!(!explicit.permits_replay(&refreshed_subsumed));
        assert_ne!(
            explicit_record.id,
            RetirementRecord::identity(
                project,
                &explicit_record.related_subjects,
                &reference.generation_identity().unwrap(),
                tip,
                &RetirementDecision::Discard {
                    authority: ActorId::from_bytes([4; 16]),
                    evidence: RemovalEvidence::ExplicitDiscard {
                        archive: Some(ArchiveProof {
                            id: "archive-other".into(),
                            tip,
                        }),
                    },
                },
            )
            .unwrap()
        );
        let invalid_archive = RetirementDecision::Discard {
            authority: ActorId::from_bytes([4; 16]),
            evidence: RemovalEvidence::ExplicitDiscard {
                archive: Some(ArchiveProof {
                    id: "../escape".into(),
                    tip,
                }),
            },
        };
        assert!(
            !RetirementRecord::plan(
                project,
                explicit_record.related_subjects,
                reference,
                tip,
                invalid_archive,
            )
            .unwrap()
            .valid_evidence()
        );
        let detached_path = "/fixture/detached";
        let detached_worktree = Subject::new(SubjectKind::Worktree, detached_path);
        let detached_reference = RefOwnership {
            repository: project,
            endpoint: RefEndpoint::Worktree {
                path: detached_path.into(),
            },
            reference: "HEAD".into(),
            generation: 1,
            origin: RefOrigin::Adopted,
        };
        let detached = RetirementRecord::plan(
            project,
            vec![detached_worktree.clone()],
            detached_reference,
            tip,
            RetirementDecision::RetireWorktree {
                authority: ActorId::from_bytes([4; 16]),
                evidence: RemovalEvidence::ExplicitDiscard {
                    archive: Some(ArchiveProof {
                        id: "archive-detached".into(),
                        tip,
                    }),
                },
            },
        )
        .unwrap();
        assert!(detached.valid_evidence());
        assert_eq!(detached.outcomes.len(), 1);
        assert_eq!(
            detached.subjects().cloned().collect::<Vec<_>>(),
            [detached_worktree]
        );
        assert!(!archive.id.is_empty());
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
                reference: "refs/heads/retained".into(),
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
                evidence: RemovalEvidence::ExplicitDiscard {
                    archive: Some(ArchiveProof {
                        id: "archive-1".into(),
                        tip: oid,
                    }),
                },
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
