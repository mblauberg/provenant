//! Derive risk from typed factors, with originating user authority for downgrades.
use provenant_records::workflow::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RiskError {
    #[error("lowering derived risk requires originating user approval")]
    ApprovalRequired,
    #[error("stored risk does not match the current factors and policy")]
    Mismatch,
}
pub fn minimum(factors: &RiskFactors, policy: &RiskPolicy) -> RiskTier {
    [
        policy.minimum,
        factors.blast_radius.minimum(),
        factors.reversibility.minimum(),
        factors.data_sensitivity.minimum(),
        factors.migration.minimum(),
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
    let downgrade_approval = if tier < derived {
        let approval = approval.ok_or(RiskError::ApprovalRequired)?;
        Some(ApprovalRef {
            actor_id: approval.actor_id,
            evidence: approval.evidence.clone(),
        })
    } else {
        None
    };
    Ok(RiskRecord {
        factors,
        derived,
        tier,
        downgrade_approval,
    })
}
/// Recompute under the current settings; decoded claims never supply authority.
pub fn validate(
    record: &RiskRecord,
    policy: &RiskPolicy,
    approval: Option<&UserApproval>,
) -> Result<(), RiskError> {
    if assess(record.factors.clone(), record.tier, policy, approval)? != *record {
        return Err(RiskError::Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn factors() -> RiskFactors {
        RiskFactors {
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
            blast_radius: BlastRadius::Production,
            reversibility: Reversibility::Easy,
            data_sensitivity: DataSensitivity::Internal,
            migration: Migration::None,
            oracle_quality: OracleQuality::Strong,
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
                path: PathWire::from_relative(
                    &RelativePath::parse("authority/approval.json").unwrap(),
                )
                .unwrap(),
                digest: Digest::from_bytes([2; 32]),
            },
        };
        let record = assess(factors, RiskTier::Substantial, &policy, Some(&approval)).unwrap();
        assert_eq!(record.derived, RiskTier::Terminal);
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
