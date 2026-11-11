//! Tests for choosing the next step after a stop.

use super::*;
use provenant_records::{NextStep, StopCause};
use serde_json::json;
use std::collections::VecDeque;

/// An owner that settles each launch with the next queued observation.
#[derive(Default)]
struct Owner {
    observations: VecDeque<AttemptObservation>,
    routes: Vec<GraphRoute>,
    prompts: Vec<String>,
    worktrees: Vec<Option<String>>,
    facts: Option<DepletionFacts>,
}

impl AttemptOwner for Owner {
    type Error = std::convert::Infallible;

    fn actor(&self) -> String {
        "chair".into()
    }

    fn depletion_facts(
        &mut self,
        _request: DepletionRequest<'_>,
    ) -> Result<Option<DepletionFacts>, Self::Error> {
        Ok(self.facts.clone())
    }

    fn observe_or_start(
        &mut self,
        _key: &AttemptKey,
        node: NodeInput<'_>,
        route: &GraphRoute,
        _access: AccessMode,
        worktree: Option<&str>,
        _replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.routes.push(route.clone());
        self.prompts.push(node.prompt.to_owned());
        self.worktrees.push(worktree.map(str::to_owned));
        Ok(self
            .observations
            .pop_front()
            .unwrap_or(AttemptObservation::Pending { attempt_ref: None }))
    }
}

/// A journal that refuses any checkpoint still owing what follows a stop:
/// every reader sees a stop only together with its restart or its outcome.
struct Settled {
    document: GraphDocument,
    limits: crate::config::GraphLimits,
    saved: Option<GraphCheckpoint>,
    saves: usize,
    facts: Option<DepletionFacts>,
}

impl GraphJournal for Settled {
    type Error = std::convert::Infallible;

    fn load(&mut self, _graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.saved.clone())
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        let limits = &self.limits;
        let expanded = plans::expand(
            &self.document,
            &checkpoint.extensions,
            &checkpoint.expansions,
            limits,
        )
        .unwrap();
        let (plan, _) = validate_flat_plan(&expanded, limits).unwrap();
        let facts = self.facts.clone();
        assert!(
            next_step::settle_stop(&self.document, &plan, checkpoint, limits, &mut |_| Ok(
                facts.clone()
            ))
            .is_none(),
            "a saved stop still owes its next step"
        );
        self.saves += 1;
        self.saved = Some(checkpoint.clone());
        Ok(())
    }
}

fn journal(document: &GraphDocument) -> Settled {
    Settled {
        document: document.clone(),
        limits: Default::default(),
        saved: None,
        saves: 0,
        facts: None,
    }
}

fn route(adapter: &str) -> serde_json::Value {
    json!({"adapter": adapter, "model": format!("{adapter}/model")})
}

fn document(access: &str, on_stop: serde_json::Value) -> GraphDocument {
    let mut node = json!({
        "id": "work", "access": access, "prompt": "Finish the brief",
        "routes": [route("first"), route("second")],
    });
    if access == "worktree_write" {
        node["worktree"] = ".worktrees/work".into();
    }
    if !on_stop.is_null() {
        node["on_stop"] = on_stop;
    }
    serde_json::from_value(json!({"schema_version": 1, "graph_id": "g-next", "nodes": [node]}))
        .unwrap()
}

fn terminal(
    reference: &str,
    outcome: AttemptOutcome,
    stop: Option<StopCause>,
    result: Option<&str>,
) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: Some(reference.into()),
        outcome,
        launched: true,
        result: result.map(|text| {
            let mut capture = crate::result_capture::ResultCapture::new(None);
            capture.push(text.as_bytes());
            let mut result = capture.finish();
            result.handoff = provenant_records::handoff::Handoff::from_turns(text, &[]);
            Box::new(result)
        }),
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop,
    }
}

fn done(reference: &str) -> AttemptObservation {
    terminal(reference, AttemptOutcome::Succeeded, None, None)
}

fn adapters(owner: &Owner) -> Vec<&str> {
    owner
        .routes
        .iter()
        .map(|route| route.adapter.as_deref().unwrap())
        .collect()
}

fn restarts(checkpoint: &GraphCheckpoint) -> Vec<(StopCause, NextStep)> {
    checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref()?.restart.as_ref())
        .map(|restart| (restart.cause, restart.step))
        .collect()
}

#[test]
fn a_transient_stop_retries_once_on_its_route_then_takes_a_new_route() {
    let document = document("read_only", serde_json::Value::Null);
    let crash = |reference| {
        terminal(
            reference,
            AttemptOutcome::OtherFailure,
            Some(StopCause::ProcessCrash),
            None,
        )
    };
    let mut owner = Owner {
        observations: [crash("a/1"), crash("a/2"), done("b/1")].into(),
        ..Default::default()
    };
    let mut journal = journal(&document);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(adapters(&owner), ["first", "first", "second"]);
    assert_eq!(
        restarts(&run.checkpoint),
        [
            (
                StopCause::ProcessCrash,
                NextStep::SameRoute {
                    continuation: provenant_records::Continuation::Fresh
                }
            ),
            (StopCause::ProcessCrash, NextStep::NewRoute),
        ]
    );
    assert!(journal.saves > 0);
}

#[test]
fn a_depleted_writer_moves_to_a_new_route_at_once_in_its_worktree() {
    for cause in [
        StopCause::UsageLimited,
        StopCause::ModelUnavailable,
        StopCause::AuthRequired,
    ] {
        let document = document("worktree_write", serde_json::Value::Null);
        let mut owner = Owner {
            observations: [
                terminal("a/1", AttemptOutcome::OtherFailure, Some(cause), None),
                done("b/1"),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = journal(&document);
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert!(run.checkpoint.successful(), "{cause}");
        assert_eq!(adapters(&owner), ["first", "second"], "{cause}");
        assert_eq!(owner.worktrees[0], owner.worktrees[1], "{cause}");
        assert_eq!(restarts(&run.checkpoint), [(cause, NextStep::NewRoute)]);
    }
}

#[test]
fn a_writer_that_lost_its_lane_token_continues_from_its_handoff_on_a_new_route() {
    let document = document("worktree_write", serde_json::Value::Null);
    let handoff = "```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"Renderer half done\",\"remaining\":[\"Finish renderer\"],\"decisions\":[],\"verify\":[]}\n```";
    let mut owner = Owner {
        observations: [
            terminal(
                "a/1",
                AttemptOutcome::TaskFailed,
                Some(StopCause::LaneTokenLost),
                Some(handoff),
            ),
            done("b/1"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = journal(&document);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(adapters(&owner), ["first", "second"]);
    assert_eq!(owner.worktrees[0], owner.worktrees[1]);
    assert!(owner.prompts[1].contains("Finish renderer"));
    assert_eq!(
        restarts(&run.checkpoint),
        [(StopCause::LaneTokenLost, NextStep::NewRoute)]
    );
}

#[test]
fn on_stop_sends_a_stop_to_the_chair_and_unproved_custody_always_goes_there() {
    for (on_stop, cause) in [
        (json!({"usage_limited": "chair"}), StopCause::UsageLimited),
        (serde_json::Value::Null, StopCause::Lost),
        // An override cannot move a stop whose custody is not released.
        (json!({"lost": "new_route"}), StopCause::Lost),
    ] {
        let document = document("worktree_write", on_stop);
        let mut owner = Owner {
            observations: [
                terminal("a/1", AttemptOutcome::OtherFailure, Some(cause), None),
                done("b/1"),
            ]
            .into(),
            ..Default::default()
        };
        let mut journal = journal(&document);
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed, "{cause}");
        assert_eq!(adapters(&owner), ["first"], "{cause}");
        assert!(restarts(&run.checkpoint).is_empty(), "{cause}");
    }
}

#[test]
fn on_stop_moves_a_chair_cause_to_a_new_route() {
    let document = document("read_only", json!({"deadline": "new_route"}));
    let mut owner = Owner {
        observations: [
            terminal(
                "a/1",
                AttemptOutcome::OtherFailure,
                Some(StopCause::Deadline),
                None,
            ),
            done("b/1"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = journal(&document);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(adapters(&owner), ["first", "second"]);
}

#[test]
fn restarts_stop_at_the_graph_wide_continuation_budget() {
    let document = document("read_only", serde_json::Value::Null);
    let limits = crate::config::GraphLimits {
        continuations: 0,
        ..Default::default()
    };
    let mut owner = Owner {
        observations: [
            terminal(
                "a/1",
                AttemptOutcome::OtherFailure,
                Some(StopCause::UsageLimited),
                None,
            ),
            done("b/1"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = Settled {
        limits: limits.clone(),
        ..journal(&document)
    };
    let run = advance_with_budget(
        &document,
        &mut owner,
        &mut journal,
        &limits,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Failed);
    assert!(restarts(&run.checkpoint).is_empty());
}

#[test]
fn on_stop_cannot_ask_a_graph_restart_to_resume() {
    let document = document(
        "read_only",
        json!({"provider_stop": {"step": "same_route", "continuation": "resume"}}),
    );
    let limits = crate::config::GraphLimits::default();
    assert!(validate_flat_plan(&document, &limits).is_err());
    let fresh = self::document(
        "read_only",
        json!({"provider_stop": {"step": "same_route", "continuation": "fresh"}}),
    );
    assert!(validate_flat_plan(&fresh, &limits).is_ok());
}

/// Ladder facts with one other ready account for the first declared model.
fn ladder_facts() -> DepletionFacts {
    use provenant_routing::{Readiness, ReadinessFact, Stopped};
    let model = provenant_routing::ModelId::parse("first/model").unwrap();
    DepletionFacts {
        catalogue: provenant_routing::Catalogue::bundled().unwrap(),
        config: Default::default(),
        stopped: Stopped {
            model: model.clone(),
            account: None,
            effort: None,
            tier: None,
            family: None,
        },
        readiness: vec![ReadinessFact {
            model,
            account: Some("spare".into()),
            state: Readiness::Ready,
        }],
        tried: Vec::new(),
        host_native: Vec::new(),
        exclusions: Vec::new(),
        intent: Default::default(),
        confidential: false,
        now: provenant_core::WallTime::from_micros(1_000_000).unwrap(),
        wait_bound_seconds: None,
    }
}

/// The restart record a settled stop saved, with the routes it launched on.
fn ladder_choices(checkpoint: &GraphCheckpoint) -> Vec<patch::LadderChoice> {
    checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| expansion.patch.as_ref()?.restart.as_ref()?.ladder.clone())
        .collect()
}

fn limited(reference: &str) -> AttemptObservation {
    terminal(
        reference,
        AttemptOutcome::OtherFailure,
        Some(StopCause::UsageLimited),
        None,
    )
}

#[test]
fn a_usage_limited_writer_restarts_on_the_ladder_route_then_on_its_declared_route() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut owner = Owner {
        observations: [limited("a/1"), limited("a/2"), done("b/1")].into(),
        facts: Some(ladder_facts()),
        ..Default::default()
    };
    let mut journal = Settled {
        facts: Some(ladder_facts()),
        ..journal(&document)
    };
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    // The ladder, not the declared route list, chose the first restart: the
    // same model on the spare account, in the same worktree. It leaves the
    // declared route behind it.
    assert_eq!(adapters(&owner)[..2], ["first", "first"]);
    assert_eq!(owner.routes[1].account.as_deref(), Some("spare"));
    assert_eq!(owner.worktrees[0], owner.worktrees[1]);
    let choices = ladder_choices(&run.checkpoint);
    assert_eq!(choices.len(), 1);
    assert_eq!(
        choices[0].rung,
        provenant_routing::Rung::SameModelOtherAccount
    );
    assert!(choices[0].target.contains("first/model") && choices[0].target.contains("spare"));
    // The ladder has nothing left, but the node declares an untried route, so
    // the chair is not asked while a route remains.
    assert_eq!(adapters(&owner), ["first", "first", "second"]);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert_eq!(
        restarts(&run.checkpoint),
        [
            (StopCause::UsageLimited, NextStep::NewRoute),
            (StopCause::UsageLimited, NextStep::NewRoute)
        ]
    );
}

#[test]
fn a_ladder_that_sees_no_ready_route_still_tries_the_declared_untried_route() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut facts = ladder_facts();
    facts.readiness.clear();
    let mut owner = Owner {
        observations: [limited("a/1"), done("b/1")].into(),
        facts: Some(facts.clone()),
        ..Default::default()
    };
    let mut journal = Settled {
        facts: Some(facts),
        ..journal(&document)
    };
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(adapters(&owner), ["first", "second"]);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    assert!(ladder_choices(&run.checkpoint).is_empty());
}

#[test]
fn a_host_native_hand_off_keeps_a_launchable_route_after_it() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut facts = ladder_facts();
    facts.readiness.clear();
    facts.host_native = vec![provenant_routing::HostNativeRoute {
        name: "claude-agent".into(),
        model: None,
    }];
    let mut owner = Owner {
        observations: [limited("a/1"), done("b/1")].into(),
        facts: Some(facts.clone()),
        ..Default::default()
    };
    let mut journal = Settled {
        facts: Some(facts),
        ..journal(&document)
    };
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(adapters(&owner), ["first", "host-native"]);
    assert!(run.checkpoint.pause.is_none(), "{:?}", run.checkpoint.pause);
    assert_eq!(run.checkpoint.nodes[0].state, NodeState::Succeeded);
    let choices = ladder_choices(&run.checkpoint);
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].rung, provenant_routing::Rung::HostNative);
    assert_eq!(choices[0].target, "host-native/claude-agent");
}
