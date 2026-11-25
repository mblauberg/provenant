#[allow(dead_code)] // Also compiled, unrun, by the harness-less worktrees driver.
fn row(number: u64, head: &str, error: Option<&str>) -> super::Line {
    super::Line {
        number,
        state: "OPEN".into(),
        head: head.into(),
        draft: false,
        checks_passed: Some(1),
        checks_total: Some(1),
        checks_failed: Some(0),
        checks_pending: Some(0),
        active_workflows: None,
        failing_checks: Vec::new(),
        pending_checks: Vec::new(),
        failed_check_details: Vec::new(),
        failures: None,
        merge: "clean".into(),
        review: "approved".into(),
        cause: None,
        next: None,
        awaiting: None,
        actions_grace: false,
        required_checks: Vec::new(),
        required_pending: Vec::new(),
        required_unreadable: false,
        error: error.map(str::to_owned),
        batch: None,
        batch_detail: None,
        batch_uncertain: None,
    }
}
#[test]
fn a_failing_line_reports_the_shared_digest_in_text_and_json() {
    let mut line = row(1669, "head", None);
    line.failing_checks = vec!["ci-status".into()];
    line.failed_check_details = vec![super::FailedCheck {
        name: "ci-status".into(),
        url: Some("https://github.com/owner/repo/actions/runs/7".into()),
        bucket: "fail".into(),
    }];
    line.failures = Some(crate::ci::Report {
        pr: Some(1669),
        run: None,
        head: Some("head".into()),
        runs: vec![crate::ci::RunFailures {
            run: 7,
            jobs: vec![crate::ci::JobFailures {
                job: "lint".into(),
                step: Some("clippy".into()),
                class: crate::ci::JobClass::Failed,
                unrelated: Vec::new(),
                tests: Vec::new(),
                errors: vec!["error[E0308]: mismatched types".into()],
                truncated: false,
            }],
            error: None,
        }],
        unlogged: Vec::new(),
        rerun: None,
    });
    let rows = line.digest_rows();
    assert!(
        rows.iter().any(|row| row.contains("mismatched types")),
        "{rows:?}"
    );
    let json = serde_json::to_value(&line).expect("line serialises");
    assert!(
        json["failures"]["runs"][0]["jobs"][0]["errors"][0]
            .as_str()
            .is_some_and(|text| text.contains("mismatched types")),
        "{json}"
    );
}

#[test]
fn completion_report_retains_every_failed_check_and_link() {
    let check = |name: &str, bucket: &str, url: &str| super::super::observer::Check {
        head: "head".into(),
        name: name.into(),
        job_id: None,
        bucket: bucket.into(),
        app: None,
        app_slug: None,
        conclusion: None,
        registered: None,
        url: Some(url.into()),
        excerpt: String::new(),
    };
    let view = super::View {
        state: "OPEN".into(),
        head: "head".into(),
        merge: "".into(),
        review: None,
        draft: false,
        url: String::new(),
    };
    let checks = [
        check("one", "fail", "https://ci/one"),
        check("two", "cancel", "https://ci/two"),
    ];
    let line = super::status_line(1668, view, Some(&checks));
    assert_eq!(
        line.failure_lines(),
        [
            "#1668 failed: one https://ci/one",
            "#1668 failed: two https://ci/two"
        ]
    );
    let json = serde_json::to_value(line).unwrap();
    assert_eq!(json["failed_check_details"][1]["url"], "https://ci/two");
}

#[test]
fn pending_check_names_survive_the_status_line_and_render() {
    let check = |name: &str, bucket: &str| super::super::observer::Check {
        head: "head".into(),
        name: name.into(),
        job_id: None,
        bucket: bucket.into(),
        app: None,
        app_slug: None,
        conclusion: None,
        registered: None,
        url: None,
        excerpt: String::new(),
    };
    let view = super::View {
        state: "OPEN".into(),
        head: "head".into(),
        merge: "clean".into(),
        review: Some("approved".into()),
        draft: false,
        url: String::new(),
    };
    let checks = [
        check("build", "pass"),
        check("lint", "pending"),
        check("deploy", "pending"),
    ];
    let line = super::status_line(7, view, Some(&checks));
    assert_eq!(line.pending_checks, ["lint", "deploy"]);
    let text = line.text();
    assert!(text.contains("pending=2"), "{text}");
    assert!(text.contains("pending_names=lint,deploy"), "{text}");
    let json = serde_json::to_value(&line).unwrap();
    assert_eq!(json["pending_checks"][0], "lint");
    // A new head has no checks yet, so no stale pending name survives.
    let mut waiting = line;
    waiting.await_head("new");
    assert!(waiting.pending_checks.is_empty());
    assert!(!waiting.text().contains("pending_names="));
}

// Spec: docs/specs/cli.md#integration-batch-membership
#[test]
fn a_superseded_member_renders_the_batch_and_drops_cancelled_failures() {
    use super::membership::Link;
    let mut member = row(7, "member-head", None);
    member.checks_failed = Some(2);
    member.failing_checks = vec!["ci-status".into(), "real-failure".into()];
    member.failed_check_details = vec![
        super::FailedCheck {
            name: "ci-status".into(),
            url: Some("https://ci/one".into()),
            bucket: "cancel".into(),
        },
        super::FailedCheck {
            name: "real-failure".into(),
            url: Some("https://ci/two".into()),
            bucket: "fail".into(),
        },
    ];
    member.suppress_own_ci();
    member.batch = Some(Link {
        number: Some(9),
        label: "#9".into(),
        head: "batch-head".into(),
        ci: "checks=2/2 fail=0 pending=0".into(),
        head_moved: false,
    });
    let text = member.text();
    assert!(
        text.contains(
            "in batch #9 (head merged); own CI superseded; batch ci=checks=2/2 fail=0 pending=0"
        ),
        "{text}"
    );
    assert!(text.contains("fail=1 names=real-failure"), "{text}");
    assert!(!text.contains("names=ci-status"), "{text}");
    assert_eq!(member.checks_failed, Some(1));
    assert_eq!(member.failing_checks, ["real-failure"]);
    assert_eq!(
        member.display_class(),
        "failing",
        "real failures still fail"
    );
    let json = serde_json::to_value(&member).unwrap();
    assert_eq!(json["batch"]["number"], 9);
    assert_eq!(json["batch"]["head"], "batch-head");
    let (summary, _) = super::summarise(&[member]);
    assert_eq!(summary.lines[0], "1 PRs: 1 failing");
    assert_eq!(summary.lines.len(), 2, "the real failure is an action row");
    assert!(summary.lines[1].contains("fail=1 names=real-failure"));
}

// Spec: docs/specs/cli.md#integration-batch-membership
#[test]
fn uncertain_ancestry_is_reported_without_claiming_a_batch_or_suppressing_ci() {
    let mut member = row(7, "member-head", None);
    member.checks_failed = Some(1);
    member.failing_checks = vec!["real-failure".into()];
    member.failed_check_details = vec![super::FailedCheck {
        name: "real-failure".into(),
        url: None,
        bucket: "fail".into(),
    }];
    member.batch_uncertain =
        Some(" batch=#9? shared history only; membership uncertain; own CI retained".into());
    let text = member.text();
    assert!(
        text.contains("batch=#9? shared history only; membership uncertain; own CI retained"),
        "{text}"
    );
    assert!(text.contains("fail=1 names=real-failure"), "{text}");
    assert!(member.batch.is_none(), "uncertainty is not a batch claim");
    assert_eq!(member.display_class(), "failing");
    let json = serde_json::to_value(&member).unwrap();
    assert!(
        json["batch_uncertain"]
            .as_str()
            .is_some_and(|note| note.contains("#9"))
    );
    assert!(json["batch"].is_null());
}

// Spec: docs/specs/cli.md#integration-batch-membership
#[test]
fn a_batch_row_renders_its_members_and_per_file_attribution() {
    use super::membership::{BatchDetail, FileOwner, MemberRow};
    let mut batch = row(9, "batch-head", None);
    batch.batch_detail = Some(BatchDetail {
        members: vec![
            MemberRow {
                number: 7,
                head_moved: false,
            },
            MemberRow {
                number: 8,
                head_moved: true,
            },
        ],
        files: vec![
            FileOwner {
                path: "src/a.rs".into(),
                member: 7,
            },
            FileOwner {
                path: "src/b.rs".into(),
                member: 8,
            },
        ],
    });
    let text = batch.text();
    assert!(text.contains("batch_members=7:merged,8:moved"), "{text}");
    assert!(text.contains("batch_files=src/a.rs:7,src/b.rs:8"), "{text}");
    let json = serde_json::to_value(&batch).unwrap();
    assert_eq!(json["batch_detail"]["files"][1]["member"], 8);
}

#[test]
fn complete_wait_keeps_failures_pending_until_all_checks_finish() {
    let mut line = row(1668, "head", None);
    line.checks_failed = Some(1);
    line.checks_pending = Some(1);
    assert_eq!(line.wait_verdict(false), Some(false));
    assert_eq!(line.wait_verdict(true), None);
    line.checks_pending = Some(0);
    line.active_workflows = Some(true);
    assert_eq!(line.wait_verdict(true), None);
    line.checks_failed = Some(0);
    assert_eq!(line.wait_verdict(true), None);
    line.checks_failed = Some(1);
    line.active_workflows = Some(false);
    assert_eq!(line.wait_verdict(true), Some(false));
    line.state = "MERGED".into();
    line.active_workflows = Some(true);
    assert_eq!(line.wait_verdict(false), Some(true));
    assert_eq!(line.wait_verdict(true), None);
    line.active_workflows = Some(false);
    assert_eq!(line.wait_verdict(true), Some(true));
    line.await_head("new");
    assert_eq!(line.wait_verdict(true), None);
}

// Spec: docs/specs/cli.md#waitpr-causes
#[test]
fn a_head_with_only_a_pending_selector_waits_for_the_aggregate_check() {
    let check = |name: &str, bucket: &str| super::super::observer::Check {
        head: "head".into(),
        name: name.into(),
        job_id: None,
        bucket: bucket.into(),
        app: None,
        app_slug: None,
        conclusion: None,
        registered: None,
        url: None,
        excerpt: String::new(),
    };
    let view = || super::View {
        state: "OPEN".into(),
        head: "head".into(),
        merge: "clean".into(),
        review: None,
        draft: false,
        url: "https://github.com/owner/repo/pull/7".into(),
    };
    let required = || {
        super::Requirement::Known(vec![super::Required {
            name: "ci-status".into(),
            app: None,
        }])
    };
    let read = |checks: &[super::super::observer::Check], policy: super::Requirement| {
        super::status_line_with(7, view(), Some(checks), policy)
    };
    let selector = check("Select CI suites", "pending");
    let passed_selector = check("Select CI suites", "pass");
    let aggregate = check("ci-status", "pass");

    let pending = read(&[selector], required());
    assert_eq!(pending.class(), "pending");
    assert_eq!(pending.verdict(), None);
    assert_eq!(pending.wait_verdict(true), None);
    assert!(pending.text().contains("required_pending=ci-status"));

    // A selector that finished does not create the required check.
    let still_pending = read(std::slice::from_ref(&passed_selector), required());
    assert_eq!(still_pending.verdict(), None);
    assert_eq!(still_pending.required_pending, ["ci-status"]);

    let green = read(&[passed_selector, aggregate.clone()], required());
    assert!(green.required_pending.is_empty());
    assert_eq!(green.verdict(), Some(true));
    assert_eq!(green.wait_verdict(true), Some(true));

    let running = read(&[check("ci-status", "pending")], required());
    assert_eq!(running.verdict(), None);
    let failed = read(&[check("ci-status", "fail")], required());
    assert_eq!(failed.verdict(), Some(false));

    // An unreadable policy is not an empty one: no verdict from observed checks.
    let unreadable = read(&[aggregate], super::Requirement::Unreadable);
    assert_eq!(unreadable.verdict(), None);
    assert_eq!(unreadable.wait_verdict(true), None);
}

#[test]
fn a_settling_head_without_actions_runs_awaits_them() {
    let mut passed = row(1668, "head", None);
    assert!(passed.awaits_actions(false));
    assert!(passed.awaits_actions(true));
    passed.hold_for_actions();
    assert_eq!(passed.verdict(), None);
    assert_eq!(passed.wait_verdict(true), None);
    assert_eq!(passed.class(), "pending");
    assert!(passed.text().contains("awaiting Actions runs"));
    assert!(
        !passed.awaits_actions(false),
        "a held reading is not held again"
    );

    // Observed runs end the wait for them, whether active or finished.
    for active in [true, false] {
        let mut seen = row(1668, "head", None);
        seen.active_workflows = Some(active);
        assert!(!seen.awaits_actions(false));
        assert!(!seen.awaits_actions(true));
    }

    // A failure settles a default wait at once, but a complete wait promises
    // to see every check, so it waits for the runs as well.
    let mut failed = row(1668, "head", None);
    failed.checks_failed = Some(1);
    assert!(!failed.awaits_actions(false));
    assert!(failed.awaits_actions(true));

    // Merged, closed, unavailable and pinned-elsewhere readings are not held.
    let mut merged = row(1668, "head", None);
    merged.state = "MERGED".into();
    assert!(!merged.awaits_actions(false));
    assert!(!merged.awaits_actions(true));
    assert!(!row(1668, "head", Some("HTTP 502")).awaits_actions(false));
    let mut pinned = row(1668, "head", None);
    pinned.await_head("new");
    assert!(!pinned.awaits_actions(false));
}

#[test]
fn failure_lines_bound_and_sanitise_check_names_and_links() {
    let mut line = row(1668, "head", None);
    line.checks_failed = Some(4);
    let long_name = "n".repeat(10_000);
    line.failed_check_details = vec![
        super::FailedCheck {
            name: "bad\nname\u{1b}[31m".into(),
            url: Some("https://ci/ok".into()),
            bucket: "fail".into(),
        },
        super::FailedCheck {
            name: long_name,
            url: Some(format!("https://ci/{}", "u".repeat(10_000))),
            bucket: "fail".into(),
        },
        super::FailedCheck {
            name: "spaced".into(),
            url: Some("https://ci/a b".into()),
            bucket: "cancel".into(),
        },
        super::FailedCheck {
            name: "scheme".into(),
            url: Some("javascript:alert(1)".into()),
            bucket: "fail".into(),
        },
    ];
    let lines = line.failure_lines();
    assert_eq!(lines.len(), 4);
    for text in &lines {
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.len() < 200, "{} bytes", text.len());
    }
    assert_eq!(
        lines[0],
        "#1668 failed: bad\\nname\\u{1b}[31m https://ci/ok"
    );
    assert!(lines[1].ends_with("..."), "{}", lines[1]);
    assert!(
        !lines[1].contains("https://"),
        "an overlong link is dropped"
    );
    assert_eq!(lines[2], "#1668 failed: spaced");
    assert_eq!(lines[3], "#1668 failed: scheme");
}

#[test]
fn awaiting_an_unreported_head_never_verdicts_the_stale_one() {
    let mut line = row(1668, "stale", None);
    line.checks_failed = Some(1);
    assert_eq!(line.verdict(), Some(false));
    line.await_head("new");
    assert_eq!(line.verdict(), None);
    assert_eq!(line.class(), "pending");
    assert!(line.text().contains("waiting for head new"));
    assert!(line.text().contains("GitHub reports stale"));
}

#[test]
fn the_summary_puts_action_rows_first() {
    let mut failing = row(3, "c", None);
    failing.checks_failed = Some(1);
    let unavailable = row(2, "b", Some("HTTP 502"));
    let action_rows = vec![failing.text(), unavailable.text()];
    let lines = vec![row(1, "a", None), failing, unavailable];
    let (summary, _) = super::summarise(&lines);
    assert_eq!(&summary.lines[1..], action_rows);
    assert_eq!(summary.omitted, 0);
}

#[test]
fn ruleset_only_protection_is_distinct_from_a_failed_policy_lookup() {
    assert_eq!(
        super::classic_protection(
            false,
            br#"{"message":"Branch not protected","status":"404"}"#
        ),
        Some(None)
    );
    assert_eq!(
        super::classic_protection(false, br#"{"message":"Not Found","status":"404"}"#),
        None
    );
    assert_eq!(
        super::classic_protection(
            false,
            br#"{"message":"Resource not accessible","status":"403"}"#
        ),
        None
    );
}
#[test]
fn failed_policy_lookup_uses_all_reported_checks() {
    use super::*;
    use provenant_fabric_runtime::graph::CiState;
    for (bucket, expected) in [("fail", CiState::Failed), ("pending", CiState::Pending)] {
        let observation = observe_ci_with(
            &[],
            false,
            || None,
            || {
                Some(vec![Check {
                    name: "build".into(),
                    bucket: bucket.into(),
                }])
            },
            || None,
        );
        assert_eq!(observation.state, expected);
        assert!(observation.summary.contains("policy=all-reported"));
    }
    let empty = observe_ci_with(&[], false, || None, || Some(vec![]), || None);
    assert_eq!(empty.state, CiState::NotScheduled);
    assert!(empty.summary.contains("next:"));
    let conflict = observe_ci_with(
        &[],
        false,
        || None,
        || Some(vec![]),
        || Some("CONFLICTING".into()),
    );
    assert_eq!(conflict.state, CiState::Conflict);
    assert!(conflict.summary.contains("next: rebase"));
    // A conflict gate runs before CI even if stale check evidence is present.
    let failed = observe_ci_with(
        &[],
        false,
        || None,
        || {
            Some(vec![Check {
                name: "build".into(),
                bucket: "fail".into(),
            }])
        },
        || Some("CONFLICTING".into()),
    );
    assert_eq!(failed.state, CiState::Conflict);
    assert!(failed.summary.contains("next: rebase"));

    let unreadable_queue_policy = observe_ci_with(
        &[],
        true,
        || None,
        || {
            Some(vec![Check {
                name: "build".into(),
                bucket: "pass".into(),
            }])
        },
        || None,
    );
    assert_eq!(unreadable_queue_policy.state, CiState::Pending);
    assert!(unreadable_queue_policy.failed_checks.is_empty());
    assert!(
        unreadable_queue_policy
            .summary
            .contains("policy=required-unavailable")
    );
}

#[test]
fn branch_requirements_merge_protection_and_paginated_rulesets() {
    use super::*;
    use provenant_fabric_runtime::graph::CiState;
    use serde_json::json;
    let protection = json!({"required_status_checks": {"contexts": ["build", "lint"]}});
    let pages = vec![
        vec![json!({"type": "required_status_checks", "parameters": {
            "required_status_checks": [{"context": "lint"}, {"context": "integration"}]
        }})],
        vec![
            json!({"type": "required_status_checks", "parameters": {
                "required_status_checks": [{"context": "deploy"}]
            }}),
            json!({"type": "pull_request"}),
        ],
    ];
    let names = parse_required_checks(Some(&protection), &pages).unwrap();
    let names: Vec<String> = names.into_iter().map(|required| required.name).collect();
    assert_eq!(names, ["build", "deploy", "integration", "lint"]);
    let checks = ["build", "deploy", "integration", "lint"]
        .into_iter()
        .map(|name| Check {
            name: name.into(),
            bucket: if name == "integration" {
                "skipping"
            } else {
                "pass"
            }
            .into(),
        })
        .collect();
    let observation = summarise_ci(Some(checks), Some(("branch-required", &names)));
    assert_eq!(observation.state, CiState::Unavailable);
    assert!(observation.summary.contains("policy=branch-required"));
}

#[test]
fn branch_requirements_reject_incomplete_data_and_accept_known_empty_policy() {
    use super::*;
    use serde_json::json;
    let no_protection = json!({"required_status_checks": null});
    assert_eq!(
        parse_required_checks(Some(&no_protection), &[]),
        Some(vec![])
    );
    assert_eq!(parse_required_checks(None, &[vec![]]), Some(vec![]));
    assert_eq!(parse_required_checks(Some(&json!({})), &[]), None);
    assert_eq!(
        parse_required_checks(
            None,
            &[vec![
                json!({"type": "required_status_checks", "parameters": {}})
            ]]
        ),
        None
    );
    let rules = vec![vec![
        json!({"type": "required_status_checks", "parameters": {
            "required_status_checks": [{"context": "build"}]
        }}),
    ]];
    assert_eq!(
        parse_required_checks(Some(&no_protection), &rules),
        Some(vec![Required {
            name: "build".into(),
            app: None
        }])
    );
}

#[test]
fn gate_check_buckets_fail_closed() {
    use super::*;
    use provenant_fabric_runtime::graph::CiState;
    let observe = |buckets: &[(&str, &str)], names: Option<&[String]>| {
        summarise_ci(
            Some(
                buckets
                    .iter()
                    .map(|(name, bucket)| Check {
                        name: (*name).into(),
                        bucket: (*bucket).into(),
                    })
                    .collect(),
            ),
            names.map(|names| ("configured", names)),
        )
    };
    let required = vec!["build".to_owned()];
    assert_eq!(
        observe(&[("build", "pass"), ("lint", "skipping")], None).state,
        CiState::Passed
    );
    assert_eq!(
        observe(&[("build", "pass"), ("lint", "pending")], None).state,
        CiState::Pending
    );
    assert_eq!(observe(&[("build", "cancel")], None).state, CiState::Failed);
    assert_eq!(
        observe(&[("build", "neutral")], None).state,
        CiState::Unavailable
    );
    assert_eq!(
        observe(&[("build", "skipping")], Some(&required)).state,
        CiState::Unavailable
    );
    assert_eq!(
        observe(&[("lint", "pass")], Some(&required)).state,
        CiState::Unavailable
    );
    assert_eq!(
        observe(&[("build", "pass"), ("lint", "fail")], Some(&required)).state,
        CiState::Passed
    );
    assert_eq!(
        observe(&[("build", "pending")], Some(&required)).state,
        CiState::Pending
    );
    // No reported checks means CI never started: still not passed.
    assert_eq!(observe(&[], None).state, CiState::NotScheduled);
    assert_eq!(observe(&[], Some(&required)).state, CiState::NotScheduled);
    assert_eq!(summarise_ci(None, None).state, CiState::Unavailable);
    assert!(
        observe(&[("build", "pass")], Some(&required))
            .summary
            .contains("policy=configured")
    );
}

#[test]
fn display_fields_degrade_gracefully_while_verdict_fields_refuse_malformed_values() {
    let display_degraded = r#"{
            "number": 1,
            "state": "OPEN",
            "head": "abc",
            "draft": false,
            "checks_passed": "invalid",
            "checks_total": 5,
            "checks_failed": 0,
            "checks_pending": 0,
            "failing_checks": null,
            "merge": 123,
            "review": "approved"
        }"#;
    let line: super::Line = serde_json::from_str(display_degraded).expect("display fields degrade");
    assert_eq!(line.number, 1);
    assert_eq!(line.checks_passed, None);
    assert_eq!(line.failing_checks, Vec::<String>::new());
    assert_eq!(line.verdict(), Some(true));

    for malformed_verdict in [
        r#"{"number":1,"state":"OPEN","head":"a","checks_failed":"bogus"}"#,
        r#"{"number":1,"state":"OPEN","head":"a","checks_pending":"bogus"}"#,
        r#"{"number":1,"state":"OPEN","head":"a","error":123}"#,
        r#"{"number":1,"state":"OPEN","head":"a","review":123}"#,
    ] {
        assert!(
            serde_json::from_str::<super::Line>(malformed_verdict).is_err(),
            "malformed verdict field must fail deserialization: {malformed_verdict}"
        );
    }
}
