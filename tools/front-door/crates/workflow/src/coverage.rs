//! Pure review coverage: do the bound review facts satisfy the risk tier?
//! Facts arrive from the composition edge as values; this module reads nothing.
use provenant_core::{ActorId, AttemptId, UtcMicros};
use provenant_records::workflow::*;
use serde::Serialize;
use std::collections::BTreeSet;

/// A recorded reason one cross-family leg is unavailable. The reference points at
/// the routing or quota evidence; an expired blocker waives nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegBlocker {
    pub reference: EvidenceRef,
    pub expires_at: UtcMicros,
}
pub struct ReviewContext<'a> {
    pub subject: &'a Subject,
    pub authors: &'a [Author],
    /// Actors who decide acceptance, including the chair. They never count.
    pub decision_makers: &'a [ActorId],
    pub risk: RiskTier,
    pub now: UtcMicros,
    pub policy: &'a ReviewPolicy,
    pub blockers: &'a [LegBlocker],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    /// A review of the exact subject requested changes.
    Failed,
    /// Coverage is short; more or different reviews could still pass.
    Incomplete,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub field: String,
    pub message: String,
    pub remedy: &'static str,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    pub status: Status,
    pub required: u64,
    pub counted: Vec<AttemptId>,
    pub diagnostics: Vec<Diagnostic>,
}

fn required(policy: &ReviewPolicy, risk: RiskTier) -> u64 {
    match risk {
        RiskTier::Routine => policy.routine,
        RiskTier::Substantial => policy.substantial,
        RiskTier::Crucial => policy.crucial,
        RiskTier::Terminal => policy.terminal,
    }
}
fn tier(route: &RouteProvenance) -> Option<ModelTier> {
    match route {
        RouteProvenance::Observed { tier, .. } | RouteProvenance::ChairAttested { tier, .. } => {
            Some(*tier)
        }
        RouteProvenance::Unknown {} => None,
    }
}
fn declared_family(route: &RouteProvenance) -> Option<&str> {
    match route {
        RouteProvenance::Observed { model_family, .. } => model_family.as_deref(),
        RouteProvenance::ChairAttested {
            declared_family, ..
        } => declared_family.as_deref(),
        RouteProvenance::Unknown {} => None,
    }
}
fn lens_key(lens: &str) -> String {
    lens.trim().to_lowercase()
}

struct Candidate<'a> {
    fact: &'a ReviewFact,
    /// The verified reviewer family when it provably differs from every author family.
    cross_family: Option<&'a str>,
}

pub fn check(ctx: &ReviewContext<'_>, facts: &[ReviewFact]) -> ValidationReport {
    let need = required(ctx.policy, ctx.risk);
    let mut diagnostics = Vec::new();
    let mut note = |code, field: String, message: String, remedy| {
        diagnostics.push(Diagnostic {
            code,
            field,
            message,
            remedy,
        })
    };
    let mut failed = false;
    let mut facts: Vec<&ReviewFact> = facts.iter().collect();
    facts.sort_by_key(|f| f.attempt_id);

    let agents: Vec<&RouteProvenance> = ctx
        .authors
        .iter()
        .filter_map(|a| match a {
            Author::Agent { route, .. } => Some(route),
            Author::User { .. } => None,
        })
        .collect();
    let author_tier = agents.iter().filter_map(|r| tier(r)).max();
    if need > 0 && agents.iter().any(|r| tier(r).is_none()) {
        note(
            "author_tier_unknown",
            "authors".into(),
            "an agent author has no recorded route tier".into(),
            "record the author's route and tier before relying on review coverage",
        );
    }
    let author_families: Option<BTreeSet<&str>> =
        agents.iter().map(|r| declared_family(r)).collect();
    let excluded: BTreeSet<ActorId> = ctx
        .authors
        .iter()
        .map(Author::actor_id)
        .chain(ctx.decision_makers.iter().copied())
        .collect();

    let mut seen_reviewers = BTreeSet::new();
    let mut candidates = Vec::new();
    for fact in facts {
        let field = format!("reviews/{}", fact.attempt_id);
        if &fact.subject != ctx.subject {
            note(
                "stale_subject",
                field,
                "review is for a different subject than the receipt".into(),
                "re-run the review against the current subject",
            );
        } else if fact.verdict == ReviewVerdict::RequestChanges {
            failed = true;
            note(
                "blocking_review",
                field,
                format!(
                    "{} blocking finding(s) outstanding",
                    fact.blocking_findings.len()
                ),
                "repair the findings, then obtain a fresh review",
            );
        } else if excluded.contains(&fact.reviewer_id) {
            note(
                "reviewer_not_independent",
                field,
                "reviewer is an author or decision maker".into(),
                "use a reviewer who did not author or decide this work",
            );
        } else if matches!(fact.eligibility.provenance, RouteProvenance::Unknown {}) {
            note(
                "reviewer_route_unknown",
                field,
                "reviewer route is unknown, so the review is findings only".into(),
                "record the reviewer's route from the attempt or host record",
            );
        } else if tier(&fact.eligibility.provenance) < author_tier {
            note(
                "reviewer_below_author_tier",
                field,
                "reviewer tier is below the highest author tier".into(),
                "review with a route at or above the author's tier",
            );
        } else if !seen_reviewers.insert(fact.reviewer_id) {
            note(
                "repeated_reviewer",
                field,
                "this reviewer already supplies coverage".into(),
                "repeating a reviewer adds no coverage",
            );
        } else {
            let cross_family = match (&fact.eligibility, &author_families) {
                (
                    ReviewEligibility {
                        provenance: RouteProvenance::Observed { .. },
                        verified_model_family: Some(family),
                    },
                    Some(families),
                ) if !families.contains(family.as_str()) => Some(family.as_str()),
                _ => None,
            };
            candidates.push(Candidate { fact, cross_family });
        }
    }

    // Adversarial first at terminal risk, then cross-family legs, then identity order.
    let adversarial = lens_key(&ctx.policy.adversarial_lens);
    let terminal = ctx.risk == RiskTier::Terminal;
    candidates.sort_by_key(|c| {
        (
            !(terminal && lens_key(&c.fact.lens) == adversarial),
            c.cross_family.is_none(),
            c.fact.attempt_id,
        )
    });
    let mut lenses = BTreeSet::new();
    let mut families = BTreeSet::new();
    let mut picked: Vec<&Candidate> = Vec::new();
    for candidate in &candidates {
        if picked.len() as u64 >= need {
            break;
        }
        if !lenses.insert(lens_key(&candidate.fact.lens)) {
            continue;
        }
        if let Some(family) = candidate.cross_family
            && !families.insert(family)
        {
            lenses.remove(&lens_key(&candidate.fact.lens));
            continue;
        }
        picked.push(candidate);
    }
    let counted: Vec<AttemptId> = picked.iter().map(|c| c.fact.attempt_id).collect();
    if (picked.len() as u64) < need {
        note(
            "missing_reviews",
            "reviews".into(),
            format!(
                "{} independent review(s) with distinct lenses are required; {} counted",
                need,
                picked.len()
            ),
            "add independent reviews through distinct lenses",
        );
    }
    let cross = picked.iter().filter(|c| c.cross_family.is_some()).count() as u64;
    let waived = ctx
        .blockers
        .iter()
        .filter(|b| b.expires_at > ctx.now)
        .count() as u64;
    if need.saturating_sub(cross) > waived {
        note(
            "missing_cross_family_leg",
            "reviews".into(),
            format!(
                "{} cross-family review leg(s) lack a verified reviewer or an unexpired blocker",
                need.saturating_sub(cross).saturating_sub(waived)
            ),
            "obtain an observed other-family review or record the unavailability with its evidence",
        );
    }
    if terminal && !picked.iter().any(|c| lens_key(&c.fact.lens) == adversarial) {
        note(
            "missing_adversarial_lens",
            "reviews".into(),
            "terminal risk needs a counted adversarial review".into(),
            "request an adversarial review",
        );
    }
    drop(note);
    diagnostics.sort_by(|a, b| (a.code, &a.field).cmp(&(b.code, &b.field)));
    // Routine work needs no review; only a blocking review still fails it.
    if need == 0 {
        diagnostics.retain(|d| matches!(d.code, "blocking_review" | "stale_subject"));
    }
    let incomplete = diagnostics
        .iter()
        .any(|d| !matches!(d.code, "blocking_review" | "stale_subject") || need > 0);
    let status = if failed {
        Status::Failed
    } else if need > 0 && incomplete {
        Status::Incomplete
    } else {
        Status::Passed
    };
    ValidationReport {
        status,
        required: need,
        counted,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{Digest, RelativePath};
    use provenant_records::PathWire;

    fn evidence() -> EvidenceRef {
        EvidenceRef {
            path: PathWire::from_relative(&RelativePath::parse("e.md").unwrap()).unwrap(),
            digest: Digest::from_bytes([0; 32]),
        }
    }
    fn subject() -> Subject {
        Subject::Document {
            path: evidence().path,
            digest: evidence().digest,
        }
    }
    fn observed(tier: ModelTier, family: &str) -> RouteProvenance {
        RouteProvenance::Observed {
            route: "codex/m@high".into(),
            tier,
            adapter: "codex".into(),
            endpoint_provider: None,
            model_family: Some(family.into()),
            evidence: evidence(),
        }
    }
    fn attested(tier: ModelTier) -> RouteProvenance {
        RouteProvenance::ChairAttested {
            route: "native@high".into(),
            tier,
            declared_family: Some("anthropic".into()),
            evidence: evidence(),
        }
    }
    fn author(route: RouteProvenance) -> Author {
        Author::Agent {
            actor_id: ActorId::from_bytes([1; 16]),
            route,
        }
    }
    fn review(n: u8, route: RouteProvenance, verified: Option<&str>, lens: &str) -> ReviewFact {
        ReviewFact {
            attempt_id: AttemptId::from_bytes([n; 16]),
            reviewer_id: ActorId::from_bytes([n; 16]),
            subject: subject(),
            output: evidence(),
            verdict: ReviewVerdict::Approve,
            blocking_findings: vec![],
            eligibility: ReviewEligibility {
                provenance: route,
                verified_model_family: verified.map(Into::into),
            },
            lens: lens.into(),
        }
    }
    fn run(
        risk: RiskTier,
        authors: &[Author],
        facts: &[ReviewFact],
        blockers: &[LegBlocker],
    ) -> ValidationReport {
        let subject = subject();
        check(
            &ReviewContext {
                subject: &subject,
                authors,
                decision_makers: &[ActorId::from_bytes([99; 16])],
                risk,
                now: UtcMicros::from_micros(1_000).unwrap(),
                policy: &ReviewPolicy::default(),
                blockers,
            },
            facts,
        )
    }
    fn codes(report: &ValidationReport) -> Vec<&'static str> {
        report.diagnostics.iter().map(|d| d.code).collect()
    }
    fn blocker(expires: i64) -> LegBlocker {
        LegBlocker {
            reference: evidence(),
            expires_at: UtcMicros::from_micros(expires).unwrap(),
        }
    }

    #[test]
    fn routine_needs_nothing_but_a_blocking_review_still_fails() {
        let authors = [author(attested(ModelTier::Senior))];
        assert_eq!(
            run(RiskTier::Routine, &authors, &[], &[]).status,
            Status::Passed
        );
        let mut bad = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "x",
        );
        bad.verdict = ReviewVerdict::RequestChanges;
        bad.blocking_findings = vec!["bug".into()];
        let report = run(RiskTier::Routine, &authors, &[bad], &[]);
        assert_eq!(report.status, Status::Failed);
        assert_eq!(codes(&report), ["blocking_review"]);
    }

    #[test]
    fn substantial_passes_with_an_observed_other_family_review() {
        let authors = [author(attested(ModelTier::Senior))];
        let good = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "correctness",
        );
        let report = run(RiskTier::Substantial, &authors, &[good], &[]);
        assert_eq!(report.status, Status::Passed);
        assert_eq!(report.counted.len(), 1);
    }

    #[test]
    fn attested_reviewer_never_fills_a_cross_family_leg_without_a_blocker() {
        let authors = [author(attested(ModelTier::Senior))];
        let same = review(2, attested(ModelTier::Senior), None, "correctness");
        let report = run(
            RiskTier::Substantial,
            &authors,
            std::slice::from_ref(&same),
            &[],
        );
        assert_eq!(report.status, Status::Incomplete);
        assert_eq!(codes(&report), ["missing_cross_family_leg"]);
        let waived = run(
            RiskTier::Substantial,
            &authors,
            std::slice::from_ref(&same),
            &[blocker(2_000)],
        );
        assert_eq!(waived.status, Status::Passed);
        let expired = run(RiskTier::Substantial, &authors, &[same], &[blocker(1_000)]);
        assert_eq!(expired.status, Status::Incomplete);
    }

    #[test]
    fn authors_decision_makers_repeats_and_low_tiers_do_not_count() {
        let authors = [author(attested(ModelTier::Principal))];
        let as_author = review(
            1,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "a",
        );
        let as_chair = review(
            99,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "b",
        );
        let junior = review(
            3,
            observed(ModelTier::Junior, "openai"),
            Some("openai"),
            "c",
        );
        let unknown = review(4, RouteProvenance::Unknown {}, None, "d");
        let report = run(
            RiskTier::Substantial,
            &authors,
            &[as_author, as_chair, junior, unknown],
            &[],
        );
        assert_eq!(report.status, Status::Incomplete);
        assert_eq!(
            codes(&report),
            [
                "missing_cross_family_leg",
                "missing_reviews",
                "reviewer_below_author_tier",
                "reviewer_not_independent",
                "reviewer_not_independent",
                "reviewer_route_unknown",
            ]
        );
        let first = review(
            2,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "a",
        );
        let mut again = first.clone();
        again.attempt_id = AttemptId::from_bytes([5; 16]);
        let report = run(RiskTier::Crucial, &authors, &[first, again], &[]);
        assert!(codes(&report).contains(&"repeated_reviewer"));
    }

    #[test]
    fn unknown_author_tier_blocks_substantial_coverage() {
        let authors = [author(RouteProvenance::Unknown {})];
        let good = review(
            2,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "a",
        );
        let report = run(RiskTier::Substantial, &authors, &[good], &[]);
        assert_eq!(report.status, Status::Incomplete);
        assert!(codes(&report).contains(&"author_tier_unknown"));
    }

    #[test]
    fn crucial_needs_two_distinct_lenses_and_terminal_an_adversarial_one() {
        let authors = [author(attested(ModelTier::Senior))];
        let a = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "correctness",
        );
        let b = review(
            3,
            observed(ModelTier::Senior, "google"),
            Some("google"),
            "security",
        );
        let same_lens = review(
            4,
            observed(ModelTier::Senior, "xai"),
            Some("xai"),
            "correctness",
        );
        let report = run(RiskTier::Crucial, &authors, &[a.clone(), same_lens], &[]);
        assert!(codes(&report).contains(&"missing_reviews"));
        let report = run(RiskTier::Crucial, &authors, &[a.clone(), b.clone()], &[]);
        assert_eq!(report.status, Status::Passed);
        let report = run(RiskTier::Terminal, &authors, &[a.clone(), b], &[]);
        assert_eq!(codes(&report), ["missing_adversarial_lens"]);
        let adv = review(
            5,
            observed(ModelTier::Senior, "google"),
            Some("google"),
            "adversarial",
        );
        let report = run(RiskTier::Terminal, &authors, &[a, adv], &[]);
        assert_eq!(report.status, Status::Passed);
    }

    #[test]
    fn a_review_of_another_subject_never_counts() {
        let authors = [author(attested(ModelTier::Senior))];
        let mut stale = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "a",
        );
        stale.subject = Subject::Document {
            path: evidence().path,
            digest: Digest::from_bytes([7; 32]),
        };
        let report = run(RiskTier::Substantial, &authors, &[stale], &[]);
        assert_eq!(report.status, Status::Incomplete);
        assert!(codes(&report).contains(&"stale_subject"));
    }
}
