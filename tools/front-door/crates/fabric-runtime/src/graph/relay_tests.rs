use super::*;
use relay::{
    ChildRequest, admit_child, cancel_subtree, child_depth, parent_of_attempt, set_waiting,
};
use serde_json::{Value, json};

fn live(limits: Option<Value>) -> (GraphDocument, GraphCheckpoint) {
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"live", "nodes":[
            {"id":"a", "access":"read_only", "prompt":"Start", "route":{"alias":"senior"}},
            {"id":"b", "access":"read_only", "prompt":"Then", "depends_on":["a"], "route":{"alias":"senior"}}
        ]
    }))
    .unwrap();
    document.limits = limits;
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
        library: &library,
        resolvers: &resolvers,
        scan: &|_| Ok(()),
        journal: None,
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
        (Some("a"), Some("lane-a-fix-1"))
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

#[test]
fn a_waiting_parent_yields_its_slot_to_its_child() {
    let (document, mut checkpoint) = live(None);
    add(&document, &mut checkpoint, "a", &child("fix", "y1")).unwrap();
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
