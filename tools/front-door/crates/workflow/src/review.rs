//! Construction checks only. Lane C owns review coverage and availability.
use provenant_records::workflow::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReviewError {
    #[error("review settings weaken the risk ladder or omit the adversarial lens")]
    Policy,
    #[error("review lens or route provenance is incomplete")]
    Incomplete,
    #[error("only observed provenance can carry verified model-family lineage")]
    UnverifiedFamily,
}
pub fn validate_policy(policy: &ReviewPolicy) -> Result<(), ReviewError> {
    if policy.substantial < 1
        || policy.crucial < 2
        || policy.terminal < policy.crucial
        || policy.crucial < policy.substantial
        || policy.substantial < policy.routine
        || policy.adversarial_lens.trim().is_empty()
    {
        return Err(ReviewError::Policy);
    }
    Ok(())
}
pub fn validate_provenance(route: &RouteProvenance) -> Result<(), ReviewError> {
    let incomplete = match route {
        RouteProvenance::Observed {
            route,
            adapter,
            endpoint_provider,
            model_family,
            ..
        } => {
            route.trim().is_empty()
                || adapter.trim().is_empty()
                || endpoint_provider
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty())
                || model_family.as_ref().is_some_and(|s| s.trim().is_empty())
        }
        RouteProvenance::ChairAttested {
            route,
            declared_family,
            ..
        } => {
            route.trim().is_empty()
                || declared_family
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty())
        }
        RouteProvenance::Unknown => false,
    };
    if incomplete {
        return Err(ReviewError::Incomplete);
    }
    Ok(())
}
pub fn fact(fact: ReviewFact, policy: &ReviewPolicy) -> Result<ReviewFact, ReviewError> {
    validate_policy(policy)?;
    validate_provenance(&fact.eligibility.provenance)?;
    if fact.lens.trim().is_empty() {
        return Err(ReviewError::Incomplete);
    }
    if let Some(family) = &fact.eligibility.verified_model_family {
        match &fact.eligibility.provenance {
            RouteProvenance::Observed {
                model_family: Some(observed),
                ..
            } if observed == family && !family.trim().is_empty() => {}
            _ => return Err(ReviewError::UnverifiedFamily),
        }
    }
    Ok(fact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, AttemptId, Digest, RelativePath};
    use provenant_records::PathWire;
    fn fact() -> ReviewFact {
        let output = EvidenceRef {
            path: PathWire::from_relative(&RelativePath::parse("reviews/result.md").unwrap())
                .unwrap(),
            digest: Digest::from_bytes([0; 32]),
        };
        ReviewFact {
            attempt_id: AttemptId::from_bytes([1; 16]),
            reviewer_id: ActorId::from_bytes([2; 16]),
            subject: Subject::Document {
                path: output.path.clone(),
                digest: output.digest,
            },
            output: output.clone(),
            verdict: ReviewVerdict::RequestChanges,
            blocking_findings: vec!["failed bound".into()],
            eligibility: ReviewEligibility {
                provenance: RouteProvenance::ChairAttested {
                    route: "native/explicit@high".into(),
                    tier: ModelTier::Principal,
                    declared_family: Some("declared".into()),
                    evidence: output,
                },
                verified_model_family: None,
            },
            lens: "correctness".into(),
        }
    }
    #[test]
    fn attestation_never_becomes_verified_family_and_failures_remain_facts() {
        let policy = ReviewPolicy::default();
        let review = super::fact(fact(), &policy).unwrap();
        assert_eq!(review.verdict, ReviewVerdict::RequestChanges);
        let mut invented = review.clone();
        invented.eligibility.verified_model_family = Some("declared".into());
        assert_eq!(
            super::fact(invented, &policy),
            Err(ReviewError::UnverifiedFamily)
        );
        let mut unknown = review;
        unknown.eligibility.provenance = RouteProvenance::Unknown;
        assert!(super::fact(unknown, &policy).is_ok());
    }
    #[test]
    fn weakened_policy_blank_lens_and_missing_route_are_refused() {
        let policy = ReviewPolicy {
            substantial: 0,
            ..ReviewPolicy::default()
        };
        assert_eq!(super::fact(fact(), &policy), Err(ReviewError::Policy));
        let mut review = fact();
        review.lens = " ".into();
        assert_eq!(
            super::fact(review, &ReviewPolicy::default()),
            Err(ReviewError::Incomplete)
        );
        let mut review = fact();
        review.eligibility.provenance = RouteProvenance::Observed {
            route: String::new(),
            tier: ModelTier::Junior,
            adapter: "codex".into(),
            endpoint_provider: None,
            model_family: None,
            evidence: review.output.clone(),
        };
        assert_eq!(
            super::fact(review, &ReviewPolicy::default()),
            Err(ReviewError::Incomplete)
        );
    }
}
