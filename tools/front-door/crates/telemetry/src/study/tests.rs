use super::*;

const DAY: i64 = 86_400;

fn spec(label: &str, from: i64, until: i64) -> CohortSpec {
    CohortSpec {
        label: label.into(),
        from,
        until,
    }
}

fn attempt(id: &str, at: i64, item: Option<&str>, role: &str, tokens: Option<u64>) -> AttemptRow {
    AttemptRow {
        id: id.into(),
        finished: at,
        work_item: item.map(str::to_owned),
        role: Some(role.into()),
        failed: false,
        route: Some("anthropic/sonnet".into()),
        tokens,
        cache_read: Some(1),
    }
}

/// `n` PRs per cohort; baseline in [0, 100d), enabled in [100d, 200d).
/// Each PR gets a worker attempt of `base`/`on` tokens and a chair session.
fn fixture(n: u64, base: u64, on: u64, chair_read: bool) -> StudyInput {
    let mut input = StudyInput {
        cut_over: CutOver {
            commit: "abc123".into(),
            date: "2026-11-02".into(),
        },
        baseline: spec("baseline", 0, 100 * DAY),
        enabled: spec("enabled", 100 * DAY, 200 * DAY),
        prs: vec![],
        attempts: vec![],
        chair: vec![],
    };
    for i in 0..2 * n {
        let enabled = i >= n;
        let at = if enabled { 150 * DAY } else { 50 * DAY } + i as i64;
        let number = 1000 + i;
        input.prs.push(MergedPr {
            number,
            merged: at,
            lines: Some(if i % 2 == 0 { 20 } else { 500 }),
            work_items: vec![],
        });
        let label = format!("#{number}");
        input.attempts.push(attempt(
            &format!("w{i}"),
            at,
            Some(&label),
            "worker",
            Some(if enabled { on } else { base }),
        ));
        input.chair.push(ChairRow {
            session: format!("c{i}"),
            finished: at,
            pr: Some(number),
            tokens: chair_read.then_some(10),
        });
    }
    input
}

#[test]
fn complete_cohorts_are_comparable_and_split_categories_once() {
    let mut input = fixture(10, 100, 50, true);
    let mut failed = attempt("f", 50 * DAY, Some("#1000"), "worker", Some(7));
    failed.failed = true;
    input.attempts.push(failed);
    input
        .attempts
        .push(attempt("r", 50 * DAY, Some("pr:1000"), "Reviewer", Some(5)));
    input
        .attempts
        .push(attempt("x", 50 * DAY, Some("#1000"), "repair", Some(3)));
    let study = reduce(&input).unwrap();
    assert_eq!(study.verdict, Verdict::Comparable, "{:?}", study.reasons);
    let c = &study.baseline.by_category;
    assert_eq!(
        (c.chair, c.worker, c.review, c.repair, c.failed_attempt),
        (100, 1000, 5, 3, 7)
    );
    assert_eq!(c.total(), study.baseline.per_pr.total);
    assert_eq!(study.baseline.per_pr.median, Some(110));
    assert_eq!(study.enabled.per_pr.median, Some(60));
    assert_eq!(study.median_change_basis_points, Some(-4545));
    assert_eq!(study.baseline.by_size["small"].prs, 5);
    assert_eq!(study.baseline.by_size["large"].prs, 5);
    assert_eq!(study.baseline.route_tokens["anthropic/sonnet"], 1015);
    assert_eq!(study.baseline.coverage.linked_basis_points, Some(10_000));
}

#[test]
fn small_cohort_is_pending() {
    let study = reduce(&fixture(9, 100, 50, true)).unwrap();
    assert_eq!(study.verdict, Verdict::Pending);
    assert_eq!(study.mean_change_basis_points, None);
}

#[test]
fn missing_chair_counters_are_inconclusive() {
    let study = reduce(&fixture(10, 100, 50, false)).unwrap();
    assert_eq!(study.verdict, Verdict::Inconclusive);
    assert_eq!(study.baseline.coverage.chair_unread, 10);
    assert_eq!(study.median_change_basis_points, None);
}

#[test]
fn unlinked_and_abandoned_work_sits_beside_totals() {
    let mut input = fixture(10, 100, 50, true);
    input
        .attempts
        .push(attempt("u", 50 * DAY, None, "worker", Some(40)));
    input
        .attempts
        .push(attempt("a", 50 * DAY, Some("#9999"), "worker", Some(30)));
    input.chair.push(ChairRow {
        session: "orphan".into(),
        finished: 50 * DAY,
        pr: None,
        tokens: Some(20),
    });
    let study = reduce(&input).unwrap();
    let b = &study.baseline.beside;
    assert_eq!((b.unlinked_attempts, b.unlinked_tokens), (1, 40));
    assert_eq!((b.abandoned_attempts, b.abandoned_tokens), (1, 30));
    assert_eq!(
        (b.unlinked_chair_sessions, b.unlinked_chair_tokens),
        (1, 20)
    );
    // Not folded into the per-PR totals.
    assert_eq!(study.baseline.per_pr.total, 1100);
    assert_eq!(study.baseline.coverage.linked_basis_points, Some(8_333));
    // 83% vs 100% attribution is a material gap, so no comparison is drawn.
    assert_eq!(study.verdict, Verdict::Inconclusive);
}

#[test]
fn materially_different_attribution_coverage_is_inconclusive() {
    let mut input = fixture(10, 100, 50, true);
    for i in 0..10 {
        input.attempts.push(attempt(
            &format!("n{i}"),
            150 * DAY,
            None,
            "worker",
            Some(1),
        ));
    }
    let study = reduce(&input).unwrap();
    assert_eq!(study.verdict, Verdict::Inconclusive);
    assert!(study.reasons.iter().any(|r| r.contains("attribution")));
}

#[test]
fn ambiguous_label_joins_nothing() {
    let mut input = fixture(10, 100, 50, true);
    input.prs[0].work_items = vec!["shared".into()];
    input.prs[1].work_items = vec!["shared".into()];
    input
        .attempts
        .push(attempt("s", 50 * DAY, Some("shared"), "worker", Some(9)));
    let study = reduce(&input).unwrap();
    assert_eq!(study.baseline.beside.unlinked_tokens, 9);
}

#[test]
fn invalid_input_is_refused() {
    let mut input = fixture(10, 1, 1, true);
    input.attempts.push(input.attempts[0].clone());
    assert!(reduce(&input).is_err());
    let mut input = fixture(10, 1, 1, true);
    input.enabled.from = 50 * DAY;
    assert!(reduce(&input).is_err());
}

#[test]
fn reduction_is_deterministic() {
    let input = fixture(10, 100, 50, true);
    assert_eq!(
        render(&reduce(&input).unwrap()),
        render(&reduce(&input).unwrap())
    );
}

#[test]
fn even_median_rounds_down() {
    assert_eq!(spread(vec![1, 2]).median, Some(1));
    assert_eq!(spread(vec![u64::MAX, u64::MAX]).median, Some(u64::MAX));
}

#[test]
fn partial_attempt_or_chair_counters_are_inconclusive_and_named() {
    let mut input = fixture(10, 100, 50, true);
    input.attempts[0].tokens = None;
    input.attempts[10].tokens = None;
    let study = reduce(&input).unwrap();
    assert_eq!(study.verdict, Verdict::Inconclusive);
    for label in ["baseline", "enabled"] {
        assert!(
            study
                .reasons
                .iter()
                .any(|r| r.contains(label) && r.contains("attempts without token")),
            "{:?}",
            study.reasons
        );
    }

    // Identical 60% read coverage in both cohorts must still not compare.
    let mut input = fixture(10, 100, 50, true);
    for i in (0..20).filter(|i| i % 10 < 4) {
        input.chair[i].tokens = None;
    }
    let study = reduce(&input).unwrap();
    assert_eq!(study.verdict, Verdict::Inconclusive);
    assert_eq!(study.median_change_basis_points, None);
    assert!(
        study
            .reasons
            .iter()
            .any(|r| r.contains("4 chair sessions with unread")),
        "{:?}",
        study.reasons
    );
}

#[test]
fn work_is_attributed_to_the_cohort_of_the_pr_merge_time() {
    let mut input = fixture(10, 100, 50, true);
    // Finished before the cut-over, for a PR merged after it.
    input
        .attempts
        .push(attempt("early", 99 * DAY, Some("#1010"), "worker", Some(7)));
    input.chair.push(ChairRow {
        session: "early-chair".into(),
        finished: 99 * DAY,
        pr: Some(1010),
        tokens: Some(3),
    });
    let study = reduce(&input).unwrap();
    assert_eq!(study.enabled.by_category.worker, 10 * 50 + 7);
    assert_eq!(study.enabled.by_category.chair, 10 * 10 + 3);
    assert_eq!(study.baseline.by_category.worker, 10 * 100);
    assert_eq!(study.baseline.by_category.chair, 10 * 10);
    assert_eq!(study.baseline.coverage.window_attempts, 10);
    assert_eq!(study.enabled.coverage.window_attempts, 11);
    assert_eq!(study.enabled.coverage.linked_attempts, 11);
    assert_eq!(study.enabled.coverage.chair_sessions, 11);
    assert_eq!(study.baseline.coverage.chair_sessions, 10);
}

#[test]
fn duplicate_chair_sessions_are_refused() {
    let mut input = fixture(10, 1, 1, true);
    input.chair.push(input.chair[0].clone());
    assert_eq!(reduce(&input).unwrap_err(), "duplicate chair session");
}

#[test]
fn render_reports_intervals_and_coverage() {
    let text = render(&reduce(&fixture(10, 100, 50, true)).unwrap());
    for needle in [
        "interval: from=0 until=8640000",
        "interval: from=8640000 until=17280000",
        "attribution: linked=10/10 bp=Some(10000)",
        "chair: sessions=10 linked=10 unread=0 prs_with_chair_bp=Some(10000)",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
}
