use std::{ffi::OsString, path::Path, time::Duration};

use provenant_telemetry::{
    outcome::{self, Attributed, CiRun, Evidence, Outcomes, Signals},
    suggest,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn git(repo: &Path, args: &[&str], day: u32) -> Result<String> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
        .collect();
    let date = format!("2026-01-{day:02}T12:00:00Z");
    env.extend(
        [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_AUTHOR_NAME", "Fixture"),
            ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
            ("GIT_COMMITTER_NAME", "Fixture"),
            ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
            ("GIT_AUTHOR_DATE", &date),
            ("GIT_COMMITTER_DATE", &date),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    let mut argv = vec![
        provenant_os::git::git_executable()
            .ok_or("Git unavailable")?
            .into_os_string(),
    ];
    argv.extend(args.iter().map(OsString::from));
    let mut options = ExecOptions::new(&argv, repo, Duration::from_secs(10));
    options.env = Some(&env);
    let output = SystemExecutor::new(1)?.execute(options)?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.drain_truncated
        || output.stdout.truncated
    {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
}

fn commit(repo: &Path, text: &str, subject: &str, day: u32) -> Result<String> {
    std::fs::write(repo.join("code.rs"), text)?;
    git(repo, &["add", "--", "code.rs"], day)?;
    git(repo, &["commit", "-qm", subject], day)?;
    git(repo, &["rev-parse", "HEAD"], day)
}

fn evidence(sample: &str) -> Evidence {
    Evidence {
        sample: sample.into(),
        graph: Some(sample.into()),
        pr: Some(42),
        pattern: Some("council".into()),
        lens: Some("correctness".into()),
        model: Some("p/model".into()),
        cost_microdollars: Some(1_000_000),
        confirmed: Some(1),
        false_positives: Some(0),
        ..Evidence::default()
    }
}

#[test]
fn fixture_history_attributes_revert_same_line_fix_and_main_ci() -> Result {
    let root = tempfile::tempdir()?;
    let repo = root.path();
    git(repo, &["init", "-q", "-b", "main"], 1)?;
    commit(repo, "first\nsecond\nthird\n", "Initial", 1)?;
    git(repo, &["checkout", "-qb", "topic"], 2)?;
    let topic_commit = commit(repo, "changed\nsecond\nthird\n", "Review change", 2)?;
    git(repo, &["checkout", "-q", "main"], 2)?;
    git(
        repo,
        &[
            "merge",
            "--no-ff",
            "-qm",
            "Merge pull request #42\n\nCloses #7",
            "topic",
        ],
        2,
    )?;
    let merge = git(repo, &["rev-parse", "HEAD"], 2)?;
    commit(
        repo,
        "inserted\nchanged\nsecond\nthird\n",
        "Add an unrelated line",
        3,
    )?;
    commit(
        repo,
        "inserted\nchanged\nsecond\nelsewhere\n",
        "fix: unrelated line",
        4,
    )?;
    git(repo, &["checkout", "-qb", "follow-up"], 5)?;
    let fix = commit(
        repo,
        "inserted\ncorrected\nsecond\nelsewhere\n",
        "fix: escaped defect after line shift",
        5,
    )?;
    // Use the canonical revert trailer while avoiding an artificial revert conflict.
    git(
        repo,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            &format!("Revert broken change\n\nThis reverts commit {topic_commit}."),
        ],
        6,
    )?;
    git(repo, &["checkout", "-q", "main"], 6)?;
    git(
        repo,
        &[
            "merge",
            "--no-ff",
            "-qm",
            "Merge pull request #43",
            "follow-up",
        ],
        6,
    )?;
    commit(
        repo,
        "inserted\nlate\nsecond\nelsewhere\n",
        "fix: outside window",
        20,
    )?;
    let ci: Vec<CiRun> = serde_json::from_value(serde_json::json!([
        {"headSha":fix, "conclusion":"failure", "createdAt":"2026-01-05T13:00:00Z", "workflowName":"checks"},
        {"headSha":fix, "conclusion":"failure", "createdAt":"2026-01-05T13:00:00Z", "workflowName":"checks"},
        {"headSha":merge, "conclusion":"success", "createdAt":"2026-01-01T13:00:00Z", "workflowName":"checks"}
    ]))?;
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let mut issue = evidence("issue");
    issue.pr = None;
    issue.issue = Some(7);
    let mut missing = evidence("missing");
    missing.graph = None;
    let report = outcome::attribute(
        repo,
        "main",
        vec![evidence("graph"), issue, missing],
        Some(&ci),
        14,
        now.as_second(),
    )?;
    assert_eq!(report.unattributed_rows, 1);
    for row in &report.rows[..2] {
        assert_eq!(row.merge.as_deref(), Some(merge.as_str()));
        assert_eq!(row.signals.reverts, 1);
        assert_eq!(row.signals.follow_up_fixes, 1);
        assert_eq!(row.signals.ci_failures, Some(1));
        assert!(row.mature);
    }
    let report = outcome::attribute(
        repo,
        "main",
        vec![evidence("graph")],
        None,
        2,
        now.as_second(),
    )?;
    assert_eq!(report.ci, "not_measured");
    assert_eq!(report.rows[0].signals.ci_failures, None);
    assert_eq!(report.rows[0].signals.reverts, 0);
    assert_eq!(report.rows[0].signals.follow_up_fixes, 0);
    Ok(())
}

fn ranking_rows(
    n: usize,
    choice: &str,
    confirmed: u64,
    false_positives: u64,
    cost: u64,
    escape: bool,
) -> Vec<Attributed> {
    (0..n)
        .map(|i| {
            let mut e = evidence(&format!("{choice}-{i}"));
            e.pattern = Some(choice.into());
            e.risk_tier = Some("low".into());
            e.task_kind = Some("repair".into());
            e.confirmed = Some(confirmed);
            e.false_positives = Some(false_positives);
            e.cost_microdollars = Some(cost);
            Attributed {
                merge: Some(e.sample.clone()),
                evidence: e,
                stratum: "attributed",
                mature: true,
                signals: Signals {
                    ci_failures: Some(u64::from(escape)),
                    ..Signals::default()
                },
            }
        })
        .collect()
}

#[test]
fn ranking_uses_verified_value_strata_minimum_and_expiry() -> Result {
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let mut rows = ranking_rows(20, "efficient", 10, 0, 1_000_000, false);
    // Raw volume is higher, but poor precision, spend and escaped outcomes lose.
    rows.extend(ranking_rows(20, "noisy", 20, 80, 100_000_000, true));
    let mut report = Outcomes {
        ci_coverage_from: None,
        skipped_ci_runs: 0,
        window_days: 14,
        ci: "measured",
        unattributed_rows: 0,
        rows,
    };
    let result = suggest::rank(&report, 20, 14, now)?;
    assert_eq!(result.suggestions.len(), 1);
    let suggestion = &result.suggestions[0];
    assert_eq!(suggestion.preferred, "efficient");
    assert_eq!(suggestion.counts, [20, 20]);
    assert!(suggestion.interval[0].low > suggestion.interval[1].high);
    assert_eq!(suggestion.expires.to_string(), "2026-02-15T00:00:00Z");
    assert!(suggestion.accept.is_none());
    assert!(suggest::rank(&report, 21, 14, now)?.suggestions.is_empty());
    for row in &mut report.rows[20..] {
        row.evidence.risk_tier = Some("high".into());
    }
    assert!(suggest::rank(&report, 20, 14, now)?.suggestions.is_empty());
    for row in &mut report.rows[20..] {
        row.evidence.risk_tier = Some("low".into());
        row.evidence.task_kind = Some("feature".into());
    }
    assert!(suggest::rank(&report, 20, 14, now)?.suggestions.is_empty());
    for row in &mut report.rows[20..] {
        row.evidence.task_kind = Some("repair".into());
    }
    report.ci = "not_measured";
    let ranked = suggest::rank(&report, 20, 14, now)?;
    assert!(ranked.suggestions.is_empty());
    assert!(
        ranked
            .arms
            .iter()
            .all(|a| a.escaped_defect_rate.value.is_none()
                && a.escaped_defect_rate.interval.is_none())
    );
    Ok(())
}

#[test]
fn incomplete_or_immature_evidence_never_generates_advice() -> Result {
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let rows = ranking_rows(20, "a", 10, 0, 1_000_000, false);
    let mut report = Outcomes {
        ci_coverage_from: None,
        skipped_ci_runs: 0,
        window_days: 14,
        ci: "measured",
        unattributed_rows: 0,
        rows,
    };
    report
        .rows
        .extend(ranking_rows(20, "b", 1, 1, 10_000_000, true));
    report.rows[0].evidence.cost_microdollars = None;
    assert!(suggest::rank(&report, 20, 14, now)?.suggestions.is_empty());
    report.rows[0].evidence.cost_microdollars = Some(1_000_000);
    report.rows[0].evidence.confirmed = None;
    assert!(suggest::rank(&report, 20, 14, now)?.suggestions.is_empty());
    report.rows[0].evidence.confirmed = Some(10);
    report.rows[0].mature = false;
    assert!(suggest::rank(&report, 20, 14, now)?.suggestions.is_empty());
    assert!(suggest::wilson(0, 0).is_none());
    let interval = suggest::wilson(0, 20).ok_or("interval")?;
    assert!(interval.low < 0.000001 && interval.high > 0.16 && interval.high < 0.17);
    Ok(())
}

#[test]
fn wilson_interval_handles_boundary_counts() -> Result {
    let zero = suggest::wilson(0, 10).ok_or("zero interval")?;
    let all = suggest::wilson(10, 10).ok_or("all interval")?;
    let one = suggest::wilson(1, 1).ok_or("single interval")?;
    assert!(zero.low.abs() < 1e-12 && (0.27..0.28).contains(&zero.high));
    assert!((0.72..0.73).contains(&all.low) && (all.high - 1.0).abs() < 1e-12);
    assert!((0.20..0.21).contains(&one.low) && (one.high - 1.0).abs() < 1e-12);
    Ok(())
}

#[test]
fn ambiguous_pr_is_unattributed_and_closes_matches_issue() -> Result {
    let root = tempfile::tempdir()?;
    let repo = root.path();
    git(repo, &["init", "-q", "-b", "main"], 1)?;
    commit(repo, "initial\n", "Initial", 1)?;
    git(repo, &["checkout", "-qb", "one"], 2)?;
    commit(repo, "one\n", "One", 2)?;
    git(repo, &["checkout", "-q", "main"], 2)?;
    git(
        repo,
        &[
            "merge",
            "--no-ff",
            "-qm",
            "Merge pull request #42\n\nCloses #7",
            "one",
        ],
        2,
    )?;
    let first = git(repo, &["rev-parse", "HEAD"], 2)?;
    git(repo, &["checkout", "-qb", "two"], 3)?;
    commit(repo, "two\n", "Two", 3)?;
    git(repo, &["checkout", "-q", "main"], 3)?;
    git(
        repo,
        &["merge", "--no-ff", "-qm", "Merge pull request #42", "two"],
        3,
    )?;
    let mut issue = evidence("issue");
    issue.pr = None;
    issue.issue = Some(7);
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let report = outcome::attribute(
        repo,
        "main",
        vec![evidence("ambiguous"), issue],
        None,
        14,
        now.as_second(),
    )?;
    assert_eq!(report.rows[0].stratum, "unattributed");
    assert_eq!(report.rows[0].merge, None);
    assert_eq!(report.rows[1].merge.as_deref(), Some(first.as_str()));
    Ok(())
}

#[test]
fn duplicate_and_out_of_window_ci_failures_are_not_counted() -> Result {
    let root = tempfile::tempdir()?;
    let repo = root.path();
    git(repo, &["init", "-q", "-b", "main"], 1)?;
    commit(repo, "initial\n", "Initial", 1)?;
    git(repo, &["checkout", "-qb", "topic"], 2)?;
    commit(repo, "changed\n", "Change", 2)?;
    git(repo, &["checkout", "-q", "main"], 2)?;
    git(
        repo,
        &["merge", "--no-ff", "-qm", "Merge pull request #42", "topic"],
        2,
    )?;
    let merge = git(repo, &["rev-parse", "HEAD"], 2)?;
    let ci: Vec<CiRun> = serde_json::from_value(serde_json::json!([
        {"headSha":merge, "conclusion":"success", "createdAt":"2026-01-02T12:00:00Z", "workflowName":"checks"},
        {"headSha":merge, "conclusion":"failure", "createdAt":"2026-01-03T12:00:00Z", "workflowName":"checks"},
        {"headSha":merge, "conclusion":"failure", "createdAt":"2026-01-03T12:00:00Z", "workflowName":"checks"},
        {"headSha":merge, "conclusion":"failure", "createdAt":"2026-01-20T12:00:00Z", "workflowName":"checks"}
    ]))?;
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let report = outcome::attribute(
        repo,
        "main",
        vec![evidence("graph")],
        Some(&ci),
        14,
        now.as_second(),
    )?;
    assert_eq!(report.rows[0].signals.ci_failures, Some(1));
    Ok(())
}

#[test]
fn missing_ci_sha_and_gitlink_do_not_abort() -> Result {
    let root = tempfile::tempdir()?;
    let repo = root.path();
    git(repo, &["init", "-q", "-b", "main"], 1)?;
    let base = commit(repo, "base\n", "Initial", 1)?;
    git(repo, &["checkout", "-qb", "topic"], 2)?;
    git(
        repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{base},module"),
        ],
        2,
    )?;
    git(repo, &["commit", "-qm", "Add gitlink"], 2)?;
    git(repo, &["checkout", "-q", "main"], 2)?;
    git(
        repo,
        &["merge", "--no-ff", "-qm", "Merge pull request #42", "topic"],
        2,
    )?;
    let ci: Vec<CiRun> = serde_json::from_value(serde_json::json!([
        {"headSha":"0000000000000000000000000000000000000001", "conclusion":"failure", "createdAt":"2026-01-03T00:00:00Z", "workflowName":"checks"}
    ]))?;
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let report = outcome::attribute(
        repo,
        "main",
        vec![evidence("graph")],
        Some(&ci),
        14,
        now.as_second(),
    )?;
    assert_eq!(report.skipped_ci_runs, 1);
    assert_eq!(report.rows[0].stratum, "attributed");
    assert_eq!(report.rows[0].signals.ci_failures, None);
    assert_eq!(
        report.ci_coverage_from.as_deref(),
        Some("2026-01-03T00:00:00Z")
    );
    Ok(())
}

#[test]
fn oversized_merge_is_unmeasured() -> Result {
    let root = tempfile::tempdir()?;
    let repo = root.path();
    git(repo, &["init", "-q", "-b", "main"], 1)?;
    commit(repo, "base\n", "Initial", 1)?;
    git(repo, &["checkout", "-qb", "topic"], 2)?;
    for i in 0..201 {
        std::fs::write(repo.join(format!("file{i}")), "content\n")?;
    }
    git(repo, &["add", "."], 2)?;
    git(repo, &["commit", "-qm", "Large change"], 2)?;
    git(repo, &["checkout", "-q", "main"], 2)?;
    git(
        repo,
        &["merge", "--no-ff", "-qm", "Merge pull request #42", "topic"],
        2,
    )?;
    let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse()?;
    let report = outcome::attribute(
        repo,
        "main",
        vec![evidence("graph")],
        None,
        14,
        now.as_second(),
    )?;
    assert_eq!(report.rows[0].stratum, "unmeasured");
    assert!(report.rows[0].signals.unmeasured);
    Ok(())
}
