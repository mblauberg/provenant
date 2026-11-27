#[allow(
    unused_imports,
    reason = "the custom worktree driver includes this module without libtest"
)]
use super::*;
#[test]
fn peer_result_capture_pages_the_complete_stream_and_checks_each_chunk()
-> Result<(), Box<dyn std::error::Error>> {
    let target = AttemptTarget {
        extra: Default::default(),
        host: Some(provenant_core::HostId::from_bytes([1; 16])),
        attempt: provenant_core::AttemptId::from_bytes([2; 16]),
    };
    let text = "x".repeat(130_000) + "\nVERDICT: PASS\n";
    let origin = provenant_records::WorkKey::new(
        provenant_core::ProjectId::from_bytes([7; 16]),
        "origin",
        "review",
        1,
    )?;
    let digest = provenant_core::Digest::from_bytes(
        *provenant_records::CanonicalV1::file_digest(text.as_bytes()).bytes(),
    );
    let chunk = |offset: u64| {
        let end = (offset + provenant_records::run::OUTPUT_LIMIT).min(text.len() as u64);
        OutputResult::Chunk {
            peer: provenant_records::run::PeerGraphFields {
                peer_origin_work_key: Some(origin.clone()),
                ..Default::default()
            },
            length: Some(text.len() as u64),
            planner_fallback: None,
            extra: Default::default(),
            target: target.clone(),
            part: OutputPart::Result,
            state: "succeeded".into(),
            reason: None,
            offset,
            next_offset: end,
            eof: end == text.len() as u64,
            digest,
            bytes: OutputBytes::Utf8(text[offset as usize..end as usize].into()),
        }
    };
    let mut pages = 0;
    let result = capture_graph_output(&target, &origin, Some("VERDICT: PASS"), |offset| {
        pages += 1;
        Ok(chunk(offset))
    })?;
    assert_eq!(pages, 2);
    assert!(result.matched_required);
    assert_eq!(result.verdict.as_deref(), Some("VERDICT: PASS"));
    assert_eq!(result.digest, digest.to_string());
    for malformed in [0, 1, 2, 3, 4, 5] {
        assert!(matches!(
            capture_graph_output(&target, &origin, None, |offset| {
                let mut response = chunk(offset);
                if let OutputResult::Chunk {
                    target,
                    offset: start,
                    next_offset,
                    digest,
                    peer,
                    ..
                } = &mut response
                {
                    match malformed {
                        0 => target.attempt = provenant_core::AttemptId::from_bytes([9; 16]),
                        1 => *start += 1,
                        2 => *next_offset = offset,
                        3 => *digest = provenant_core::Digest::from_bytes([9; 32]),
                        4 => {
                            peer.peer_origin_work_key = Some(origin.next_attempt().unwrap());
                        }
                        _ => {
                            peer.peer_origin_work_key = None;
                        }
                    }
                }
                Ok(response)
            }),
            Err(FacadeError::StateCorrupt)
        ));
    }
    Ok(())
}

#[test]
// Spec: docs/specs/run-launch.md#writer-binding — a remote writer is
// provisioned from a branch and a base ref in the peer's own clone; a caller
// worktree path is refused before it can cross the host boundary.
fn peer_bound_writer_refuses_a_caller_path_but_carries_branch_intent()
-> Result<(), Box<dyn std::error::Error>> {
    if super::super::peer_tests::host_test(
        "fabric::owner::peer_capture_tests::peer_bound_writer_refuses_a_caller_path_but_carries_branch_intent",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, AttemptOwner};
    struct FreeWorkers;
    impl WorkerObserver for FreeWorkers {
        fn observe_roots(
            &self,
            roots: &[PathBuf],
        ) -> std::collections::BTreeMap<PathBuf, provenant_confinement::worktrees::Occupancy>
        {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::Occupancy::Available))
                .collect()
        }
    }
    let rig = crate::worktree_tests::repository()?;
    let host = provenant_core::HostId::from_bytes([2; 16]);
    std::fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric.peers.peer]\nhost='{host}'\ndestination='fixture'\n"
        ),
    )?;
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
    let cancel = CancelToken::new();
    let mut owner = BoundGraphOwner {
        writer_launch_budget: None,
        admission_timeout: None,
        launch_warnings: Vec::new(),
        repo_facts: Default::default(),
        factory: &factory,
        integration_base: None,
        integration_branch: None,
        network: None,
        capability_overrides: Default::default(),
        exclude: Vec::new(),
        budget: WaitBudget::cli(factory.clock.mono_now(), 30)?,
        cancel: &cancel,
        prices: None,
        tick: false,
    };
    let actor = owner.actor();
    let route: graph::GraphRoute = serde_json::from_value(serde_json::json!({"alias":"junior"}))?;
    let new_worktree: provenant_fabric_runtime::coordination::facade::NewWorktree =
        serde_json::from_value(serde_json::json!({"branch":"peer-writer","base":"HEAD"}))?;
    let spec = graph::AttemptSpec {
        host: Some("peer".into()),
        new_worktree: Some(new_worktree),
        ..Default::default()
    };
    let key = |attempt: u32| graph::AttemptKey {
        extra: Default::default(),
        retry_generation: None,
        previous_attempt_ref: None,
        graph_id: "peer-writer-boundary".into(),
        node_id: "write".into(),
        route_index: 0,
        attempt,
    };
    fn node<'a>(worktree: Option<&'a str>, spec: &'a graph::AttemptSpec) -> graph::NodeInput<'a> {
        graph::NodeInput {
            writer_dirty_preflight: false,
            review_launch: false,
            require_skill_location: false,
            settlement_retries: None,
            prior_attempts: &[],
            family_peers: &[],
            context: provenant_records::run::WorkContext {
                graph_id: Some("peer-writer-boundary".into()),
                node_id: Some("write".into()),
                attempt: Some(1),
                ..Default::default()
            },
            read_roots: &[],
            attachments: &[],
            worktree,
            fixture: None,
            fixture_bytes: None,
            capabilities: None,
            capability_sources: None,
            attempt: Some(spec),
            distinct_from_attempts: &[],
            require_distinct: false,
            source_commit: None,
            review_base: None,
            title: "write",
            prompt: "write",
            required_verdict: None,
            deadline_seconds: None,
            inputs_dir: None,
        }
    }
    fn replay(actor: &str) -> graph::Replay<'_> {
        graph::Replay {
            delivery: None,
            branch: None,
            fast_forward_only: false,
            stored_ref: None,
            launch_actor: actor,
        }
    }
    // A caller path is sender-local and never crosses to the peer.
    let caller = owner
        .observe_or_start(
            &key(1),
            node(Some("/caller/checkout"), &spec),
            &route,
            graph::AccessMode::WorktreeWrite,
            None,
            replay(&actor),
        )
        .unwrap_err();
    assert_eq!(caller.code(), "WORKTREE_NOT_APPLICABLE", "{caller:?}");
    // Branch and base intent carry no path, so the peer provisions the
    // writer in its own clone instead of refusing the boundary. The launch
    // may fail downstream without a live provider, but never at this gate.
    match owner.observe_or_start(
        &key(2),
        node(None, &spec),
        &route,
        graph::AccessMode::WorktreeWrite,
        None,
        replay(&actor),
    ) {
        Ok(_) => {}
        Err(error) => assert_ne!(
            error.code(),
            "WORKTREE_NOT_APPLICABLE",
            "branch intent must reach the peer: {error:?}"
        ),
    }
    Ok(())
}
