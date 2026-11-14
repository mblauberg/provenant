use super::*;
use provenant_fabric_runtime::graph::AttemptKey;

// Medium: retained receiver checkpoints, without provider processes.
#[test]
fn submission_cancellation_drains_only_attached_work() -> TestResult {
    use provenant_fabric_runtime::graph::GraphJournal;
    if host_test(
        "fabric::peer_tests::peer_graph_tests::cancellation_tests::submission_cancellation_drains_only_attached_work",
    )? {
        return Ok(());
    }
    let (rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let actor = factory.context.identity.inbox.actor;
    let operation: provenant_core::OperationId = "detached-cancel".parse()?;
    let graph = crate::fabric::peer::peer_graph_name(actor, &operation)?;
    let budget = WaitBudget::cli(factory.clock.mono_now(), 30)?;
    let mut checkpoint: graph::GraphCheckpoint = serde_json::from_value(json!({
        "schema_version":1, "graph_id":graph, "plan_digest":"digest", "cancelled":true,
        "nodes":[
            {"id":"parent", "state":"skipped", "worktree":null, "attempts":[]},
            {"id":"parent--child", "state":"running", "worktree":null, "attempts":[]}
        ]
    }))?;
    for state in [graph::NodeState::Pending, graph::NodeState::Running] {
        checkpoint.nodes[1].state = state;
        for marker in [
            None,
            Some(json!(false)),
            Some(json!("true")),
            Some(json!(true)),
        ] {
            let key = "detached_from_graph_cancel";
            checkpoint.nodes[1].extra.remove(key);
            if let Some(value) = marker.clone() {
                checkpoint.nodes[1].extra.insert(key.into(), value);
            }
            let mut journal = FileJournal::open(
                &factory.context.roots.state,
                factory.context.identity.project,
                &graph,
                budget,
                &cancel,
                &factory.clock,
            )?;
            journal.bind(graph::GraphExecution {
                extra: Default::default(),
                bases: None,
                directory: rig.project.clone(),
                document: serde_json::from_value(json!({
                    "schema_version":1, "graph_id":graph,
                    "nodes":[{"id":"parent", "prompt":"work", "access":"read_only",
                        "route":{"alias":"senior"}}]
                }))?,
            })?;
            journal.save(&checkpoint)?;
            drop(journal);
            assert_eq!(
                super::super::super::peer::peer_cancel_submission(
                    &factory, actor, &operation, false, budget, &cancel,
                )?,
                marker == Some(json!(true)),
                "state={state:?}, detached marker={marker:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn lost_peer_reply_cancellation_survives_reconnect_and_stops_receiver_subtree() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::cancellation_tests::lost_peer_reply_cancellation_survives_reconnect_and_stops_receiver_subtree",
    )? {
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;
    let (_receiver_rig, receiver_factory) = receiver()?;
    let cancel = CancelToken::new();
    let key = AttemptKey {
        extra: Default::default(),
        graph_id: "origin-cancel".into(),
        node_id: "parent".into(),
        route_index: 0,
        attempt: 1,
    };
    let operation = graph_operation("graph-attempt-operation", &key)?;
    let receipt = submit_parent(&receiver_factory, operation.as_str(), &cancel)?;
    let receiver_graph = receipt.extra["peer_graph"]["graph_id"]
        .as_str()
        .ok_or("graph")?;
    let parent = receipt.targets[0].attempt;
    add_child(&receiver_factory, parent, "lost-reply-child", &cancel)?;
    let receiver_run = advance_receiver(&receiver_factory, receiver_graph, &cancel)?;
    let child_ref = receiver_run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id == "parent--child")
        .ok_or("child")?
        .attempts[0]
        .attempt_ref
        .as_deref()
        .ok_or("child ref")?;
    let child: AttemptId = child_ref.split_once('/').ok_or("child target")?.1.parse()?;

    let (mut origin_rig, mut origin) = receiver()?;
    // Peer project locators are relative to the sending host's home.
    let origin_project = origin_rig.home.join("project");
    std::fs::rename(&origin_rig.project, &origin_project)?;
    origin_rig.project = origin_project;
    let remote_host = receiver_factory.context.identity.host;
    // The fixture's local identities are deterministic; choose a distinct
    // receiver address in the origin routing table and response envelope.
    let remote_host = if remote_host == origin.context.identity.host {
        provenant_core::HostId::from_bytes([91; 16])
    } else {
        remote_host
    };
    let ssh = origin_rig.root.join("cancel-ssh");
    let response = origin_rig.root.join("cancel-response");
    let frame = origin_rig.root.join("cancel-request");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\ncat > '{}'\nif test -f '{}'; then cat '{}'; else exit 255; fi\n",
            frame.display(),
            response.display(),
            response.display()
        ),
    )?;
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        origin_rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric]\nssh_executable={:?}\n[domains.fabric.peers.renamed]\nhost='{remote_host}'\ndestination='receiver'\n",
            ssh.to_string_lossy()
        ),
    )?;
    let reopen = || -> TestResult<Factory> {
        let roots = crate::roots::child(
            &origin_rig.home,
            &origin_rig.project,
            origin_rig.state.clone(),
        )?;
        Ok(Factory {
            context: Context::open(roots, &origin_rig.home, &origin_rig.project)?,
            project_attribution: false,
            clock: SystemClock,
            backoff: ThreadPause,
            executor: SystemExecutor::new(1)?,
            home: origin_rig.home.clone(),
            cwd: origin_rig.project.clone(),
            assets: origin_rig.project.clone(),
            host_launch: None,
            workers: &FreeWorkers,
        })
    };
    origin = reopen()?;
    let execution = FileJournal::execution(
        &receiver_factory.context.roots.state,
        receiver_factory.context.identity.project,
        receiver_graph,
    )?
    .ok_or("execution")?;
    let mut execution = execution;
    execution.directory = origin.cwd.clone();
    execution.document.graph_id = key.graph_id.clone();
    execution.document.nodes[0]
        .attempt
        .get_or_insert_with(Default::default)
        .host = Some("removed-old-alias".into());
    execution.document.extra.remove("submission_patch");
    graph::patch::submission(
        &mut execution.document,
        "origin-lost-reply",
        &origin.context.config.graph.limits,
        origin.context.port_table.worktree_layout.clone(),
    )?;
    struct LostReplyOwner {
        actor: String,
        host: String,
    }
    impl graph::AttemptOwner for LostReplyOwner {
        type Error = FacadeError;
        fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
            provenant_config::PortTable::builtin().worktree_layout
        }
        fn actor(&self) -> String {
            self.actor.clone()
        }
        fn placement_host_id(&self, _name: &str) -> Result<Option<String>, Self::Error> {
            Ok(Some(self.host.clone()))
        }
        fn observe_or_start(
            &mut self,
            _key: &AttemptKey,
            _node: graph::NodeInput<'_>,
            _route: &graph::GraphRoute,
            _access: graph::AccessMode,
            _worktree: Option<&str>,
            _replay: graph::Replay<'_>,
        ) -> Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Unknown { attempt_ref: None })
        }
    }
    let budget = WaitBudget::cli(origin.clock.mono_now(), 30)?;
    {
        let mut journal = FileJournal::open(
            &origin.context.roots.state,
            origin.context.identity.project,
            &key.graph_id,
            budget,
            &cancel,
            &origin.clock,
        )?;
        journal.bind(execution.clone())?;
        let mut owner = LostReplyOwner {
            actor: receiver_factory.context.identity.inbox.actor.to_string(),
            host: remote_host.to_string(),
        };
        let run = graph::advance_with_budget(
            &execution.document,
            &mut owner,
            &mut journal,
            &origin.context.config.graph.limits,
            &origin.context.config.graph.budget,
        )?;
        assert!(run.checkpoint.nodes[0].attempts[0].attempt_ref.is_none());
        assert_eq!(
            run.checkpoint.nodes[0].attempts[0].extra["placement_host_id"],
            json!(remote_host)
        );
    }
    // Offline cancellation must persist the fence, never mistake the empty
    // local operation store for proof that the receiver accepted nothing.
    let offline = origin.graph(
        GraphInput::Cancel {
            extra: Default::default(),
            id: key.graph_id.clone(),
            reason: None,
        },
        budget,
        &cancel,
    );
    assert!(offline.is_err());
    assert!(
        frame.exists(),
        "cancellation must reach configured stable host: {offline:?}"
    );
    let retained = FileJournal::read_checkpoint(
        &origin.context.roots.state,
        origin.context.identity.project,
        &key.graph_id,
    )?
    .ok_or("origin checkpoint")?;
    assert!(retained.cancelled);
    assert_eq!(retained.nodes[0].state, graph::NodeState::Running);
    let request: provenant_fabric_runtime::hosts::PeerRequest =
        serde_json::from_slice(&std::fs::read(&frame)?)?;
    assert_eq!(
        request.agent,
        format!("graph-{}", receiver_factory.context.identity.inbox.actor)
    );
    let PeerAction::CancelSubmission {
        submission,
        operation_id,
        stop,
        ..
    } = &request.action
    else {
        panic!("submission cancellation")
    };
    assert!(*stop);
    assert_eq!(submission, &operation);
    assert_eq!(
        operation_id,
        &super::super::super::peer::peer_cancel_operation(submission)?
    );
    let mut wrong_namespace = request.clone();
    wrong_namespace.agent = "ordinary-agent".into();
    assert!(matches!(
        super::super::super::peer::peer_cancel_submission_request(
            &receiver_factory,
            &wrong_namespace,
            WaitBudget::cli(receiver_factory.clock.mono_now(), 30)?,
            &cancel,
        ),
        Err(FacadeError::ConfinementRefused { .. })
    ));
    let mut wrong_operation = request.clone();
    if let PeerAction::CancelSubmission { operation_id, .. } = &mut wrong_operation.action {
        *operation_id = "rebound-cancel".parse()?;
    }
    assert!(matches!(
        super::super::super::peer::peer_cancel_submission_request(
            &receiver_factory,
            &wrong_operation,
            WaitBudget::cli(receiver_factory.clock.mono_now(), 30)?,
            &cancel,
        ),
        Err(FacadeError::Conflict)
    ));
    let settled = super::super::super::peer::peer_cancel_submission_request(
        &receiver_factory,
        &request,
        WaitBudget::cli(receiver_factory.clock.mono_now(), 30)?,
        &cancel,
    )?;
    assert!(settled);
    let owner = BinaryRunOwner {
        factory: &receiver_factory,
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
    let reply = PeerResponse {
        extra: Default::default(),
        version: 1,
        protocol: PROTOCOL,
        host: remote_host,
        project: ProjectLocator::new(&origin.context.identity, &origin.home)?.key,
        result: PeerResult::SubmissionCancelled {
            settled,
            extra: Default::default(),
        },
    };
    std::fs::write(&response, serde_json::to_vec(&reply)?)?;
    drop(origin);
    let origin = reopen()?; // A new process resumes the retained cancelled graph.
    let run = advance_receiver(&origin, &key.graph_id, &cancel)?;
    assert!(run.checkpoint.cancelled);
    assert_eq!(run.checkpoint.nodes[0].state, graph::NodeState::Skipped);
    assert!(run.checkpoint.nodes[0].attempts[0].attempt_ref.is_none());
    Ok(())
}

#[test]
fn cancellation_without_a_receiver_checkpoint_remains_uncertain() -> TestResult {
    if host_test(
        "fabric::peer_tests::peer_graph_tests::cancellation_tests::cancellation_without_a_receiver_checkpoint_remains_uncertain",
    )? {
        return Ok(());
    }
    let (_rig, factory) = receiver()?;
    let cancel = CancelToken::new();
    let operation = "late-peer-submission".parse()?;
    for stop in [false, true] {
        assert!(matches!(
            super::super::super::peer::peer_cancel_submission(
                &factory,
                factory.context.identity.inbox.actor,
                &operation,
                stop,
                WaitBudget::cli(factory.clock.mono_now(), 30)?,
                &cancel
            ),
            Err(FacadeError::NotFound)
        ));
    }
    submit_parent(&factory, operation.as_str(), &cancel)?;
    assert!(super::super::super::peer::peer_cancel_submission(
        &factory,
        factory.context.identity.inbox.actor,
        &operation,
        true,
        WaitBudget::cli(factory.clock.mono_now(), 30)?,
        &cancel
    )?);
    Ok(())
}
