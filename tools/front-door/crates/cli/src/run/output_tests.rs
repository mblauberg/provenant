#[test]
fn committed_paths_follow_git_across_edits_and_push() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| -> String {
        String::from_utf8(provenant_os::fixture::run_git(&repo, args).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["commit", "--allow-empty", "-qm", "base"]);
    let base: provenant_core::GitOid = git(&["rev-parse", "HEAD"]).parse().unwrap();
    git(&["init", "--bare", "-q", remote.to_str().unwrap()]);
    for step in 0..4 {
        if step == 1 || step == 3 {
            std::fs::write(repo.join(format!("file{step}")), step.to_string()).unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "change"]);
        }
        if step == 2 {
            std::fs::write(repo.join("uncommitted"), "edit").unwrap();
            git(&[
                "push",
                "-q",
                remote.to_str().unwrap(),
                "HEAD:refs/heads/lane",
            ]);
        }
        let head: provenant_core::GitOid = git(&["rev-parse", "HEAD"]).parse().unwrap();
        let expected = git(&["diff", "--name-only", &base.to_string(), &head.to_string()]);
        let paths = super::changed_paths(&repo, base, head).unwrap();
        assert_eq!(
            paths,
            expected.lines().map(str::to_owned).collect::<Vec<_>>()
        );
    }
    let missing: provenant_core::GitOid =
        "1111111111111111111111111111111111111111".parse().unwrap();
    assert!(super::changed_paths(&repo, base, missing).is_none());
}

use super::{
    Status, add_planner_fallback, fallback_reason_for_graph, finding_human_line, output_summary,
    output_summary_default, provider_error_from_result, provider_error_from_stderr,
    tail_start_bytes, tail_start_lines,
};
use serde_json::json;

#[test]
fn output_followups_use_the_public_handle_for_local_and_peer_placements() {
    for host in [
        serde_json::Value::Null,
        json!("11111111111111111111111111111111"),
    ] {
        let mut data = json!({
            "attempt_id":"22222222222222222222222222222222", "host":host,
            "state":"running", "eof":false,
            "part":"result", "next_offset":42
        });
        let command = crate::Output {
            target: "repair/writer#2".parse().unwrap(),
            part: crate::Part::Result,
            to: None,
            final_result: false,
            path: false,
            offset: 0,
            tail: None,
            tail_lines: None,
            max_bytes: 65536,
        };
        let rendered =
            super::render_output(command.clone(), Ok((data.clone(), String::new())), false);
        assert_eq!(
            rendered.result.next[0].argv,
            [
                "provenant",
                "output",
                "repair/writer#2",
                "--part",
                "result",
                "--offset",
                "42"
            ]
        );
        data["eof"] = json!(true);
        let rendered = super::render_output(command, Ok((data, String::new())), false);
        assert_eq!(
            rendered.result.next[0].argv,
            ["provenant", "wait", "repair/writer#2"]
        );
    }
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn a_running_result_envelope_is_ok_and_labels_partial_or_pending() {
    let command = crate::Output {
        target: "repair/writer#2".parse().unwrap(),
        part: crate::Part::Result,
        to: None,
        final_result: false,
        path: false,
        offset: 0,
        tail: None,
        tail_lines: None,
        max_bytes: 65536,
    };
    // An unsealed result with no bytes is a typed pending page, never an
    // empty partial result.
    let pending = json!({
        "attempt_id": "22222222222222222222222222222222",
        "state": "running",
        "reason": null,
        "part": "result",
        "offset": 0, "next_offset": 0, "length": 0,
        "eof": true, "clipped": false, "complete": false,
        "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "bytes": {"encoding": "utf8", "data": ""},
    });
    let reply = super::render_output(command.clone(), Ok((pending, String::new())), false);
    assert_eq!(reply.result.status, Status::Ok);
    assert_eq!(reply.result.data["status"], "running");
    assert_eq!(reply.result.data["state"], "running");
    assert_eq!(reply.result.data["result"], "pending");
    assert!(
        reply
            .human
            .starts_with("running\nno result yet: the result is not sealed"),
        "{}",
        reply.human
    );
    assert!(!reply.human.contains("partial result"), "{}", reply.human);

    // Bytes that did arrive before settlement are labelled partial.
    let partial = json!({
        "attempt_id": "22222222222222222222222222222222",
        "state": "running",
        "reason": null,
        "part": "result",
        "offset": 0, "next_offset": 13, "length": 13,
        "eof": true, "clipped": false, "complete": false,
        "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "bytes": {"encoding": "utf8", "data": "half a result"},
    });
    let reply = super::render_output(
        command.clone(),
        Ok((partial, "half a result".into())),
        false,
    );
    assert_eq!(reply.result.status, Status::Ok);
    assert_eq!(reply.result.data["result"], "partial");
    assert!(reply.human.starts_with("running\npartial result\n"));
    assert!(reply.human.contains("half a result"));
    let summary = reply
        .summary
        .expect("a headed result has a summary")
        .lines
        .join("\n");
    assert!(summary.contains("result: partial"), "{summary}");

    let settled = json!({
        "attempt_id": "22222222222222222222222222222222",
        "state": "succeeded",
        "part": "result",
        "eof": true, "complete": true,
        "bytes": {"encoding": "utf8", "data": "all of it"},
    });
    let reply = super::render_output(command, Ok((settled, "all of it".into())), false);
    assert!(reply.result.data.get("status").is_none());
    assert!(reply.result.data.get("result").is_none());
    assert!(!reply.human.contains("partial result"));
    assert!(!reply.human.contains("no result yet"));
}

#[test]
fn output_summary_reports_stopped_commits_and_dirty_paths() {
    let unpushed = serde_json::json!({
        "state":"cancelled",
        "stop_work":{"head":"abcdef0123456789","ahead":2,"dirty_paths":3,"push":"not_granted"}
    });
    let summary = output_summary(&unpushed, "", true);
    assert!(summary.lines.iter().any(
        |line| line == "2 commits unpushed at abcdef0123456789; 3 dirty paths left uncommitted"
    ));
    let pushed = serde_json::json!({
        "state":"cancelled",
        "stop_work":{"head":"abcdef0123456789","ahead":2,"dirty_paths":null,"push":"pushed"}
    });
    let summary = output_summary(&pushed, "", true);
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line == "2 commits pushed at abcdef0123456789")
    );
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn output_summary_prints_headers_only_for_non_null_values() {
    let null_headers = json!({
        "state":"succeeded",
        "writer_base":null, "writer_head":null, "publication":null,
        "residual_tree":null, "changed_files":null
    });
    let summary = output_summary(&null_headers, "answer", true);
    for key in [
        "base",
        "committed head",
        "publication",
        "residual tree state",
        "changed files",
    ] {
        let prefix = format!("{key}:");
        assert!(
            !summary.lines.iter().any(|line| line.starts_with(&prefix)),
            "null {key} still rendered a header: {:?}",
            summary.lines
        );
    }

    let real_headers = json!({
        "state":"succeeded",
        "writer_base":"aaaa", "writer_head":"bbbb", "publication":"pushed",
        "residual_tree":"clean (current observation)",
        "changed_files":["src/main.rs"], "changed_files_total":1
    });
    let summary = output_summary(&real_headers, "", true);
    for (label, value) in [
        ("base", "aaaa"),
        ("committed head", "bbbb"),
        ("publication", "pushed"),
        ("residual tree state", "clean (current observation)"),
    ] {
        let expected = format!("{label}: {value}");
        assert!(
            summary.lines.contains(&expected),
            "non-null {label} lost its header: {:?}",
            summary.lines
        );
    }
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.starts_with("changed files (1):"))
    );
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn output_summary_keeps_local_headers_without_unavailable_lines() {
    // The local read path (`output_read_part`) always inserts the writer
    // keys, as null on a read-only or unpublished lane. Drive that shape
    // through `render_output`: non-applicable headers are omitted, and no
    // line says "unavailable". This invariant already held before the gate
    // removal; keep it as a regression guard for the local path.
    let command = crate::Output {
        target: "repair/writer#2".parse().unwrap(),
        part: crate::Part::Result,
        to: None,
        final_result: false,
        path: false,
        offset: 0,
        tail: None,
        tail_lines: None,
        max_bytes: 65536,
    };
    let local = json!({
        "state":"succeeded", "part":"result",
        "writer_base":null, "writer_head":null,
        "residual_tree":null, "publication":null, "changed_files":null
    });
    let rendered = super::render_output(command, Ok((local, "answer".into())), false);
    let summary = output_summary(&rendered.result.data, &rendered.human, true);
    for label in [
        "base",
        "committed head",
        "publication",
        "residual tree state",
    ] {
        assert!(
            !summary
                .lines
                .iter()
                .any(|line| line.starts_with(&format!("{label}:"))),
            "null {label} rendered a header: {:?}",
            summary.lines
        );
    }
    assert!(
        !summary
            .lines
            .iter()
            .any(|line| line.contains("unavailable")),
        "a local null writer field rendered as unavailable: {:?}",
        summary.lines
    );

    // An unobserved base must not hide a real committed head or publication.
    let with_real_values = json!({
        "state":"succeeded", "writer_base":null,
        "writer_head":"cccc", "publication":"pushed"
    });
    let summary = output_summary(&with_real_values, "", true);
    for expected in ["committed head: cccc", "publication: pushed"] {
        assert!(
            summary.lines.iter().any(|line| line == expected),
            "{expected} lost when base was unobserved: {:?}",
            summary.lines
        );
    }
    assert!(
        !summary.lines.iter().any(|line| line.starts_with("base:")),
        "absent base rendered a header: {:?}",
        summary.lines
    );
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn peer_output_renders_committed_head_and_publication_from_lane_projection() {
    use provenant_fabric_runtime::coordination::facade::{
        AttemptTarget, OutputBytes, OutputInput, OutputPart, OutputResult,
    };
    let attempt = provenant_core::AttemptId::from_bytes([2; 16]);
    let host = provenant_core::HostId::from_bytes([3; 16]);
    let command = crate::Output {
        target: "repair/writer#2".parse().unwrap(),
        part: crate::Part::Result,
        to: None,
        final_result: false,
        path: false,
        offset: 0,
        tail: None,
        tail_lines: None,
        max_bytes: 65536,
    };
    // A peer chunk's forward evidence is the owning lane projection: it
    // carries `head` and `delivery`, never `writer_*` fields. Feed that
    // shape through the real peer read so the assertion pins the path a
    // reader actually takes, not a hand-built summary.
    let extra: provenant_records::ExtraFields = serde_json::from_value(json!({
        "head": "cccc",
        "delivery": {
            "schema_version": 1,
            "remote": "origin",
            "branch": "lane",
            "state": {"state": "delivered"},
            "integration": {
                "remote": "origin", "target": "refs/heads/main",
                "previous": null, "integrated": "dddd"
            }
        }
    }))
    .unwrap();
    let chunk = OutputResult::Chunk {
        peer: Default::default(),
        length: Some(2),
        planner_fallback: None,
        extra,
        target: AttemptTarget {
            extra: Default::default(),
            attempt,
            host: Some(host),
        },
        part: OutputPart::Result,
        state: "succeeded".into(),
        reason: None,
        offset: 0,
        next_offset: 2,
        eof: true,
        digest: provenant_core::Digest::from_bytes([6; 32]),
        bytes: OutputBytes::Utf8("ok".into()),
    };
    let reply = super::output_from_host(
        command,
        host,
        attempt,
        |_: OutputInput| Ok(chunk.clone()),
        false,
    );
    assert_eq!(reply.result.status, Status::Ok);
    assert_eq!(reply.result.data["head"], json!("cccc"));
    assert_eq!(
        reply.result.data["delivery"]["state"]["state"],
        json!("delivered")
    );
    let summary = output_summary(&reply.result.data, &reply.human, true);
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line == "committed head: cccc"),
        "peer committed head was dropped: {:?}",
        summary.lines
    );
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line == "publication: pushed"),
        "peer publication was dropped: {:?}",
        summary.lines
    );
    assert!(
        !summary
            .lines
            .iter()
            .any(|line| line.contains("unavailable")),
        "peer lane fields rendered as unavailable: {:?}",
        summary.lines
    );
}

#[test]
fn output_summary_shows_malformed_handoff_field_diagnostics() {
    let error =
        provenant_records::handoff::HandoffError::new("spec_concerns[0]", "expected concern");
    let data =
        json!({"state":"review_required", "cause":"handoff_malformed", "handoff":{"Err":error}});
    let summary = output_summary(&data, "captured answer", true);
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.contains("spec_concerns[0]"))
    );
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.contains("expected concern"))
    );
}

#[test]
fn output_summary_surfaces_structured_route_warnings_once() {
    let data = json!({
        "state":"succeeded",
        "applied_route":{
            "availability_warning":"account work availability is stale; the provider will check it",
            "launch_warnings":[
                "warning: pinned model is outside the configured provider scope",
                "warning: image input is not supported"
            ]
        }
    });
    let summary = output_summary(&data, "answer", true);
    assert_eq!(
        summary
            .lines
            .iter()
            .filter(|line| line.as_str()
                == "warning: pinned model is outside the configured provider scope")
            .count(),
        1
    );
    assert!(summary.lines.iter().any(|line| {
        line == "warning: account work availability is stale; the provider will check it"
    }));
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line == "warning: image input is not supported")
    );
}

#[test]
fn output_summary_names_planner_fallback_and_reason() {
    let data = json!({
        "attempt_id":"attempt",
        "state":"succeeded",
        "planner_fallback":{"stage":"static", "reason":"planner route failed"}
    });
    let summary = output_summary(&data, "fallback completed", true);
    let text = summary.lines.join("\n");
    assert!(text.contains("planner fallback: static (reason: planner route failed)"));
}

#[test]
fn output_summary_sanitises_planner_fallback_reason() {
    let data = json!({
        "planner_fallback":{"stage":"static", "reason":"fragment \u{1b}[31minvalid"}
    });
    let summary = output_summary(&data, "fallback completed", true);
    let text = summary.lines.join("\n");
    assert!(text.contains("planner fallback: static (reason: fragment [31minvalid)"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn planner_fallback_text_sanitises_reason_and_keeps_json_value() {
    let reason = "invalid fragment \u{1b}[31mpattern";
    let mut data = serde_json::Value::Null;
    let mut text = String::new();
    add_planner_fallback(&mut data, &mut text, reason.into());
    assert!(text.contains("Reason: invalid fragment [31mpattern"));
    assert!(!text.contains('\u{1b}'));
    assert_eq!(data["planner_fallback"]["reason"], reason);
}

#[test]
fn output_finds_fallback_reason_for_every_node_of_the_graph() {
    // `synthesis` depends on the expansion but is not part of it.
    let checkpoint = serde_json::from_value(serde_json::json!({
            "schema_version":1,"graph_id":"graph","plan_digest":"digest","nodes":[
                {"id":"planner", "state":"succeeded", "attempts":[],
                 "outputs":{"schema_version":2,"result":null,"verdict":null,"findings":[],"findings_available":null,"branch":null,"commit":null,"warnings":[],
                    "plan":{"fallback_reason":"planner fragment invalid: invalid fragment"}}},
                {"id":"planner--fallback--static", "state":"succeeded", "attempts":[]},
                {"id":"synthesis", "state":"succeeded", "attempts":[]}
            ],
            "expansions":[{"source":"planner","nodes":[{
                "id":"planner--fallback--static","access":"read_only","prompt":"Static stage","route":{"alias":"junior"}
            }]}]
        }))
        .unwrap();
    assert_eq!(
        fallback_reason_for_graph(&checkpoint).as_deref(),
        Some("planner fragment invalid: invalid fragment")
    );
}

#[test]
fn output_reports_no_fallback_when_no_planner_fell_back() {
    let checkpoint = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"graph","plan_digest":"digest","nodes":[
            {"id":"synthesis", "state":"succeeded", "attempts":[]}
        ]
    }))
    .unwrap();
    assert_eq!(fallback_reason_for_graph(&checkpoint), None);
}

#[test]
fn local_provider_errors_are_raw_in_off_and_secrets_modes() {
    let text = b"failure ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    for mode in [provenant_redact::Mode::Off, provenant_redact::Mode::Secrets] {
        let _mode = provenant_redact::scoped(mode);
        assert_eq!(
            provider_error_from_result(text).unwrap(),
            String::from_utf8_lossy(text)
        );
        assert_eq!(
            provider_error_from_stderr(text).unwrap(),
            String::from_utf8_lossy(text)
        );
    }
}

#[test]
fn local_provider_errors_keep_credential_shapes_and_assignments_raw() {
    let text = b"provider failed: sk-abcdefghijklmnopqrstuvwxyz password=\"printed-value\"";
    for mode in [provenant_redact::Mode::Off, provenant_redact::Mode::Secrets] {
        let _mode = provenant_redact::scoped(mode);
        for reason in [
            provider_error_from_result(text),
            provider_error_from_stderr(text),
        ] {
            let reason = reason.unwrap();
            assert!(reason.contains("sk-abcdefghijklmnopqrstuvwxyz"));
            assert!(reason.contains("printed-value"));
        }
    }
}

#[test]
fn provider_error_keeps_multiline_context() {
    let message = provider_error_from_result(
            b"Model not recognized: gemini-3.8-pro\npass: fix the code\nAvailable models:\nGemini 2.5 Flash\nGPT-OSS 120B (Medium)\n",
        )
        .unwrap();
    assert!(message.starts_with("Model not recognized: gemini-3.8-pro; "));
    assert!(message.ends_with("GPT-OSS 120B (Medium)"));
    assert!(message.contains("pass: fix the code"));
    assert_eq!(provider_error_from_result(b"\n  \n"), None);
}

#[test]
fn provider_error_uses_failure_before_trailing_tracing_warning() {
    let stderr = b"provider process failed: exit code 1\n\
2026-11-02T00:00:00.123456Z  WARN codex_core: skill scan failed\n";
    assert_eq!(
        provider_error_from_stderr(stderr).as_deref(),
        Some("provider process failed: exit code 1")
    );
}

#[test]
fn provider_error_keeps_non_warning_lines_that_mention_warn() {
    assert_eq!(
        provider_error_from_stderr(b"test-failed WARN: pass: fix the code\n").as_deref(),
        Some("test-failed WARN: pass: fix the code")
    );
}

#[test]
fn provider_error_skips_warning_level_lines() {
    assert_eq!(
        provider_error_from_stderr(
            b"warning: retrying request\nreal failure\n2026-11-02T00:00:00Z WARN cleanup failed\n"
        )
        .as_deref(),
        Some("real failure")
    );
}

#[test]
fn provider_error_does_not_headline_a_warning_without_a_failure_line() {
    assert_eq!(
        provider_error_from_stderr(
            b"WARN: skill scan failed\n2026-11-02T00:00:00Z WARN cleanup failed\n"
        ),
        None
    );
}

// Spec: docs/specs/lane-contracts.md#result-projection
#[test]
fn default_view_leads_with_settlement_verdict_findings_and_verify() {
    let data = json!({
        "state": "succeeded",
        "selected": {
            "verdict": "PASS",
            "findings": [{"id":"F1"}],
            "findings_available": true
        },
        "handoff": {"Ok": {
            "schema_version": 2, "status": "done", "note": "done",
            "remaining": [], "decisions": [], "verify": ["ran tests"]
        }}
    });
    let summary = output_summary_default(&data, "narration", true);
    let lines = &summary.lines;
    let position = |prefix: &str| lines.iter().position(|line| line.starts_with(prefix));
    let settlement = position("settlement:");
    let verdict = position("review verdict:");
    let findings = position("findings:");
    let verify = position("verify:");
    assert!(
        [settlement, verdict, findings, verify]
            .into_iter()
            .all(|position| position.is_some()),
        "{lines:?}"
    );
    let ordered = [
        settlement.unwrap(),
        verdict.unwrap(),
        findings.unwrap(),
        verify.unwrap(),
    ];
    assert!(
        ordered.windows(2).all(|pair| pair[0] < pair[1]),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("base:")),
        "{lines:?}"
    );
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn final_export_preserves_the_complete_result_across_pages() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("result.md");
    let command = crate::Output {
        target: "repair/writer#2".parse().unwrap(),
        part: crate::Part::Result,
        to: Some(path.clone()),
        final_result: true,
        path: false,
        offset: 0,
        tail: None,
        tail_lines: None,
        max_bytes: 65536,
    };
    let text = format!(
        "opening é\n{}\nVERDICT: PASS\n```handoff\n{{\"schema_version\":2,\"status\":\"done\",\"note\":\"finished\",\"remaining\":[],\"decisions\":[],\"verify\":[]}}\n```\n",
        "x".repeat(140_000)
    );
    let bytes = text.as_bytes();
    let collected = super::collect_final(&command, |page| {
        let start = page.offset as usize;
        let end = (start + page.max_bytes as usize).min(bytes.len());
        Ok((
            json!({
                "state":"succeeded", "part":"result", "attempt_id":"fixture", "digest":"same",
                "offset":start, "next_offset":end, "eof":end == bytes.len(),
                "bytes":provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(bytes[start..end].to_vec())
            }),
            String::new(),
        ))
    });
    let reply = super::render_output(command, collected, false);
    assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn default_result_keeps_the_end_of_a_bounded_page() {
    let verdict = "VERDICT: PASS";
    let handoff = "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"finished\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";
    // A long middle must be omitted, never the end a driver parses.
    let text = format!(
        "the opening of the final message\n{}\n{verdict}\n{handoff}\n",
        "x".repeat(20_000)
    );
    let data = json!({
        "state": "succeeded",
        "selected": {"verdict": verdict, "findings": [], "findings_available": true},
        "handoff": {"Ok": {"schema_version":2,"status":"done","note":"finished","remaining":[],"decisions":[],"verify":[]}}
    });
    let compact = output_summary_default(&data, &text, true);
    let rendered = compact.lines.join("\n");
    assert!(
        rendered.lines().any(|line| line.starts_with(verdict)),
        "the bounded result cut the final verdict line: {rendered}"
    );
    assert!(
        rendered.contains("```handoff"),
        "the bounded result cut the sealed handoff: {rendered}"
    );
    assert!(
        rendered.contains("bytes omitted"),
        "a bounded result must name its omitted middle: {rendered}"
    );
}

// Spec: docs/specs/lane-contracts.md#result-projection
#[test]
fn compact_result_keeps_non_result_parts_and_structured_facts() {
    let data = json!({
        "state": "succeeded",
        "selected": {"verdict": "PASS", "findings": [], "findings_available": true}
    });
    let prose = "a lane narrative a reader can skip past in the structured facts";
    let compact = output_summary_default(&data, prose, true);
    assert!(
        compact
            .lines
            .iter()
            .any(|line| line.starts_with("settlement:")),
        "{:?}",
        compact.lines
    );
    assert!(
        compact.lines.iter().any(|line| line.contains(prose)),
        "a bounded result must keep its final message: {:?}",
        compact.lines
    );
    // A transcript or stderr read is the requested bytes, not narration.
    let part = output_summary_default(&data, prose, false);
    assert!(
        part.lines.iter().any(|line| line.contains(prose)),
        "a non-result part lost its text: {:?}",
        part.lines
    );
    let full = output_summary(&data, prose, true);
    assert!(
        full.lines.iter().any(|line| line.contains(prose)),
        "--full lost the prose: {:?}",
        full.lines
    );
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn result_full_renders_each_finding_summary_and_location() {
    let data = json!({
        "state": "succeeded",
        "selected": {
            "verdict": "VERDICT: FAIL",
            "findings": [
                {"id":"F1","severity":"high","file":"src/a.rs","line":12,
                 "summary":"critical bug","status":"open","proposed_fix":"handle x"},
                {"id":"F2","severity":"low","file":"src/b.rs","line":3,
                 "summary":"style nit","status":"open","proposed_fix":"format y"}
            ],
            "findings_available": true
        }
    });
    let summary = output_summary(&data, "", true);
    let text = summary.lines.join("\n");
    assert!(text.contains("src/a.rs:12"), "{text}");
    assert!(text.contains("critical bug"), "{text}");
    assert!(text.contains("handle x"), "{text}");
    assert!(text.contains("src/b.rs:3"), "{text}");
    assert!(text.contains("style nit"), "{text}");
    assert!(text.contains("format y"), "{text}");
    let compact = output_summary_default(&data, "", true);
    assert!(
        !compact
            .lines
            .iter()
            .any(|line| line.contains("critical bug")),
        "the compact view stays a count: {:?}",
        compact.lines
    );
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn findings_without_location_or_fix_render_no_placeholders() {
    let line = finding_human_line(&json!({
        "id":"F1","severity":"low","summary":"note","status":"open"
    }));
    assert_eq!(line, "finding: [low] — note", "{line}");
    let with_file = finding_human_line(&json!({
        "id":"F2","severity":"medium","file":"a.rs",
        "summary":"defect","status":"open"
    }));
    assert_eq!(with_file, "finding: [medium] a.rs — defect", "{with_file}");
    assert!(
        !line.contains('?') && !with_file.contains('?'),
        "no '?' placeholder: {line} / {with_file}"
    );
}

// Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
#[test]
fn finding_human_line_strips_control_and_bidi_characters() {
    let line = finding_human_line(&json!({
        "id":"F1","severity":"high","file":"src/a.rs","line":12,
        "summary":"first\nsecond\u{1b}[2J\u{202e}reordered","status":"open",
        "proposed_fix":"handle\u{200b}it\nnow"
    }));
    assert_eq!(line.lines().count(), 1, "one terminal line: {line:?}");
    assert!(
        line.chars()
            .all(|c| !provenant_records::diagnostic::unsafe_display_character(c)),
        "no control or bidi characters: {line:?}"
    );
    assert!(line.contains("firstsecond[2Jreordered"), "{line:?}");
    assert!(line.contains("handleitnow"), "{line:?}");
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn tail_returns_at_most_the_requested_bytes() {
    let bytes = b"0123456789abcdef";
    for n in [0u64, 1, 4, 15, 16, 64] {
        let start = tail_start_bytes(bytes, n);
        assert!(bytes.len() - start <= n as usize, "n={n}, start={start}");
        assert!(std::str::from_utf8(&bytes[start..]).is_ok(), "n={n}");
    }
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn tail_lines_returns_at_most_the_requested_lines_and_clamps() {
    let text = "first\nsecond\nthird\nfourth\n";
    let bytes = text.as_bytes();
    assert_eq!(&text[tail_start_lines(bytes, 1)..], "fourth\n");
    assert_eq!(&text[tail_start_lines(bytes, 2)..], "third\nfourth\n");
    // A request beyond the available lines keeps the whole text; a
    // trailing newline does not count as an extra empty line.
    assert_eq!(tail_start_lines(bytes, 4), 0);
    assert_eq!(tail_start_lines(bytes, 9), 0);
    assert_eq!(tail_start_lines(b"one line", 5), 0);
    assert_eq!(tail_start_lines(bytes, 0), bytes.len());
    assert!(
        std::str::from_utf8(&bytes[tail_start_lines(bytes, 3)..]).is_ok(),
        "a line tail starts on a text boundary"
    );
    let selection = crate::Output {
        target: "repair/writer#2".parse().unwrap(),
        part: crate::Part::Result,
        to: None,
        final_result: false,
        path: false,
        offset: 0,
        tail: None,
        tail_lines: Some(501),
        max_bytes: 65536,
    };
    assert_eq!(
        selection.tail_selection(),
        Some(crate::Tail::Lines(crate::limits::TAIL_LINES))
    );
    assert!(selection.tail_clamped());
    assert_eq!(
        selection.tail_selection(),
        Some(crate::Tail::Lines(500)),
        "a line request is clamped once, for every reader"
    );
    assert_eq!(
        crate::Output {
            tail: Some(64),
            tail_lines: None,
            ..selection
        }
        .tail_selection(),
        Some(crate::Tail::Bytes(64)),
        "a byte request stays a byte request"
    );
}
