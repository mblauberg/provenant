#[allow(unused_imports)]
use super::*;

#[test]
fn completion_gc_retries_busy_collection_and_requeued_exhausted_request()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_writer_delivery::completion_gc_retries_busy_collection_and_requeued_exhausted_request")? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let requests = fixture.roots.state.as_path().join("gc/completions");
    let request_path = std::fs::read_dir(&requests)?.next().ok_or("queued request")??.path();
    let request = std::fs::read_to_string(&request_path)?;
    let name = request_path.file_name().ok_or("request name")?;
    let lock = crate::worktrees::collection_lock(&fixture.roots)?;
    let old_wait = gc_cleanup::COLLECTION_WAIT_SECONDS.replace(1);
    let result = gc_cleanup::run(&request, &FreeWorkers);
    gc_cleanup::COLLECTION_WAIT_SECONDS.set(old_wait);
    result?;
    let claimed_path = requests.join(format!("{}.claim", name.to_string_lossy()));
    assert!(claimed_path.exists(), "busy collection must retain its request");
    assert!(fixture.worktree.exists());
    let retry = std::fs::read(&claimed_path)?;
    let record: serde_json::Value = serde_json::from_slice(&retry)?;
    assert_eq!(record["retry"]["attempts"], 1);
    assert_eq!(record["retry"]["last_cause"], crate::worktrees::COLLECTION_BUSY);
    gc_cleanup::retry_pending(fixture.roots.state.as_path(), &fixture.rig.home)?;
    assert_eq!(std::fs::read(&claimed_path)?, retry, "host tick must honour backoff");
    drop(lock);
    gc_cleanup::run(&request, &FreeWorkers)?;
    assert!(!fixture.worktree.exists(), "later pass must clean the lane");
    assert!(!request_path.exists());
    assert!(!claimed_path.exists());

    // The diagnostic record contains both payload and exhausted budget.
    let exhausted = fixture.roots.state.as_path().join("gc/completion-exhausted");
    std::fs::create_dir_all(&exhausted)?;
    let mut record: serde_json::Value = serde_json::from_str(&request)?;
    record["retry"] = serde_json::json!({"attempts":5,"next_at":0,"last_cause":"busy"});
    std::fs::write(exhausted.join(name), serde_json::to_vec(&record)?)?;
    gc_cleanup::queue(&fixture.settled, &fixture.roots, None)?;
    let fresh: serde_json::Value = serde_json::from_slice(&std::fs::read(&request_path)?)?;
    assert_eq!(fresh["request"], record["request"]);
    assert_eq!(fresh["retry"]["attempts"], 0, "identical requeue must get a fresh budget");
    assert_eq!(fresh["retry"]["next_at"], 0);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(exhausted.join(name))?)?, record);
    Ok(())
}

#[test]
fn completion_queue_publishes_promptly_while_dispatch_is_contended()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_writer_delivery::completion_queue_publishes_promptly_while_dispatch_is_contended")? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    // Hold the old dispatch lock too: queue must never wait for either lock.
    let locks = provenant_config::private_directory(
        &fixture.roots.state.as_path().join("gc/completion-retries"), true)?;
    let _dispatch = locks.try_lock("dispatch.lock".as_ref(), provenant_os::fs::LockMode::Exclusive)?;
    let started = SystemClock.mono_now();
    gc_cleanup::queue(&fixture.settled, &fixture.roots, None)?;
    assert!(SystemClock.mono_now().as_micros().saturating_sub(started.as_micros()) < 5_000_000);
    let requests = fixture.roots.state.as_path().join("gc/completions");
    let paths = std::fs::read_dir(&requests)?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(paths.len(), 1);
    let bytes = std::fs::read(paths[0].path())?;
    let record: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(record["retry"]["attempts"], 0);
    assert!(record["request"].as_str().is_some());
    gc_cleanup::queue(&fixture.settled, &fixture.roots, None)?;
    assert_eq!(std::fs::read(paths[0].path())?, bytes);
    Ok(())
}

#[test]
fn settlement_defers_adopted_writer_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_writer_delivery::settlement_defers_adopted_writer_cleanup")?
    {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    worktrees::with_context(
        &fixture.roots,
        &fixture.rig.home,
        &fixture.rig.project,
        &clock,
        &cancel,
        |context| {
            let tree = provenant_confinement::verify_worktree(context.repo, &fixture.worktree)?;
            let mut owner = provenant_confinement::worktrees::load_owner(&tree)?
                .ok_or(provenant_confinement::worktrees::WorktreeError::InvalidOutput)?;
            owner.origin = provenant_confinement::worktrees::WorktreeOrigin::Adopted;
            provenant_confinement::worktrees::save_owner(&tree, &owner)?;
            Ok(())
        },
    )?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let output = fixture.rig.execute(
        &git,
        &["remote", "add", "origin", "/nonexistent/provenant-remote"],
        None,
    )?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let mut receipt = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("deferred receipt")?;
    assert_eq!(receipt["state"], "retained");
    assert_eq!(receipt["cleanup_deferred"], true);
    assert!(fixture.worktree.exists());
    let pending = provenant_config::private_directory(
        &fixture.roots.state.as_path().join("gc/completions"), false,
    )?;
    let queued = pending.entries()?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(queued.len(), 1);
    let first_request = pending.read_bounded(&queued[0], 128 * 1024)?;
    let dispatch: serde_json::Value = serde_json::from_slice(&first_request)?;
    assert_eq!(dispatch["retry"]["attempts"], 0);
    // Repeating settlement preserves the pending request and its budget.
    receipt.as_object_mut().ok_or("receipt object")?.remove("cleanup_warning");
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    assert_eq!(fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?, Some(receipt));
    assert_eq!(pending.read_bounded(&queued[0], 128 * 1024)?, first_request);
    assert_eq!(pending.entries()?.collect::<Result<Vec<_>, _>>()?, queued);
    gc_cleanup::collect_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    assert!(fixture.worktree.exists());
    let retained = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?.ok_or("retained receipt")?;
    assert_eq!(retained["state"], "retained");
    assert_eq!(retained["cleanup_complete"], true);
    Ok(())
}

#[test]
fn settlement_defers_tracking_ref_refresh()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::peer_writer_delivery::settlement_defers_tracking_ref_refresh",
    )? {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let output = fixture.rig.execute(
        &git,
        &["remote", "add", "origin", "/nonexistent/provenant-remote"],
        None,
    )?;
    assert_eq!(crate::worktree_tests::support::code(&output), 0);
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let receipt = fixture
        .store
        .worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("settlement")?;
    assert!(receipt["tracking_ref_warning"].is_null(), "{receipt}");
    assert_eq!(receipt["state"], "retained", "{receipt}");
    assert_eq!(receipt["cleanup_deferred"], true);
    gc_cleanup::collect_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let completed = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?.ok_or("completed receipt")?;
    assert_eq!(completed["state"], "pruned", "{completed}");
    assert!(completed["tracking_ref_warning"].is_string(), "{completed}");
    Ok(())
}

#[test]
fn unmatched_graph_writer_keeps_its_worktree() -> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_writer_delivery::unmatched_graph_writer_keeps_its_worktree")? {
        return Ok(());
    }
    for corrupt in [false, true] {
        let fixture = if corrupt {
            SettlementFixture::settled_fixture("graph-unmatched", Some("broken"))?
        } else {
            SettlementFixture::resumed_as("graph-unmatched")?
        };
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let call = provenant_run::Request { clock: &clock, cancel: &cancel };
        if corrupt {
            drop(provenant_fabric_runtime::graph::FileJournal::open(
                &fixture.roots.state, fixture.settled.project_id, "broken",
                call.lock_budget(), &cancel, &clock,
            )?);
            let path = fixture.roots.state.as_path().join("fabric/graphs")
                .join(fixture.settled.project_id.to_string()).join("graph-broken.json");
            std::fs::write(path, b"invalid checkpoint")?;
        }
        settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let receipt = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?
            .ok_or("missing settlement")?;
        assert_eq!(receipt["state"], "retained");
        assert!(receipt["reason"].as_str().is_some_and(|reason| !reason.is_empty()));
        assert!(fixture.worktree.exists());
    }
    Ok(())
}

#[test]
fn resumed_lane_settlement_ignores_terminal_graph()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_writer_delivery::resumed_lane_settlement_ignores_terminal_graph")?
    {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, GraphJournal};
    let fixture = SettlementFixture::resumed()?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"terminal-unrelated","nodes":[{"id":"one","prompt":"work",
            "access":"read_only","route":{"adapter":"fixture","model":"fixture/model"}}]
    }))?;
    let checkpoint: graph::GraphCheckpoint = serde_json::from_value(serde_json::json!({
        "schema_version":1,"graph_id":"terminal-unrelated","plan_digest":"digest",
        "nodes":[{"id":"one","state":"succeeded","attempts":[]}]
    }))?;
    let mut journal = graph::FileJournal::open(&fixture.roots.state, fixture.settled.project_id,
        "terminal-unrelated", WaitBudget::cli(clock.mono_now(), 10)?, &cancel, &clock)?;
    journal.bind(graph::GraphExecution { review_base: None, extra: Default::default(), integration_base: None,
        directory: fixture.rig.project.clone(), document })?;
    journal.save(&checkpoint)?;
    drop(journal);
    graph::take_checkpoint_read_ids();
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    assert!(graph::take_checkpoint_read_ids().is_empty());
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let receipt = fixture
        .store
        .worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("missing resumed settlement receipt")?;
    assert_eq!(receipt["state"], "retained", "{receipt}");
    assert_eq!(receipt["cleanup_deferred"], true);
    assert!(fixture.worktree.exists());
    assert!(
        fixture
            .rig
            .state
            .join("runs")
            .join(fixture.settled.run_id.to_string())
            .join("attempts")
            .join(fixture.settled.attempt_id.to_string())
            .join("worktree-settlement.json")
            .is_file()
    );
    Ok(())
}

#[test]
fn resumed_lane_settlement_defers_cleanup_and_records_its_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::peer_writer_delivery::resumed_lane_settlement_defers_cleanup_and_records_its_receipt")?
    {
        return Ok(());
    }
    let fixture = SettlementFixture::resumed()?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 10)?;
    drop(provenant_fabric_runtime::graph::FileJournal::open(
        &fixture.roots.state, fixture.settled.project_id, "unrelated", budget, &cancel, &clock,
    )?);
    std::fs::write(fixture.roots.state.as_path().join("fabric/graphs")
        .join(fixture.settled.project_id.to_string()).join("graph-unrelated.json"), b"invalid checkpoint")?;
    settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let receipt = fixture
        .store
        .worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("missing resumed settlement receipt")?;
    assert_eq!(receipt["state"], "retained", "{receipt}");
    assert_eq!(receipt["cleanup_deferred"], true);
    assert!(fixture.worktree.exists());
    // Settlement only queues; the detached GC entry point owns actual removal.
    let queue = fixture.roots.state.as_path().join("gc/completions");
    let request = std::fs::read_dir(&queue)?.next().ok_or("missing completion request")??.path();
    let request_json = std::fs::read_to_string(&request)?;
    gc_cleanup::run(&request_json, &FreeWorkers)?;
    assert!(!fixture.worktree.exists());
    assert!(!request.exists());
    let completed = fixture.store.worktree_settlement(fixture.settled.attempt_id, &call)?.ok_or("completed receipt")?;
    assert_eq!(completed["state"], "pruned", "{completed}");
    // A replay still completes after the worktree has gone.
    gc_cleanup::run(&request_json, &FreeWorkers)?;

    assert!(
        fixture
            .rig
            .state
            .join("runs")
            .join(fixture.settled.run_id.to_string())
            .join("attempts")
            .join(fixture.settled.attempt_id.to_string())
            .join("worktree-settlement.json")
            .is_file()
    );
    Ok(())
}

#[test]
fn deliver_none_does_not_try_to_settle_a_missing_delivery()
-> Result<(), Box<dyn std::error::Error>> {
    struct NoWorkers;
    impl provenant_confinement::worktrees::WorkerObserver for NoWorkers {
        fn observe_roots(
            &self,
            _: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::Occupancy,
        > {
            std::collections::BTreeMap::new()
        }
    }
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let assets = root.join("assets");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&assets)?;
    let roots = crate::roots::child(&home, &assets, root.join("state"))?;
    let edge = provenant_cli::run::Edge {
        roots: &roots,
        home: &home,
        assets: &assets,
        cwd: &assets,
    };
    let mut reply = provenant_cli::run::Reply {
        result: provenant_records::CommandResult::new(
            "lane deliver",
            provenant_records::Outcome::Ok,
            serde_json::json!({"attempt_id":"unused", "deliver":"none"}),
        ),
        human: "delivery disabled\n".into(),
        summary: None,
        read_only: false,
    };
    super::settle_delivered_attempt(&edge, &mut reply, &NoWorkers)?;
    assert_eq!(reply.human, "delivery disabled\n");
    assert!(reply.result.data["worktree_settlement"].is_null());
    Ok(())
}

#[test]
fn unsuccessful_lanes_retain_the_worktree_even_after_delivery()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_run::{AttemptState, Outcome, WriterDeliveryReceipt, WriterDeliveryState};
    let delivered = WriterDeliveryReceipt {
        extra: Default::default(),
        schema_version: 1,
        remote: "origin".into(),
        branch: "lane".into(),
        head: None,
        review_acceptance: None,
        state: WriterDeliveryState::Delivered {
            extra: Default::default(),
            pull_request: None,
        },
        integration: None,
    };
    for outcome in [
        Outcome::Failed,
        Outcome::RateLimited,
        Outcome::Cancelled,
        Outcome::TimedOut,
        Outcome::Rejected,
    ] {
        let state = AttemptState::Terminal {
            extra: Default::default(),
            outcome,
            exit_code: Some(1),
            finished_at: provenant_core::WallTime::from_micros(0)?,
            reason: None,
        };
        assert!(super::settlement_retention_reason(&state, false, None).is_some());
        assert!(super::settlement_retention_reason(&state, true, Some(&delivered)).is_some());
    }
    let succeeded = AttemptState::Terminal {
        extra: Default::default(),
        outcome: Outcome::Succeeded,
        exit_code: Some(0),
        finished_at: provenant_core::WallTime::from_micros(0)?,
        reason: None,
    };
    assert!(super::settlement_retention_reason(&succeeded, false, None).is_none());
    assert!(super::settlement_retention_reason(&succeeded, true, Some(&delivered)).is_none());
    assert!(super::settlement_retention_reason(&succeeded, true, None).is_some());
    let failed_delivery = WriterDeliveryReceipt {
        state: WriterDeliveryState::NotAttempted {
            extra: Default::default(),
        },
        ..delivered
    };
    assert!(
        super::settlement_retention_reason(&succeeded, true, Some(&failed_delivery)).is_some()
    );
    Ok(())
}

#[test]
fn settlement_intent_replays_a_prune_after_a_crash() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let worktree = temp.path().join("lane");
    std::fs::create_dir(&worktree)?;
    let intent = serde_json::json!({
        "schema_version":1, "state":"pruning", "worktree":worktree
    });
    assert!(super::replay_worktree_settlement(Some(&intent), &worktree)?.is_none());
    std::fs::remove_dir(&worktree)?;
    let receipt = super::replay_worktree_settlement(Some(&intent), &worktree)?
        .ok_or("missing recovered prune receipt")?;
    assert_eq!(receipt["state"], "pruned");
    assert_eq!(receipt["recovered_from_intent"], true);
    assert_eq!(
        super::replay_worktree_settlement(Some(&receipt), &worktree)?,
        Some(receipt)
    );
    assert!(
        super::replay_worktree_settlement(Some(&intent), &temp.path().join("other"))?.is_none()
    );
    let retained = serde_json::json!({"state":"retained", "worktree":worktree});
    assert!(super::replay_worktree_settlement(Some(&retained), &worktree)?.is_none());
    Ok(())
}

#[test]
fn submit_io_keeps_the_inner_operation_without_inventing_a_path() {
    let error = super::scope_io(super::FacadeError::io("access Fabric database"), "submit");
    assert_eq!(
        error,
        super::FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
            operation: "submit: access Fabric database".into(),
            path: None,
        }
    );
}

#[test]
fn submit_io_retains_single_prefix_for_peer_error_and_scopes_local_error() {
    let peer_error = super::FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
        operation: "submit: open writer repository".into(),
        path: Some("/repo".into()),
    };
    let rescoped = super::scope_io(peer_error.clone(), "submit");
    assert_eq!(
        rescoped,
        super::FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
            operation: "submit: open writer repository".into(),
            path: Some("/repo".into()),
        }
    );

    let local_error = super::FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
        operation: "open writer repository".into(),
        path: Some("/repo".into()),
    };
    let scoped = super::scope_io(local_error, "submit");
    assert_eq!(
        scoped,
        super::FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
            operation: "submit: open writer repository".into(),
            path: Some("/repo".into()),
        }
    );
}

#[test]
fn writer_repository_open_failure_hides_unanchored_project_path()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let request = provenant_run::WriterRequest {
        extra: Default::default(),
        project: root.clone(),
        worktree: root.join("writer"),
    };
    let project = provenant_core::ProjectId::from_bytes([1; 16]);
    let error = match provenant_run::prepare_writer(&request, &root, project) {
        Ok(_) => return Err("non-repository unexpectedly opened".into()),
        Err(error) => super::run_error(error),
    };
    assert_eq!(
        serde_json::to_value(error)?,
        serde_json::json!({"code": "confinement_refused"})
    );
    Ok(())
}

#[test]
fn typed_run_errors_keep_their_codes() {
    for (error, expected) in [
        (provenant_run::RunError::Nonterminal, "not_terminal"),
        (
            provenant_run::RunError::OperationConflict,
            "operation_conflict",
        ),
        (provenant_run::RunError::Conflict, "conflict"),
        (
            provenant_run::RunError::ProviderStateExpired,
            "provider_state_expired",
        ),
        // The CLI and MCP share one categorisation.
        (
            provenant_run::RunError::Validation("input"),
            "invalid_input",
        ),
        (
            provenant_run::RunError::ArtifactMismatch(provenant_run::ArtifactProvenance {
                check: provenant_run::ArtifactCheck::RecordedDigest,
                artifact: "result.md".into(),
                expected: None,
                actual: None,
            }),
            "state_corrupt",
        ),
        (provenant_run::RunError::Refused, "confinement_refused"),
        (provenant_run::RunError::Capacity, "busy"),
    ] {
        assert_eq!(super::run_error(error).code(), expected);
    }
}

#[test]
fn peer_run_errors_stay_typed_through_cli_rendering() -> Result<(), Box<dyn std::error::Error>>
{
    for error in [
        super::FacadeError::NotTerminal,
        super::FacadeError::OperationConflict,
        super::FacadeError::OwnerUnavailable,
        super::FacadeError::Io {
        delivery_limit: None,
        delivery_stage: None,
        resource: None,
        input_failure: None,
            operation: "openat directory".into(),
            path: Some("/state/worktrees".into()),
        },
        super::FacadeError::WorktreeUnregistered {
            path: "/repo/.worktrees/writer".into(),
        },
    ] {
        let expected_code = error.code().to_owned();
        let response = super::peer_result::<()>(Err(error), |()| super::PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError {
                extra: Default::default(),
                error: super::FacadeError::io("test"),
            },
            extra: Default::default(),
        });
        let response: super::PeerResult =
            serde_json::from_slice(&serde_json::to_vec(&response)?)?;
        let super::PeerResult::Error {
            data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
            ..
        } = response
        else {
            return Err("expected error".into());
        };
        let (status, kind) = provenant_cli::fabric::facade_outcome(&error);
        let fail = provenant_cli::run::Fail {
            status,
            kind,
            message: error.to_string(),
            detail: Some(Box::new(error)),
            projection: None,
        };
        let result = super::fail_error(fail);
        assert_eq!(result.code(), expected_code);
        let encoded = serde_json::to_string(&result)?;
        assert!(!encoded.contains("/state/worktrees"));
        assert!(!encoded.contains("/repo/.worktrees/writer"));
    }
    Ok(())
}

#[test]
fn submit_lane_refusal_maps_to_dispatch_refused() -> Result<(), Box<dyn std::error::Error>> {
    // A nonexistent read root is refused before any state tree is touched.
    let fail = provenant_cli::run::lane_access(
        &provenant_cli::Access {
    attach: Vec::new(),
            mode: None,
            worktree: None,
            read_root: vec!["/fixture/missing-root".into()],
            write_root: Vec::new(),
            network: None,
            capability: Vec::new(),
            no_capability: Vec::new(),
        },
        Path::new("never-opened-state"),
        Path::new("/fixture/host-home"),
    )
    .err()
    .ok_or("missing root refuses")?;
    let kind = fail.kind;
    match super::dispatch_refused(fail) {
        FacadeError::DispatchRefused { reason, .. } => {
            assert!(reason.contains("missing-root"));
        }
        error => panic!("a refused submit kept no reason: {error:?}"),
    }
    // The kind alone renders as invalid input; the dispatch edge must not
    // lose the refusal by rerendering it.
    assert_eq!(super::facade_error_for_kind(kind), FacadeError::InvalidInput { detail: None });
    Ok(())
}

#[test]
fn events_within_trims_whole_events_and_counts_separators() {
    // Serialised sizes are 4, 12 and 3 bytes; the array adds brackets and commas.
    let events = ["aa", "bbbbbbbbbb", "c"];
    for (max_bytes, kept) in [
        (65536, 3),
        // Two events are 2 + 4 + 1 + 12 = 19 bytes; the comma counts.
        (19, 2),
        (18, 1),
        // One event is 2 + 4 bytes.
        (6, 1),
        (5, 0),
        (0, 0),
    ] {
        assert_eq!(
            super::events_within(&events, max_bytes).unwrap(),
            kept,
            "{max_bytes}"
        );
    }
    // A large event stops the page even when a later small one would fit.
    assert_eq!(super::events_within(&events, 10).unwrap(), 1);
    assert_eq!(super::events_within::<&str>(&[], 2).unwrap(), 0);
}

#[test]
fn host_probe_budget_is_shared_fairly() {
    assert_eq!(super::fair_probe_micros(10_000_000, 4), Some(2_500_000));
    assert_eq!(super::fair_probe_micros(55_000_000, 100), Some(550_000));
}

#[test]
fn forwarded_unobservable_cancel_keeps_its_operation() -> Result<(), Box<dyn std::error::Error>>
{
    let error = super::FacadeError::LaunchUnknown {
        operation_id: provenant_core::OperationId::parse("stop")?,
    };
    let reply = super::peer_result::<()>(Err(error), |()| super::PeerResult::Error {
        data: provenant_fabric_runtime::hosts::PeerResultError {
            extra: Default::default(),
            error: super::FacadeError::io("test"),
        },
        extra: Default::default(),
    });
    let super::PeerResult::Error {
        data: provenant_fabric_runtime::hosts::PeerResultError { error, .. },
        ..
    } = reply
    else {
        return Err("expected error".into());
    };
    assert!(
        matches!(error, super::FacadeError::LaunchUnknown { operation_id } if operation_id.as_str() == "stop")
    );
    Ok(())
}
