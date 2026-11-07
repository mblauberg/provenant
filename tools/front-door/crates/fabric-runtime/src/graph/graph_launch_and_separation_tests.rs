use super::*;

#[test]
fn node_attempt_spec_reaches_the_launch() {
    let spec = AttemptSpec {
        host: Some("workshop".into()),
        confidential: true,
        provider_timeout: Some(42),
        account: Some("work".into()),
        placement: Some(provenant_records::run::CloudPlacement {
            target: "cloud".into(),
            provider: "provider".into(),
            environment: "env".into(),
            repository: "repo".into(),
            branch: "branch".into(),
            model: "model".into(),
            effort: None,
            deadline_seconds: 60,
        }),
        build_heavy: true,
        capabilities: vec!["postgres".into()],
        write_roots: vec!["/tmp/graph-write".into()],
        ..Default::default()
    };
    let mut lane = node("lane", route("first"));
    lane.attempt = Some(spec.clone());
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![lane]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(
        run.checkpoint.nodes[0].attempts[0].extra["placement_host"],
        "workshop"
    );
    assert_eq!(owner.attempt_specs, vec![Some(spec)]);
    assert_eq!(owner.capabilities, vec![Some(vec!["postgres".into()])]);

    // An empty spec is no spec.
    let mut lane = node("lane", route("first"));
    lane.attempt = Some(AttemptSpec::default());
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    advance(
        &graph(vec![lane]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(owner.attempt_specs, vec![None]);
}

#[test]
fn node_attempt_spec_refuses_what_the_graph_cannot_launch() {
    let cases = [
        (
            serde_json::json!({"new_worktree": {"base": "main", "branch": "b"}}),
            None,
            "new worktree requires a writer",
        ),
        (
            serde_json::json!({"write_roots": [""]}),
            None,
            "invalid write roots",
        ),
        (
            serde_json::json!({"capabilities": ["postgres"]}),
            Some(Vec::new()),
            "set capabilities on the node or its attempt, not both",
        ),
        (
            serde_json::json!({"account": "other"}),
            None,
            "attempt account conflicts with a route account",
        ),
    ];
    for (spec, capabilities, reason) in cases {
        let mut pinned = route("first");
        pinned.account = Some("work".into());
        let mut lane = node("lane", pinned);
        lane.attempt = Some(serde_json::from_value(spec).unwrap());
        lane.capabilities = capabilities;
        assert_eq!(
            validate_plan(&graph(vec![lane])).unwrap_err(),
            format!("node lane: {reason}")
        );
    }
}

#[test]
fn node_capabilities_preserve_inheritance_and_explicit_empty() {
    for capabilities in [None, Some(Vec::new()), Some(vec!["postgres".into()])] {
        let mut lane = node("lane", route("first"));
        lane.capabilities = capabilities.clone();
        let roundtrip: GraphNode =
            serde_json::from_value(serde_json::to_value(&lane).unwrap()).unwrap();
        assert_eq!(roundtrip.capabilities, capabilities);
        let mut owner = FakeOwner {
            observations: [terminal(AttemptOutcome::Succeeded)].into(),
            ..Default::default()
        };
        advance(
            &graph(vec![roundtrip]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(owner.capabilities, vec![capabilities]);
    }
}

#[test]
fn graph_skill_location_requirement_reaches_the_lane() {
    for required in [false, true] {
        let mut lane = node("lane", route("first"));
        lane.require_skill_location = required;
        let encoded = serde_json::to_value(&lane).unwrap();
        let decoded: GraphNode = serde_json::from_value(encoded).unwrap();
        let mut owner = FakeOwner {
            observations: [terminal(AttemptOutcome::Succeeded)].into(),
            ..Default::default()
        };
        advance(
            &graph(vec![decoded]),
            &mut owner,
            &mut MemoryJournal::default(),
        )
        .unwrap();
        assert_eq!(owner.skill_requirements, vec![required]);
    }
}

#[test]
fn node_deadline_is_forwarded_to_the_lane_run() {
    let mut lane = node("lane", route("first"));
    lane.deadline_seconds = Some(7);
    let mut owner = FakeOwner {
        observations: [terminal(AttemptOutcome::DeadlineExceeded)].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![lane]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(owner.deadlines, vec![Some(7)]);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert_eq!(
        run.checkpoint.nodes[0].advance_error_code.as_deref(),
        Some("deadline_exceeded")
    );
    assert_eq!(
        run.checkpoint.nodes[0].attempts[0].outcome,
        Some(AttemptOutcome::DeadlineExceeded)
    );
}

#[test]
fn distinct_from_writer_passes_the_writer_attempts_to_the_owner() {
    let mut review = node("review", route("first"));
    review.source = Some(SourceBinding::Node {
        node: "write".into(),
    });
    review.distinct_from = Some(DistinctFrom::Writer);
    review.depends_on = vec!["write".into()];
    let mut write = node("write", route("first"));
    write.access = AccessMode::WorktreeWrite;
    write.worktree = Some(".worktrees/write".into());
    let writer_done = AttemptObservation::Terminal {
        attempt_ref: Some("host/7".into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: Some("writer".into()),
        commit: Some("3".repeat(40)),
    };
    let mut owner = FakeOwner {
        observations: [writer_done, terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![write, review]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.distinct_from[0], Vec::<String>::new());
    assert_eq!(owner.distinct_from[1], vec!["host/7".to_owned()]);
    assert_eq!(
        owner.source_commits[1].as_deref(),
        Some("3333333333333333333333333333333333333333")
    );
    // Without a writing source the constraint has nothing to exclude.
    let mut unsourced = node("lone", route("first"));
    unsourced.distinct_from = Some(DistinctFrom::Writer);
    let mut lone = FakeOwner {
        observations: [terminal(AttemptOutcome::Succeeded)].into(),
        ..Default::default()
    };
    advance(
        &graph(vec![unsourced]),
        &mut lone,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert_eq!(lone.distinct_from[0], Vec::<String>::new());
}

#[test]
fn family_group_replay_keeps_launch_peers() {
    let mut nodes = Vec::new();
    for id in ["one", "two", "three"] {
        let mut member = node(id, route("first"));
        member.family_group = Some("panel".into());
        nodes.push(member);
    }
    let mut document = graph(nodes);
    document.schema_version = 2;
    let mut owner = FakeOwner {
        observations: [
            done_as("h/1"),
            AttemptObservation::Pending {
                attempt_ref: Some("h/2".into()),
            },
            done_as("h/3"),
            done_as("h/2"),
        ]
        .into(),
        families: [
            ("h/1".into(), "a".into()),
            ("h/2".into(), "b".into()),
            ("h/3".into(), "c".into()),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let run = resume(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(owner.family_peers[3], ["a"]);
    assert_eq!(
        run.checkpoint.nodes[1]
            .separation
            .as_ref()
            .unwrap()
            .excluded,
        ["a"]
    );
}

#[test]
fn alias_retry_receives_the_prior_model() {
    let mut member = node("lens", route("first"));
    let alias = GraphRoute {
        extra: Default::default(),
        adapter: None,
        name: None,
        model: None,
        alias: Some("senior".into()),
        effort: None,
        account: None,
    };
    member.route = None;
    member.routes = Some(vec![alias.clone(), alias]);
    let mut limited = done_as("h/1");
    if let AttemptObservation::Terminal {
        outcome, launched, ..
    } = &mut limited
    {
        *outcome = AttemptOutcome::UsageLimited;
        *launched = false;
    }
    let mut owner = FakeOwner {
        observations: [limited, done_as("h/2")].into(),
        models: [("h/1".into(), "first/model".into())].into(),
        ..Default::default()
    };
    let run = advance(
        &graph(vec![member]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 2);
    assert_eq!(owner.prior_attempts[1], ["first/model"]);
}

#[test]
fn family_group_passes_peers_and_records_shared_family() {
    for families in [["a", "b", "c"], ["a", "a", "a"]] {
        let mut nodes = Vec::new();
        for id in ["one", "two", "three"] {
            let mut member = node(id, route("first"));
            member.family_group = Some("panel".into());
            nodes.push(member);
        }
        let mut document = graph(nodes);
        document.schema_version = 2;
        let mut owner = FakeOwner {
            family_pool: families.iter().map(|family| (*family).to_owned()).collect(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        assert!(run.checkpoint.successful());
        assert_eq!(owner.family_peers[1], [families[0]]);
        assert_eq!(owner.families["h/1"], families[0]);
        assert_eq!(owner.families["h/2"], families[1]);
        assert_eq!(owner.families["h/3"], families[2]);
        assert_eq!(
            run.checkpoint.nodes[2].separation.as_ref().unwrap().outcome,
            if families[0] == families[2] {
                SeparationOutcome::Unavailable
            } else {
                SeparationOutcome::Separated
            }
        );
    }
}

#[test]
fn reduced_panel_reports_unavailable_family_separation() {
    let mut document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":2, "graph_id":"family-warning", "nodes":[{
            "id":"panel", "kind":"composite", "access":"read_only", "composite":{
                "reduce_verdict":true, "nodes":[
                    {"id":"one", "access":"read_only", "prompt":"review", "family_group":"panel", "route":{"alias":"senior"}},
                    {"id":"two", "access":"read_only", "prompt":"review", "family_group":"panel", "route":{"alias":"senior"}}
                ], "exports":[{"node":"one","outputs":["result"]},{"node":"two","outputs":["result"]}]
            }
        }]
    })).unwrap();
    document.schema_version = 2;
    let mut owner = FakeOwner {
        family_pool: vec!["same".into()],
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    let panel = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "panel")
        .unwrap();
    assert!(
        panel
            .outputs
            .as_ref()
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("family"))
    );
    assert!(
        panel
            .outputs
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .excerpt
            .contains("not cross-family evidence")
    );
}

#[test]
fn synthesis_input_reports_same_family_reviewers() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":2, "graph_id":"family-synthesis", "nodes":[{
            "id":"panel", "kind":"composite", "access":"read_only", "composite":{
                "nodes":[
                    {"id":"one", "access":"read_only", "prompt":"review", "family_group":"panel", "route":{"alias":"senior"}},
                    {"id":"two", "access":"read_only", "prompt":"review", "family_group":"panel", "route":{"alias":"senior"}}
                ], "exports":[{"node":"one","outputs":["result"]},{"node":"two","outputs":["result"]}]
            }
        }]
    })).unwrap();
    for (families, expected) in [(vec!["a"], true), (vec!["a", "b"], false)] {
        let mut owner = FakeOwner {
            family_pool: families.into_iter().map(str::to_owned).collect(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
        let panel = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "panel")
            .unwrap();
        let second = run
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "panel--two")
            .unwrap();
        assert_eq!(
            second.separation.as_ref().unwrap().outcome,
            if expected {
                SeparationOutcome::Unavailable
            } else {
                SeparationOutcome::Separated
            }
        );
        assert_eq!(
            panel
                .outputs
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .is_some_and(|result| result.excerpt.contains("not cross-family evidence")),
            expected
        );
    }
}

#[test]
fn panel_with_one_usage_limited_member_reaches_synthesis() {
    let alias = GraphRoute {
        extra: Default::default(),
        adapter: None,
        name: None,
        model: None,
        alias: Some("junior".into()),
        effort: None,
        account: None,
    };
    let mut nodes = Vec::new();
    for id in ["one", "two", "three"] {
        let mut member = node(id, route("first"));
        member.route = None;
        member.routes = Some(vec![alias.clone(), alias.clone()]);
        member.family_group = Some("panel".into());
        nodes.push(member);
    }
    let mut synthesis = node("synthesis", route("first"));
    synthesis.depends_on = vec!["one".into(), "two".into(), "three".into()];
    nodes.push(synthesis);
    let mut document = graph(nodes);
    document.schema_version = 2;
    let mut limited = done_as("h/1");
    if let AttemptObservation::Terminal {
        outcome, launched, ..
    } = &mut limited
    {
        *outcome = AttemptOutcome::UsageLimited;
        *launched = false;
    }
    let mut owner = FakeOwner {
        observations: [
            limited,
            done_as("h/2"),
            done_as("h/3"),
            done_as("h/4"),
            done_as("h/5"),
        ]
        .into(),
        models: [("h/1".into(), "go/glm-flash".into())].into(),
        families: [
            ("h/1".into(), "zhipu".into()),
            ("h/2".into(), "openai".into()),
            ("h/3".into(), "anthropic".into()),
            ("h/4".into(), "google".into()),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful(), "{:#?}", run.checkpoint.nodes);
    assert_eq!(run.checkpoint.nodes[0].attempts.len(), 2);
    // The retry excludes the refused model by id, as the host receives it.
    assert_eq!(owner.prior_attempts[1], ["go/glm-flash"]);
    // Siblings prefer the families already used; none of them is a gate.
    assert_eq!(owner.family_peers[2], ["zhipu", "openai"]);
    assert_eq!(owner.family_peers[3], ["zhipu", "openai", "anthropic"]);
    assert!(owner.require_distinct.iter().all(|required| !required));
    let separation = run.checkpoint.nodes[2].separation.as_ref().unwrap();
    assert_eq!(separation.strength, FamilyStrength::Prefer);
    assert_eq!(separation.outcome, SeparationOutcome::Separated);
}

#[test]
fn required_writer_separation_records_enforced_and_preferred_families() {
    let mut write = node("write", route("first"));
    write.access = AccessMode::WorktreeWrite;
    write.worktree = Some(".worktrees/write".into());
    let mut lenses = Vec::new();
    for id in ["one", "two"] {
        let mut lens = node(id, route("first"));
        lens.source = Some(SourceBinding::Node {
            node: "write".into(),
        });
        lens.depends_on = vec!["write".into()];
        lens.distinct_from = Some(DistinctFrom::Writer);
        lens.distinct_strength = Some(FamilyStrength::Require);
        lens.family_group = Some("panel".into());
        lenses.push(lens);
    }
    let mut nodes = vec![write];
    nodes.extend(lenses);
    let mut document = graph(nodes);
    document.schema_version = 2;
    let mut writer_done = done_as("h/1");
    if let AttemptObservation::Terminal { commit, .. } = &mut writer_done {
        *commit = Some("3".repeat(40));
    }
    let mut owner = FakeOwner {
        observations: [writer_done, done_as("h/2"), done_as("h/3")].into(),
        families: [
            ("h/1".into(), "openai".into()),
            ("h/2".into(), "anthropic".into()),
            ("h/3".into(), "anthropic".into()),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert!(run.checkpoint.successful(), "{:#?}", run.checkpoint.nodes);
    assert_eq!(owner.family_peers[2], ["anthropic"]);
    assert!(owner.require_distinct[2]);
    // The writer exclusion is mandatory and sibling diversity is preferred.
    let separation = run.checkpoint.nodes[2].separation.as_ref().unwrap();
    assert_eq!(separation.strength, FamilyStrength::Require);
    assert_eq!(separation.excluded, ["openai"]);
    assert_eq!(separation.preferred, ["anthropic"]);
    assert_eq!(separation.outcome, SeparationOutcome::Unavailable);
}

#[test]
fn pending_lens_journals_required_writer_and_sibling_preference() {
    let mut writer = node("write", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/write".into());
    let mut lenses = Vec::new();
    for id in ["one", "two"] {
        let mut lens = node(id, route("first"));
        lens.source = Some(SourceBinding::Node {
            node: "write".into(),
        });
        lens.depends_on = vec!["write".into()];
        lens.distinct_from = Some(DistinctFrom::Writer);
        lens.distinct_strength = Some(FamilyStrength::Require);
        lens.family_group = Some("panel".into());
        lenses.push(lens);
    }
    let mut document = graph(vec![writer, lenses.remove(0), lenses.remove(0)]);
    document.schema_version = 2;
    let mut writer_done = done_as("h/1");
    if let AttemptObservation::Terminal { commit, .. } = &mut writer_done {
        *commit = Some("3".repeat(40));
    }
    let mut owner = FakeOwner {
        observations: [
            writer_done,
            done_as("h/2"),
            AttemptObservation::Pending {
                attempt_ref: Some("h/3".into()),
            },
        ]
        .into(),
        families: [
            ("h/1".into(), "openai".into()),
            ("h/2".into(), "anthropic".into()),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let separation = run.checkpoint.nodes[2].separation.as_ref().unwrap();
    assert_eq!(separation.strength, FamilyStrength::Require);
    assert_eq!(separation.excluded, ["openai"]);
    assert_eq!(separation.preferred, ["anthropic"]);
    assert_eq!(
        journal.0.as_ref().unwrap().nodes[2].separation.as_ref(),
        Some(separation)
    );
}

#[test]
fn retry_start_clears_previous_family_snapshot_before_saving() {
    #[derive(Default)]
    struct History(Vec<GraphCheckpoint>);
    impl GraphJournal for History {
        type Error = Infallible;
        fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self
                .0
                .last()
                .filter(|row| row.graph_id == graph_id)
                .cloned())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            self.0.push(checkpoint.clone());
            Ok(())
        }
    }
    let mut writer = node("write", route("first"));
    writer.access = AccessMode::WorktreeWrite;
    writer.worktree = Some(".worktrees/write".into());
    let mut lens = node("lens", route("first"));
    lens.route = None;
    lens.routes = Some(vec![route("first"), route("second")]);
    lens.source = Some(SourceBinding::Node {
        node: "write".into(),
    });
    lens.depends_on = vec!["write".into()];
    lens.distinct_from = Some(DistinctFrom::Writer);
    lens.distinct_strength = Some(FamilyStrength::Require);
    lens.family_group = Some("panel".into());
    let mut document = graph(vec![writer, lens]);
    document.schema_version = 2;
    let mut writer_done = done_as("h/1");
    if let AttemptObservation::Terminal { commit, .. } = &mut writer_done {
        *commit = Some("3".repeat(40));
    }
    let mut limited = done_as("h/2");
    if let AttemptObservation::Terminal {
        outcome, launched, ..
    } = &mut limited
    {
        *outcome = AttemptOutcome::UsageLimited;
        *launched = false;
    }
    let mut owner = FakeOwner {
        observations: [writer_done, limited, done_as("h/3")].into(),
        families: [
            ("h/1".into(), "openai".into()),
            ("h/2".into(), "anthropic".into()),
            ("h/3".into(), "google".into()),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = History::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    let retry = journal
        .0
        .iter()
        .find_map(|row| {
            let lens = &row.nodes[1];
            (lens.attempts.len() == 2 && lens.attempts[1].state == AttemptState::Starting)
                .then_some(lens)
        })
        .unwrap();
    assert!(
        retry.separation.is_none(),
        "retry inherited a stale separation"
    );
}

#[test]
fn distinct_from_dependencies_excludes_every_upstream_attempt_and_records_the_separation() {
    let (owner, checkpoint) = separation_run(None, "openai");
    let verify = owner.distinct_from.last().unwrap();
    assert_eq!(verify, &["h/1", "h/2", "h/3"].map(String::from));
    assert_eq!(owner.require_distinct.last(), Some(&false));
    let separation = checkpoint.nodes[3].separation.as_ref().unwrap();
    assert_eq!(separation.strength, FamilyStrength::Prefer);
    assert_eq!(separation.excluded, ["anthropic"]);
    assert_eq!(separation.applied.as_deref(), Some("openai"));
    assert_eq!(separation.outcome, SeparationOutcome::Separated);
    // Nodes without the option record nothing.
    assert!(checkpoint.nodes[0].separation.is_none());
}

#[test]
fn distinct_from_dependencies_excludes_the_authors_inside_an_expanded_council() {
    // The council shape: a framing subject, a lens composite whose members
    // are dependencies only through expansion, then the verifier.
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "g-council", "nodes": [
            {"id": "subject", "access": "read_only", "prompt": "frame",
             "route": {"adapter": "first", "model": "first/model"}},
            {"id": "lenses", "kind": "composite", "access": "read_only",
             "depends_on": ["subject"], "composite": {"nodes": [
                {"id": "lens-a", "access": "read_only", "prompt": "a", "depends_on": ["subject"],
                 "route": {"adapter": "first", "model": "first/model"}},
                {"id": "lens-b", "access": "read_only", "prompt": "b", "depends_on": ["subject"],
                 "route": {"adapter": "first", "model": "first/model"}}
             ], "exports": [{"node": "lens-a", "outputs": ["result"]}]}},
            {"id": "verify", "access": "read_only", "prompt": "falsify", "depends_on": ["lenses"],
             "distinct_from": "dependencies",
             "route": {"adapter": "first", "model": "first/model"}}
        ]
    }))
    .unwrap();
    let mut owner = FakeOwner {
        observations: [
            done_as("h/1"),
            done_as("h/2"),
            done_as("h/3"),
            done_as("h/4"),
        ]
        .into(),
        families: [
            ("h/1".to_owned(), "google".to_owned()),
            ("h/2".to_owned(), "anthropic".to_owned()),
            ("h/3".to_owned(), "anthropic".to_owned()),
            ("h/4".to_owned(), "anthropic".to_owned()),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap();
    assert_eq!(
        owner.distinct_from.last().unwrap(),
        &["h/1", "h/2", "h/3"].map(String::from)
    );
    let verify = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "verify")
        .unwrap();
    let separation = verify.separation.as_ref().unwrap();
    assert_eq!(separation.excluded, ["google", "anthropic"]);
    // The verifier shares the lens authors' family, so nothing separated it.
    assert_eq!(separation.outcome, SeparationOutcome::Unavailable);
}

#[test]
fn a_preferred_separation_falls_back_with_a_recorded_note_instead_of_failing() {
    let (_, checkpoint) = separation_run(Some(FamilyStrength::Prefer), "anthropic");
    assert!(checkpoint.successful());
    let separation = checkpoint.nodes[3].separation.as_ref().unwrap();
    assert_eq!(separation.outcome, SeparationOutcome::Unavailable);
    assert_eq!(separation.applied.as_deref(), Some("anthropic"));
}

#[test]
fn a_required_separation_is_passed_to_the_router_and_recorded_when_refused() {
    let (owner, checkpoint) = separation_run(Some(FamilyStrength::Require), "openai");
    assert_eq!(owner.require_distinct.last(), Some(&true));
    assert_eq!(
        checkpoint.nodes[3].separation.as_ref().unwrap().outcome,
        SeparationOutcome::Separated
    );
    // The router refused every route outside the used family: the attempt
    // never launched, and the node fails with the separation recorded.
    let mut owner = FakeOwner {
        observations: [
            done_as("h/1"),
            done_as("h/2"),
            done_as("h/3"),
            AttemptObservation::Terminal {
                attempt_ref: Some("h/4".into()),
                outcome: AttemptOutcome::AdapterUnavailable,
                launched: false,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
            },
        ]
        .into(),
        families: [
            ("h/1".to_owned(), "anthropic".to_owned()),
            ("h/2".to_owned(), "anthropic".to_owned()),
            ("h/3".to_owned(), "anthropic".to_owned()),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &lens_then_verify(Some(FamilyStrength::Require)),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    assert!(!run.checkpoint.successful());
    let separation = run.checkpoint.nodes[3].separation.as_ref().unwrap();
    assert_eq!(separation.strength, FamilyStrength::Require);
    assert_eq!(separation.outcome, SeparationOutcome::Unavailable);
    assert_eq!(separation.applied, None);
}
