#[allow(unused_imports)] // The worktree driver includes the peer harness without libtest.
use super::*;
use provenant_fabric_runtime::graph::{self, FileJournal};
use provenant_fabric_runtime::runs::SubmissionReceipt;
use serde_json::json;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn origin_key() -> provenant_records::WorkKey {
    provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([77; 16]),
        "origin-graph",
        "parent",
        1,
    )
    .unwrap()
}

fn receiver() -> TestResult<(crate::worktree_tests::support::TestRig, Factory)> {
    let rig = crate::worktree_tests::repository()?;
    provenant_os::fixture::private_state_dir(&rig.state)?;
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
        "op":"submit", "operation_id":operation,
        "origin_work_key":origin_key(),
        "task":{"prompt":"Launch a child and collect its answer", "access":"read_only",
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent", "attempt":1, "graph_template_launch":true}}
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

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn peer_document_and_journal_withhold_sender_local_attempt_paths() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_document_and_journal_withhold_sender_local_attempt_paths",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let sender = rig.root.join("sender");
    let checkout = sender.join("checkout");
    let brief = sender.join("private-brief.md");
    let write_root = sender.join("writes");
    std::fs::create_dir_all(&checkout)?;
    std::fs::create_dir_all(&write_root)?;
    std::fs::write(&brief, "Inspect the parser")?;
    let sealed = provenant_records::run::LaunchPolicy {
        confidential: true,
        sources: vec![
            ".provenant/config.toml [launch_policy].protected_paths: inputs/brief".into(),
        ],
        repository_config: Some(".provenant/config.toml".into()),
    };
    let attempt = provenant_fabric_runtime::graph::AttemptSpec {
        directory: Some(checkout.to_string_lossy().into_owned()),
        host: Some("workshop".into()),
        confidential: true,
        policy_inputs: vec![brief.clone()],
        write_roots: vec![write_root.to_string_lossy().into_owned()],
        extra: [(
            "sender_note".into(),
            json!(sender.join("note.md").to_string_lossy()),
        )]
        .into(),
        admitted_by: Some("parent".into()),
        parent_sandboxed: true,
        launch_policy: Some(sealed.clone()),
        ..Default::default()
    };
    let input = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-path-projection",
        "origin_work_key":origin_key(),
        "task":{"prompt":"Launch a child and collect its answer", "access":"read_only",
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent", "attempt":1, "graph_template_launch":true}}
    }))?;
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            input,
            factory.context.identity.inbox.actor,
            attempt,
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            &cancel,
        )
    })?;
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let execution = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )?
    .ok_or("receiver graph missing")?;
    let document = serde_json::to_string(&execution.document)?;
    for path in [&checkout, &brief, &write_root, &sender.join("note.md")] {
        let path = path.to_string_lossy().into_owned();
        assert!(
            !document.contains(&path),
            "sender path {path} reached the peer document"
        );
    }
    let stored = execution.document.nodes[0]
        .attempt
        .as_ref()
        .ok_or("stored attempt")?;
    assert!(stored.directory.is_none());
    assert!(stored.host.is_none());
    assert!(stored.policy_inputs.is_empty());
    assert!(stored.write_roots.is_empty());
    assert!(stored.extra.is_empty());
    assert_eq!(stored.launch_policy, Some(sealed));
    assert!(stored.confidential);
    assert!(stored.parent_sandboxed);
    assert_eq!(stored.admitted_by.as_deref(), Some("parent"));
    Ok(())
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
        writer_launch_budget: None,
        admission_timeout: None,
        repo_facts: Default::default(),
        launch_warnings: Vec::new(),
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
        json!({"op":"add","id":"child","use":"task","prompt":"Return the child result",
            "idempotency_key":idempotency,"launch":{"node":{"id":"child","access":"read_only",
                "prompt":"Return the child result","route":{"adapter":"fixture","model":"fixture/model"}},"templates":{}}}),
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        cancel,
        &factory.clock,
    )?)
}

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn graph_output_claim_requires_the_retained_launch_and_origin() -> TestResult {
    use provenant_fabric_runtime::hosts::{OutputAudience, PeerAction, PeerRequest, ProjectLocator, };
    use provenant_fabric_runtime::identity::{Address, CrossHostProjectKey};
    if host_test(
        "fabric::peer_tests::peer_graph_tests::graph_output_claim_requires_the_retained_launch_and_origin",
    )? {
        return Ok(());
    }
    let (_rig, mut factory) = receiver().map_err(|error| format!("receiver: {error}"))?;
    let source = provenant_core::HostId::from_bytes([42; 16]);
    let actor = Address::new(source, "origin")?.actor;
    let agent = format!("graph-{actor}");
    factory.context.identity.inbox = Address::new(source, &agent)?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let operation = "graph-output-claim".parse()?;
    let submit = serde_json::from_value(json!({
        "op":"submit", "operation_id":operation,
        "origin_work_key":origin_key(),
        "task":{"prompt":"collect output", "access":"read_only",
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent", "attempt":1, "graph_template_launch":true}}
    }))?;
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            submit,
            actor,
            Default::default(),
            None,
            None,
            budget,
            &cancel,
        )
    })
    .map_err(|error| format!("submission: {error:?}"))?;
    let input = OutputInput::Read {
        extra: Default::default(),
        target: receipt.targets[0].clone(),
        part: OutputPart::Result,
        offset: 0,
        max_bytes: provenant_records::run::OUTPUT_LIMIT,
    };
    let audience = OutputAudience::GraphExecution {
        launch_actor: actor,
        launch_operation: operation,
        origin_work_key: origin_key(),
    };
    let request = PeerRequest::new(
        factory.context.identity.host,
        source,
        agent,
        ProjectLocator {
            extra: Default::default(),
            key: CrossHostProjectKey::from_home(Path::new("/fixture"), Path::new("/fixture/repo"))?,
            common_dir: "repo".into(),
        },
        PeerAction::Output {
            extra: Default::default(),
            audience: audience.clone(),
            input: input.clone(),
        },
    );
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let validate = |request: &PeerRequest, audience: &OutputAudience, input: &OutputInput| {
        super::super::peer::validate_graph_output(
            &factory, &owner, request, audience, input, budget, &cancel,
        )
    };
    validate(&request, &audience, &input).map_err(|error| format!("valid claim: {error:?}"))?;
    let mut wrong_agent = request.clone();
    wrong_agent.agent = "other".into();
    assert!(validate(&wrong_agent, &audience, &input).is_err());
    let mut wrong_target = input.clone();
    if let OutputInput::Read { target, .. } = &mut wrong_target {
        target.host = Some(source);
    }
    assert!(validate(&request, &audience, &wrong_target).is_err());
    for forged in [
        OutputAudience::GraphExecution {
            launch_actor: actor,
            launch_operation: "different-operation".parse()?,
            origin_work_key: origin_key(),
        },
        OutputAudience::GraphExecution {
            launch_actor: actor,
            launch_operation: "graph-output-claim".parse()?,
            origin_work_key: provenant_records::WorkKey::new(
                provenant_core::ProjectId::from_bytes([77; 16]),
                "origin-graph",
                "other",
                1,
            )?,
        },
    ] {
        assert!(validate(&request, &forged, &input).is_err());
    }
    Ok(())
}

#[test]
fn peer_placed_reader_binds_its_own_source_like_a_local_reader() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_placed_reader_binds_its_own_source_like_a_local_reader",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let input = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-reader-confinement",
        "origin_work_key":origin_key(),
        "task":{"prompt":"read the fixture", "access":"read_only", "head":head,
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent", "attempt":1, "graph_template_launch":true}}
    }))?;
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            input,
            factory.context.identity.inbox.actor,
            Default::default(),
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            &cancel,
        )
    })?;
    let attempt = receipt.targets[0].attempt;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    let record = store.record(attempt, &call)?;
    let request = store.request(record.attempt_id, &call)?;
    let access = &request.task.access;
    // The receiver binds its own project checkout for a pinned reader, exactly
    // as a local read-only graph node does. No sender-local path or grant is
    // admitted on this boundary.
    assert_eq!(
        access.source_root.as_deref(),
        Some(factory.context.selected_project()),
        "peer bound the wrong source checkout: {access:?}"
    );
    assert!(access.source_identity.is_some(), "{access:?}");
    assert!(!access.source_live, "{access:?}");
    assert!(access.read_roots.is_empty(), "{access:?}");
    assert!(access.write_roots.is_empty(), "{access:?}");
    Ok(())
}

/// Invariant 3: a peer placed reader pins to the head its initiator submitted,
/// never to a caller checkout or an absent source (#4015). The task the peer
/// seals carries the same head, so the reader sees the admitted subject.
#[test]
fn peer_reader_is_pinned_to_the_initiator_subject() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_reader_is_pinned_to_the_initiator_subject",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let input = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-reader-subject-pin",
        "origin_work_key":origin_key(),
        "task":{"prompt":"Read the pinned subject", "access":"read_only", "head":head,
            "subject":{"head":head,"origin":{"kind":"explicit"},
                "pinned_at":"2000-01-01T00:00:00.000000Z","policy":"pinned"},
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"read", "attempt":1,
                "graph_template_launch":true}}
    }))?;
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            input,
            factory.context.identity.inbox.actor,
            Default::default(),
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            &cancel,
        )
    })?;
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let execution = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )?
    .ok_or("receiver graph missing")?;
    let node = execution.document.nodes.first().ok_or("receiver node")?;
    let bound = node.source.as_ref().and_then(|binding| match binding {
        // This module is also compiled under `feature = "fixture"` without
        // libtest, where the `#[test]` bodies are absent and a top-level
        // import would be unused. Keep the path at its single use site.
        provenant_fabric_runtime::graph::SourceBinding::Commit { commit } => {
            Some(commit.as_str())
        }
        _ => None,
    });
    assert_eq!(
        bound,
        Some(head.as_str()),
        "peer reader must pin to the initiator's subject: {node:?}"
    );
    let attempt = receipt.targets[0].attempt;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    let record = store.record(attempt, &call)?;
    let request = store.request(record.attempt_id, &call)?;
    assert_eq!(
        request.task.head.as_deref(),
        Some(head.as_str()),
        "peer task head must be the initiator subject, never None"
    );
    assert_eq!(
        request
            .task
            .subject
            .as_ref()
            .map(|subject| subject.head.to_string())
            .as_deref(),
        request.task.head.as_deref(),
        "peer task subject must name the same pinned head"
    );
    Ok(())
}

#[test]
// Spec invariant 3: a peer reader reads the initiator's pinned task.head, never
// its own checkout HEAD, even when the peer checkout has moved past it.
fn peer_reader_reads_the_pinned_head_not_the_peer_checkout_head() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_reader_reads_the_pinned_head_not_the_peer_checkout_head",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    // The initiator pinned the peer's current commit; the peer checkout then
    // moves on, so its HEAD differs from the pinned subject.
    let pinned = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    crate::worktree_tests::git(&rig, &["commit", "-q", "--allow-empty", "-m", "later"])?;
    let peer_head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    assert_ne!(pinned, peer_head, "the fixture must move the peer HEAD");
    let input = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-reader-pinned-not-peer-head",
        "origin_work_key":provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([77; 16]), "origin-graph", "read", 1)?,
        "task":{"prompt":"Read the pinned subject.", "access":"read_only", "head":pinned,
            "subject":{"head":pinned,"origin":{"kind":"explicit"},
                "pinned_at":"2000-01-01T00:00:00.000000Z","policy":"pinned"},
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"read", "attempt":1,
                "graph_template_launch":true}}
    }))?;
    let cancel = CancelToken::new();
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            input,
            factory.context.identity.inbox.actor,
            Default::default(),
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            &cancel,
        )
    })?;
    let attempt = receipt.targets[0].attempt;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    let record = store.record(attempt, &call)?;
    let request = store.request(record.attempt_id, &call)?;
    assert_eq!(
        request.task.head.as_deref(),
        Some(pinned.as_str()),
        "the peer reader must read the pinned head, not its checkout HEAD {peer_head}: {:?}",
        request.task
    );
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#writer-binding — a remote smoke read node gets
// the same fixture as a local one, carried as bytes rather than a caller path.
fn peer_placed_smoke_reader_carries_its_fixture_to_the_remote_attempt() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_placed_smoke_reader_carries_its_fixture_to_the_remote_attempt",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    let input = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-smoke-reader-fixture",
        "origin_work_key":provenant_records::WorkKey::new(
            provenant_core::ProjectId::from_bytes([77; 16]), "origin-graph", "read", 1)?,
        "task":{"prompt":"Read the supplied fixture and report its single stated fact.",
            "access":"read_only", "head":head,
            "fixture":{"name":"fixture.txt","content":b"smoke fixture\n".to_vec()},
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"read", "attempt":1,
                "graph_template_launch":true}}
    }))?;
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            input,
            factory.context.identity.inbox.actor,
            Default::default(),
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            &cancel,
        )
    })?;
    let attempt = receipt.targets[0].attempt;
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    let record = store.record(attempt, &call)?;
    let request = store.request(record.attempt_id, &call)?;
    let fixture = request
        .task
        .fixture
        .as_ref()
        .ok_or("the remote read node lost its fixture")?;
    assert_eq!(fixture.name, "fixture.txt");
    assert_eq!(fixture.content, b"smoke fixture\n");
    assert!(
        !String::from_utf8_lossy(&fixture.content).contains(factory.cwd.to_string_lossy().as_ref()),
        "a caller path leaked into the fixture bytes"
    );
    Ok(())
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
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let parent = receipt.targets[0].attempt;
    assert_eq!(receipt.targets[0].host, Some(factory.context.identity.host));
    let replay = submit_parent(&factory, "peer-parent-result", &cancel)?;
    assert_eq!(replay.targets, receipt.targets);
    assert_eq!(replay.peer_graph, receipt.peer_graph);
    let changed: SubmitInput = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-parent-result",
        "origin_work_key":origin_key(),
        "task":{"prompt":"A changed brief must not reuse the accepted operation", "access":"read_only",
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent", "attempt":1, "graph_template_launch":true}}
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
    let request = parent_store.request(parent_record.attempt_id, &parent_call)?;
    assert_eq!(request.task.provider_timeout_seconds, 17);
    let directory =
        provenant_os::fs::Directory::open_anchor(&rig.state.join(format!("attempts/{parent}")))?;
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
    assert_eq!(
        pointer
            .peer_graph
            .as_ref()
            .map(|graph| graph.graph_id.as_str()),
        Some(graph)
    );
    assert_eq!(
        pointer
            .peer_origin_context
            .as_ref()
            .and_then(|context| context.graph_id.as_deref()),
        Some("origin-graph")
    );
    assert!(super::super::peer::peer_origin_matches(
        &pointer,
        &origin_key()
    ));
    let execution = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )?
    .ok_or("receiver graph document")?;
    assert_eq!(
        execution.document.peer_origin_work_key.as_ref(),
        Some(&origin_key())
    );
    for wrong in [
        provenant_records::WorkKey::new(
            factory.context.identity.project,
            "origin-graph",
            "parent",
            1,
        )?,
        provenant_records::WorkKey::new(origin_key().project(), "other-graph", "parent", 1)?,
        provenant_records::WorkKey::new(origin_key().project(), "origin-graph", "other-node", 1)?,
        origin_key().next_attempt().ok_or("next origin attempt")?,
    ] {
        assert!(!super::super::peer::peer_origin_matches(&pointer, &wrong));
    }
    let page = super::super::peer::peer_graph_fields_page(&factory, parent, 1)?;
    assert_eq!(
        page.peer_subtree
            .as_ref()
            .ok_or("subtree")?
            .checkpoint
            .nodes
            .len(),
        1
    );
    assert_eq!(
        page.peer_subtree
            .as_ref()
            .ok_or("subtree")?
            .checkpoint
            .nodes[0]
            .id,
        "parent--child"
    );
    assert!(
        page.peer_subtree
            .as_ref()
            .ok_or("subtree")?
            .next_node
            .is_none()
    );
    assert!(matches!(
        super::super::peer::peer_graph_fields_page(&factory, parent, 3),
        Err(FacadeError::InvalidInput { .. })
    ));

    assert_eq!(
        pointer
            .peer_subtree
            .as_ref()
            .ok_or("subtree")?
            .checkpoint
            .nodes[1]
            .state,
        graph::NodeState::Succeeded
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

// Spec: docs/specs/fabric.md#publication-boundary
#[test]
fn peer_review_base_submission_dispatches_with_the_senders_schema() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_review_base_submission_dispatches_with_the_senders_schema",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let head = crate::worktree_tests::git(&rig, &["rev-parse", "HEAD"])?;
    // A review or explore launch names a base. The sender stamps its document
    // schema 2 because a review base is a version-2 field; the peer rebuilds
    // the document from the task and must derive the same schema or its own
    // dispatch admits version 1 carrying version-2 fields and refuses with
    // GraphSchema (#3752).
    let input: SubmitInput = serde_json::from_value(json!({
        "op":"submit", "operation_id":"peer-review-base-schema",
        "origin_work_key":origin_key(),
        "task":{"prompt":"Inspect the parser", "access":"read_only",
            "base": head,
            "route":{"kind":"model","provider":"fixture","model":"fixture/model"},
            "timeout_seconds":30,
            "context":{"graph_id":"origin-graph", "node_id":"parent", "attempt":1, "graph_template_launch":true}}
    }))?;
    let receipt = provenant_cli::run::with_deferred_provider_launch(|| {
        super::super::peer::peer_submit_graph(
            &factory,
            input,
            factory.context.identity.inbox.actor,
            Default::default(),
            None,
            None,
            WaitBudget::cli(factory.clock.mono_now(), 30).unwrap(),
            &cancel,
        )
    })
    .map_err(|error| format!("peer submission with a review base: {error:?}"))?;
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let execution = FileJournal::execution(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
    )?
    .ok_or("receiver graph document")?;
    assert_eq!(execution.document.review_base.as_deref(), Some(head.as_str()));
    assert_eq!(
        execution.document.schema_version,
        u64::from(graph::minimum_schema_version(&execution.document)),
        "the peer must stamp the schema its rebuilt document requires"
    );
    assert_eq!(execution.document.schema_version, 2);

    // The submission went through the peer's real dispatch, not decode alone,
    // and reached a launched node. Fixture-run that node to settlement here.
    let running = advance_receiver(&factory, graph, &cancel)?;
    let node = running
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "parent")
        .ok_or("launched parent node")?;
    let attempt_ref = node.attempts[0]
        .attempt_ref
        .as_deref()
        .ok_or("launched attempt reference")?;
    let (host, id) = attempt_ref.split_once('/').ok_or("attempt reference")?;
    assert_eq!(host, factory.context.identity.host.to_string());
    let attempt: AttemptId = id.parse()?;

    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let store = owner.store(&cancel)?;
    let call = owner.call(&cancel);
    store.mark_starting_for_fixture(
        attempt,
        &provenant_run::RouteRecord {
            provider: Some("fixture".into()),
            model: Some("fixture/model".into()),
            ..Default::default()
        },
        &call,
    )?;
    let directory =
        provenant_os::fs::Directory::open_anchor(&rig.state.join(format!("attempts/{attempt}")))?;
    // Admission moves the attempt to Running and bumps its revision. Publish
    // against the record admission returns, not the pre-admission one, or the
    // terminal commit conflicts on the stale revision.
    let record = store.admit(
        attempt,
        &directory,
        &provenant_run::SystemResources,
        &factory.clock,
        &cancel,
    )?;
    publish_success(&rig, &store, &record, head.parse()?, &call)?;
    let settled = advance_receiver(&factory, graph, &cancel)?;
    assert_eq!(
        settled.checkpoint.nodes[0].state,
        graph::NodeState::Succeeded,
        "{settled:?}"
    );
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
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
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
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
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
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
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
        journal.prepare_fixture_checkpoint(&mut checkpoint)?;
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
        let subtree = fields.peer_subtree.as_ref().ok_or("subtree")?;
        for row in &subtree.checkpoint.nodes {
            assert!(unique.insert(row.id.clone()));
            observed.push(row.id.clone());
            assert!(row.worktree.is_none());
            let projected = serde_json::to_value(row)?;
            assert!(projected.get("prompt").is_none());
            assert!(projected.get("source_path").is_none());
            for attempt in &row.attempts {
                let projected = serde_json::to_value(attempt)?;
                assert!(projected.get("prompt").is_none());
                assert!(projected.get("source_path").is_none());
            }
        }
        let Some(next) = subtree.next_node else {
            break;
        };
        assert!(next > after && next < expected.len());
        after = next;
    }
    assert!(pages > 1);
    assert!(bytes > MAX_FRAME / 4);
    assert_eq!(observed, expected);
    Ok(())
}

#[test]
fn peer_hosted_lane_collection_preserves_origin_status() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_hosted_lane_collection_preserves_origin_status",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let receipt = submit_parent(&factory, "peer-collected-status", &cancel)?;
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let parent = receipt.targets[0].attempt;
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
    let advanced = advance_receiver(&factory, graph, &cancel)?;
    assert_eq!(
        advanced.status,
        graph::GraphStatus::Complete,
        "{advanced:?}"
    );
    assert_eq!(
        FileJournal::collect_terminal(
            &factory.context.roots.state,
            factory.context.identity.project,
            i128::MAX,
            &provenant_core::SweepControl {
                cancel: cancel.clone(),
                ..Default::default()
            },
            &factory.clock
        )?
        .removed,
        1
    );
    assert!(
        FileJournal::read_checkpoint(
            &factory.context.roots.state,
            factory.context.identity.project,
            graph
        )?
        .is_none()
    );
    let fields = super::super::peer::peer_graph_fields(&factory, parent)?;
    assert_eq!(fields.peer_graph_collected, Some(true));
    assert!(super::super::peer::peer_origin_matches(
        &fields,
        &origin_key()
    ));
    assert_eq!(fields.peer_graph, receipt.peer_graph);
    let mut record = store.record(parent, &call)?;
    record.peer.merge(fields);
    let peer = factory.context.identity.host;
    let mut local = provenant_core::HostId::from_bytes([0; 16]);
    if local == peer {
        local = provenant_core::HostId::from_bytes([1; 16]);
    }
    let mut row = json!({"graph_id":"origin-graph","node_id":"parent","state":"terminal","outcome":"succeeded"});
    assert!(super::super::unified::retain_collected_peer_row(
        &mut row, &record, peer, local
    ));
    assert_eq!(row["graph_id"], "origin-graph");
    assert_eq!(row["outcome"], "succeeded");
    assert_eq!(row["peer_graph_collected"], true);
    let mut local_row = json!({});
    assert!(!super::super::unified::retain_collected_peer_row(
        &mut local_row,
        &record,
        peer,
        peer
    ));
    super::super::peer::peer_cancel_subtree(
        &factory,
        parent,
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel,
    )?;
    Ok(())
}

#[test]
fn peer_pacing_refusal_is_definite_and_preserves_its_fact_on_receipt_replay() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::peer_pacing_refusal_is_definite_and_preserves_its_fact_on_receipt_replay",
    )? {
        return Ok(());
    }
    use graph::GraphJournal;
    use provenant_records::diagnostic::{PacingReserveSetting, PacingSettingSource, ResourceFact};
    struct Pacing(ResourceFact);
    impl provenant_run::Providers for Pacing {
        fn prepare(
            &self,
            _: &provenant_run::TaskRequest,
            _: &str,
            _: &provenant_run::LaunchPaths,
            _: Option<&provenant_run::AttemptRecord>,
        ) -> Result<provenant_run::PreparedLaunch, Box<provenant_run::PrepareRefusal>> {
            let mut route = provenant_records::run::SkippedRoute {
                skipped_accounts: Vec::new(),
                extra: Default::default(),
                route: "fixture/model".into(),
                kind: provenant_records::run::SkipKind::Unavailable,
                plan_error: None,
                guard_boundary: None,
                guard_resource: None,
                confinement_guard: None,
                continuation_guard: None,
                cause: Some(provenant_records::RouteExclusionReason::PacingReserve),
                offer_mismatch: None,
                detail: "PRIVATE_PACING_SENTINEL".into(),
            };
            route.set_guard_facts(None, Some(self.0.clone())).unwrap();
            let mut refusal = provenant_run::PrepareRefusal::boxed(
                provenant_records::run::TerminalCause::RouteResolutionRefused,
                "PRIVATE_PACING_SENTINEL",
            );
            refusal.skipped_routes.push(route);
            Err(refusal)
        }
        fn decode(
            &self,
            _: &provenant_run::RouteRecord,
            _: &[u8],
            _: &provenant_supervisor::custody::ExitObservation,
            _: &std::path::Path,
        ) -> Result<provenant_run::Decoded, String> {
            Err("refused provider must never decode".into())
        }
    }
    let (_rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let operation: provenant_core::OperationId = "peer-paced-parent".parse()?;
    let receipt = submit_parent(&factory, operation.as_str(), &cancel)?;
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
        budget,
        &cancel,
        &factory.clock,
    )?;
    let mut checkpoint = journal.load(graph)?.ok_or("receiver graph")?;
    let root = &checkpoint.nodes[0].attempts[0];
    let owner = BinaryRunOwner {
        factory: &factory,
        allow_remote: false,
        actor: Some(root.launch_actor.parse()?),
        chair: None,
    };
    let session = owner.session(&cancel)?;
    let call = owner.call(&cancel);
    let record = session.store.record(receipt.targets[0].attempt, &call)?;
    let fact = ResourceFact::PacingReserve {
        percent: 23,
        setting: PacingReserveSetting::AdapterFiveHour,
        origin: PacingSettingSource::Configuration,
        hold: Box::new(provenant_records::diagnostic::PacingHold {
            account: "codex/opaque-account".into(),
            window: "five_hour".into(),
            setting_path: "domains.routing.pacing_reserve.codex.five_hour_percent".into(),
        }),
    };
    let started = session.store.start(
        record.attempt_id,
        &Pacing(fact.clone()),
        &|_, _| Ok(Vec::new()),
        &call,
    )?;
    let first = owner
        .receipt(&session, operation.clone(), &record, &started, &call)
        .unwrap_err();
    // Recreate the reply-loss boundary: the host sealed the refusal, while the
    // graph has not yet recorded an attempt reference.
    checkpoint.nodes[0].attempts[0].attempt_ref = None;
    checkpoint.nodes[0].attempts[0].state = graph::AttemptState::Starting;
    journal.prepare_fixture_checkpoint(&mut checkpoint)?;
    journal.save(&checkpoint)?;
    drop(journal);
    let blocked = advance_receiver(&factory, graph, &cancel)?;
    assert_eq!(blocked.checkpoint.nodes[0].state, graph::NodeState::Blocked);
    assert!(
        blocked.checkpoint.nodes[0].attempts[0]
            .attempt_ref
            .is_none()
    );
    let current = super::super::peer::peer_graph_receipt(
        &factory,
        operation.clone(),
        graph,
        &blocked.checkpoint,
    )
    .unwrap_err();
    let replay = super::super::peer::peer_retained_receipt(
        &factory,
        &operation,
        factory.context.identity.inbox.actor,
    )
    .unwrap_err();
    for error in [first, current, replay] {
        assert!(matches!(&error, FacadeError::DispatchRefused {
            fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::Resource(actual)), ..
        } if *actual == fact));
        let wire = serde_json::to_string(&error)?;
        assert!(!wire.contains("PRIVATE_PACING_SENTINEL"));
        let decoded: FacadeError = serde_json::from_str(&wire)?;
        assert!(matches!(&decoded, FacadeError::DispatchRefused {
            fact: Some(provenant_fabric_runtime::coordination::facade::DispatchRefusalFact::Resource(actual)), ..
        } if actual == &fact));
        let diagnostic = decoded.diagnostic();
        assert!(diagnostic.detail.as_deref().is_some_and(|detail| {
            detail.contains("23") && detail.contains("domains.routing.pacing_reserve")
        }));
    }
    assert!(blocked.checkpoint.nodes[0].launch_retry.is_none());
    Ok(())
}

#[cfg(test)]
#[allow(unused_imports)] // The harness=false worktree driver includes this file without libtest cases.
mod cancellation_tests {
    include!("peer_cancellation_tests.rs");
}

#[test]
fn retry_output_names_the_recorded_refusal_route_and_reason() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::retry_output_names_the_recorded_refusal_route_and_reason",
    )? {
        return Ok(());
    }
    use graph::GraphJournal;
    struct Allowance;
    impl provenant_run::Providers for Allowance {
        fn prepare(
            &self,
            _: &provenant_run::TaskRequest,
            _: &str,
            _: &provenant_run::LaunchPaths,
            _: Option<&provenant_run::AttemptRecord>,
        ) -> Result<provenant_run::PreparedLaunch, Box<provenant_run::PrepareRefusal>> {
            let mut refusal = provenant_run::PrepareRefusal::boxed(
                provenant_records::run::TerminalCause::RouteResolutionRefused,
                "allowance spent",
            );
            refusal
                .skipped_routes
                .push(provenant_records::run::SkippedRoute {
                    skipped_accounts: Vec::new(),
                    extra: Default::default(),
                    route: "codex/gpt".into(),
                    kind: provenant_records::run::SkipKind::Unavailable,
                    plan_error: None,
                    guard_boundary: None,
                    guard_resource: None,
                    confinement_guard: None,
                    continuation_guard: None,
                    cause: None,
                    offer_mismatch: None,
                    detail: "allowance spent".into(),
                });
            Err(refusal)
        }
        fn decode(
            &self,
            _: &provenant_run::RouteRecord,
            _: &[u8],
            _: &provenant_supervisor::custody::ExitObservation,
            _: &std::path::Path,
        ) -> Result<provenant_run::Decoded, String> {
            Err("refused provider must never decode".into())
        }
    }
    let (_rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let receipt = submit_parent(&factory, "refused-allowance-parent", &cancel)?;
    let graph = receipt
        .peer_graph
        .as_ref()
        .ok_or("graph pointer")?
        .graph_id
        .as_str();
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut journal = FileJournal::open(
        &factory.context.roots.state,
        factory.context.identity.project,
        graph,
        budget,
        &cancel,
        &factory.clock,
    )?;
    let checkpoint = journal.load(graph)?.ok_or("receiver graph")?;
    let attempt = receipt.targets[0].attempt;
    {
        let owner = BinaryRunOwner {
            factory: &factory,
            allow_remote: false,
            actor: Some(checkpoint.nodes[0].attempts[0].launch_actor.parse()?),
            chair: None,
        };
        let session = owner.session(&cancel)?;
        let call = owner.call(&cancel);
        let record = session.store.record(attempt, &call)?;
        let _ = session
            .store
            .start(record.attempt_id, &Allowance, &|_, _| Ok(Vec::new()), &call);
    }
    let mut result = json!({"checkpoint":{"nodes":[{
        "id":"parent", "advance_error_code":"preparation_refused",
        "advance_error":"The launch was refused before admission.",
        "attempts":[{"attempt_ref": format!("{}/{attempt}", factory.context.identity.host)}]
    }]}});
    factory.name_refusal_causes(&mut result);
    let message = result["checkpoint"]["nodes"][0]["advance_error"]
        .as_str()
        .ok_or("advance_error")?;
    let route = provenant_records::run::SkippedRoute {
        skipped_accounts: Vec::new(),
        extra: Default::default(),
        route: "codex/gpt".into(),
        kind: provenant_records::run::SkipKind::Unavailable,
        plan_error: None,
        guard_boundary: None,
        guard_resource: None,
        confinement_guard: None,
        continuation_guard: None,
        cause: None,
        offer_mismatch: None,
        detail: "allowance spent".into(),
    };
    assert!(message.contains(&route.describe()), "{message}");
    assert!(!message.contains(&route.detail), "{message}");
    Ok(())
}
