//! Derive risk from typed factors, with recorded authority for downgrades.
use provenant_records::workflow::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RiskError {
    #[error("lowering derived risk requires recorded chair authority")]
    ApprovalRequired,
    #[error("a protected boundary cannot be downgraded below crucial")]
    BoundaryFloor,
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error("stored risk does not match the current factors and policy")]
    Mismatch,
}
pub fn minimum(factors: &RiskFactors, policy: &RiskPolicy) -> RiskTier {
    [
        policy.minimum,
        factors.reversibility.minimum(),
        factors.data_sensitivity.minimum(),
        factors.oracle_quality.minimum(),
        factors.external_effects.minimum(),
        factors.critical_surface.minimum(),
    ]
    .into_iter()
    .fold(RiskTier::Routine, std::cmp::max)
}
pub fn assess(
    factors: RiskFactors,
    tier: RiskTier,
    policy: &RiskPolicy,
    approval: Option<&UserApproval>,
) -> Result<RiskRecord, RiskError> {
    let derived = minimum(&factors, policy);
    if tier < RiskTier::Crucial
        && (factors.critical_surface == CriticalSurface::AuthSecurity
            || factors.external_effects == ExternalEffects::Irreversible)
    {
        return Err(RiskError::BoundaryFloor);
    }
    let downgrade_approval = if tier < derived {
        let approval = approval.ok_or(RiskError::ApprovalRequired)?;
        Some(ApprovalRef {
            extra: Default::default(),
            actor_id: approval.actor_id,
            evidence: approval.evidence.clone(),
        })
    } else {
        None
    };
    Ok(RiskRecord::new(factors, derived, tier, downgrade_approval)?)
}
/// Recompute under the current settings; decoded claims never supply authority.
pub fn validate(
    record: &RiskRecord,
    policy: &RiskPolicy,
    approval: Option<&UserApproval>,
) -> Result<(), RiskError> {
    if assess(record.factors().clone(), record.tier(), policy, approval)? != *record {
        return Err(RiskError::Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn factors() -> RiskFactors {
        RiskFactors {
            extra: Default::default(),
            blast_radius: BlastRadius::Local,
            reversibility: Reversibility::Easy,
            data_sensitivity: DataSensitivity::Public,
            migration: Migration::None,
            oracle_quality: OracleQuality::Strong,
            external_effects: ExternalEffects::None,
            critical_surface: CriticalSurface::Legal,
        }
    }
    #[test]
    fn size_and_migration_do_not_raise_risk() {
        let mut f = factors();
        f.critical_surface = CriticalSurface::None;
        f.blast_radius = BlastRadius::Production;
        f.migration = Migration::Destructive;
        assert_eq!(minimum(&f, &RiskPolicy::default()), RiskTier::Routine);
    }

    #[test]
    fn protected_factors_cannot_be_downgraded() {
        let mut f = factors();
        f.critical_surface = CriticalSurface::AuthSecurity;
        assert_eq!(
            assess(f.clone(), RiskTier::Routine, &RiskPolicy::default(), None),
            Err(RiskError::BoundaryFloor)
        );
        f.critical_surface = CriticalSurface::None;
        f.external_effects = ExternalEffects::Irreversible;
        assert_eq!(
            assess(f, RiskTier::Routine, &RiskPolicy::default(), None),
            Err(RiskError::BoundaryFloor)
        );
    }
    #[test]
    fn strongest_factor_and_policy_floor_determine_minimum() {
        assert_eq!(
            minimum(&factors(), &RiskPolicy::default()),
            RiskTier::Crucial
        );
        assert_eq!(
            minimum(
                &factors(),
                &RiskPolicy {
                    minimum: RiskTier::Terminal
                }
            ),
            RiskTier::Terminal
        );
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    use provenant_core::{ActorId, Digest, RelativePath};
    use provenant_records::PathWire;
    #[test]
    fn lowering_risk_requires_matching_current_user_authority() {
        let factors = RiskFactors {
            extra: Default::default(),
            blast_radius: BlastRadius::Production,
            reversibility: Reversibility::Easy,
            data_sensitivity: DataSensitivity::Internal,
            migration: Migration::None,
            oracle_quality: OracleQuality::Weak,
            external_effects: ExternalEffects::None,
            critical_surface: CriticalSurface::None,
        };
        let policy = RiskPolicy::default();
        assert_eq!(
            assess(factors.clone(), RiskTier::Routine, &policy, None),
            Err(RiskError::ApprovalRequired)
        );
        let approval = UserApproval {
            actor_id: ActorId::from_bytes([1; 16]),
            evidence: EvidenceRef {
                extra: Default::default(),
                path: PathWire::from_relative(
                    &RelativePath::parse("authority/approval.json").unwrap(),
                )
                .unwrap(),
                digest: Digest::from_bytes([2; 32]),
            },
        };
        let record = assess(factors, RiskTier::Substantial, &policy, Some(&approval)).unwrap();
        assert_eq!(record.derived(), RiskTier::Crucial);
        assert!(validate(&record, &policy, Some(&approval)).is_ok());
        assert_eq!(
            validate(&record, &policy, None),
            Err(RiskError::ApprovalRequired)
        );
        let different = UserApproval {
            actor_id: ActorId::from_bytes([3; 16]),
            ..approval
        };
        assert_eq!(
            validate(&record, &policy, Some(&different)),
            Err(RiskError::Mismatch)
        );
    }
}
