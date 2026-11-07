use super::*;

#[test]
fn quota_alias_redraw_uses_graph_retry_keeps_spent_models_and_is_bounded() {
    let mut alias = route("first");
    alias.adapter = None;
    alias.model = None;
    alias.alias = Some("junior".into());
    for launched in [false, true] {
        for with_fallback in [false, true] {
            let mut task = node("lane", alias.clone());
            if with_fallback {
                task.route = None;
                task.routes = Some(vec![alias.clone(), route("fallback")]);
            }
            let document = graph(vec![task]);
            let quota = |reference: &str| {
                let mut observation = terminal(AttemptOutcome::UsageLimited);
                if let AttemptObservation::Terminal {
                    attempt_ref,
                    launched: did_launch,
                    ..
                } = &mut observation
                {
                    *attempt_ref = Some(reference.into());
                    *did_launch = launched;
                }
                observation
            };
            let mut owner = FakeOwner {
                quota_redraw: true,
                retry_limit: Some(2),
                observations: [
                    quota("host/first"),
                    quota("host/second"),
                    quota("host/third"),
                ]
                .into(),
                models: [
                    ("host/first".into(), "codex/one".into()),
                    ("host/second".into(), "claude/two".into()),
                ]
                .into(),
                ..Default::default()
            };
            let mut journal = MemoryJournal::default();
            let status = advance(&document, &mut owner, &mut journal).unwrap();
            assert_eq!(status.checkpoint.nodes[0].state, NodeState::Failed);
            assert_eq!(owner.calls.len(), 3);
            assert!(owner.calls.iter().all(|(_, route)| route == &alias));
            assert_ne!(owner.calls[0].0, owner.calls[1].0);
            assert_ne!(owner.calls[1].0, owner.calls[2].0);
            assert_eq!(
                owner.prior_attempts,
                vec![
                    vec![],
                    vec!["codex/one".to_owned()],
                    vec!["codex/one".to_owned(), "claude/two".to_owned()]
                ]
            );
            let checkpoint = journal.0.unwrap();
            assert_eq!(checkpoint.nodes[0].previous_attempts.len(), 2);
            assert_eq!(checkpoint.nodes[0].attempts.len(), 1);
            assert_eq!(checkpoint.nodes[0].extra["retry"]["retries"], 2);
            assert_eq!(checkpoint.nodes[0].extra["quota_redraw_spent"], 2);
            if with_fallback {
                assert_eq!(
                    checkpoint.nodes[0].advance_error_code.as_deref(),
                    Some("fallback_budget_exhausted")
                );
            }
        }
    }
}

#[test]
fn quota_alias_redraw_retains_partial_writer_work_and_predecessor() {
    let mut alias = route("first");
    alias.adapter = None;
    alias.model = None;
    alias.alias = Some("junior".into());
    let mut writer = node("writer", alias);
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some("lane".into());
    let mut document = graph(vec![writer]);
    document.schema_version = 2;
    let head = "1".repeat(40);
    let partial_commit = "2".repeat(40);
    let mut quota = terminal_with_result(AttemptOutcome::UsageLimited, "Partial implementation");
    if let AttemptObservation::Terminal {
        attempt_ref,
        commit,
        ..
    } = &mut quota
    {
        *attempt_ref = Some("host/partial".into());
        *commit = Some(partial_commit.clone());
    }
    let mut owner = FakeOwner {
        implicit_head: Some(head.clone()),
        quota_redraw: true,
        retry_limit: Some(1),
        observations: [quota, terminal(AttemptOutcome::Succeeded)].into(),
        models: [("host/partial".into(), "codex/one".into())].into(),
        ..Default::default()
    };
    let status = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let progress = &status.checkpoint.nodes[0];
    assert_eq!(progress.state, NodeState::Succeeded);
    assert_eq!(owner.implicit_bindings, 1);
    assert_eq!(progress.writer_binding.as_ref().unwrap().head, head);
    assert_eq!(
        progress.writer_binding.as_ref().unwrap().worktree,
        "/fixture/lane"
    );
    assert_eq!(
        owner.permissions,
        vec![
            (AccessMode::WorktreeWrite, Some("/fixture/lane".into())),
            (AccessMode::WorktreeWrite, Some("/fixture/lane".into())),
        ]
    );
    assert_eq!(progress.previous_attempts.len(), 1);
    assert_eq!(
        progress.previous_attempts[0].attempt_ref.as_deref(),
        Some("host/partial")
    );
    assert_eq!(
        owner.calls[1].0.extra["previous_attempt_ref"],
        "host/partial"
    );
    assert_eq!(owner.prior_attempts[1], vec!["codex/one"]);
    assert_ne!(owner.calls[0].0, owner.calls[1].0);
}

#[test]
fn independent_template_review_bases_are_pinned_and_replayed() {
    let library = template::TemplateLibrary::bundled();
    let compiler = template::Compiler::new(&library);
    let subject = "1".repeat(40);
    let bases = ["2".repeat(40), "3".repeat(40)];
    let nodes = bases
        .iter()
        .enumerate()
        .map(|(index, base)| {
            compiler
                .compile(&serde_json::json!({
                    "id": format!("review-{index}"), "use": "review",
                    "source": {"type":"commit", "commit":subject}, "extensions":{"review_base":base}
                }))
                .unwrap()
        })
        .collect();
    let mut document = graph(nodes);
    document.schema_version = 2;
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text("")),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text("")),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert_eq!(owner.review_base_resolutions, 2);
    for (node, base) in run.checkpoint.nodes.iter().zip(&bases) {
        assert_eq!(node.source_commit.as_deref(), Some(subject.as_str()));
        assert_eq!(node.review_base.as_deref(), Some(base.as_str()));
        assert_eq!(
            node.attempts[0]
                .input_manifest
                .as_ref()
                .unwrap()
                .review_base
                .as_deref(),
            Some(base.as_str())
        );
    }
    journal.0 =
        Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
    owner.resolved_review_base = Some("4".repeat(40));
    let replay = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(replay.status, GraphStatus::Complete);
    assert_eq!(replay.checkpoint.nodes, run.checkpoint.nodes);
    assert_eq!(owner.review_base_resolutions, 2);
}

#[test]
fn template_review_admission_rejects_expanded_ids_over_the_limit() {
    let library = template::TemplateLibrary::bundled();
    let compiler = template::Compiler::new(&library);
    let mut boundary = graph(vec![
        compiler
            .compile(&serde_json::json!({
                "id":"x".repeat(190),"use":"review","over":["security"]
            }))
            .unwrap(),
    ]);
    boundary.admit(&Default::default()).unwrap();
    boundary.nodes = vec![
        compiler
            .compile(&serde_json::json!({
                "id":"x".repeat(191),"use":"review","over":["security"]
            }))
            .unwrap(),
    ];
    assert!(boundary.admit(&Default::default()).is_err());
}

#[test]
fn writer_provision_recovery_keeps_the_checkpointed_base() {
    struct CrashJournal {
        checkpoint: Option<GraphCheckpoint>,
        fail_binding_save: bool,
    }
    impl GraphJournal for CrashJournal {
        type Error = NoObservation;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self.checkpoint.clone())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if self.fail_binding_save
                && checkpoint
                    .nodes
                    .iter()
                    .any(|node| node.writer_binding.is_some())
            {
                self.fail_binding_save = false;
                return Err(NoObservation);
            }
            self.checkpoint = Some(checkpoint.clone());
            Ok(())
        }
    }
    let mut writer = node("writer", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/topic-fix".into());
    writer.attempt = Some(
        serde_json::from_value(serde_json::json!({
            "new_worktree":{"branch":"topic/fix","base":"main"}
        }))
        .unwrap(),
    );
    let mut document = graph(vec![writer]);
    document.schema_version = 2;
    let pinned = "a".repeat(40);
    let mut owner = FakeOwner {
        resolved_review_base: Some(pinned.clone()),
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let mut journal = CrashJournal {
        checkpoint: None,
        fail_binding_save: true,
    };
    assert!(matches!(
        advance(&document, &mut owner, &mut journal),
        Err(GraphError::Journal(_))
    ));
    let checkpoint = journal.checkpoint.as_ref().unwrap();
    assert_eq!(checkpoint.nodes[0].extra["new_worktree_base"], pinned);
    assert!(checkpoint.nodes[0].writer_binding.is_none());
    assert_eq!(owner.provisions.len(), 1);
    owner.resolved_review_base = Some("b".repeat(40));
    let recovered = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(recovered.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(owner.provisions[1].2, pinned);
    assert_eq!(owner.review_base_resolutions, 1);
}

#[test]
fn new_worktree_authority_requires_matching_writer_location_and_safe_refs() {
    for (worktree, branch, base) in [
        (".worktrees/other", "topic/fix", "main"),
        (".worktrees/-bad", "-bad", "main"),
        (".worktrees/topic-fix", "topic/fix", "--bad"),
        (".worktrees/topic-fix", "topic/fix", ""),
        (".worktrees/..-escape", "../escape", "main"),
    ] {
        let mut writer = node("writer", route("first"));
        writer.access = AccessMode::WorktreeWrite;
        writer.worktree = Some(worktree.into());
        writer.attempt = Some(
            serde_json::from_value(serde_json::json!({
                "new_worktree":{"branch":branch,"base":base}
            }))
            .unwrap(),
        );
        assert_eq!(
            validate_plan(&graph(vec![writer])).unwrap_err(),
            "node writer: invalid new worktree binding"
        );
    }
    let mut writer = node("writer", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/topic-fix".into());
    writer.attempt = Some(
        serde_json::from_value(serde_json::json!({
            "new_worktree":{"branch":"topic/fix","base":"main"}
        }))
        .unwrap(),
    );
    writer.source = Some(SourceBinding::Commit {
        commit: "a".repeat(40),
    });
    assert_eq!(
        validate_plan(&graph(vec![writer])).unwrap_err(),
        "node writer: new worktree cannot override a source binding"
    );
}

#[test]
fn typed_review_synthesis_retains_omitted_findings_and_refuses_pass() {
    let bundled = template::TemplateLibrary::bundled();
    let mut synthesis = template::Compiler::new(&bundled)
        .compile(&serde_json::json!({
            "id":"synthesis", "use":"review.synthesis", "after":["lens"],
            "inputs":[{"node":"lens","outputs":["findings"]}]
        }))
        .unwrap();
    synthesis.route = Some(route("fixture"));
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(""))),
            terminal_with_result(
                AttemptOutcome::Succeeded,
                "```findings\n{\"schema_version\":1,\"findings\":[]}\n```\nVERDICT: PASS",
            ),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![node("lens", route("fixture")), synthesis]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    let synthesis = &run.checkpoint.nodes[1];
    assert_eq!(synthesis.state, NodeState::Failed);
    assert_eq!(synthesis.outputs.as_ref().unwrap().findings.len(), 1);
    assert_eq!(
        synthesis.advance_error_code.as_deref(),
        Some("disposition_count")
    );
}

#[test]
fn typed_delta_verifier_can_falsify_a_previously_upheld_finding() {
    let bundled = template::TemplateLibrary::bundled();
    let verify = template::Compiler::new(&bundled)
        .compile(&serde_json::json!({
            "id":"verify", "use":"review.verify", "route":{"alias":"fixture"}, "after":["prior"],
            "inputs":[{"node":"prior","outputs":["findings"]}]
        }))
        .unwrap();
    let mut owner = FakeOwner {
        observations: [
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(&entry(",\"verification\":\"upheld\""))),
            terminal_with_result(AttemptOutcome::Succeeded, &findings_text(r#"{"id":"F1","verification":"falsified","reason":"Delta removed the failing path"}"#)),
        ].into(),
        ..Default::default()
    };
    let document = graph(vec![node("prior", route("fixture")), verify]);
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let output = run.checkpoint.nodes[1].outputs.as_ref().unwrap();
    assert_eq!(run.checkpoint.nodes[1].state, NodeState::Succeeded);
    assert_eq!(output.findings[0].extra["verification"], "falsified");
    assert_eq!(output.findings[0].extra["propagates"], "prior:F1");
    assert_eq!(output.verdict.as_deref(), Some("VERDICT: PASS"));
    let (plan, _) = validate_plan(&document).unwrap();
    assert!(
        review_verification_error(
            &plan.nodes[1],
            &[],
            &[],
            &[(output.findings[0].id.clone(), "member:0".into())],
            output
        )
        .is_none()
    );
    // An overflow proof owns completeness for these known upstream identities.
    assert!(
        review_verification_error(&plan.nodes[1], &output.findings, &[], &[], output).is_none()
    );
}
