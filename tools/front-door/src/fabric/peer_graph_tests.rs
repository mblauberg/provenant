#[allow(unused_imports)] // The worktree driver includes the peer harness without libtest.
use super::*;
use provenant_fabric_runtime::graph::{self, FileJournal};
use provenant_fabric_runtime::runs::SubmissionReceipt;
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn receiver() -> TestResult<(crate::worktree_tests::support::TestRig, Factory)> {
    let rig = crate::worktree_tests::repository()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let factory = Factory {
        context: Context::open(roots, &rig.home, &rig.project)?,
        project_attribution: false,
        clock: SystemClock,
        backoff: ThreadPause,
        executor: SystemExecutor::new(1)?,
        home: rig.home.clone(),
        cwd: rig.project.clone(),
        assets: rig.project.clone(),
        host_launch: None,
        mcp_project: None,
        workers: &FreeWorkers,
    };
    Ok((rig, factory))
}

fn submit_parent(
    factory: &Factory,
    operation: &str,
    cancel: &CancelToken,
) -> TestResult<SubmissionReceipt> {
    let input = serde_json::from_value(json!({
        "op":"submit", "operation_id":operation, "concurrency":1,
        "tasks":[{"prompt":"Launch a child and collect its answer", "access":"read_only",
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent"}}]
    }))?;
    Ok(provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            factory,
            input,
            factory.context.identity.inbox.actor,
            provenant_fabric_runtime::graph::AttemptSpec {
                provider_timeout: Some(17),
                ..Default::default()
            },
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            cancel,
        )
    })?)
}

fn advance_receiver(
    factory: &Factory,
    id: &str,
    cancel: &CancelToken,
) -> TestResult<graph::GraphRun> {
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let execution = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        id,
    )?
    .ok_or("receiver graph missing")?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        id,
        budget,
        cancel,
        &factory.clock,
    )?;
    let mut owner = BoundGraphOwner {
        factory,
        integration_base: None,
        integration_branch: None,
        network: None,
        capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget,
        cancel,
        prices: None,
        tick: false,
    };
    Ok(provenant_cli::run::with_deferred_provider_launch(|| {
        graph::advance_with_budget(
            &execution.document,
            &mut owner,
            &mut journal,
            &factory.context.config.graph.limits,
            &factory.context.config.graph.budget,
        )
    })?)
}

fn add_child(
    factory: &Factory,
    parent: AttemptId,
    idempotency: &str,
    cancel: &CancelToken,
) -> TestResult<serde_json::Value> {
    Ok(provenant_cli::fabric::graph_relay::serve(
        &factory.context,
        &parent.to_string(),
        json!({"op":"add","id":"child","use":"task","brief":"Return the child result",
            "idempotency_key":idempotency,"launch":{"node":{"id":"child","access":"read_only",
                "prompt":"Return the child result","route":{"adapter":"fixture","model":"fixture/model"}},"templates":{}}}),
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        cancel,
        &factory.clock,
    )?)
}

#[test]
fn peer_hosted_lane_launches_a_local_child_and_collects_its_result() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_hosted_lane_launches_a_local_child_and_collects_its_result",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let receipt = submit_parent(&factory, "peer-parent-result", &cancel)?;
    let graph = receipt.extra["peer_graph"]["graph_id"]
        .as_str()
        .ok_or("graph pointer")?;
    let parent = receipt.targets[0].attempt;
    assert_eq!(receipt.targets[0].host, Some(factory.context.identity.host));
    let replay = submit_parent(&factory, "peer-parent-result", &cancel)?;
    assert_eq!(replay.targets, receipt.targets);
    assert_eq!(replay.extra["peer_graph"], receipt.extra["peer_graph"]);
    let changed: SubmitInput = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-parent-result", "concurrency":1,
        "tasks":[{"prompt":"A changed brief must not reuse the accepted operation", "access":"read_only",
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent"}}]
    }))?;
    assert!(matches!(
        super::super::peer::peer_submit_graph(
            &factory,
            changed,
            factory.context.identity.inbox.actor,
            Default::default(),
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel
        ),
        Err(FacadeError::Conflict)
    ));

    let parent_owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let parent_store = parent_owner.store(&cancel)?;
    let parent_call = parent_owner.call(&cancel);
    let parent_record = parent_store.mark_starting_for_fixture(
        parent,
        &provenant_run::RouteRecord {
            provider: Some("fixture".into()),
            model: Some("fixture/model".into()),
            ..Default::default()
        },
        &parent_call,
    )?;
    let request = parent_store.request(parent_record.run_id, &parent_call)?;
    assert_eq!(request.tasks[0].provider_timeout_seconds, 17);
    let directory = provenant_os::fs::Directory::open_anchor(
        &rig.state
            .join(format!("runs/{}/attempts/{parent}", parent_record.run_id)),
    )?;
    parent_store.admit(
        parent,
        &directory,
        &provenant_run::SystemResources,
        &factory.clock,
        &cancel,
    )?;
    let yielded = provenant_cli::fabric::graph_relay::serve(
        &factory.context,
        &parent.to_string(),
        json!({"op":"wait","waiting":true}),
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
        &factory.clock,
    )?;
    assert_eq!(yielded["waiting"], true);
    let child_receipt = add_child(&factory, parent, "add-child", &cancel)?;
    assert_eq!(child_receipt["graph_id"], graph);
    assert_eq!(child_receipt["parent"], "parent");
    assert_eq!(child_receipt["node"], "parent--child");
    assert_eq!(
        add_child(&factory, parent, "add-child", &cancel)?["replayed"],
        true
    );
    let running = advance_receiver(&factory, graph, &cancel)?;
    let child = running
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "parent--child")
        .ok_or("child node")?;
    let reference = child.attempts[0]
        .attempt_ref
        .as_deref()
        .ok_or("child attempt")?;
    let (host, id) = reference.split_once('/').ok_or("child reference")?;
    assert_eq!(host, factory.context.identity.host.to_string());
    let child_id: AttemptId = id.parse()?;
    let mut owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = provenant_run::Request {
        clock: &factory.clock,
        cancel: &cancel,
    };
    let record = store.mark_starting_for_fixture(
        child_id,
        &provenant_run::RouteRecord {
            provider: Some("fixture".into()),
            model: Some("fixture/model".into()),
            ..Default::default()
        },
        &call,
    )?;
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    publish_success(&rig, &store, &record, head.parse()?, &call)?;
    let settled = advance_receiver(&factory, graph, &cancel)?;
    assert_eq!(
        settled
            .checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "parent--child")
            .ok_or("child")?
            .state,
        graph::NodeState::Succeeded
    );
    let children = provenant_cli::fabric::graph_relay::serve(
        &factory.context,
        &parent.to_string(),
        json!({"op":"children"}),
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
        &factory.clock,
    )?;
    assert_eq!(children["children"][0]["settled"], true);
    let pointer = super::super::peer::peer_graph_fields(&factory, parent)?;
    assert_eq!(pointer["peer_graph"]["graph_id"], graph);
    assert_eq!(pointer["peer_origin_context"]["graph_id"], "origin-graph");
    let page = super::super::peer::peer_graph_fields_page(&factory, parent, 1)?;
    assert_eq!(
        page["peer_subtree"]["checkpoint"]["nodes"]
            .as_array()
            .ok_or("node page")?
            .len(),
        1
    );
    assert_eq!(
        page["peer_subtree"]["checkpoint"]["nodes"][0]["id"],
        "parent--child"
    );
    assert!(page["peer_subtree"]["next_node"].is_null());
    assert!(matches!(
        super::super::peer::peer_graph_fields_page(&factory, parent, 3),
        Err(FacadeError::InvalidInput { .. })
    ));

    assert_eq!(
        pointer["peer_subtree"]["checkpoint"]["nodes"][1]["state"],
        "succeeded"
    );
    let result = owner.output(
        OutputInput::Read {
            extra: Default::default(),
            target: AttemptTarget {
                extra: Default::default(),
                host: Some(factory.context.identity.host),
                attempt: child_id,
            },
            part: OutputPart::Result,
            offset: 0,
            max_bytes: 4096,
        },
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
    )?;
    assert!(
        matches!(result, OutputResult::Chunk { bytes: OutputBytes::Utf8(ref text), eof: true, .. } if text == "OK")
    );
    assert!(!matches!(
        store.record(parent, &call)?.state,
        AttemptState::Terminal { .. }
    ));
    Ok(())
}

#[test]
fn peer_hosted_lane_cancellation_stops_parent_and_child() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_hosted_lane_cancellation_stops_parent_and_child",
    )? {
        return Ok(());
    }
    let (_rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let receipt = submit_parent(&factory, "peer-parent-cancel", &cancel)?;
    let graph = receipt.extra["peer_graph"]["graph_id"]
        .as_str()
        .ok_or("graph pointer")?;
    let parent = receipt.targets[0].attempt;
    add_child(&factory, parent, "add-child", &cancel)?;
    let running = advance_receiver(&factory, graph, &cancel)?;
    let child = running
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "parent--child")
        .ok_or("child")?
        .attempts[0]
        .attempt_ref
        .as_deref()
        .ok_or("child attempt")?
        .split_once('/')
        .ok_or("reference")?
        .1
        .parse()?;
    super::super::peer::peer_cancel_subtree(
        &factory,
        parent,
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
    )?;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    for id in [parent, child] {
        assert!(matches!(
            store.record(id, &owner.call(&cancel))?.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
    }
    let checkpoint = FileJournal::read_checkpoint(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )?
    .ok_or("receiver graph")?;
    assert!(checkpoint.nodes.iter().all(|node| !matches!(
        node.state,
        graph::NodeState::Pending | graph::NodeState::Running
    )));
    Ok(())
}

#[test]
fn peer_hosted_lane_cancellation_reaches_child_after_parent_settles() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_hosted_lane_cancellation_reaches_child_after_parent_settles",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let receipt = submit_parent(&factory, "peer-settled-parent-cancel", &cancel)?;
    let graph = receipt.extra["peer_graph"]["graph_id"]
        .as_str()
        .ok_or("graph pointer")?;
    let parent = receipt.targets[0].attempt;
    add_child(&factory, parent, "add-child", &cancel)?;
    let running = advance_receiver(&factory, graph, &cancel)?;
    let child: AttemptId = running
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "parent--child")
        .ok_or("child")?
        .attempts[0]
        .attempt_ref
        .as_deref()
        .ok_or("child attempt")?
        .split_once('/')
        .ok_or("reference")?
        .1
        .parse()?;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    let record = store.mark_starting_for_fixture(
        parent,
        &provenant_run::RouteRecord {
            provider: Some("fixture".into()),
            model: Some("fixture/model".into()),
            ..Default::default()
        },
        &call,
    )?;
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    publish_success(&rig, &store, &record, head.parse()?, &call)?;
    super::super::peer::peer_cancel_subtree(
        &factory,
        parent,
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
    )?;
    assert!(matches!(
        store.record(parent, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
    assert!(matches!(
        store.record(child, &call)?.state,
        AttemptState::Terminal {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    Ok(())
}

#[test]
fn peer_hosted_lane_subtree_pages_are_bounded_complete_and_private() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_hosted_lane_subtree_pages_are_bounded_complete_and_private",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::GraphJournal;
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let receipt = submit_parent(&factory, "peer-subtree-pages", &cancel)?;
    let graph = receipt.extra["peer_graph"]["graph_id"]
        .as_str()
        .ok_or("graph pointer")?;
    let parent = receipt.targets[0].attempt;
    let expected = {
        let mut journal = FileJournal::open(
            &factory.context.roots.state,
            factory.context.identity.project,
            graph,
            WaitBudget::cli(factory.clock.mono_now(), 30)?,
            &cancel,
            &factory.clock,
        )?;
        let mut checkpoint = journal.load(graph)?.ok_or("receiver graph")?;
        let anchor = checkpoint.nodes[0].clone();
        // Retained node metadata exceeds one peer page without needing provider
        // processes or large prompts. Captured fields must stay out of frames.
        for index in 0_u128..1200 {
            let mut node = anchor.clone();
            node.id = format!("parent--child-{index:04}");
            node.worktree = Some(rig.project.join("private-checkout").display().to_string());
            node.extra.insert("prompt".into(), json!("private prompt"));
            node.extra.insert("source_path".into(), json!(rig.project));
            for attempt in &mut node.attempts {
                attempt.key.node_id = node.id.clone();
                attempt.attempt_ref = Some(format!(
                    "{}/{}",
                    factory.context.identity.host,
                    AttemptId::from_bytes(index.to_be_bytes())
                ));
                attempt
                    .extra
                    .insert("prompt".into(), json!("private prompt"));
                attempt
                    .extra
                    .insert("source_path".into(), json!(rig.project));
            }
            checkpoint.nodes.push(node);
        }
        let expected = checkpoint
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        journal.save(&checkpoint)?;
        expected
    };
    let corrupt = factory
        .context
        .roots
        .state
        .as_path()
        .join("fabric/graphs")
        .join(factory.context.identity.project.to_string())
        .join("graph-a-corrupt.json");
    std::fs::write(corrupt, b"invalid json")?;
    assert!(
        FileJournal::read_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            "a-corrupt"
        )
        .is_err()
    );
    let mut after = 0;
    let mut pages = 0;
    let mut bytes = 0;
    let mut observed = Vec::new();
    let mut unique = std::collections::BTreeSet::new();
    loop {
        let fields = super::super::peer::peer_graph_fields_page(&factory, parent, after)?;
        let size = serde_json::to_vec(&fields)?.len();
        assert!(size < MAX_FRAME / 2);
        bytes += size;
        pages += 1;
        let subtree = &fields["peer_subtree"];
        for row in subtree["checkpoint"]["nodes"]
            .as_array()
            .ok_or("node page")?
        {
            let id = row["id"].as_str().ok_or("node id")?.to_owned();
            assert!(unique.insert(id.clone()));
            observed.push(id);
            assert!(row["worktree"].is_null());
            assert!(row.get("prompt").is_none());
            assert!(row.get("source_path").is_none());
            for attempt in row["attempts"].as_array().ok_or("attempt metadata")? {
                assert!(attempt.get("prompt").is_none());
                assert!(attempt.get("source_path").is_none());
            }
        }
        let Some(next) = subtree["next_node"].as_u64() else {
            break;
        };
        let next = usize::try_from(next)?;
        assert!(next > after && next < expected.len());
        after = next;
    }
    assert!(pages > 1);
    assert!(bytes > MAX_FRAME / 4);
    assert_eq!(observed, expected);
    Ok(())
}
