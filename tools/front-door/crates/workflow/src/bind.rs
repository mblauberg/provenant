//! Bind a chair's review claim to run's verified attempt fact. What the attempt
//! proves (terminal success, reviewer, output digest, head, route) comes from
//! the fact; what only the reviewer's output says (verdict, findings, lens) and
//! the route tier, which no owner publishes yet, come from the claim.
use crate::review::{self, ReviewError};
use provenant_core::{DeliveryId, Digest};
use provenant_records::{AttemptFact, AttemptOutcome, ReviewerProvenance, workflow::*};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewClaim {
    pub lens: String,
    pub verdict: ReviewVerdict,
    #[serde(default)]
    pub blocking_findings: Vec<String>,
    pub tier: ModelTier,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindError {
    #[error("review attempt did not succeed")]
    NotSucceeded,
    #[error("review attempt names no reviewer")]
    NoReviewer,
    #[error("review attempt retained no output")]
    NoOutput,
    #[error("review attempt retained more than one output")]
    MultipleOutputs,
    #[error("review attempt has no observed route")]
    NoObservedRoute,
    #[error("review attempt belongs to a different delivery")]
    Delivery,
    #[error("review attempt reviewed a different head than the subject")]
    Head,
    #[error(transparent)]
    Review(#[from] ReviewError),
}

pub fn review_fact(
    attempt: &AttemptFact,
    claim: &ReviewClaim,
    subject: &Subject,
    delivery: DeliveryId,
    policy: &ReviewPolicy,
) -> Result<ReviewFact, BindError> {
    if attempt.terminal_state != AttemptOutcome::Succeeded {
        return Err(BindError::NotSucceeded);
    }
    if attempt.delivery_id != Some(delivery) {
        return Err(BindError::Delivery);
    }
    let reviewer = attempt.reviewer.as_ref().ok_or(BindError::NoReviewer)?;
    if attempt.output_refs.len() > 1 {
        return Err(BindError::MultipleOutputs);
    }
    let output = attempt.output_refs.first().ok_or(BindError::NoOutput)?;
    match subject {
        Subject::Pr { head, .. } if attempt.head.as_ref() != Some(head) => {
            return Err(BindError::Head);
        }
        _ => {}
    }
    let evidence = EvidenceRef {
        extra: Default::default(),
        path: output.path.clone(),
        digest: Digest::from_bytes(*output.digest.bytes()),
    };
    let route = attempt
        .observed_route
        .as_ref()
        .or(attempt.resolved_route.as_ref())
        .map(|r| {
            format!(
                "{}/{}@{}",
                r.provider.as_deref().unwrap_or("unknown"),
                r.model.as_deref().unwrap_or("unknown"),
                r.effort.as_deref().unwrap_or("default")
            )
        })
        .unwrap_or_else(|| "unknown".into());
    let (provenance, verified_model_family) = match reviewer.provenance {
        ReviewerProvenance::Observed => {
            let observed = attempt
                .observed_route
                .as_ref()
                .ok_or(BindError::NoObservedRoute)?;
            let adapter = observed
                .provider
                .clone()
                .filter(|provider| !provider.is_empty())
                .ok_or(BindError::NoObservedRoute)?;
            let route = format!(
                "{}/{}@{}",
                adapter,
                observed.model.as_deref().unwrap_or("unknown"),
                observed.effort.as_deref().unwrap_or("default")
            );
            (
                RouteProvenance::Observed {
                    extra: Default::default(),
                    route,
                    tier: claim.tier,
                    adapter,
                    endpoint_provider: None,
                    model_family: reviewer.family.clone(),
                    evidence: evidence.clone(),
                },
                reviewer.family.clone(),
            )
        }
        ReviewerProvenance::ChairAttested => (
            RouteProvenance::ChairAttested {
                extra: Default::default(),
                route,
                tier: claim.tier,
                declared_family: reviewer.family.clone(),
                evidence: evidence.clone(),
            },
            None,
        ),
        ReviewerProvenance::Unknown => (
            RouteProvenance::Unknown {
                extra: Default::default(),
            },
            None,
        ),
    };
    Ok(review::fact(
        ReviewFact {
            attempt_id: attempt.attempt_id,
            reviewer_id: reviewer.actor_id,
            subject: subject.clone(),
            output: evidence,
            verdict: claim.verdict,
            blocking_findings: claim.blocking_findings.clone(),
            eligibility: ReviewEligibility {
                provenance,
                verified_model_family,
            },
            lens: claim.lens.clone(),
        },
        subject,
        policy,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::*;
    use provenant_records::{FileDigest, OutputRef, PathWire, ReviewerFact, Usage};

    fn attempt() -> AttemptFact {
        let path = PathWire::from_relative(&RelativePath::parse("out.md").unwrap()).unwrap();
        AttemptFact {
            context: None,
            attempt_id: AttemptId::from_bytes([1; 16]),
            task_id: TaskId::from_bytes([2; 16]),
            attempt_number: 1,
            operation_id: OperationId::parse("op").unwrap(),
            project_id: ProjectId::from_bytes([4; 16]),
            terminal_state: AttemptOutcome::Succeeded,
            exit_code: Some(0),
            queued_at: UtcMicros::from_micros(1).unwrap(),
            started_at: None,
            provider_ms: None,
            finished_at: UtcMicros::from_micros(2).unwrap(),
            resolved_route: None,
            usage: Usage::default(),
            reviewer: Some(ReviewerFact {
                provenance: ReviewerProvenance::Observed,
                actor_id: ActorId::from_bytes([5; 16]),
                family: Some("openai".into()),
                model: None,
            }),
            delivery_id: Some(DeliveryId::from_bytes([9; 16])),
            subject: None,
            head: Some(GitOid::Sha1([6; 20])),
            output_refs: vec![OutputRef {
                path,
                digest: FileDigest::from_bytes([7; 32]),
            }],
            requested_route: None,
            applied_route: None,
            observed_route: Some(provenant_records::RouteFact {
                provider: Some("codex".into()),
                model: Some("m".into()),
                effort: None,
                account_label: None,
            }),
            verified_owner: None,
            lineage: vec![],
            reason: None,
            trace: Default::default(),
            transcript: None,
        }
    }
    fn subject() -> Subject {
        Subject::Pr {
            extra: Default::default(),
            repository: RepoId::from_bytes([8; 16]),
            number: 1,
            head: GitOid::Sha1([6; 20]),
        }
    }
    fn claim() -> ReviewClaim {
        ReviewClaim {
            lens: "correctness".into(),
            verdict: ReviewVerdict::Approve,
            blocking_findings: vec![],
            tier: ModelTier::Senior,
        }
    }
    fn bind(a: &AttemptFact, s: &Subject) -> Result<ReviewFact, BindError> {
        review_fact(
            a,
            &claim(),
            s,
            DeliveryId::from_bytes([9; 16]),
            &ReviewPolicy::default(),
        )
    }

    #[test]
    fn observed_reviewer_family_is_verified_and_attested_is_not() {
        let fact = bind(&attempt(), &subject()).unwrap();
        assert_eq!(
            fact.eligibility.verified_model_family.as_deref(),
            Some("openai")
        );
        let mut a = attempt();
        a.reviewer.as_mut().unwrap().provenance = ReviewerProvenance::ChairAttested;
        let fact = bind(&a, &subject()).unwrap();
        assert_eq!(fact.eligibility.verified_model_family, None);
    }

    #[test]
    fn failed_headless_wrong_head_and_foreign_delivery_attempts_are_refused() {
        let mut a = attempt();
        a.terminal_state = AttemptOutcome::Failed;
        assert_eq!(bind(&a, &subject()), Err(BindError::NotSucceeded));
        let mut a = attempt();
        a.reviewer = None;
        assert_eq!(bind(&a, &subject()), Err(BindError::NoReviewer));
        let mut a = attempt();
        a.head = Some(GitOid::Sha1([0; 20]));
        assert_eq!(bind(&a, &subject()), Err(BindError::Head));
        let mut a = attempt();
        a.delivery_id = Some(DeliveryId::from_bytes([1; 16]));
        assert_eq!(bind(&a, &subject()), Err(BindError::Delivery));
        let mut a = attempt();
        a.delivery_id = None;
        assert_eq!(bind(&a, &subject()), Err(BindError::Delivery));
    }

    #[test]
    fn observed_provenance_requires_the_actual_observed_route() {
        let mut a = attempt();
        a.observed_route = None;
        assert_eq!(
            bind(&a, &subject()).unwrap_err().to_string(),
            "review attempt has no observed route"
        );
    }

    #[test]
    fn review_attempt_with_multiple_outputs_is_rejected() {
        let mut a = attempt();
        a.output_refs.push(a.output_refs[0].clone());
        assert_eq!(
            bind(&a, &subject()).unwrap_err().to_string(),
            "review attempt retained more than one output"
        );
    }
}
