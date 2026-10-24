//! Minimal workflow wire records. Decoded receipts are stored claims, not verified facts.
//! Lane F's store enforces ID/creation immutability, resets the decision when
//! the subject changes, and keeps updated_at monotonic across revisions.
use crate::PathWire;
use provenant_core::{
    ActorId, AttemptId, DeliveryId, Digest, GitOid, ProjectId, RepoId, UtcMicros,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    Routine,
    Substantial,
    Crucial,
    Terminal,
}

macro_rules! factors {
    ($($name:ident { $($variant:ident => $tier:ident),+ }),+ $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const fn minimum(self) -> RiskTier {
                match self { $(Self::$variant => RiskTier::$tier),+ }
            }
        }
    )+};
}
factors! {
    BlastRadius { Local => Routine, MultiModule => Substantial, SharedSystem => Crucial, Production => Terminal },
    Reversibility { Easy => Routine, Moderate => Substantial, Hard => Crucial, Irreversible => Terminal },
    DataSensitivity { Public => Routine, Internal => Routine, Confidential => Crucial, Regulated => Terminal },
    Migration { None => Routine, Reversible => Substantial, Stateful => Crucial, Destructive => Terminal },
    OracleQuality { Strong => Routine, Mixed => Substantial, Weak => Crucial },
    ExternalEffects { None => Routine, Reversible => Crucial, Irreversible => Terminal },
    CriticalSurface { None => Routine, PublicContract => Substantial, AuthSecurity => Crucial, Privacy => Crucial, Financial => Crucial, Legal => Crucial, BuildReleaseGate => Crucial, LifeSafety => Terminal },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskFactors {
    pub blast_radius: BlastRadius,
    pub reversibility: Reversibility,
    pub data_sensitivity: DataSensitivity,
    pub migration: Migration,
    pub oracle_quality: OracleQuality,
    pub external_effects: ExternalEffects,
    pub critical_surface: CriticalSurface,
}
/// Compiled factor floors used by the composition edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskPolicy {
    pub minimum: RiskTier,
}
impl Default for RiskPolicy {
    fn default() -> Self {
        Self {
            minimum: RiskTier::Routine,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub path: PathWire,
    pub digest: Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRef {
    pub actor_id: ActorId,
    pub evidence: EvidenceRef,
}
/// Evidence reference supplied by the composition edge for user approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserApproval {
    pub actor_id: ActorId,
    pub evidence: EvidenceRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRecord {
    factors: RiskFactors,
    derived: RiskTier,
    tier: RiskTier,
    downgrade_approval: Option<ApprovalRef>,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordError {
    #[error("downgrade approval must be present exactly when tier is below derived risk")]
    RiskApproval,
    #[error("decision requires user approval")]
    DecisionApproval,
    #[error("decision reason or timestamp is invalid")]
    DecisionBounds,
    #[error("receipt identity, authors or timestamps are invalid")]
    Receipt,
    #[error("receipt revision overflow")]
    RevisionOverflow,
}
impl RiskRecord {
    pub fn new(
        factors: RiskFactors,
        derived: RiskTier,
        tier: RiskTier,
        downgrade_approval: Option<ApprovalRef>,
    ) -> Result<Self, RecordError> {
        let record = Self {
            factors,
            derived,
            tier,
            downgrade_approval,
        };
        record.validate_shape()?;
        Ok(record)
    }
    pub fn validate_shape(&self) -> Result<(), RecordError> {
        if (self.tier < self.derived) != self.downgrade_approval.is_some() {
            return Err(RecordError::RiskApproval);
        }
        Ok(())
    }
    pub fn factors(&self) -> &RiskFactors {
        &self.factors
    }
    pub fn derived(&self) -> RiskTier {
        self.derived
    }
    pub fn tier(&self) -> RiskTier {
        self.tier
    }
    pub fn downgrade_approval(&self) -> Option<&ApprovalRef> {
        self.downgrade_approval.as_ref()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Subject {
    Pr {
        repository: RepoId,
        number: u64,
        head: GitOid,
    },
    Document {
        path: PathWire,
        digest: Digest,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Junior,
    Senior,
    Principal,
}
/// Stored provenance claims. Deserializing these never verifies lineage or authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteProvenance {
    Observed {
        route: String,
        tier: ModelTier,
        adapter: String,
        endpoint_provider: Option<String>,
        model_family: Option<String>,
        evidence: EvidenceRef,
    },
    ChairAttested {
        route: String,
        tier: ModelTier,
        declared_family: Option<String>,
        evidence: EvidenceRef,
    },
    Unknown {},
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Author {
    User {
        actor_id: ActorId,
    },
    Agent {
        actor_id: ActorId,
        route: RouteProvenance,
    },
}
impl Author {
    pub fn actor_id(&self) -> ActorId {
        match self {
            Self::User { actor_id } | Self::Agent { actor_id, .. } => *actor_id,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Decision(DecisionState);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionStatus {
    Pending,
    Accepted,
    Abandoned,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum DecisionState {
    Pending {},
    Accepted {
        actor_id: ActorId,
        #[serde(with = "micros")]
        at: UtcMicros,
        reason: String,
        user_approval: Option<ApprovalRef>,
    },
    Abandoned {
        actor_id: ActorId,
        #[serde(with = "micros")]
        at: UtcMicros,
        reason: String,
        user_approval: Option<ApprovalRef>,
    },
}
/// Current receipt and scope inputs, supplied by the composition edge.
#[derive(Debug, Clone, Copy)]
pub struct DecisionContext {
    pub tier: RiskTier,
    pub created_at: UtcMicros,
    pub updated_at: UtcMicros,
}
impl Decision {
    pub fn status(&self) -> DecisionStatus {
        match &self.0 {
            DecisionState::Pending {} => DecisionStatus::Pending,
            DecisionState::Accepted { .. } => DecisionStatus::Accepted,
            DecisionState::Abandoned { .. } => DecisionStatus::Abandoned,
        }
    }
    pub fn pending() -> Self {
        Self(DecisionState::Pending {})
    }
    pub fn accept(
        context: DecisionContext,
        actor_id: ActorId,
        at: UtcMicros,
        reason: String,
        approval: Option<&UserApproval>,
    ) -> Result<Self, RecordError> {
        let decision = Self(DecisionState::Accepted {
            actor_id,
            at,
            reason,
            user_approval: approval.map(ApprovalRef::from),
        });
        decision.validate(context)?;
        Ok(decision)
    }
    pub fn abandon(
        context: DecisionContext,
        actor_id: ActorId,
        at: UtcMicros,
        reason: String,
        approval: Option<&UserApproval>,
    ) -> Result<Self, RecordError> {
        let decision = Self(DecisionState::Abandoned {
            actor_id,
            at,
            reason,
            user_approval: approval.map(ApprovalRef::from),
        });
        decision.validate(context)?;
        Ok(decision)
    }
    pub fn at(&self) -> Option<UtcMicros> {
        match &self.0 {
            DecisionState::Pending {} => None,
            DecisionState::Accepted { at, .. } | DecisionState::Abandoned { at, .. } => Some(*at),
        }
    }
    pub fn actor_id(&self) -> Option<ActorId> {
        match &self.0 {
            DecisionState::Pending {} => None,
            DecisionState::Accepted { actor_id, .. }
            | DecisionState::Abandoned { actor_id, .. } => Some(*actor_id),
        }
    }
    pub fn reason(&self) -> Option<&str> {
        match &self.0 {
            DecisionState::Pending {} => None,
            DecisionState::Accepted { reason, .. } | DecisionState::Abandoned { reason, .. } => {
                Some(reason)
            }
        }
    }
    pub fn user_approval(&self) -> Option<&ApprovalRef> {
        match &self.0 {
            DecisionState::Pending {} => None,
            DecisionState::Accepted { user_approval, .. }
            | DecisionState::Abandoned { user_approval, .. } => user_approval.as_ref(),
        }
    }
    pub fn validate(&self, context: DecisionContext) -> Result<(), RecordError> {
        if let Some(at) = self.at() {
            if at < context.created_at
                || at > context.updated_at
                || self.reason().is_none_or(|r| r.trim().is_empty())
            {
                return Err(RecordError::DecisionBounds);
            }
            if context.tier == RiskTier::Terminal && self.user_approval().is_none() {
                return Err(RecordError::DecisionApproval);
            }
        }
        Ok(())
    }
}
impl From<&UserApproval> for ApprovalRef {
    fn from(approval: &UserApproval) -> Self {
        Self {
            actor_id: approval.actor_id,
            evidence: approval.evidence.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryReceipt {
    schema_version: u64,
    delivery_id: DeliveryId,
    project_id: ProjectId,
    revision: u64,
    subject: Subject,
    authors: Vec<Author>,
    risk: RiskRecord,
    review_attempts: Vec<AttemptId>,
    decision: Decision,
    #[serde(with = "micros")]
    created_at: UtcMicros,
    #[serde(with = "micros")]
    updated_at: UtcMicros,
}
impl DeliveryReceipt {
    pub fn new(
        delivery_id: DeliveryId,
        project_id: ProjectId,
        subject: Subject,
        authors: Vec<Author>,
        risk: RiskRecord,
        now: UtcMicros,
    ) -> Result<Self, RecordError> {
        let receipt = Self {
            schema_version: 1,
            delivery_id,
            project_id,
            revision: 1,
            subject,
            authors,
            risk,
            review_attempts: vec![],
            decision: Decision::pending(),
            created_at: now,
            updated_at: now,
        };
        receipt.validate_shape()?;
        Ok(receipt)
    }
    pub fn validate_shape(&self) -> Result<(), RecordError> {
        if self.authors.is_empty()
            || self.revision == 0
            || self.updated_at < self.created_at
            || matches!(self.subject, Subject::Pr { number: 0, .. })
        {
            return Err(RecordError::Receipt);
        }
        let authors: std::collections::BTreeSet<_> =
            self.authors.iter().map(Author::actor_id).collect();
        let reviews: std::collections::BTreeSet<_> = self.review_attempts.iter().collect();
        if authors.len() != self.authors.len() || reviews.len() != self.review_attempts.len() {
            return Err(RecordError::Receipt);
        }
        self.risk.validate_shape()?;
        self.decision.validate(DecisionContext {
            tier: self.risk.tier(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
    /// Candidate for the next save with the review attempts the chair binds.
    pub fn with_review_attempts(mut self, attempts: Vec<AttemptId>) -> Result<Self, RecordError> {
        self.review_attempts = attempts;
        self.validate_shape()?;
        Ok(self)
    }
    /// Candidate decision stamped at `at`; the store recomputes `updated_at`
    /// from the clock, which never precedes `at`.
    pub fn with_decision(
        mut self,
        at: UtcMicros,
        decide: impl FnOnce(DecisionContext) -> Result<Decision, RecordError>,
    ) -> Result<Self, RecordError> {
        self.updated_at = self.updated_at.max(at);
        self.decision = decide(DecisionContext {
            tier: self.risk.tier(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })?;
        self.validate_shape()?;
        Ok(self)
    }
    /// Derives commit metadata and subject reset from stored claims and the clock.
    /// Workflow owns the lock and supplies the receipt read under that lock.
    pub fn with_commit_metadata(
        mut self,
        previous: Option<&Self>,
        now: UtcMicros,
    ) -> Result<Self, RecordError> {
        if let Some(previous) = previous {
            if self.delivery_id != previous.delivery_id || self.project_id != previous.project_id {
                return Err(RecordError::Receipt);
            }
            if self.subject != previous.subject {
                self.decision = Decision::pending();
                self.review_attempts.clear();
            }
        }
        self.revision = previous
            .map_or(0, Self::revision)
            .checked_add(1)
            .ok_or(RecordError::RevisionOverflow)?;
        self.created_at = previous.map_or(now, Self::created_at);
        self.updated_at = previous.map_or(now, Self::updated_at).max(now);
        self.validate_shape()?;
        Ok(self)
    }
    pub fn schema_version(&self) -> u64 {
        self.schema_version
    }
    pub fn delivery_id(&self) -> DeliveryId {
        self.delivery_id
    }
    pub fn project_id(&self) -> ProjectId {
        self.project_id
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
    pub fn authors(&self) -> &[Author] {
        &self.authors
    }
    pub fn risk(&self) -> &RiskRecord {
        &self.risk
    }
    pub fn review_attempts(&self) -> &[AttemptId] {
        &self.review_attempts
    }
    pub fn decision(&self) -> &Decision {
        &self.decision
    }
    pub fn created_at(&self) -> UtcMicros {
        self.created_at
    }
    pub fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }
}
/// Policy is supplied by the caller; production currently uses compiled defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPolicy {
    pub routine: u64,
    pub substantial: u64,
    pub crucial: u64,
    pub terminal: u64,
    pub adversarial_lens: String,
}
impl Default for ReviewPolicy {
    fn default() -> Self {
        Self {
            routine: 0,
            substantial: 1,
            crucial: 2,
            terminal: 2,
            adversarial_lens: "adversarial".into(),
        }
    }
}
/// Owner-verified eligibility is never accepted from request JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewEligibility {
    pub provenance: RouteProvenance,
    pub verified_model_family: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    ApproveWithNits,
    RequestChanges,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewFact {
    pub attempt_id: AttemptId,
    pub reviewer_id: ActorId,
    pub subject: Subject,
    pub output: EvidenceRef,
    pub verdict: ReviewVerdict,
    pub blocking_findings: Vec<String>,
    pub eligibility: ReviewEligibility,
    pub lens: String,
}
/// Workflow wire time is integer UTC microseconds, using the shared checked value.
mod micros {
    use super::*;
    use serde::{Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &UtcMicros, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(value.as_micros())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<UtcMicros, D::Error> {
        UtcMicros::from_micros(i64::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::RelativePath;
    use serde_json::json;

    fn time(n: i64) -> UtcMicros {
        UtcMicros::from_micros(n).unwrap()
    }
    fn approval() -> UserApproval {
        UserApproval {
            actor_id: ActorId::from_bytes([1; 16]),
            evidence: EvidenceRef {
                path: PathWire::from_relative(&RelativePath::parse("approval.md").unwrap())
                    .unwrap(),
                digest: Digest::from_bytes([2; 32]),
            },
        }
    }
    fn risk() -> RiskRecord {
        RiskRecord::new(
            RiskFactors {
                blast_radius: BlastRadius::Local,
                reversibility: Reversibility::Easy,
                data_sensitivity: DataSensitivity::Public,
                migration: Migration::None,
                oracle_quality: OracleQuality::Strong,
                external_effects: ExternalEffects::None,
                critical_surface: CriticalSurface::None,
            },
            RiskTier::Routine,
            RiskTier::Routine,
            None,
        )
        .unwrap()
    }
    fn receipt() -> DeliveryReceipt {
        DeliveryReceipt::new(
            DeliveryId::from_bytes([1; 16]),
            ProjectId::from_bytes([2; 16]),
            Subject::Pr {
                repository: RepoId::from_bytes([3; 16]),
                number: 1,
                head: GitOid::Sha1([4; 20]),
            },
            vec![Author::Agent {
                actor_id: ActorId::from_bytes([5; 16]),
                route: RouteProvenance::ChairAttested {
                    route: "codex/gpt-6.1-sol@high".into(),
                    tier: ModelTier::Principal,
                    declared_family: None,
                    evidence: approval().evidence,
                },
            }],
            risk(),
            time(10),
        )
        .unwrap()
    }
    #[test]
    fn decisions_enforce_approval_bounds_and_nonblank_reason() {
        for constructor in [Decision::accept, Decision::abandon] {
            for tier in [RiskTier::Terminal] {
                let context = DecisionContext {
                    tier,
                    created_at: time(10),
                    updated_at: time(20),
                };
                assert_eq!(
                    constructor(context, approval().actor_id, time(15), "done".into(), None),
                    Err(RecordError::DecisionApproval)
                );
                for at in [10, 20] {
                    let d = constructor(
                        context,
                        approval().actor_id,
                        time(at),
                        "done".into(),
                        Some(&approval()),
                    )
                    .unwrap();
                    assert_eq!(d.at(), Some(time(at)));
                    assert_eq!(d.actor_id(), Some(approval().actor_id));
                    assert_eq!(d.user_approval(), Some(&ApprovalRef::from(&approval())));
                    assert_eq!(
                        serde_json::from_value::<Decision>(serde_json::to_value(&d).unwrap())
                            .unwrap(),
                        d
                    );
                }
                for (at, reason) in [(9, "done"), (21, "done"), (15, " ")] {
                    assert_eq!(
                        constructor(
                            context,
                            approval().actor_id,
                            time(at),
                            reason.into(),
                            Some(&approval())
                        ),
                        Err(RecordError::DecisionBounds)
                    );
                }
            }
            let context = DecisionContext {
                tier: RiskTier::Routine,
                created_at: time(10),
                updated_at: time(20),
            };
            assert!(
                constructor(context, approval().actor_id, time(15), "done".into(), None).is_ok()
            );
        }
    }
    #[test]
    fn nested_records_and_enum_tags_reject_unknown_input() {
        let r = receipt();
        for pointer in [
            "/risk",
            "/risk/factors",
            "/authors/0",
            "/authors/0/route",
            "/authors/0/route/evidence",
            "/subject",
            "/decision",
        ] {
            let mut value = serde_json::to_value(&r).unwrap();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("unknown".into(), json!(0));
            assert!(
                serde_json::from_value::<DeliveryReceipt>(value).is_err(),
                "{pointer}"
            );
        }
        let mut approval_value = serde_json::to_value(ApprovalRef::from(&approval())).unwrap();
        approval_value["unknown"] = json!(0);
        assert!(serde_json::from_value::<ApprovalRef>(approval_value).is_err());
        for (pointer, tag) in [
            ("/subject/kind", "future"),
            ("/decision/status", "future"),
            ("/authors/0/route/source", "future"),
            ("/risk/tier", "future"),
        ] {
            let mut value = serde_json::to_value(&r).unwrap();
            *value.pointer_mut(pointer).unwrap() = json!(tag);
            assert!(serde_json::from_value::<DeliveryReceipt>(value).is_err());
        }
        let mut value = serde_json::to_value(&r).unwrap();
        value["authors"][0]["route"] = json!({"source": "unknown", "extra": 1});
        assert!(serde_json::from_value::<DeliveryReceipt>(value).is_err());
        let context = DecisionContext {
            tier: RiskTier::Routine,
            created_at: time(10),
            updated_at: time(20),
        };
        let accepted = Decision::accept(
            context,
            approval().actor_id,
            time(15),
            "done".into(),
            Some(&approval()),
        )
        .unwrap();
        assert_eq!(accepted.status(), DecisionStatus::Accepted);
        let mut value = serde_json::to_value(&accepted).unwrap();
        value["user_approval"]["evidence"]["extra"] = json!(1);
        assert!(serde_json::from_value::<Decision>(value).is_err());
        let policy = ReviewPolicy::default();
        let mut value = serde_json::to_value(&policy).unwrap();
        value["unknown"] = json!(0);
        assert!(serde_json::from_value::<ReviewPolicy>(value).is_err());
        assert_eq!(
            serde_json::from_value::<ReviewPolicy>(serde_json::to_value(&policy).unwrap()).unwrap(),
            policy
        );
        assert!(serde_json::from_value::<ReviewVerdict>(json!("unknown")).is_err());
    }
    #[test]
    fn commit_metadata_is_derived_from_previous_receipt_and_clock() {
        let mut candidate = receipt();
        candidate.revision = 999;
        candidate.created_at = time(0);
        candidate.updated_at = time(900);
        let committed = candidate.with_commit_metadata(None, time(20)).unwrap();
        assert_eq!(committed.revision(), 1);
        assert_eq!(committed.created_at(), time(20));
        assert_eq!(committed.updated_at(), time(20));

        let mut previous = committed;
        previous.revision = 7;
        previous.review_attempts = vec![AttemptId::from_bytes([8; 16])];
        previous.decision = Decision::accept(
            DecisionContext {
                tier: RiskTier::Routine,
                created_at: time(20),
                updated_at: time(20),
            },
            approval().actor_id,
            time(20),
            "done".into(),
            None,
        )
        .unwrap();
        let mut candidate = previous.clone();
        candidate.revision = 999;
        candidate.created_at = time(0);
        candidate.updated_at = time(900);
        let unchanged = candidate
            .clone()
            .with_commit_metadata(Some(&previous), time(15))
            .unwrap();
        assert_eq!(unchanged.revision(), 8);
        assert_eq!(unchanged.created_at(), time(20));
        assert_eq!(unchanged.updated_at(), time(20));
        assert_eq!(unchanged.decision(), previous.decision());
        assert_eq!(unchanged.review_attempts(), previous.review_attempts());

        candidate.subject = Subject::Pr {
            repository: RepoId::from_bytes([3; 16]),
            number: 1,
            head: GitOid::Sha256([9; 32]),
        };
        let changed = candidate
            .clone()
            .with_commit_metadata(Some(&previous), time(30))
            .unwrap();
        assert_eq!(changed.revision(), 8);
        assert_eq!(changed.created_at(), time(20));
        assert_eq!(changed.updated_at(), time(30));
        assert_eq!(changed.decision().status(), DecisionStatus::Pending);
        assert!(changed.review_attempts().is_empty());
        previous.delivery_id = DeliveryId::from_bytes([9; 16]);
        assert_eq!(
            candidate
                .clone()
                .with_commit_metadata(Some(&previous), time(30)),
            Err(RecordError::Receipt)
        );
        previous.delivery_id = candidate.delivery_id;
        previous.revision = u64::MAX;
        assert_eq!(
            candidate.with_commit_metadata(Some(&previous), time(30)),
            Err(RecordError::RevisionOverflow)
        );
    }
    #[test]
    fn receipt_and_risk_construction_expose_read_only_values() {
        let r = receipt();
        assert_eq!(r.schema_version(), 1);
        assert_eq!(r.revision(), 1);
        assert_eq!(r.delivery_id(), DeliveryId::from_bytes([1; 16]));
        assert_eq!(r.project_id(), ProjectId::from_bytes([2; 16]));
        assert_eq!(r.created_at(), r.updated_at());
        assert!(r.decision().at().is_none());
        assert!(r.review_attempts().is_empty());
        assert_eq!(r.authors()[0].actor_id(), ActorId::from_bytes([5; 16]));
        assert_eq!(r.risk().derived(), RiskTier::Routine);
        assert!(r.risk().downgrade_approval().is_none());
        assert_eq!(
            RiskRecord::new(
                r.risk().factors().clone(),
                RiskTier::Terminal,
                RiskTier::Routine,
                None
            ),
            Err(RecordError::RiskApproval)
        );
        assert_eq!(
            RiskRecord::new(
                r.risk().factors().clone(),
                RiskTier::Routine,
                RiskTier::Routine,
                Some(ApprovalRef::from(&approval()))
            ),
            Err(RecordError::RiskApproval)
        );
        assert!(
            DeliveryReceipt::new(
                r.delivery_id(),
                r.project_id(),
                r.subject().clone(),
                vec![],
                risk(),
                time(10)
            )
            .is_err()
        );
    }
}
