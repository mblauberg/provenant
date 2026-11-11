use super::*;
use relay::{
    ChildRequest, admit_child, cancel_subtree, child_depth, parent_of_attempt, set_waiting,
};
use serde_json::{Value, json};

fn live(limits: Option<Value>) -> (GraphDocument, GraphCheckpoint) {
    live_with_session(limits, None)
}

fn live_with_session(
    limits: Option<Value>,
    session: Option<&str>,
) -> (GraphDocument, GraphCheckpoint) {
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"live", "nodes":[
            {"id":"a", "access":"read_only", "prompt":"Start", "route":{"alias":"senior"}},
            {"id":"b", "access":"read_only", "prompt":"Then", "depends_on":["a"], "route":{"alias":"senior"}}
        ]
    }))
    .unwrap();
    document.limits = limits;
    document.decision_session = session.map(str::to_owned);
    let mut owner = FakeOwner::default();
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("attempt-a".into()),
    });
    let checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default())
        .unwrap()
        .checkpoint;
    (document, checkpoint)
}

fn child(id: &str, key: &str) -> ChildRequest {
    serde_json::from_value(json!({"id":id, "brief":"Fix the parser", "idempotency_key":key}))
        .unwrap()
}

fn add(
    document: &GraphDocument,
    checkpoint: &mut GraphCheckpoint,
    parent: &str,
    request: &ChildRequest,
) -> Result<relay::ChildReceipt, patch::PatchError> {
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        journal: None,
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        limits: &limits,
    };
    admit_child(
        &mut patch::LiveGraph {
            document,
            checkpoint,
        },
        parent,
        request,
        &context,
    )
}

fn ids(document: &GraphDocument, checkpoint: &GraphCheckpoint) -> Vec<String> {
    plans::expand(
        document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap()
    .nodes
    .into_iter()
    .map(|node| node.id)
    .collect()
}

fn progress<'a>(checkpoint: &'a GraphCheckpoint, id: &str) -> &'a NodeProgress {
    checkpoint.nodes.iter().find(|node| node.id == id).unwrap()
}

fn mark_running(checkpoint: &mut GraphCheckpoint, id: &str) {
    checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == id)
        .unwrap()
        .state = NodeState::Running;
}

#[test]
fn lane_child_joins_the_same_graph_with_an_idempotent_receipt() {
    let (document, mut checkpoint) = live(None);
    // The relay names the attempt; the host finds the node that owns it.
    let parent = parent_of_attempt(&checkpoint, "attempt-a")
        .unwrap()
        .to_owned();
    assert_eq!(parent, "a");
    assert_eq!(parent_of_attempt(&checkpoint, "someone-else"), None);

    let request = child("fix", "lane-a-fix-1");
    let receipt = add(&document, &mut checkpoint, &parent, &request).unwrap();
    assert_eq!(
        (receipt.graph_id.as_str(), receipt.node.as_str()),
        ("live", "a--fix")
    );
    assert_eq!(
        (receipt.revision, receipt.replayed, receipt.worktree),
        (1, false, None)
    );
    assert!(ids(&document, &checkpoint).contains(&"a--fix".to_owned()));
    let record = checkpoint.expansions[0].patch.as_ref().unwrap();
    assert_eq!(
        (record.parent.as_deref(), record.key.as_deref()),
        (Some("a"), Some(format!("child:{}", CanonicalV1::record_digest("graph-child-key", 1, &("a", "lane-a-fix-1")).unwrap()).as_str()))
    );
    assert_eq!(checkpoint.expansions[0].uses[0].name, "task");
    assert_eq!(progress(&checkpoint, "a--fix").state, NodeState::Pending);

    // A retried relay call replays the receipt; a different child under the
    // same key is refused.
    let replay = add(&document, &mut checkpoint, "a", &request).unwrap();
    assert_eq!(
        (replay.node.as_str(), replay.revision, replay.replayed),
        ("a--fix", 1, true)
    );
    assert_eq!(checkpoint.expansions.len(), 1);
    let other = child("other", "lane-a-fix-1");
    assert_eq!(
        add(&document, &mut checkpoint, "a", &other)
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    for bad in ["", "a--b", "has space"] {
        assert_eq!(
            add(&document, &mut checkpoint, "a", &child(bad, "k"))
                .unwrap_err()
                .code,
            "invalid",
            "{bad:?}"
        );
    }
    // Only a running parent adds children.
    assert_eq!(
        add(&document, &mut checkpoint, "b", &child("late", "k2"))
            .unwrap_err()
            .code,
        "parent_settled"
    );
}

#[test]
fn rich_child_attempt_controls_define_replay_identity_and_reach_the_launch() {
    let (document, mut checkpoint) = live(None);
    let spec = AttemptSpec {
        host: Some("local".into()),
        confidential: true,
        provider_timeout: Some(42),
        ..Default::default()
    };
    let mut request = child("controlled", "controlled-launch");
    request.launch = Some(relay::ChildLaunch {
        templates: Default::default(),
        node: json!({
            "id":"controlled", "kind":"task", "access":"read_only",
            "prompt":"Inspect launch policy", "route":{"alias":"senior"},
            "attempt":spec
        }),
    });
    let receipt = add(&document, &mut checkpoint, "a", &request).unwrap();
    // The durable request identity must survive a checkpoint reload.
    let mut checkpoint: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let before = checkpoint.clone();
    let replay = add(&document, &mut checkpoint, "a", &request).unwrap();
    assert_eq!(replay, relay::ChildReceipt { replayed: true, ..receipt.clone() });
    assert_eq!(checkpoint, before);
    for (field, value) in [
        ("host", json!("localhost")),
        ("confidential", json!(false)),
        ("provider_timeout", json!(43)),
    ] {
        let mut changed = request.clone();
        changed.launch.as_mut().unwrap().node["attempt"][field] = value;
        assert_eq!(
            add(&document, &mut checkpoint, "a", &changed).unwrap_err().code,
            "idempotency_conflict",
            "attempt.{field}"
        );
        assert_eq!(checkpoint, before, "attempt.{field}");
    }
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    ).unwrap();
    let (plan, _) = validate_flat_plan(&expanded, &plans::generous()).unwrap();
    let child = plan.nodes.iter().find(|node| node.id == receipt.node).unwrap();
    assert_eq!(child.attempt.as_ref(), Some(&spec));
    let mut owner = FakeOwner {
        observations: [
            AttemptObservation::Pending { attempt_ref: Some("attempt-a".into()) },
            terminal(AttemptOutcome::Succeeded),
        ].into(),
        ..Default::default()
    };
    // A live child shares its producer's lifetime: keep the producer running
    // until the child's launch has completed, then let the graph settle.
    let mut journal = MemoryJournal(Some(checkpoint));
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(progress(&run.checkpoint, "a").state, NodeState::Running);
    assert_eq!(progress(&run.checkpoint, &receipt.node).state, NodeState::Succeeded);
    let launched = owner.calls.iter().position(|(key, _)| key.node_id == receipt.node).unwrap();
    assert_eq!(owner.attempt_specs[launched].as_ref(), Some(&spec));
    owner.observations.extend([
        terminal(AttemptOutcome::Succeeded),
        terminal(AttemptOutcome::Succeeded),
    ]);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
}

#[test]
fn writer_children_get_their_own_worktree_and_depth_is_bounded() {
    let (document, mut checkpoint) = live(None);
    assert_eq!(child_depth(&document), relay::DEFAULT_CHILD_DEPTH);
    let mut request = child("impl", "w1");
    request.writer = true;
    request.write_scope = vec!["src/parser".into()];
    let receipt = add(&document, &mut checkpoint, "a", &request).unwrap();
    assert_eq!(receipt.worktree.as_deref(), Some(".worktrees/live-a--impl"));
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    let writer = expanded
        .nodes
        .iter()
        .find(|node| node.id == "a--impl")
        .unwrap();
    assert_eq!(writer.access, AccessMode::WorktreeWrite);
    assert_eq!(writer.worktree.as_deref(), Some(".worktrees/live-a--impl"));
    assert_eq!(writer.write_scope, ["src/parser"]);
    let mut scoped_reader = child("read", "r1");
    scoped_reader.write_scope = vec!["src".into()];
    assert_eq!(
        add(&document, &mut checkpoint, "a", &scoped_reader)
            .unwrap_err()
            .code,
        "invalid"
    );

    // `limits.child_depth` overrides the default.
    let (document, mut checkpoint) = live(Some(json!({"child_depth": 2})));
    assert_eq!(child_depth(&document), 2);
    add(&document, &mut checkpoint, "a", &child("one", "d1")).unwrap();
    mark_running(&mut checkpoint, "a--one");
    add(&document, &mut checkpoint, "a--one", &child("two", "d2")).unwrap();
    mark_running(&mut checkpoint, "a--one--two");
    let error = add(
        &document,
        &mut checkpoint,
        "a--one--two",
        &child("three", "d3"),
    )
    .unwrap_err();
    assert_eq!(error.code, "child_depth", "{}", error.message);
    assert!(error.message.contains("limits.child_depth=2"));
}

/// Small: admitting a child preserves the graph's decision owner; a child
/// without an explicit inbox must ask the same chair as its parent graph.
#[test]
fn child_decision_inherits_the_graph_chair_session() {
    let (document, mut checkpoint) = live_with_session(None, Some("parent-chair"));
    let mut request = child("choose", "child-decision");
    request.launch = Some(relay::ChildLaunch {
        templates: Default::default(),
        node: json!({
            "id":"choose", "kind":"decide", "access":"read_only",
            "decide":{"question":"Continue the child?", "options":[
                {"id":"continue", "label":"Continue"},
                {"id":"hold", "label":"Hold"}
            ]}
        }),
    });
    let receipt = add(&document, &mut checkpoint, "a", &request).unwrap();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending {
            attempt_ref: Some("attempt-a".into()),
        }]
        .into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal(Some(checkpoint));
    let waiting = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(waiting.checkpoint.graph_id, receipt.graph_id);
    assert_eq!(progress(&waiting.checkpoint, &receipt.node).state, NodeState::Running);
    let ask = waiting
        .checkpoint
        .asks
        .iter()
        .find(|ask| ask.node.as_deref() == Some(receipt.node.as_str()))
        .unwrap();
    assert_eq!(ask.kind, AskKind::Decide);
    assert_eq!(ask.session, "parent-chair");
    assert_eq!(owner.calls.len(), 1, "only the existing parent runs an agent");
    assert_eq!(owner.calls[0].0.node_id, "a");
}

/// Small: a refused child cannot consume a revision or leave a partial node or
/// receipt that would turn a corrected relay request into an idempotency clash.
#[test]
fn child_admission_refusals_leave_the_graph_unchanged() {
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        journal: None,
        library: &library,
        resolvers: &resolvers,
        scan: &patch::scan_for_secrets,
        limits: &limits,
    };
    for (parent, request, expected) in [
        ("missing", child("child", "unknown-parent"), "invalid"),
        ("b", child("child", "settled-parent"), "parent_settled"),
        ("a", child("child", "depth-limit"), "child_depth"),
        ("a", {
            let mut request = child("reader", "scoped-reader");
            request.write_scope = vec!["src".into()];
            request
        }, "invalid"),
        ("a", {
            let mut request = child("secret", "secret-child");
            request.brief = "Use sk-abcdefghijklmnopqrstuvwx".into();
            request
        }, "secret"),
    ] {
        let (document, mut checkpoint) = live((expected == "child_depth").then(|| json!({"child_depth":0})));
        let before = checkpoint.clone();
        let error = relay::admit_child_bound(
            &mut patch::LiveGraph { document: &document, checkpoint: &mut checkpoint },
            parent,
            &request,
            &context,
            Some(("parent-branch", "1111111111111111111111111111111111111111")),
        ).unwrap_err();
        assert_eq!(error.code, expected, "{}", request.idempotency_key);
        assert_eq!(checkpoint, before, "{}", request.idempotency_key);
        assert_eq!(patch::revision(&checkpoint), 0);
    }
}

#[test]
fn a_waiting_parent_yields_its_slot_to_its_child() {
    let (document, mut checkpoint) = live(None);
    let mut writer = child("fix", "y1");
    writer.writer = true;
    writer.write_scope = vec!["src/parser".into()];
    add(&document, &mut checkpoint, "a", &writer).unwrap();
    let limits = crate::config::GraphLimits {
        concurrency: Some(1),
        ..Default::default()
    };
    let run = |checkpoint: &GraphCheckpoint| {
        let mut owner = FakeOwner {
            observations: [
                AttemptObservation::Pending {
                    attempt_ref: Some("attempt-a".into()),
                },
                AttemptObservation::Pending {
                    attempt_ref: Some("attempt-fix".into()),
                },
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = MemoryJournal(Some(checkpoint.clone()));
        let run = advance_with_budget(
            &document,
            &mut owner,
            &mut journal,
            &limits,
            &Default::default(),
        )
        .unwrap();
        if progress(&run.checkpoint, "a--fix").state == NodeState::Running {
            let binding = progress(&run.checkpoint, "a--fix")
                .writer_binding
                .as_ref()
                .unwrap();
            assert!(
                owner
                    .permissions
                    .contains(&(AccessMode::WorktreeWrite, Some(binding.worktree.clone())))
            );
        }
        (run.checkpoint, owner.calls.len())
    };
    // The parent holds the only slot while it works.
    let (held, _) = run(&checkpoint);
    assert_eq!(progress(&held, "a--fix").state, NodeState::Pending);

    assert!(set_waiting(&mut checkpoint, "a", true));
    assert!(relay::yields_slot(progress(&checkpoint, "a")));
    let (yielded, _) = run(&checkpoint);
    assert_eq!(progress(&yielded, "a--fix").state, NodeState::Running);
    assert_eq!(progress(&yielded, "a").state, NodeState::Running);
    // Spawning is not completion: the parent's dependant waits for the child.
    assert_eq!(progress(&yielded, "b").state, NodeState::Pending);

    set_waiting(&mut checkpoint, "a", false);
    assert!(!relay::yields_slot(progress(&checkpoint, "a")));
}

#[test]
fn cancelling_a_parent_cancels_its_subtree_except_detached_children() {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("one", "c1")).unwrap();
    mark_running(&mut checkpoint, "a--one");
    add(&document, &mut checkpoint, "a--one", &child("deep", "c2")).unwrap();
    let mut keep = child("keep", "c3");
    keep.detached = true;
    let receipt = add(&document, &mut checkpoint, "a", &keep).unwrap();
    assert!(receipt.detached);
    mark_running(&mut checkpoint, "a--keep");
    add(&document, &mut checkpoint, "a--keep", &child("under", "c4")).unwrap();

    let cancelled = cancel_subtree(&document, &mut checkpoint, "a").unwrap();
    let mut skipped = cancelled.skipped.clone();
    skipped.sort();
    assert_eq!(skipped, ["a--one", "a--one--deep"]);
    let mut detached = cancelled.detached.clone();
    detached.sort();
    assert_eq!(detached, ["a--keep", "a--keep--under"]);
    assert!(cancelled.running.is_empty());
    let one = progress(&checkpoint, "a--one");
    assert_eq!(
        (one.state, one.cancelled_from),
        (NodeState::Skipped, Some(NodeState::Running))
    );
    assert_eq!(one.skip_reason.as_deref(), Some("parent a cancelled"));
    assert_eq!(progress(&checkpoint, "a--keep").state, NodeState::Running);
    assert_eq!(
        progress(&checkpoint, "a--keep--under").state,
        NodeState::Pending
    );
    // The parent and unrelated nodes are the caller's business.
    assert_eq!(progress(&checkpoint, "a").state, NodeState::Running);
    assert_eq!(progress(&checkpoint, "b").state, NodeState::Pending);
}

#[test]
fn relay_frames_are_tagged_and_strict() {
    let add: relay::RelayOp = serde_json::from_value(
        json!({"op":"add","id":"fix","brief":"Fix","idempotency_key":"k","writer":true}),
    )
    .unwrap();
    assert!(
        matches!(add, relay::RelayOp::Add(ref request) if request.writer && request.template == "task")
    );
    let wait: relay::RelayOp = serde_json::from_value(json!({"op":"wait","waiting":true})).unwrap();
    assert_eq!(wait, relay::RelayOp::Wait { waiting: true });
    assert!(
        serde_json::from_value::<relay::RelayOp>(
            json!({"op":"add","id":"x","brief":"b","idempotency_key":"k","host":"elsewhere"})
        )
        .is_err()
    );
}

#[test]
fn detached_children_do_not_gate_their_parents_dependants() {
    let (document, mut checkpoint) = live(None);
    let mut library = template::TemplateLibrary::bundled();
    library
        .insert_toml(
            template::Layer::Repository,
            "branch",
            "nodes = [{id = 'leaf', use = 'task'}]",
        )
        .unwrap();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        journal: None,
        library: &library,
        resolvers: &resolvers,
        limits: &limits,
        scan: &|_| Ok(()),
    };
    patch::admit_patch(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        Some("a"),
        &patch::Patch {
            schema_version: patch::PATCH_SCHEMA_VERSION,
            nodes: vec![json!({"id":"keep", "use":"branch", "extensions":{"detached":true}})],
            ..Default::default()
        },
        &context,
    )
    .unwrap();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    let consumer = expanded.nodes.iter().find(|node| node.id == "b").unwrap();
    assert!(consumer.after_expansion.is_empty());
    let cancelled = cancel_subtree(&document, &mut checkpoint, "a--keep").unwrap();
    assert_eq!(cancelled.skipped, ["a--keep--leaf"]);
    assert!(cancelled.detached.is_empty());
}

fn settled_parent_stops_running_children(outcome: AttemptOutcome) {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("stop", "stop")).unwrap();
    let mut keep = child("keep", "keep");
    keep.detached = true;
    add(&document, &mut checkpoint, "a", &keep).unwrap();
    set_waiting(&mut checkpoint, "a", true);
    let mut owner = FakeOwner {
        observations: ["attempt-a", "attempt-stop", "attempt-keep"]
            .into_iter()
            .map(|id| AttemptObservation::Pending {
                attempt_ref: Some(id.into()),
            })
            .collect(),
        ..Default::default()
    };
    let launched = advance(&document, &mut owner, &mut MemoryJournal(Some(checkpoint))).unwrap();
    let mut owner = FakeOwner {
        observations: [
            AttemptObservation::Terminal {
                outcome,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                launched: true,
                result: None,
                attempt_ref: Some("attempt-a".into()),
            },
            AttemptObservation::Pending {
                attempt_ref: Some("attempt-keep".into()),
            },
        ]
        .into(),
        ..Default::default()
    };
    let mut launched = launched;
    cancel_node_subtree(&document, &mut launched.checkpoint, "b", None).unwrap();
    let mut journal = MemoryJournal(Some(launched.checkpoint));
    let failed = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.cancellation_stops, 1);
    assert_eq!(
        progress(&failed.checkpoint, "a--keep").state,
        NodeState::Running
    );
    owner.cancelled_attempt_settled = true;
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("attempt-keep".into()),
    });
    let settled = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        progress(&settled.checkpoint, "a--stop").state,
        NodeState::Skipped
    );
    assert_eq!(
        progress(&settled.checkpoint, "a--keep").state,
        NodeState::Running
    );
    owner.observations.push_back(AttemptObservation::Pending {
        attempt_ref: Some("attempt-keep".into()),
    });
    let reloaded = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        progress(&reloaded.checkpoint, "a--stop").state,
        NodeState::Skipped
    );
}

#[test]
fn unstarted_child_cancellation_survives_checkpoint_reload() {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("stop", "stop")).unwrap();
    let parent = checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "a")
        .unwrap();
    parent.state = NodeState::Failed;
    parent.attempts[0].state = AttemptState::Terminal;
    parent.attempts[0].outcome = Some(AttemptOutcome::OtherFailure);
    let mut journal = MemoryJournal(Some(checkpoint));
    let mut owner = FakeOwner::default();
    advance(&document, &mut owner, &mut journal).unwrap();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(
        progress(&run.checkpoint, "a--stop").state,
        NodeState::Skipped
    );
}

#[test]
fn child_timeout_is_preserved_in_the_admitted_graph_node() {
    let (document, mut checkpoint) = live(None);
    let mut request = child("bounded", "bounded-1");
    request.timeout_seconds = Some(17);
    add(&document, &mut checkpoint, "a", &request).unwrap();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    let node = expanded
        .nodes
        .iter()
        .find(|node| node.id == "a--bounded")
        .unwrap();
    assert_eq!(node.deadline_seconds, Some(17));
    let mut invalid = child("unbounded", "bounded-2");
    invalid.timeout_seconds = Some(0);
    assert_eq!(
        add(&document, &mut checkpoint, "a", &invalid)
            .unwrap_err()
            .code,
        "invalid"
    );
}

#[test]
fn composite_child_timeout_reaches_every_provider_leaf_and_retains_template_evidence() {
    let (document, mut checkpoint) = live(None);
    let mut request = child("chain", "chain-1");
    request.template = "task-chain".into();
    request.timeout_seconds = Some(23);
    request.tier = Some("junior".into());
    add(&document, &mut checkpoint, "a", &request).unwrap();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &plans::generous(),
    )
    .unwrap();
    let leaves: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("a--chain--") && node.composite.is_none())
        .collect();
    assert_eq!(leaves.len(), 2);
    for leaf in leaves {
        assert_eq!(leaf.deadline_seconds, Some(23));
        assert_eq!(leaf.prompt, request.brief);
        assert_eq!(leaf.extra[template::TEMPLATE_FIELD], "task");
        assert!(!leaf.extra.contains_key("extensions"));
    }
    let evidence = &checkpoint.expansions[0].uses[0];
    assert_eq!(evidence.name, "task-chain");
    assert_eq!(evidence.parameters["brief"], request.brief);
    assert!(
        add(&document, &mut checkpoint, "a", &request)
            .unwrap()
            .replayed
    );
    let mut unbound = child("panel", "panel-1");
    unbound.template = "review-panel".into();
    assert_eq!(
        add(&document, &mut checkpoint, "a", &unbound)
            .unwrap_err()
            .code,
        "invalid"
    );
    request.id = "writerchain".into();
    request.idempotency_key = "writerchain-1".into();
    request.writer = true;
    assert_eq!(
        add(&document, &mut checkpoint, "a", &request)
            .unwrap_err()
            .code,
        "invalid"
    );
}

#[test]
fn writer_child_binds_parent_branch_and_replays_original_source() {
    let (document, mut checkpoint) = live(None);
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        limits: &limits,
        scan: &|_| Ok(()),
        journal: None,
    };
    let mut request = child("writer", "bound-writer");
    request.writer = true;
    let base = "1111111111111111111111111111111111111111";
    let receipt = relay::admit_child_bound(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "a",
        &request,
        &context,
        Some(("parent", base)),
    )
    .unwrap();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &limits,
    )
    .unwrap();
    let node = expanded
        .nodes
        .iter()
        .find(|node| node.id == receipt.node)
        .unwrap();
    assert_eq!(
        node.delivery,
        Some(provenant_records::run::DeliveryMode::Integrate(
            "parent".into()
        ))
    );
    assert_eq!(
        node.source,
        Some(SourceBinding::Commit {
            commit: base.into()
        })
    );
    let replay = relay::admit_child_bound(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "a",
        &request,
        &context,
        Some(("parent", "2222222222222222222222222222222222222222")),
    )
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        relay::child_states(&checkpoint, "a")["children"][0]["settled"],
        false
    );
    checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == receipt.node)
        .unwrap()
        .state = NodeState::Succeeded;
    assert_eq!(
        relay::child_states(&checkpoint, "a")["children"][0]["settled"],
        true
    );
}

#[test]
fn review_panel_child_resolves_host_lenses_preserves_subject_and_replays() {
    let (document, mut checkpoint) = live(None);
    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([
        ("review-frame".into(), vec!["true".into()]), ("review-verify".into(), vec!["true".into()]), ("review-synthesis".into(), vec!["true".into()]),
        ("review-verifiers".into(), vec!["1".into()]),
        ("review-panel".into(), vec!["correctness".into(), "tests".into()]),
        ("review-advice-correctness".into(), vec![String::new()]),
        ("review-advice-tests".into(), vec![String::new()]),
    ]);
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        limits: &limits,
        scan: &|_| Ok(()),
        journal: None,
    };
    let mut request = child("panel", "resolved-panel");
    request.template = "review-panel".into();
    request.brief = "Review the parser at commit 1111111111111111111111111111111111111111".into();
    let receipt = admit_child(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "a",
        &request,
        &context,
    )
    .unwrap();
    let expanded = plans::expand(
        &document,
        &checkpoint.extensions,
        &checkpoint.expansions,
        &limits,
    )
    .unwrap();
    let leaves: Vec<_> = expanded
        .nodes
        .iter()
        .filter(|node| node.id.starts_with("a--panel--") && node.composite.is_none())
        .collect();
    for lens in ["correctness", "tests"] {
        assert!(
            leaves.iter().any(
                |node| node.prompt.contains(&format!("through the {lens} lens"))
                    && node.prompt.contains(&request.brief)
            ),
            "missing lens {lens}"
        );
    }
    assert!(
        leaves
            .iter()
            .all(|node| node.prompt.contains(&request.brief))
    );
    let before = checkpoint.clone();
    let replay = admit_child(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "a",
        &request,
        &context,
    )
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.node, receipt.node);
    assert_eq!(checkpoint, before);
}

#[test]
fn child_receipt_replay_uses_original_request_after_template_and_parent_changes() {
    let (document, mut checkpoint) = live(None);
    let request = child("chain", "stable-receipt");
    let mut request = ChildRequest {
        template: "task-chain".into(),
        ..request
    };
    let record = std::cell::RefCell::new(None);
    let journal = |admission: &patch::PatchRecord| {
        *record.borrow_mut() = Some(admission.clone());
        Ok(())
    };
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        limits: &limits,
        scan: &|_| Ok(()),
        journal: Some(&journal),
    };
    let receipt = admit_child(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "a",
        &request,
        &context,
    )
    .unwrap();
    // Replay also works with the evidence delivered to the durable journal callback.
    checkpoint.expansions[0].patch = record.into_inner();
    let encoded = serde_json::to_vec(&checkpoint).unwrap();
    let mut checkpoint: GraphCheckpoint = serde_json::from_slice(&encoded).unwrap();
    checkpoint
        .nodes
        .iter_mut()
        .find(|node| node.id == "a")
        .unwrap()
        .state = NodeState::Succeeded;
    let before = checkpoint.clone();
    // Neither the template nor the resolver is needed once admission is durable.
    let library = template::TemplateLibrary::default();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        library: &library,
        resolvers: &resolvers,
        limits: &limits,
        scan: &|_| Ok(()),
        journal: None,
    };
    let replay = admit_child(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        "a",
        &request,
        &context,
    )
    .unwrap();
    assert_eq!(
        replay,
        relay::ChildReceipt {
            replayed: true,
            ..receipt
        }
    );
    assert_eq!(checkpoint, before);
    assert_eq!(
        add(&document, &mut checkpoint, "b", &request)
            .unwrap_err()
            .code,
        "parent_settled"
    );
    request.brief = "Different brief".into();
    assert_eq!(
        add(&document, &mut checkpoint, "a", &request)
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert_eq!(checkpoint, before);
}

#[test]
fn receipt_replays_for_a_historical_attempt_after_the_parent_settles() {
    let (document, mut checkpoint) = live(None);
    mark_running(&mut checkpoint, "a");
    let request = child("late", "lost-reply");
    let receipt = add(&document, &mut checkpoint, "a", &request).unwrap();
    // The parent settles; its attempt is no longer a live node.
    let node = checkpoint.nodes.iter_mut().find(|n| n.id == "a").unwrap();
    node.state = NodeState::Succeeded;
    for row in &mut node.attempts {
        row.state = AttemptState::Terminal;
    }
    assert_eq!(parent_of_attempt(&checkpoint, "attempt-a"), None);
    let parent = relay::historical_parent_of_attempt(&checkpoint, "attempt-a").unwrap();
    assert_eq!(parent, "a");
    let replay = relay::replay_child(&document, &checkpoint, parent, &request)
        .unwrap()
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.node, receipt.node);
    assert_eq!(replay.revision, receipt.revision);
    // A new key on a settled parent is not a replay.
    let fresh = child("other", "new-key");
    assert!(
        relay::replay_child(&document, &checkpoint, parent, &fresh)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        add(&document, &mut checkpoint, "a", &fresh).unwrap_err().code,
        "parent_settled"
    );
    // A graph retry archives the settled generation. Its endpoint may still
    // replay a lost acknowledgement, without acquiring fresh lane authority.
    let node = checkpoint.nodes.iter_mut().find(|node| node.id == "a").unwrap();
    node.previous_attempts.append(&mut node.attempts);
    node.state = NodeState::Pending;
    let checkpoint: GraphCheckpoint = serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    assert_eq!(parent_of_attempt(&checkpoint, "attempt-a"), None);
    let parent = relay::historical_parent_of_attempt(&checkpoint, "attempt-a").unwrap();
    assert_eq!(parent, "a");
    let replay = relay::replay_child(&document, &checkpoint, parent, &request).unwrap().unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.node, receipt.node);
    assert_eq!(replay.revision, receipt.revision);

}

#[test]
fn provisioning_fence_survives_a_checkpoint_round_trip_until_cleared() {
    let (_, mut checkpoint) = live(None);
    relay::fence_provisioning(&mut checkpoint, "a--w", ".worktrees/live-a--w", "abc123");
    let reloaded: GraphCheckpoint =
        serde_json::from_value(serde_json::to_value(&checkpoint).unwrap()).unwrap();
    assert_eq!(
        relay::pending_provisioning(&reloaded),
        vec![("a--w".into(), ".worktrees/live-a--w".into(), "abc123".into())]
    );
    relay::clear_provisioning(&mut checkpoint, "a--w");
    assert!(relay::pending_provisioning(&checkpoint).is_empty());
    assert!(!checkpoint.extra.contains_key("pending_writer_provisioning"));
}

#[test]
fn successful_parent_closes_attached_children_but_not_detached_children() {
    let (document, mut checkpoint) = live(None);
    checkpoint.nodes.iter_mut().find(|node| node.id == "a").unwrap().extra.insert("children_cancelled".into(), true.into());
    add(&document, &mut checkpoint, "a", &child("stop", "s1")).unwrap();
    let mut keep = child("keep", "s2");
    keep.detached = true;
    add(&document, &mut checkpoint, "a", &keep).unwrap();
    let parent = checkpoint.nodes.iter_mut().find(|node| node.id == "a").unwrap();
    parent.state = NodeState::Succeeded;
    parent.attempts[0].state = AttemptState::Terminal;
    parent.attempts[0].outcome = Some(AttemptOutcome::Succeeded);
    let mut journal = MemoryJournal(Some(checkpoint.clone()));
    relay::settle_subtrees(&document, &mut checkpoint, &mut FakeOwner::default(), &mut journal, &mut Default::default()).unwrap();
    assert_eq!(progress(&checkpoint, "a--stop").state, NodeState::Skipped);
    assert_eq!(progress(&checkpoint, "a--keep").state, NodeState::Pending);
}

#[test]
fn whole_graph_cancel_preserves_detached_children_and_their_descendants() {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("stop", "g1")).unwrap();
    let mut keep = child("keep", "g2");
    keep.detached = true;
    add(&document, &mut checkpoint, "a", &keep).unwrap();
    mark_running(&mut checkpoint, "a--keep");
    add(&document, &mut checkpoint, "a--keep", &child("under", "g3")).unwrap();
    let protected = relay::fence_detached_cancellation(&document, &mut checkpoint).unwrap();
    assert_eq!(protected.into_iter().collect::<Vec<_>>(), ["a--keep", "a--keep--under"]);
    checkpoint.cancelled = true;
    let mut owner = FakeOwner::default();
    let mut journal = MemoryJournal(Some(checkpoint.clone()));
    settle_cancelled_nodes(&mut checkpoint, &mut owner, &mut journal).unwrap();
    assert_eq!(owner.cancellation_stops, 1);
    assert_eq!(progress(&checkpoint, "a--stop").state, NodeState::Skipped);
    assert_eq!(progress(&checkpoint, "a--keep").state, NodeState::Running);
    assert_eq!(progress(&checkpoint, "a--keep--under").state, NodeState::Pending);
}

#[test]
fn yielded_writer_parent_drains_more_children_than_the_slot_limit() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"writers", "integration_branch":"integration", "nodes":[
            {"id":"a", "access":"worktree_write", "worktree":".worktrees/parent", "write_scope":["src"], "prompt":"Start", "route":{"alias":"senior"}}
        ]
    })).unwrap();
    let mut owner = FakeOwner {
        observations: [AttemptObservation::Pending { attempt_ref: Some("attempt-a".into()) }].into(),
        ..Default::default()
    };
    let mut checkpoint = advance(&document, &mut owner, &mut MemoryJournal::default()).unwrap().checkpoint;
    for index in 0..4 {
        let mut request = child(&format!("c{index}"), &format!("slot{index}"));
        request.writer = true;
        request.write_scope = vec!["src".into()];
        add(&document, &mut checkpoint, "a", &request).unwrap();
    }
    set_waiting(&mut checkpoint, "a", true);
    let limits = crate::config::GraphLimits { concurrency: Some(1), ..Default::default() };
    for index in 0..4 {
        let mut owner = FakeOwner {
            observations: [AttemptObservation::Pending { attempt_ref: Some("attempt-a".into()) }, AttemptObservation::Pending { attempt_ref: Some(format!("child-{index}")) }].into(),
            ..Default::default()
        };
        let run = advance_with_budget(&document, &mut owner, &mut MemoryJournal(Some(checkpoint)), &limits, &Default::default()).unwrap();
        checkpoint = run.checkpoint;
        assert_eq!(progress(&checkpoint, &format!("a--c{index}")).state, NodeState::Running);
        assert_eq!(progress(&checkpoint, "a").state, NodeState::Running);
        let completed = checkpoint.nodes.iter_mut().find(|node| node.id == format!("a--c{index}")).unwrap();
        completed.state = NodeState::Succeeded;
        for attempt in &mut completed.attempts {
            attempt.state = AttemptState::Terminal;
            attempt.outcome = Some(AttemptOutcome::Succeeded);
        }
    }
}

#[test]
fn failed_parent_stops_running_children_and_preserves_detached_subtrees() {
    settled_parent_stops_running_children(AttemptOutcome::OtherFailure);
}

#[test]
fn successful_parent_stops_running_children_and_preserves_detached_subtrees() {
    settled_parent_stops_running_children(AttemptOutcome::Succeeded);
}

#[test]
fn successful_final_patch_producer_leaves_its_planned_work_pending() {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("producer", "final-producer")).unwrap();
    mark_running(&mut checkpoint, "a--producer");
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        journal: None, library: &library, resolvers: &resolvers,
        limits: &limits, scan: &|_| Ok(()),
    };
    patch::admit_patch(&mut patch::LiveGraph { document: &document, checkpoint: &mut checkpoint }, Some("a--producer"), &patch::Patch {
        schema_version: patch::PATCH_SCHEMA_VERSION,
        nodes: vec![json!({"id":"planned", "use":"task", "brief":"Scheduled final work"})],
        ..Default::default()
    }, &context).unwrap();
    for id in ["a", "a--producer"] {
        let parent = checkpoint.nodes.iter_mut().find(|node| node.id == id).unwrap();
        parent.state = NodeState::Succeeded;
        for attempt in &mut parent.attempts {
            attempt.state = AttemptState::Terminal;
            attempt.outcome = Some(AttemptOutcome::Succeeded);
        }
    }
    relay::settle_subtrees(&document, &mut checkpoint, &mut FakeOwner::default(), &mut MemoryJournal::default(), &mut Default::default()).unwrap();
    assert_eq!(progress(&checkpoint, "a--producer--planned").state, NodeState::Pending);
}

#[test]
fn cancelled_graph_keeps_advancing_its_detached_children() {
    let (document, mut checkpoint) = live(None);
    let mut keep = child("keep", "advance-keep");
    keep.detached = true;
    add(&document, &mut checkpoint, "a", &keep).unwrap();
    checkpoint.cancelled = true;
    let mut owner = FakeOwner {
        cancelled_attempt_settled: true,
        observations: [AttemptObservation::Pending { attempt_ref: Some("keep-attempt".into()) }].into(),
        ..Default::default()
    };
    let mut journal = MemoryJournal(Some(checkpoint));
    let running = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(running.status, GraphStatus::Pending);
    assert!(running.checkpoint.cancelled);
    assert_eq!(progress(&running.checkpoint, "a").state, NodeState::Skipped);
    assert_eq!(progress(&running.checkpoint, "b").state, NodeState::Skipped);
    assert_eq!(progress(&running.checkpoint, "a--keep").state, NodeState::Running);
    assert_eq!(owner.calls.len(), 1);
    owner.observations.push_back(done_as("keep-attempt"));
    let completed = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(completed.status, GraphStatus::Cancelled);
    assert_eq!(progress(&completed.checkpoint, "a--keep").state, NodeState::Succeeded);
}

#[test]
fn stopping_selected_child_preserves_parent_and_sibling() {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("one", "select-one")).unwrap();
    add(&document, &mut checkpoint, "a", &child("two", "select-two")).unwrap();
    mark_running(&mut checkpoint, "a--one");
    add(&document, &mut checkpoint, "a--one", &child("deep", "select-deep")).unwrap();
    let stopped = cancel_node_subtree(&document, &mut checkpoint, "a--one", None).unwrap();
    assert_eq!(stopped.skipped, ["a--one--deep", "a--one"]);
    assert_eq!(progress(&checkpoint, "a").state, NodeState::Running);
    assert_eq!(progress(&checkpoint, "a--two").state, NodeState::Pending);
}

#[test]
fn detached_node_asks_survive_whole_graph_cancellation() {
    let (document, mut checkpoint) = live(None);
    let mut keep = child("keep", "ask-keep");
    keep.detached = true;
    add(&document, &mut checkpoint, "a", &keep).unwrap();
    checkpoint.cancelled = true;
    relay::fence_detached_cancellation(&document, &mut checkpoint).unwrap();
    for (id, node) in [("attached-ask", "a"), ("detached-ask", "a--keep")] {
        checkpoint.asks.push(serde_json::from_value(json!({
            "ask_id":id, "kind":"decide", "node":node, "session":"chair",
            "title":"Choose", "body":"Choose", "options":[], "resolved":false, "closed":false
        })).unwrap());
    }
    close_resolved_asks(&mut checkpoint, &mut FakeOwner::default(), &mut MemoryJournal::default()).unwrap();
    assert!(checkpoint.asks[0].resolved);
    assert!(!checkpoint.asks[1].resolved);
}

#[test]
fn child_key_is_scoped_to_logical_parent_and_replays_a_retried_attempt() {
    let (mut document, mut checkpoint) = live(None);
    document.integration_branch = Some("integration".into());
    let mut request = child("fix", "same-child");
    request.template = "implement".into();
    request.writer = true;
    let first = add(&document, &mut checkpoint, "a", &request).unwrap();
    let parent = checkpoint.nodes.iter_mut().find(|node| node.id == "a").unwrap();
    parent.previous_attempts.append(&mut parent.attempts);
    let mut next = parent.previous_attempts[0].clone();
    next.attempt_ref = Some("attempt-a-retry".into());
    parent.attempts.push(next);
    let checkpoint: GraphCheckpoint = serde_json::from_slice(&serde_json::to_vec(&checkpoint).unwrap()).unwrap();
    let mut checkpoint = checkpoint;
    assert_eq!(parent_of_attempt(&checkpoint, "attempt-a-retry"), Some("a"));
    let replay = add(&document, &mut checkpoint, "a", &request).unwrap();
    assert_eq!(replay, relay::ChildReceipt { replayed:true, ..first.clone() });
    mark_running(&mut checkpoint, "b");
    let other = add(&document, &mut checkpoint, "b", &request).unwrap();
    assert_eq!(other.node, "b--fix");
    assert_ne!(other.worktree, first.worktree);
    assert!(!other.replayed);
    assert_eq!(checkpoint.expansions.len(), 2);
}

#[test]
fn honest_stop_continuation_replays_its_logical_parents_writer_child() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"continuing", "integration_branch":"integration", "nodes":[
            {"id":"parent", "access":"worktree_write", "worktree":"/fixture/parent", "write_scope":["parent"], "prompt":"Start", "route":{"alias":"senior"}}
        ]
    })).unwrap();
    let mut owner = FakeOwner::default();
    owner.observations.push_back(AttemptObservation::Pending { attempt_ref:Some("first-attempt".into()) });
    let mut journal = MemoryJournal::default();
    let mut checkpoint = advance(&document, &mut owner, &mut journal).unwrap().checkpoint;
    let mut request = child("fix", "stable-writer");
    request.template = "implement".into();
    request.writer = true;
    request.write_scope = vec!["child".into()];
    let first = add(&document, &mut checkpoint, "parent", &request).unwrap();
    journal.0 = Some(checkpoint);
    owner.observations.extend([
        AttemptObservation::Terminal {
            attempt_ref:Some("first-attempt".into()), outcome:AttemptOutcome::Succeeded, launched:true,
            result:Some(Box::new(capture("```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"Continue checking\"}\n```"))),
            result_warning:None, result_path:None, branch:Some("parent".into()), commit:Some("a".repeat(40)),
        },
        AttemptObservation::Terminal {
            attempt_ref:Some("child-attempt".into()), outcome:AttemptOutcome::Succeeded, launched:true,
            result:Some(Box::new(capture("Child completed"))), result_warning:None, result_path:None,
            branch:Some("child".into()), commit:Some("a".repeat(40)),
        },
        AttemptObservation::Pending { attempt_ref:Some("continuation-attempt".into()) },
    ]);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    let mut checkpoint: GraphCheckpoint = serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap();
    let parent = parent_of_attempt(&checkpoint, "continuation-attempt").unwrap_or_else(|| panic!("missing live continuation: {checkpoint:?}; launches={:?}", owner.calls)).to_owned();
    assert_eq!(parent, "parent--continue");
    assert!(checkpoint.expansions.iter().any(|expansion| expansion.continuation && expansion.source == "parent"));
    let before = checkpoint.clone();
    let replay = add(&document, &mut checkpoint, &parent, &request).unwrap();
    assert_eq!(replay, relay::ChildReceipt { replayed:true, ..first });
    assert_eq!(checkpoint, before);
    let children = relay::child_states(&checkpoint, &parent);
    assert_eq!(children["children"].as_array().unwrap().len(), 1);
    assert_eq!(children["children"][0]["node"], "parent--fix");
    assert_eq!(children["children"][0]["settled"], true);
    assert_eq!(progress(&checkpoint, "parent--fix").state, NodeState::Succeeded);
    assert_eq!(owner.cancellation_stops, 0);
    checkpoint.nodes.iter_mut().find(|node| node.id == "parent--fix").unwrap().state = NodeState::Pending;
    let cancelled = relay::cancel_subtree(&document, &mut checkpoint, &parent).unwrap();
    assert_eq!(cancelled.skipped, ["parent--fix"]);
    assert_eq!(progress(&checkpoint, &parent).state, NodeState::Running);
}

#[test]
fn bound_document_child_preserves_explicit_worktree_and_upstream_source() {
    let (document, mut checkpoint) = live(None);
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext { journal:None, library:&library, resolvers:&resolvers, scan:&|_| Ok(()), limits:&limits };
    let request: ChildRequest = serde_json::from_value(json!({
        "id":"bound", "brief":"", "writer":true, "idempotency_key":"bound", "launch":{
            "node":{"id":"bound", "use":"implement", "brief":"Preserve existing work", "worktree":".worktrees/existing",
                "source":{"type":"node", "node":"a"}, "depends_on":["a"], "delivery":"push"}
        }
    })).unwrap();
    assert!(!request.needs_worktree_provisioning());
    let receipt = relay::admit_child_bound(&mut patch::LiveGraph { document:&document, checkpoint:&mut checkpoint },
        "a", &request, &context, Some(("parent", &"a".repeat(40)))).unwrap();
    assert_eq!(receipt.worktree.as_deref(), Some(".worktrees/existing"));
    let expanded = plans::expand(&document, &checkpoint.extensions, &checkpoint.expansions, &limits).unwrap();
    let node = expanded.nodes.iter().find(|node| node.id == receipt.node).unwrap();
    assert_eq!(node.worktree, receipt.worktree);
    assert_eq!(node.source, Some(SourceBinding::Node { node:"a".into() }));
    assert_eq!(node.delivery, Some(provenant_records::run::DeliveryMode::Push));
}

#[test]
fn planner_native_child_runs_before_parent_settles_and_final_fragment_closes_once() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"planning-parent", "nodes":[
            {"id":"parent", "kind":"task", "access":"read_only", "prompt":"Plan", "route":{"alias":"senior"},
                "source":{"type":"commit", "commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
                "plan":{"kinds":["task"], "max_nodes":1, "planned_review":true}}
        ]
    })).unwrap();
    let mut owner = FakeOwner::default();
    owner.observations.push_back(AttemptObservation::Pending { attempt_ref:Some("planner-attempt".into()) });
    let mut journal = MemoryJournal::default();
    let mut checkpoint = advance(&document, &mut owner, &mut journal).unwrap().checkpoint;
    let receipt = add(&document, &mut checkpoint, "parent", &child("scout", "scout")).unwrap();
    let expanded = plans::expand(&document, &checkpoint.extensions, &checkpoint.expansions, &plans::generous()).unwrap();
    assert!(expanded.nodes.iter().find(|node| node.id == receipt.node).unwrap().depends_on.is_empty());
    journal.0 = Some(checkpoint);
    owner.observations.extend([
        AttemptObservation::Pending { attempt_ref:Some("planner-attempt".into()) },
        AttemptObservation::Pending { attempt_ref:Some("scout-attempt".into()) },
    ]);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(parent_of_attempt(&run.checkpoint, "scout-attempt"), Some("parent--scout"));
    assert_eq!(progress(&run.checkpoint, "parent").state, NodeState::Running);
    journal.0 = Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
    // A live child shares its producer's lifetime. The planner waits for its
    // scout before returning the final patch, as a lane waits through its relay.
    owner.observations.extend([
        AttemptObservation::Pending { attempt_ref:Some("planner-attempt".into()) },
        terminal_from("Scout done", "scout-attempt"),
    ]);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(progress(&run.checkpoint, "parent").state, NodeState::Running);
    assert_eq!(progress(&run.checkpoint, "parent--scout").state, NodeState::Succeeded);
    journal.0 = Some(serde_json::from_slice(&serde_json::to_vec(&run.checkpoint).unwrap()).unwrap());
    owner.observations.extend([
        terminal_from("```fragment\n{\"schema_version\":1,\"nodes\":[{\"id\":\"final\",\"use\":\"task\",\"brief\":\"Finish\",\"source\":{\"type\":\"commit\",\"commit\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},\"acceptance_checks\":[\"Done\"],\"after\":[]}]}\n```", "planner-attempt"),
        terminal_from("Finished", "final-attempt"),
    ]);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint.pause_reason());
    assert!(run.checkpoint.successful());
    assert_eq!(run.checkpoint.expansions.len(), 2);
    assert_eq!(run.checkpoint.expansions[1].extra.get("final_result"), Some(&json!(true)));
    assert!(ids(&document, &run.checkpoint).contains(&"parent--final".into()));
    assert_eq!(advance(&document, &mut owner, &mut journal).unwrap().checkpoint, run.checkpoint);
}

#[test]
fn rich_detached_children_keep_their_subtree_after_parent_and_graph_cancellation() {
    let (document, mut checkpoint) = live(None);
    let mut request = child("detached", "rich-detached");
    request.detached = true;
    request.launch = Some(relay::ChildLaunch {
        templates: Default::default(),
        node: json!({"id":"detached", "kind":"composite", "access":"read_only",
            "composite":{"nodes":[{"id":"work", "access":"read_only", "prompt":"Inspect", "route":{"alias":"senior"}}],
                "exports":[{"node":"work", "outputs":["result"]}]}}),
    });
    add(&document, &mut checkpoint, "a", &request).unwrap();
    let stopped = cancel_subtree(&document, &mut checkpoint, "a").unwrap();
    assert!(stopped.skipped.is_empty());
    assert!(stopped.running.is_empty());
    assert_eq!(stopped.detached, ["a--detached--work", "a--detached"]);
    let protected = relay::fence_detached_cancellation(&document, &mut checkpoint).unwrap();
    assert_eq!(protected.into_iter().collect::<Vec<_>>(), ["a--detached", "a--detached--work"]);
    assert_eq!(progress(&checkpoint, "a--detached--work").state, NodeState::Pending);
}

#[test]
fn composite_child_preserves_compiled_review_metadata() {
    let (document, mut checkpoint) = live(None);
    let mut library = template::TemplateLibrary::bundled();
    library.insert_toml(template::Layer::Repository, "checks", r#"
nodes = [{id = "lens", use = "review", with = {lens = "tests"}}]
"#).unwrap();
    let resolvers = Default::default();
    let limits = plans::generous();
    let context = patch::PatchContext {
        journal: None, library: &library, resolvers: &resolvers,
        scan: &|_| Ok(()), limits: &limits,
    };
    let mut request = child("review", "review-metadata");
    request.template = "checks".into();
    admit_child(&mut patch::LiveGraph {document:&document, checkpoint:&mut checkpoint},
        "a", &request, &context).unwrap();
    let expanded = plans::expand(&document, &checkpoint.extensions,
        &checkpoint.expansions, &limits).unwrap();
    let lens = expanded.nodes.iter().find(|node| node.id == "a--review--lens").unwrap();
    assert_eq!(lens.extra[template::TEMPLATE_FIELD], "review");
    assert_eq!(lens.extra["review_lens"], "tests");
    assert_eq!(lens.extra["review_behaviour"], "lens");
    assert!(!lens.extra.contains_key("extensions"));
}
