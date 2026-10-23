//! Minimal workflow wire records. Decoded receipts are stored claims, not verified facts.
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
/// User settings may strengthen the compiled factor floors.
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
/// Constructed only by the composition edge after reading originating user authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserApproval {
    pub actor_id: ActorId,
    pub evidence: EvidenceRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRecord {
    pub factors: RiskFactors,
    pub derived: RiskTier,
    pub tier: RiskTier,
    pub downgrade_approval: Option<ApprovalRef>,
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
    Unknown,
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
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    Pending,
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
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryReceipt {
    pub schema_version: u64,
    pub delivery_id: DeliveryId,
    pub project_id: ProjectId,
    pub revision: u64,
    pub subject: Subject,
    pub authors: Vec<Author>,
    pub risk: RiskRecord,
    pub review_attempts: Vec<AttemptId>,
    pub decision: Decision,
    #[serde(with = "micros")]
    pub created_at: UtcMicros,
    #[serde(with = "micros")]
    pub updated_at: UtcMicros,
}
/// Settings are passed in by the caller; workflow never loads configuration.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub subject: Subject,
    pub review_attempts: Vec<AttemptId>,
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
