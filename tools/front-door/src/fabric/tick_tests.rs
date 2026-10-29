#[allow(unused_imports)]
use super::*;

#[test]
fn tick_advances_expired_asks_and_skips_future_cancelled_and_busy_graphs()
-> Result<(), Box<dyn std::error::Error>> {
    if host_test("fabric::peer_tests::tick_tests::tick_advances_expired_asks_and_skips_future_cancelled_and_busy_graphs")? {
        return Ok(());
    }
    use provenant_fabric_runtime::graph::{self, GraphJournal};
    struct Seed {
        actor: String,
        at: provenant_core::WallTime,
    }
    impl graph::AttemptOwner for Seed {
        type Error = FacadeError;
        fn actor(&self) -> String { self.actor.clone() }
        fn wall_now(&self) -> Option<provenant_core::WallTime> { Some(self.at) }
        fn observe_or_start(
            &mut self, _: &graph::AttemptKey, _: graph::NodeInput<'_>, _: &graph::GraphRoute,
            _: graph::AccessMode, _: Option<&str>, _: graph::Replay<'_>,
        ) -> Result<graph::AttemptObservation, Self::Error> {
            Err(FacadeError::OwnerUnavailable)
        }
    }
    let rig = crate::worktree_tests::support::TestRig::new()?;
    let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
    let context = Context::open(roots.clone(), &rig.home, &rig.project)?;
    let project = context.identity.project;
    let clock = SystemClock;
    let now = clock.wall_now()?;
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 10)?;
    let mut seed = Seed { actor: context.identity.inbox.actor.to_string(), at: provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)? };
    // IDs sort the busy graph before the due graph, exercising failure isolation.
    for id in ["a-busy", "b-due", "c-future", "d-cancelled"] {
        let document: graph::GraphDocument = serde_json::from_value(serde_json::json!({
            "schema_version":1, "graph_id":id, "decision_session":"chair-session",
            "nodes":[{"id":"choose", "kind":"decide", "access":"read_only", "decide":{
                "question":"Ship?", "options":[{"id":"ship","label":"Ship"},{"id":"hold","label":"Hold"}],
                "default":"hold", "timeout_seconds":60
            }}]
        }))?;
        let mut journal = graph::FileJournal::open(&roots.state, project, id, budget, &cancel, &clock)?;
        journal.bind(graph::GraphExecution { extra: Default::default(), integration_base: None, directory: rig.project.clone(), document: document.clone() })?;
        seed.at = if id == "c-future" { now } else { provenant_core::WallTime::from_micros(now.as_micros() - 120_000_000)? };
        let mut run = graph::advance_with_budget(&document, &mut seed, &mut journal, &Default::default(), &Default::default())?;
        assert_eq!(run.checkpoint.nodes[0].state, graph::NodeState::Running);
        if id == "d-cancelled" {
            run.checkpoint.cancelled = true;
            journal.save(&run.checkpoint)?;
        }
    }
    let busy = graph::FileJournal::open(&roots.state, project, "a-busy", budget, &cancel, &clock)?;
    let resolved = crate::roots::Resolved {
        assets: rig.project.clone(), home: rig.home.clone(), project: rig.project.clone(), roots: roots.clone(),
        claude_config: None, codex_home: None,
    };
    let report = tick_graphs(&resolved, now, &FreeWorkers)?;
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.advanced, 1);
    let due = graph::FileJournal::read_checkpoint(&roots.state, project, "b-due")?.ok_or("missing due graph")?;
    assert_eq!(due.nodes[0].state, graph::NodeState::Succeeded);
    assert_eq!(due.asks[0].answer.as_ref().ok_or("missing default")?.source, graph::AnswerSource::Default);
    assert!(due.wake_at.is_none());
    for id in ["a-busy", "c-future", "d-cancelled"] {
        let checkpoint = graph::FileJournal::read_checkpoint(&roots.state, project, id)?.ok_or("missing graph")?;
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
