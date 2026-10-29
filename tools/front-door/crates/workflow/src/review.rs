//! Construction checks only. Lane C owns review coverage and availability.
use provenant_records::workflow::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReviewError {
    #[error("review settings weaken the risk ladder or omit the adversarial lens")]
    Policy,
    #[error("review lens or route provenance is incomplete")]
    Incomplete,
    #[error("review verdict contradicts its findings or output subject")]
    Output,
    #[error("only observed provenance can carry verified model-family lineage")]
    UnverifiedFamily,
}
pub fn validate_policy(policy: &ReviewPolicy) -> Result<(), ReviewError> {
    if policy.routine != 0
        || policy.substantial < 1
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
        RouteProvenance::Unknown { .. } => false,
    };
    if incomplete {
        return Err(ReviewError::Incomplete);
    }
    Ok(())
}
/// The composition edge reads output_subject from the retained review output.
pub fn fact(
    fact: ReviewFact,
    output_subject: &Subject,
    policy: &ReviewPolicy,
) -> Result<ReviewFact, ReviewError> {
    validate_policy(policy)?;
    if &fact.subject != output_subject
        || match fact.verdict {
            ReviewVerdict::Approve | ReviewVerdict::ApproveWithNits => {
                !fact.blocking_findings.is_empty()
            }
            ReviewVerdict::RequestChanges => fact.blocking_findings.is_empty(),
        }
    {
        return Err(ReviewError::Output);
    }
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
            extra: Default::default(),
            path: PathWire::from_relative(&RelativePath::parse("reviews/result.md").unwrap())
                .unwrap(),
            digest: Digest::from_bytes([0; 32]),
        };
        ReviewFact {
            attempt_id: AttemptId::from_bytes([1; 16]),
            reviewer_id: ActorId::from_bytes([2; 16]),
            subject: Subject::Document {
                extra: Default::default(),
                path: output.path.clone(),
                digest: output.digest,
            },
            output: output.clone(),
            verdict: ReviewVerdict::RequestChanges,
            blocking_findings: vec!["failed bound".into()],
            eligibility: ReviewEligibility {
                provenance: RouteProvenance::ChairAttested {
                    extra: Default::default(),
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
    fn checked(review: ReviewFact, policy: &ReviewPolicy) -> Result<ReviewFact, ReviewError> {
        super::fact(review.clone(), &review.subject, policy)
    }
    #[test]
    fn output_subject_must_match_reviewed_subject() {
        let review = fact();
        let subject = Subject::Document {
            extra: Default::default(),
            path: review.output.path.clone(),
            digest: Digest::from_bytes([9; 32]),
        };
        assert_eq!(
            super::fact(review, &subject, &ReviewPolicy::default()),
            Err(ReviewError::Output)
        );
    }
    #[test]
    fn attestation_never_becomes_verified_family_and_failures_remain_facts() {
        let policy = ReviewPolicy::default();
        let review = checked(fact(), &policy).unwrap();
        assert_eq!(review.verdict, ReviewVerdict::RequestChanges);
        let mut invented = review.clone();
        invented.eligibility.verified_model_family = Some("declared".into());
        assert_eq!(
            checked(invented, &policy),
            Err(ReviewError::UnverifiedFamily)
        );
        let mut unknown = review;
        unknown.eligibility.provenance = RouteProvenance::Unknown {
            extra: Default::default(),
        };
        assert!(checked(unknown, &policy).is_ok());
    }
    #[test]
    fn verdict_must_agree_with_blocking_findings() {
        let mut review = fact();
        review.verdict = ReviewVerdict::Approve;
        assert!(checked(review, &ReviewPolicy::default()).is_err());
        let mut review = fact();
        review.blocking_findings.clear();
        assert!(checked(review, &ReviewPolicy::default()).is_err());
        assert!(
            validate_policy(&ReviewPolicy {
                routine: 1,
                ..ReviewPolicy::default()
            })
            .is_err()
        );
    }
    #[test]
    fn weakened_policy_blank_lens_and_missing_route_are_refused() {
        let policy = ReviewPolicy {
            substantial: 0,
            ..ReviewPolicy::default()
        };
        assert_eq!(checked(fact(), &policy), Err(ReviewError::Policy));
        let mut review = fact();
        review.lens = " ".into();
        assert_eq!(
            checked(review, &ReviewPolicy::default()),
            Err(ReviewError::Incomplete)
        );
        let mut review = fact();
        review.eligibility.provenance = RouteProvenance::Observed {
            extra: Default::default(),
            route: String::new(),
            tier: ModelTier::Junior,
            adapter: "codex".into(),
            endpoint_provider: None,
            model_family: None,
            evidence: review.output.clone(),
        };
        assert_eq!(
            checked(review, &ReviewPolicy::default()),
            Err(ReviewError::Incomplete)
        );
    }
}
