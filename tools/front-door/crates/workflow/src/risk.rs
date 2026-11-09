//! Derive risk from typed factors, with recorded authority for downgrades.
use provenant_records::workflow::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RiskError {
    #[error("lowering derived risk requires recorded chair authority")]
    ApprovalRequired,
    #[error("a protected factor floor cannot be downgraded")]
    BoundaryFloor,
    #[error("boundary path cannot be downgraded: {0}")]
    BoundaryPath(String),
    #[error("changed paths must be nonempty and repository-relative for a downgrade")]
    InvalidPaths,
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
    approval: Option<&ChairDowngrade>,
    changed_paths: &[String],
) -> Result<RiskRecord, RiskError> {
    let review = provenant_routing::ReviewConfig::default();
    let boundary = changed_paths.iter().find_map(|path| {
        let floor = review.path_floors(std::slice::from_ref(path));
        match floor.tier.as_deref() {
            Some("terminal") => Some((path, RiskTier::Terminal)),
            Some("crucial") => Some((path, RiskTier::Crucial)),
            _ => None,
        }
    });
    let derived = std::cmp::max(
        minimum(&factors, policy),
        boundary.map_or(RiskTier::Routine, |(_, floor)| floor),
    );
    if tier < derived
        && let Some((path, _)) = boundary
    {
        return Err(RiskError::BoundaryPath(path.clone()));
    }
    if (tier < RiskTier::Crucial && factors.critical_surface == CriticalSurface::AuthSecurity)
        || (tier < RiskTier::Terminal && factors.external_effects == ExternalEffects::Irreversible)
    {
        return Err(RiskError::BoundaryFloor);
    }
    let downgrade_approval = if tier < derived {
        let approval = approval.ok_or(RiskError::ApprovalRequired)?;
        if changed_paths.is_empty()
            || changed_paths.iter().any(|path| {
                provenant_core::RelativePath::parse(path).is_err()
                    || path.contains('\\')
                    || path
                        .split('/')
                        .any(|part| part == "." || part == ".." || part.is_empty())
            })
        {
            return Err(RiskError::InvalidPaths);
        }
        Some(approval.clone())
    } else {
        None
    };
    Ok(RiskRecord::new(factors, derived, tier, downgrade_approval)?
        .with_changed_paths(changed_paths.to_vec()))
}
/// Recompute under the current settings; decoded claims never supply authority.
pub fn validate(
    record: &RiskRecord,
    policy: &RiskPolicy,
    approval: Option<&ChairDowngrade>,
    changed_paths: &[String],
) -> Result<(), RiskError> {
    if assess(
        record.factors().clone(),
        record.tier(),
        policy,
        approval,
        changed_paths,
    )? != *record
    {
        return Err(RiskError::Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn factors() -> RiskFactors {
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
            assess(
                f.clone(),
                RiskTier::Routine,
                &RiskPolicy::default(),
                None,
                &[]
            ),
            Err(RiskError::BoundaryFloor)
        );
        f.critical_surface = CriticalSurface::None;
        f.external_effects = ExternalEffects::Irreversible;
        assert_eq!(
            assess(f, RiskTier::Routine, &RiskPolicy::default(), None, &[]),
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
    fn recorded_chair_downgrade_lowers_nonboundary_risk() {
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
            assess(
                factors.clone(),
                RiskTier::Routine,
                &policy,
                None,
                &["src/build_graph.rs".into()]
            ),
            Err(RiskError::ApprovalRequired)
        );
        let approval = ChairDowngrade {
            reason: "Weak oracle is covered by manual inspection".into(),
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
        let record = assess(
            factors,
            RiskTier::Substantial,
            &policy,
            Some(&approval),
            &["src/build_graph.rs".into()],
        )
        .unwrap();
        assert_eq!(record.derived(), RiskTier::Crucial);
        assert_eq!(record.tier(), RiskTier::Substantial);
        assert_eq!(record.downgrade_approval(), Some(&approval));
        assert_eq!(record.changed_paths(), &["src/build_graph.rs"]);
        assert_eq!(
            validate(&record, &policy, Some(&approval), &["src/other.rs".into()]),
            Err(RiskError::Mismatch)
        );
        assert!(
            validate(
                &record,
                &policy,
                Some(&approval),
                &["src/build_graph.rs".into()]
            )
            .is_ok()
        );
        assert_eq!(
            validate(&record, &policy, None, &["src/build_graph.rs".into()]),
            Err(RiskError::ApprovalRequired)
        );
        let different = ChairDowngrade {
            actor_id: ActorId::from_bytes([3; 16]),
            ..approval
        };
        assert_eq!(
            validate(
                &record,
                &policy,
                Some(&different),
                &["src/build_graph.rs".into()]
            ),
            Err(RiskError::Mismatch)
        );
    }
    #[test]
    fn recorded_chair_downgrade_refuses_boundary_paths() {
        let mut factors = super::tests::factors();
        factors.critical_surface = CriticalSurface::None;
        factors.oracle_quality = OracleQuality::Weak;
        let approval = ChairDowngrade {
            actor_id: ActorId::from_bytes([1; 16]),
            reason: "Manual inspection covers the weak oracle".into(),
            evidence: EvidenceRef {
                extra: Default::default(),
                path: PathWire::from_relative(
                    &RelativePath::parse("authority/reason.json").unwrap(),
                )
                .unwrap(),
                digest: Digest::from_bytes([2; 32]),
            },
        };
        for path in [
            "src/credentials/store.rs",
            "tools/front-door/crates/confinement/src/run_config.rs",
            "tools/front-door/crates/run/src/objects/view.rs",
            "tools/front-door/src/pr/landing/publish.rs",
            ".github/workflows/release.yml",
        ] {
            // A mixed diff, including a rename's old boundary path, stays protected.
            let paths = vec![
                "docs/readme.md".into(),
                path.into(),
                "src/new_name.rs".into(),
            ];
            assert_eq!(
                assess(
                    factors.clone(),
                    RiskTier::Routine,
                    &RiskPolicy::default(),
                    Some(&approval),
                    &paths
                ),
                Err(RiskError::BoundaryPath(path.into()))
            );
        }
        let mut routine = factors.clone();
        routine.oracle_quality = OracleQuality::Strong;
        assert_eq!(
            assess(
                routine,
                RiskTier::Routine,
                &RiskPolicy::default(),
                None,
                &["src/credentials/store.rs".into()]
            ),
            Err(RiskError::BoundaryPath("src/credentials/store.rs".into()))
        );
        for paths in [vec![], vec!["../credentials/store.rs".into()]] {
            assert_eq!(
                assess(
                    factors.clone(),
                    RiskTier::Routine,
                    &RiskPolicy::default(),
                    Some(&approval),
                    &paths
                ),
                Err(RiskError::InvalidPaths)
            );
        }
        let blank = ChairDowngrade {
            reason: " ".into(),
            ..approval
        };
        assert_eq!(
            assess(
                factors,
                RiskTier::Routine,
                &RiskPolicy::default(),
                Some(&blank),
                &["src/plain.rs".into()]
            ),
            Err(RiskError::Record(RecordError::RiskApproval))
        );
    }
}
