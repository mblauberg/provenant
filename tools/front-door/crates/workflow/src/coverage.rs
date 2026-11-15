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
macro_rules! declare_diagnostic_codes {
    ($($code:ident),+ $(,)?) => {
        /// Stable machine-readable reasons returned by review-coverage evaluation.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum DiagnosticCode { $($code),+ }

        impl DiagnosticCode {
            /// Every emitted reason is paired with a coverage test case.
            pub const ALL: &'static [Self] = &[$(Self::$code),+];
        }
    };
}

declare_diagnostic_codes! {
    AuthorTierUnknown,
    StaleSubject,
    BlockingReview,
    InconclusiveReview,
    ReviewerNotIndependent,
    ReviewerRouteUnknown,
    ReviewerBelowAuthorTier,
    RepeatedReviewer,
    MissingReviews,
    MissingCrossFamilyLeg,
    MissingAdversarialLens,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
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
        RouteProvenance::Unknown { .. } => None,
    }
}
/// Most candidates the exhaustive selection considers (2^16 subsets).
const MAX_SEARCH: usize = 16;

fn declared_family(route: &RouteProvenance) -> Option<&str> {
    match route {
        RouteProvenance::Observed { model_family, .. } => model_family.as_deref(),
        RouteProvenance::ChairAttested {
            declared_family, ..
        } => declared_family.as_deref(),
        RouteProvenance::Unknown { .. } => None,
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
            Author::Agent { route, .. } => Some(route.as_ref()),
            Author::User { .. } => None,
        })
        .collect();
    let author_tier = agents.iter().filter_map(|r| tier(r)).max();
    if need > 0 && agents.iter().any(|r| tier(r).is_none()) {
        note(
            DiagnosticCode::AuthorTierUnknown,
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
                DiagnosticCode::StaleSubject,
                field,
                "review is for a different subject than the receipt".into(),
                "re-run the review against the current subject",
            );
        } else if fact.verdict == ReviewVerdict::RequestChanges {
            failed = true;
            note(
                DiagnosticCode::BlockingReview,
                field,
                format!(
                    "{} blocking finding(s) outstanding",
                    fact.blocking_findings.len()
                ),
                "repair the findings, then obtain a fresh review",
            );
        } else if fact.verdict == ReviewVerdict::Inconclusive {
            note(
                DiagnosticCode::InconclusiveReview,
                field,
                "review is inconclusive and supplies no coverage".into(),
                "obtain a fresh review with a passing verdict",
            );
        } else if excluded.contains(&fact.reviewer_id) {
            note(
                DiagnosticCode::ReviewerNotIndependent,
                field,
                "reviewer is an author or decision maker".into(),
                "use a reviewer who did not author or decide this work",
            );
        } else if matches!(fact.eligibility.provenance, RouteProvenance::Unknown { .. }) {
            note(
                DiagnosticCode::ReviewerRouteUnknown,
                field,
                "reviewer route is unknown, so the review is findings only".into(),
                "record the reviewer's route from the attempt or host record",
            );
        } else if tier(&fact.eligibility.provenance) < author_tier {
            note(
                DiagnosticCode::ReviewerBelowAuthorTier,
                field,
                "reviewer tier is below the highest author tier".into(),
                "review with a route at or above the author's tier",
            );
        } else if !seen_reviewers.insert(fact.reviewer_id) {
            note(
                DiagnosticCode::RepeatedReviewer,
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
    // Exhaustive choice over a bounded candidate list: distinct lenses and
    // distinct cross-family reviewers, preferring an adversarial leg at
    // terminal risk, then more counted reviews, then more cross-family legs.
    let pool = &candidates[..candidates.len().min(MAX_SEARCH)];
    let mut best: Option<(u32, (bool, usize, usize))> = None;
    for mask in 0u32..(1u32 << pool.len()) {
        let size = mask.count_ones() as u64;
        if size > need {
            continue;
        }
        let mut lenses = BTreeSet::new();
        let mut families = BTreeSet::new();
        let mut valid = true;
        let (mut adv, mut cross) = (false, 0);
        for (idx, c) in pool.iter().enumerate() {
            if mask & (1 << idx) == 0 {
                continue;
            }
            let lens = lens_key(&c.fact.lens);
            adv |= terminal && lens == adversarial;
            if !lenses.insert(lens) || c.cross_family.is_some_and(|f| !families.insert(f)) {
                valid = false;
                break;
            }
            cross += usize::from(c.cross_family.is_some());
        }
        if !valid {
            continue;
        }
        let score = (adv, size as usize, cross);
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((mask, score));
        }
    }
    let best_mask = best.map_or(0, |(m, _)| m);
    let picked: Vec<&Candidate> = pool
        .iter()
        .enumerate()
        .filter(|(idx, _)| best_mask & (1 << idx) != 0)
        .map(|(_, c)| c)
        .collect();
    let counted: Vec<AttemptId> = picked.iter().map(|c| c.fact.attempt_id).collect();
    if (picked.len() as u64) < need {
        note(
            DiagnosticCode::MissingReviews,
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
    let mut blocker_evidence = Vec::new();
    for blocker in ctx.blockers.iter().filter(|b| b.expires_at > ctx.now) {
        if !blocker_evidence.contains(&&blocker.reference) {
            blocker_evidence.push(&blocker.reference);
        }
    }
    let waived = (need.saturating_sub(cross) as usize).min(blocker_evidence.len()) as u64;
    if need.saturating_sub(cross) > waived {
        note(
            DiagnosticCode::MissingCrossFamilyLeg,
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
            DiagnosticCode::MissingAdversarialLens,
            "reviews".into(),
            "terminal risk needs a counted adversarial review".into(),
            "request an adversarial review",
        );
    }
    drop(note);
    diagnostics.sort_by(|a, b| (a.code, &a.field).cmp(&(b.code, &b.field)));
    // Routine work needs no review; only a blocking review still fails it.
    if need == 0 {
        diagnostics.retain(|d| {
            matches!(
                d.code,
                DiagnosticCode::BlockingReview
                    | DiagnosticCode::StaleSubject
                    | DiagnosticCode::InconclusiveReview
            )
        });
    }
    let incomplete = diagnostics.iter().any(|d| {
        matches!(
            d.code,
            DiagnosticCode::MissingReviews
                | DiagnosticCode::MissingCrossFamilyLeg
                | DiagnosticCode::MissingAdversarialLens
                | DiagnosticCode::AuthorTierUnknown
        )
    });
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
            extra: Default::default(),
            path: PathWire::from_relative(&RelativePath::parse("e.md").unwrap()).unwrap(),
            digest: Digest::from_bytes([0; 32]),
        }
    }
    fn subject() -> Subject {
        Subject::Document {
            extra: Default::default(),
            path: evidence().path,
            digest: evidence().digest,
        }
    }
    fn observed(tier: ModelTier, family: &str) -> RouteProvenance {
        RouteProvenance::Observed {
            extra: Default::default(),
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
            extra: Default::default(),
            route: "native@high".into(),
            tier,
            declared_family: Some("anthropic".into()),
            evidence: evidence(),
        }
    }
    fn author(route: RouteProvenance) -> Author {
        Author::Agent {
            extra: Default::default(),
            actor_id: ActorId::from_bytes([1; 16]),
            route: Box::new(route),
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
    fn codes(report: &ValidationReport) -> Vec<DiagnosticCode> {
        report.diagnostics.iter().map(|d| d.code).collect()
    }
    fn blocker(expires: i64) -> LegBlocker {
        LegBlocker {
            reference: evidence(),
            expires_at: UtcMicros::from_micros(expires).unwrap(),
        }
    }

    #[test]
    fn every_declared_diagnostic_code_has_an_emission_case() {
        let unknown_author = [author(RouteProvenance::Unknown {
            extra: Default::default(),
        })];
        let mut stale = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "stale",
        );
        stale.subject = Subject::Document {
            extra: Default::default(),
            path: evidence().path,
            digest: Digest::from_bytes([7; 32]),
        };
        let mut blocking = review(
            3,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "blocking",
        );
        blocking.verdict = ReviewVerdict::RequestChanges;
        let mut inconclusive = review(
            4,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "inconclusive",
        );
        inconclusive.verdict = ReviewVerdict::Inconclusive;
        let mut self_review = review(
            5,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "self",
        );
        self_review.reviewer_id = ActorId::from_bytes([1; 16]);
        let unknown_route = review(
            6,
            RouteProvenance::Unknown {
                extra: Default::default(),
            },
            None,
            "unknown",
        );
        let incomplete = run(
            RiskTier::Terminal,
            &unknown_author,
            &[stale, blocking, inconclusive, self_review, unknown_route],
            &[],
        );

        let principal_author = [author(attested(ModelTier::Principal))];
        let junior = review(
            7,
            observed(ModelTier::Junior, "openai"),
            Some("openai"),
            "junior",
        );
        let repeated = review(
            8,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "review",
        );
        let mut duplicate = repeated.clone();
        duplicate.attempt_id = AttemptId::from_bytes([9; 16]);
        let repeated = run(
            RiskTier::Substantial,
            &principal_author,
            &[junior, repeated, duplicate],
            &[],
        );

        let emitted: BTreeSet<_> = incomplete
            .diagnostics
            .iter()
            .chain(&repeated.diagnostics)
            .map(|diagnostic| diagnostic.code)
            .collect();
        let declared: BTreeSet<_> = DiagnosticCode::ALL.iter().copied().collect();
        assert_eq!(emitted, declared);
    }

    #[test]
    fn inconclusive_review_is_not_counted_and_never_blocks() {
        let authors = [author(attested(ModelTier::Senior))];
        let mut inconclusive = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "correctness",
        );
        inconclusive.verdict = ReviewVerdict::Inconclusive;
        inconclusive.blocking_findings = vec!["unresolved concern".into()];
        let inconclusive = crate::review::fact(
            inconclusive.clone(),
            &inconclusive.subject,
            &ReviewPolicy::default(),
        )
        .unwrap();
        for (risk, expected) in [
            (RiskTier::Routine, Status::Passed),
            (RiskTier::Substantial, Status::Incomplete),
        ] {
            let report = run(risk, &authors, std::slice::from_ref(&inconclusive), &[]);
            assert_eq!(report.status, expected);
            assert!(report.counted.is_empty());
            assert!(codes(&report).contains(&DiagnosticCode::InconclusiveReview));
            assert!(!codes(&report).contains(&DiagnosticCode::BlockingReview));
        }
        let good = review(
            3,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "correctness",
        );
        let report = run(RiskTier::Substantial, &authors, &[inconclusive, good], &[]);
        assert_eq!(report.status, Status::Passed);
        assert_eq!(report.counted.len(), 1);
        assert!(codes(&report).contains(&DiagnosticCode::InconclusiveReview));
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
        assert!(codes(&report).contains(&DiagnosticCode::BlockingReview));
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
    fn surplus_ineligible_review_does_not_block_satisfied_coverage() {
        let authors = [author(attested(ModelTier::Senior))];
        let good = review(
            2,
            observed(ModelTier::Senior, "openai"),
            Some("openai"),
            "correctness",
        );
        let extra = review(
            3,
            RouteProvenance::Unknown {
                extra: Default::default(),
            },
            None,
            "security",
        );
        let report = run(RiskTier::Substantial, &authors, &[good, extra], &[]);
        assert_eq!(report.status, Status::Passed);
        assert_eq!(report.counted.len(), 1);
        assert!(codes(&report).contains(&DiagnosticCode::ReviewerRouteUnknown));
    }

    #[test]
    fn selection_finds_distinct_lens_and_family_legs_a_greedy_pick_would_miss() {
        let authors = [author(attested(ModelTier::Senior))];
        let reviews = [
            review(
                2,
                observed(ModelTier::Senior, "openai"),
                Some("openai"),
                "correctness",
            ),
            review(
                3,
                observed(ModelTier::Senior, "google"),
                Some("google"),
                "correctness",
            ),
            review(
                4,
                observed(ModelTier::Senior, "openai"),
                Some("openai"),
                "security",
            ),
        ];
        let report = run(RiskTier::Crucial, &authors, &reviews, &[]);
        assert_eq!(report.status, Status::Passed);
        assert_eq!(report.counted.len(), 2);
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
        assert!(codes(&report).contains(&DiagnosticCode::MissingCrossFamilyLeg));
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
    fn duplicate_blocker_evidence_waives_only_one_cross_family_leg() {
        let authors = [author(observed(ModelTier::Senior, "openai"))];
        let reviews = [
            review(
                2,
                observed(ModelTier::Senior, "openai"),
                Some("openai"),
                "correctness",
            ),
            review(
                3,
                observed(ModelTier::Senior, "openai"),
                Some("openai"),
                "security",
            ),
        ];
        let same_blocker = blocker(2_000);
        let report = run(
            RiskTier::Crucial,
            &authors,
            &reviews,
            &[same_blocker.clone(), same_blocker],
        );
        assert_eq!(report.status, Status::Incomplete);
        assert!(codes(&report).contains(&DiagnosticCode::MissingCrossFamilyLeg));
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
        let unknown = review(
            4,
            RouteProvenance::Unknown {
                extra: Default::default(),
            },
            None,
            "d",
        );
        let report = run(
            RiskTier::Substantial,
            &authors,
            &[as_author, as_chair, junior, unknown],
            &[],
        );
        assert_eq!(report.status, Status::Incomplete);
        for code in [
            DiagnosticCode::MissingCrossFamilyLeg,
            DiagnosticCode::MissingReviews,
            DiagnosticCode::ReviewerBelowAuthorTier,
            DiagnosticCode::ReviewerNotIndependent,
            DiagnosticCode::ReviewerRouteUnknown,
        ] {
            assert!(codes(&report).contains(&code));
        }
        let first = review(
            2,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "a",
        );
        let mut again = first.clone();
        again.attempt_id = AttemptId::from_bytes([5; 16]);
        let report = run(RiskTier::Crucial, &authors, &[first, again], &[]);
        assert!(codes(&report).contains(&DiagnosticCode::RepeatedReviewer));
    }

    #[test]
    fn unknown_author_tier_blocks_substantial_coverage() {
        let authors = [author(RouteProvenance::Unknown {
            extra: Default::default(),
        })];
        let good = review(
            2,
            observed(ModelTier::Principal, "openai"),
            Some("openai"),
            "a",
        );
        let report = run(RiskTier::Substantial, &authors, &[good], &[]);
        assert_eq!(report.status, Status::Incomplete);
        assert!(codes(&report).contains(&DiagnosticCode::AuthorTierUnknown));
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
        assert!(codes(&report).contains(&DiagnosticCode::MissingReviews));
        let report = run(RiskTier::Crucial, &authors, &[a.clone(), b.clone()], &[]);
        assert_eq!(report.status, Status::Passed);
        let report = run(RiskTier::Terminal, &authors, &[a.clone(), b], &[]);
        assert!(codes(&report).contains(&DiagnosticCode::MissingAdversarialLens));
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
            extra: Default::default(),
            path: evidence().path,
            digest: Digest::from_bytes([7; 32]),
        };
        let report = run(RiskTier::Substantial, &authors, &[stale], &[]);
        assert_eq!(report.status, Status::Incomplete);
        assert!(codes(&report).contains(&DiagnosticCode::StaleSubject));
    }
}
