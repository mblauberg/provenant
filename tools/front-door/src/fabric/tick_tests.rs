#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)] // The custom worktree driver includes this module without libtest.
use provenant_fabric_runtime::graph;

#[test]
fn tick_advances_expired_asks_and_cancelled_graphs_and_skips_future_and_busy_graphs()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::tick_tests::tick_advances_expired_asks_and_cancelled_graphs_and_skips_future_and_busy_graphs",
    )? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, GraphJournal};
    let rig = crate::worktree_tests::support::TestRig::new()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
    let project = context.identity.project;
    let clock = SystemClock;
    let now = clock.wall_now()?;
    let actor = context.identity.inbox.actor.to_string();
    let seed_at = provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)?;
    let seed_clock = provenant_core::TestClock::new(
        seed_at,
        provenant_core::MonoTime::from_micros(0),
    );
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 10)?;
    let inbox = provenant_fabric_runtime::inbox::InboxService::open(
        &roots.state,
        &context.identity,
        &seed_clock,
        &cancel,
    )?;
    let mut seed = super::InboxSeedOwner::new(actor, seed_at, &inbox, &cancel);
    // IDs sort the busy graph before the due graph, exercising failure isolation.
    for id in ["a-busy", "b-due", "c-future", "d-cancelled"] {
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":id, "decision_session":"chair-session",
            "nodes":[{"id":"choose", "kind":"decide", "access":"read_only", "decide":{"on_unanswered":"block_dependants",
                "question":"Ship?", "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
                "default":"hold", "timeout_seconds":60
            }}]
        }))?;
        let mut journal =
            graph::FileJournal::open(&roots.state, project, id, budget, &cancel, &clock)?;
        journal.bind(graph::GraphExecution {
            peer_origin_context: None,
            peer_submission_digest: None,
            requester_path: None,
            extra: Default::default(),
            bases: None,
            directory: rig.project.clone(),
            document: document.clone(),
        })?;
        seed.at = if id == "c-future" { now } else { seed_at };
        let mut run = graph::advance_with_budget(
            &document,
            &mut seed,
            &mut journal,
            &Default::default(),
            &Default::default(),
        )?;
        if id == "b-due" {
            assert!(run.checkpoint.wake_at.is_some_and(|wake| wake <= now));
        }
        if id == "c-future" {
            assert!(run.checkpoint.wake_at.is_some_and(|wake| wake > now));
        }
        assert_eq!(run.checkpoint.nodes[0].state, graph::NodeState::Running);
        if id == "d-cancelled" {
            run.checkpoint.cancelled = true;
            journal.save(&run.checkpoint)?;
        }
    }
    let busy = graph::FileJournal::open(&roots.state, project, "a-busy", budget, &cancel, &clock)?;
    let resolved = crate::roots::Resolved {
        assets: rig.project.clone(),
        home: rig.home.clone(),
        project: rig.project.clone(),
        roots: roots.clone(),
        claude_config: None,
        codex_home: None,
    };
    let report = tick_graphs(&resolved, now, &FreeWorkers)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.advanced, 2);
    let due = graph::FileJournal::read_checkpoint(&roots.state, project, "b-due")?
        .ok_or("missing due graph")?;
    assert_eq!(due.nodes[0].state, graph::NodeState::Succeeded);
    // The deadline took the default; the checkpoint keeps the verdict, not the link.
    assert!(due.asks.is_empty());
    assert_eq!(
        due.nodes[0]
            .outputs
            .as_ref()
            .and_then(|outputs| outputs.verdict.as_deref()),
        Some("hold")
    );
    assert!(due.wake_at.is_none());
    let cancelled = graph::FileJournal::read_checkpoint(&roots.state, project, "d-cancelled")?
        .ok_or("missing cancelled graph")?;
    assert_eq!(cancelled.nodes[0].state, graph::NodeState::Skipped);
    assert_eq!(
        cancelled.nodes[0].skip_reason.as_deref(),
        Some("graph cancelled")
    );
    for id in ["a-busy", "c-future"] {
        let checkpoint = graph::FileJournal::read_checkpoint(&roots.state, project, id)?
            .ok_or("missing graph")?;
        assert_eq!(checkpoint.nodes[0].state, graph::NodeState::Running);
    }
    // Restart picks up a previously busy journal once its existing owner exits.
    drop(busy);
    let next = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert!(next.errors.is_empty(), "{:?}", next.errors);
    assert_eq!(next.advanced, 1);
    let idle = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert_eq!(idle.advanced, 0);
    assert!(idle.errors.is_empty());
    Ok(())
}

/// Seed a graph whose nodes have settled but which still carries an open ask,
/// so tick discovers it with a missing worktree.
fn seed_settled_missing_graph(
    roots: &provenant_core::Roots,
    project: provenant_core::ProjectId,
    id: &str,
    node_state: &str,
    worktree: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use provenant_fabric_runtime::graph::{self, GraphJournal, NodeState};

    let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1, "graph_id":id, "decision_session":"chair-session",
        "nodes":[{"id":"choose","kind":"decide","access":"read_only","decide":{"on_unanswered":"block_dependants",
            "question":"Ship?", "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
            "default":"hold", "timeout_seconds":60}}]
    }))?;
    let plan_digest = graph::graph_plan_digest(&document)?.to_string();
    let checkpoint: graph::GraphCheckpoint = serde_json::from_value(serde_json::json!({
        "schema_version":2, "graph_id":id, "plan_digest":plan_digest,
        "asks":[{"ask_id":"ask","kind":"decide","node":"choose","session":"chair","request":"hrq-ask"}],
        "nodes":[{"id":"choose","state":node_state,"worktree":null,"attempts":[]}]
    }))?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let mut journal = graph::FileJournal::open(
        &roots.state,
        project,
        id,
        WaitBudget::cli(clock.mono_now(), 10)?,
        &cancel,
        &clock,
    )?;
    journal.bind(graph::GraphExecution {
        peer_origin_context: None,
        peer_submission_digest: None,
        requester_path: None,
        extra: Default::default(),
        bases: None,
        directory: worktree.to_owned(),
        document,
    })?;
    journal.save(&checkpoint)?;
    let expected: NodeState = serde_json::from_value(serde_json::json!(node_state))?;
    assert_eq!(checkpoint.nodes[0].state, expected);
    Ok(())
}

#[test]
fn tick_auto_settles_finished_graphs_with_removed_worktrees()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::tick_tests::tick_auto_settles_finished_graphs_with_removed_worktrees",
    )? {
        return Ok(());
    }
    let rig = crate::worktree_tests::support::TestRig::new()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
    let project = context.identity.project;
    let gone = rig.root.join("removed-worktree");
    seed_settled_missing_graph(&roots, project, "settled-ok", "succeeded", &gone)?;
    seed_settled_missing_graph(&roots, project, "settled-bad", "failed", &gone)?;
    let resolved = crate::roots::Resolved {
        assets: rig.project.clone(),
        home: rig.home.clone(),
        project: rig.project.clone(),
        roots: roots.clone(),
        claude_config: None,
        codex_home: None,
    };
    let clock = SystemClock;
    let report = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    assert!(
        report
            .warnings
            .iter()
            .all(|warning| warning.contains("worktree removed after settlement"))
    );

    let ok = graph::FileJournal::read_checkpoint(&roots.state, project, "settled-ok")?
        .ok_or("settled graph")?;
    assert!(!ok.cancelled);
    assert!(ok.asks.is_empty());
    let ok_run = graph_snapshot(ok);
    assert_eq!(ok_run.status, graph::GraphStatus::Complete);
    assert_eq!(ok_run.outcome, Some(graph::GraphOutcome::Succeeded));

    let bad = graph::FileJournal::read_checkpoint(&roots.state, project, "settled-bad")?
        .ok_or("stopped graph")?;
    assert!(bad.cancelled);
    assert_eq!(
        bad.cancel_reason.as_deref(),
        Some("worktree removed after settlement")
    );
    assert!(bad.asks.is_empty());
    let bad_run = graph_snapshot(bad);
    assert_eq!(bad_run.status, graph::GraphStatus::Cancelled);
    assert_eq!(bad_run.outcome, None);

    // Auto-settled graphs leave the pending set: no repeated warnings.
    let again = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert!(again.errors.is_empty(), "{:?}", again.errors);
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    Ok(())
}

#[test]
fn live_custody_lease_blocks_auto_settlement()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test(
        "fabric::peer_tests::tick_tests::live_custody_lease_blocks_auto_settlement",
    )? {
        return Ok(());
    }
    let rig = crate::worktree_tests::support::TestRig::new()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
    let project = context.identity.project;
    let gone = rig.root.join("removed-worktree-held");
    let id = "held-lease";
    seed_settled_missing_graph(&roots, project, id, "succeeded", &gone)?;

    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = provenant_fabric_store::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let ledger = provenant_fabric_store::HostStore::open(&roots.state, &call)?;
    ledger.acquire_custody(
        project,
        &provenant_records::custody::Subject::new(
            provenant_records::custody::SubjectKind::Graph,
            id,
        ),
        &provenant_records::custody::Holder::Chair {
            chair: provenant_cli::fabric::current_chair(&context)?,
        },
        provenant_records::custody::Origin::Provenant,
        &call,
    )?;
    drop(ledger);

    let resolved = crate::roots::Resolved {
        assets: rig.project.clone(),
        home: rig.home.clone(),
        project: rig.project.clone(),
        roots: roots.clone(),
        claude_config: None,
        codex_home: None,
    };
    let report = tick_graphs(&resolved, clock.wall_now()?, &FreeWorkers)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("worktree is missing")),
        "{:?}",
        report.warnings
    );
    let checkpoint = graph::FileJournal::read_checkpoint(&roots.state, project, id)?
        .ok_or("held graph")?;
    assert!(!checkpoint.asks.is_empty());
    assert!(!checkpoint.cancelled);
    Ok(())
}
