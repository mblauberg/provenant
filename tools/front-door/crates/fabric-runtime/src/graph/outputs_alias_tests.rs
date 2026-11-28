use super::*;

#[test]
fn missing_trigger_preserves_severity_without_parse_refusal() {
    let block = serde_json::json!({"schema_version":1,"findings":[{
        "id":"F1","severity":"high","file":"a.rs","summary":"hypothesis","status":"open"
    }]})
    .to_string();
    let parsed = parse_findings(&block, &[]).unwrap();
    assert!(parsed.warnings.is_empty());
    assert!(parsed.skipped.is_empty());
    let findings = parsed.findings;
    assert_eq!(findings[0].severity, "high");
    assert_eq!(findings[0].effective_severity(), "high");
    assert!(repair::repair_eligible(&findings[0]));
    let mut restored = findings[0].clone();
    restored.trigger = Some(FindingTrigger {
        sequence: "default invocation".into(),
        reach: FindingReach::DefaultPath,
        observed: true,
    });
    assert_eq!(restored.effective_severity(), "high");
}

#[test]
fn parsed_severity_applies_on_every_trigger_path() {
    for trigger in [
        serde_json::Value::Null,
        serde_json::json!({"sequence":"operator action","reach":"operator_mistake","observed":true}),
        serde_json::json!({"sequence":"attacker action","reach":"attacker_or_unreachable","observed":true}),
    ] {
        let block = serde_json::json!({"schema_version":1,"findings":[{
            "id":"F1", "severity":"blocking", "file":"a.rs", "summary":"defect",
            "status":"open", "trigger":trigger
        }]})
        .to_string();
        let parsed = parse_findings(&block, &[]).unwrap();
        let finding = &parsed.findings[0];
        assert!(repair::blocks_acceptance_for(finding, AcceptBelow::High));
        assert!(repair::repair_eligible(finding));
        assert!(matches!(
            review::acceptance_decision(&parsed.findings, AcceptBelow::Blocking, true),
            review::Acceptance::Reject(_)
        ));
    }
}

#[test]
fn unrecognised_severity_is_warned_and_treated_as_high() {
    let block = serde_json::json!({"schema_version":1,"findings":[{
        "id":"F1","severity":"urgent","file":"a.rs","summary":"s","status":"open",
        "trigger":{"sequence":"invoke the default operation","reach":"default_path","observed":true},
        "triage":{"decision":"confirmed","reason":"reproduced","worth_fixing":true,
            "root_cause":"constraint","blocking_class":"none","shared_flaw":false}
    }]})
    .to_string();
    let text = format!("```findings\n{block}\n```\nVERDICT: PASS\n");
    let mut capture = crate::result_capture::ResultCapture::new(None);
    capture.push(text.as_bytes());
    let outputs = make_outputs(Some(&capture.finish()), None, None, None, None);
    assert_eq!(outputs.findings_available, Some(true));
    assert_eq!(outputs.findings.len(), 1);
    assert_eq!(outputs.findings[0].severity, "high");
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("FINDING_SEVERITY_UNKNOWN")
                && warning.contains("unrecognised value")
                && warning.contains("critical, blocker, blocking")),
        "{:?}",
        outputs.warnings
    );
    assert!(repair::repair_eligible(&outputs.findings[0]));
    assert!(repair::blocks_acceptance_for(
        &outputs.findings[0],
        AcceptBelow::High
    ));
}

#[test]
fn missing_severity_keeps_the_fail_safe_and_marks_findings_incomplete() {
    let block = serde_json::json!({"schema_version":1,"findings":[{
        "id":"F1","file":"a.rs","summary":"s","status":"open"
    }]})
    .to_string();
    let parsed = parse_findings(&block, &[]).unwrap();
    assert_eq!(parsed.findings[0].severity, "blocking");
    assert!(
        parsed
            .skipped
            .iter()
            .any(|warning| warning.contains("field severity"))
    );
    assert!(parsed.warnings.is_empty());
}

#[test]
fn malformed_trigger_reports_the_field_and_expected_type() {
    let block = serde_json::json!({"schema_version":1,"findings":[{
        "id":"F1","severity":"blocking","file":"a.rs","summary":"trigger shape","status":"open",
        "trigger":{"sequence":["call the command", "observe the refusal"],"reach":"default_path","observed":true}
    }]})
    .to_string();
    let error = parse_findings(&block, &[]).unwrap_err();
    assert!(
        error.contains("entry 1 (F1) field findings.trigger.sequence"),
        "{error}"
    );
    assert!(error.contains("expected a string"), "{error}");
}

#[test]
fn spec_concern_handoff_reaches_chair_view() {
    let mut capture = crate::result_capture::ResultCapture::new(None);
    let text = String::from_utf8_lossy(b"Report.\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"spec_concerns\":[{\"criterion\":\"Keep old format\",\"claim\":\"Format is retired\",\"evidence\":\"Approved scope removes reader\",\"proposed_change\":\"Use current format\",\"status\":\"as_written\"}]}\n```\n").into_owned();
    capture.push(text.as_bytes());
    let mut captured = capture.finish();
    captured.handoff =
        provenant_records::handoff::Handoff::from_json(text.lines().nth(2).unwrap().as_bytes())
            .map(Some);
    let outputs = make_outputs(Some(&captured), None, None, None, None);
    let checkpoint: GraphCheckpoint = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"g","plan_digest":"digest","asks":[],
        "nodes":[{"id":"writer","state":"succeeded","worktree":null,"outputs":outputs,"attempts":[]}]
    })).unwrap();
    let view = graph_waits(&checkpoint);
    assert_eq!(view[0].spec_concern_count, 1);
    assert_eq!(view[0].spec_concerns[0].criterion, "Keep old format");
    assert_eq!(
        view[0].spec_concerns[0].status,
        provenant_records::handoff::SpecConcernStatus::AsWritten
    );

    let mut malformed: serde_json::Value =
        serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
    malformed["verify"] = serde_json::json!([false]);
    malformed["remaining"] = serde_json::json!(["Finish the work"]);
    captured.handoff =
        provenant_records::handoff::Handoff::from_json(malformed.to_string().as_bytes()).map(Some);
    let partial = make_outputs(Some(&captured), None, None, None, None);
    assert_eq!(
        partial.remaining.as_deref(),
        Some(["Finish the work".to_owned()].as_slice())
    );
    assert_eq!(partial.spec_concern_count, Some(1));
    assert!(
        partial
            .warnings
            .iter()
            .any(|warning| warning.contains("verify[0]"))
    );

    // An individual concern may fill the handoff's field limit. Its full
    // content remains in the source while the checkpoint stays bounded.
    let concern = serde_json::json!({
        "criterion":"retired reader", "claim":"scope conflict", "evidence":"x".repeat(4096),
        "proposed_change":"remove reader", "status":"as_written"
    });
    let full_handoff = serde_json::json!({
        "schema_version":2, "status":"done", "note":"done", "remaining":["continue delivery"],
        "decisions":[], "verify":[], "spec_concerns":[concern.clone(), concern]
    });
    let mut capture = crate::result_capture::ResultCapture::new(None);
    let text = format!("```handoff\n{full_handoff}\n```\n");
    capture.push(text.as_bytes());
    let mut captured = capture.finish();
    captured.handoff =
        provenant_records::handoff::Handoff::from_json(full_handoff.to_string().as_bytes())
            .map(Some);
    assert_eq!(
        captured
            .handoff
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .spec_concerns
            .len(),
        2
    );
    let outputs = make_outputs(
        Some(&captured),
        None,
        Some("/attempt/result.md".into()),
        None,
        None,
    );
    assert_eq!(outputs.spec_concern_count, Some(2));
    assert!(outputs.spec_concerns.as_ref().is_some_and(Vec::is_empty));
    assert_eq!(
        outputs.remaining.as_deref(),
        Some(["continue delivery".to_owned()].as_slice())
    );
    assert!(
        canonical_len(&serde_json::json!({
            "remaining":outputs.remaining,
            "spec_concerns":outputs.spec_concerns,
            "spec_concern_count":outputs.spec_concern_count,
        })) <= REMAINING_BYTES
    );
    assert!(outputs.warnings.iter().any(
        |warning| warning.contains("retained 0 of 2") && warning.contains("/attempt/result.md")
    ));

    let remaining = "r".repeat(4090);
    let no_concerns = serde_json::json!({
        "schema_version":2, "status":"done", "note":"done", "remaining":[remaining],
        "decisions":[], "verify":[]
    });
    let mut capture = crate::result_capture::ResultCapture::new(None);
    let text = format!("```handoff\n{no_concerns}\n```\n");
    capture.push(text.as_bytes());
    let mut captured = capture.finish();
    captured.handoff =
        provenant_records::handoff::Handoff::from_json(no_concerns.to_string().as_bytes())
            .map(Some);
    let outputs = make_outputs(Some(&captured), None, None, None, None);
    assert_eq!(outputs.remaining.as_ref().unwrap(), &vec!["r".repeat(4090)]);
    assert!(outputs.spec_concerns.is_none());
    assert!(outputs.spec_concern_count.is_none());
    assert!(
        !outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("remaining items cut"))
    );
}

#[test]
fn display_id_uniqueness_property_for_colon_containing_roots() {
    use std::collections::BTreeSet;
    let roots: Vec<_> = ["a", "b", "a:F1"]
        .into_iter()
        .flat_map(|node| {
            ["F1", "a:F1", "a:F1:1", "b:F1"]
                .into_iter()
                .map(move |id| (node.to_owned(), id.to_owned()))
        })
        .collect();
    for mask in 0usize..(1 << roots.len()) {
        let selected: BTreeSet<_> = roots
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, root)| root.clone())
            .collect();
        let ids = display_ids(&selected);
        assert_eq!(
            ids.values().collect::<BTreeSet<_>>().len(),
            selected.len(),
            "{selected:?}"
        );
        for root in &selected {
            if selected.iter().filter(|other| other.1 == root.1).count() == 1 {
                assert_eq!(ids[root], root.1);
            }
        }
    }
}

#[test]
fn later_collision_preserves_prior_display_identity() {
    let first = ("lens-a".to_owned(), "F1".to_owned());
    let second = ("lens-b".to_owned(), "F1".to_owned());
    let roots = std::collections::BTreeSet::from([first.clone(), second.clone()]);
    let previous = std::collections::BTreeMap::from([(first.clone(), "F1".to_owned())]);
    let mut findings = vec![
        (
            first.clone(),
            serde_json::from_value(serde_json::json!({
                "id":"F1", "severity":"high", "file":"a.rs", "summary":"first", "status":"open"
            }))
            .unwrap(),
        ),
        (
            second.clone(),
            serde_json::from_value(serde_json::json!({
                "id":"F1", "severity":"high", "file":"b.rs", "summary":"second", "status":"open"
            }))
            .unwrap(),
        ),
    ];
    assert_eq!(display_ids(&roots)[&first], "lens-a:F1");
    qualify_finding_origins(&mut findings, &previous);
    let actual = findings
        .into_iter()
        .map(|(origin, finding)| (origin, finding.id))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual[&first], "F1");
    assert_eq!(actual[&second], "lens-b:F1");
}

#[test]
fn exact_identity_takes_precedence_over_suffix_alias() {
    let finding = |id: &str, summary: &str| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id, "severity": "high", "file": "a.rs", "summary": summary, "status": "open"
        }))
        .unwrap()
    };
    let block =
        r#"{"schema_version":1,"findings":[{"id":"F1","disposition":"rejected","reason":"no"}]}"#;
    let result = parse_findings(
        block,
        &[finding("lens-a:F1", "suffix"), finding("F1", "exact")],
    )
    .unwrap();
    assert_eq!(result.findings[0].id, "F1");
    assert_eq!(result.findings[0].summary, "exact");
}

#[test]
fn unknown_identity_warns_when_another_disposition_is_valid() {
    let upstream: Finding = serde_json::from_value(serde_json::json!({
        "id":"F1", "severity":"high", "file":"a.rs", "summary":"s", "status":"open"
    }))
    .unwrap();
    let block = r#"{"schema_version":1,"findings":[{"id":"F1","disposition":"accepted","reason":"yes"},{"id":"F9","disposition":"rejected","reason":"no"}]}"#;
    let mut capture = crate::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
    capture.push(format!("```findings\n{block}\n```\nVERDICT: PASS\n").as_bytes());
    let outputs =
        make_outputs_with_upstream(Some(&capture.finish()), None, None, None, None, &[upstream]);
    assert_eq!(outputs.findings.len(), 1);
    assert_eq!(outputs.findings[0].id, "F1");
    assert_eq!(outputs.verdict.as_deref(), Some("VERDICT: PASS"));
    assert!(outputs.warnings[0].contains("(F9) field id: unknown upstream identity"));
}

#[test]
fn large_inherited_dispositions_trim_optional_metadata() {
    let mut upstream: Finding = serde_json::from_value(serde_json::json!({
        "id":"F1", "severity":"high", "file":"src/lib.rs", "line":17,
        "summary":"description", "status":"open", "detail":"x".repeat(8000),
        "propagates":"review:F1", "lens":"tests", "evidence":"observed",
        "proposed_fix":"repair", "design_defect":true, "risk_grew":true, "scope_grew":true
    }))
    .unwrap();
    upstream.summary = "s".repeat(1000);
    let upstream = vec![upstream; 240];
    let mut capture = crate::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
    capture.push(b"```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"disposition\":\"accepted\",\"reason\":\"yes\"}]}\n```\nVERDICT: PASS\n");
    let outputs =
        make_outputs_with_upstream(Some(&capture.finish()), None, None, None, None, &upstream);
    assert_eq!(outputs.findings.len(), upstream.len());
    assert!(canonical_len(&outputs.findings) <= DISPOSITION_TEXT_TARGET);
    assert!(
        outputs
            .warnings
            .iter()
            .any(|warning| warning.contains("findings metadata trimmed"))
    );
    for finding in outputs.findings {
        assert_eq!(finding.id, "F1");
        assert_eq!(finding.severity, "high");
        assert_eq!(finding.effective_severity(), "high");
        assert_eq!(finding.file, "src/lib.rs");
        assert_eq!(finding.line, Some(17));
        assert_eq!(finding.disposition.as_deref(), Some("accepted"));
        assert_eq!(finding.reason.as_deref(), Some("yes"));
        assert_eq!(finding.lens.as_deref(), Some("tests"));
        assert_eq!(finding.evidence.as_deref(), Some("observed"));
        assert_eq!(finding.proposed_fix.as_deref(), Some("repair"));
        assert_eq!(finding.design_defect, Some(true));
        assert_eq!(finding.risk_grew, Some(true));
        assert_eq!(finding.scope_grew, Some(true));
        assert_eq!(finding.propagates.as_deref(), Some("review:F1"));
    }
}

#[test]
fn a_disposition_may_name_a_verified_finding_by_the_lens_finding_it_carries() {
    let verified = serde_json::from_value::<Finding>(serde_json::json!({
        "id": "F1", "severity": "high", "file": "a.rs", "summary": "s", "status": "open",
        "propagates": "lenses--spec:F7"
    }))
    .unwrap();
    for named in ["lenses--spec:F1", "lenses--spec:F7"] {
        let block = format!(
            r#"{{"schema_version":1,"findings":[{{"id":"{named}","disposition":"rejected","reason":"no"}}]}}"#
        );
        let parsed = parse_findings(&block, std::slice::from_ref(&verified)).unwrap();
        assert_eq!(parsed.findings[0].id, "F1", "{named}");
    }
}

#[test]
fn qualified_alias_requires_a_unique_colon_delimited_suffix() {
    let finding = |id: &str| {
        serde_json::from_value::<Finding>(serde_json::json!({
            "id": id, "severity": "high", "file": "a.rs", "summary": "s", "status": "open"
        }))
        .unwrap()
    };
    let block = r#"{"schema_version":1,"findings":[{"id":"inner:F2","disposition":"rejected","reason":"no"}]}"#;
    let ambiguous = parse_findings(block, &[finding("a:inner:F2"), finding("b:inner:F2")]);
    assert!(
        ambiguous
            .unwrap_err()
            .contains("ambiguous upstream identity")
    );
    let non_boundary = parse_findings(block, &[finding("a:xinner:F2")]);
    assert!(
        non_boundary
            .unwrap_err()
            .contains("unknown upstream identity")
    );
}
