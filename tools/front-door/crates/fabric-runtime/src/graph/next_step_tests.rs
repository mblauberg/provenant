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
    spawn_custody_released: Option<bool>,
    on_stop_defaults: provenant_records::OnStop,
}

impl AttemptOwner for Owner {
    type Error = std::convert::Infallible;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }

    fn graph_on_stop_defaults(&self) -> provenant_records::OnStop {
        self.on_stop_defaults.clone()
    }

    fn actor(&self) -> String {
        "chair".into()
    }

    fn preparation_fallback_evidence(
        &mut self,
        _key: &AttemptKey,
        _reference: &str,
    ) -> Result<Option<PreparationFallbackEvidence>, Self::Error> {
        Ok(self
            .spawn_custody_released
            .map(|custody_released| PreparationFallbackEvidence {
                failure: provenant_records::run::PreparationFailure::AdapterUnavailable,
                handoff: provenant_routing::FallbackHandoff::Local,
                custody_released,
            }))
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
            next_step::settle_stop(
                &self.document,
                &plan,
                checkpoint,
                limits,
                &provenant_config::PortTable::builtin().worktree_layout,
                &mut |_| Ok(facts.clone()),
            )
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
        settlement_class: Some(provenant_records::run::SettlementClass::Completed),
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
        cause: None,
    }
}

fn done(reference: &str) -> AttemptObservation {
    terminal(reference, AttemptOutcome::Succeeded, None, None)
}

/// Small: final handoff status, committed work and the host's no-work evidence
/// jointly decide completion; a blocked writer preserves its stated cause.
#[test]
fn writer_handoff_classes_settle_without_mistaking_done_for_a_stop() {
    for status in [Some("done"), Some("blocked"), Some("stopped_early"), None] {
        let document = document("worktree_write", json!({"stopped_early":"chair"}));
        let text = status.map(|status| format!("```handoff\n{}\n```", json!({
            "schema_version":2, "status":status, "note":"Owner must supply the missing capability",
            "remaining":if status == "done" { vec![] } else { vec!["Finish the brief"] },
            "decisions":[], "verify":[]
        })));
        let mut observation = terminal(
            "a/1",
            if status.is_some() {
                AttemptOutcome::Succeeded
            } else {
                AttemptOutcome::StoppedNoWork
            },
            None,
            text.as_deref(),
        );
        if let AttemptObservation::Terminal { commit, .. } = &mut observation {
            *commit = status.map(|_| "a".repeat(40));
        }
        let mut owner = Owner {
            observations: [observation].into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
        let progress = &run.checkpoint.nodes[0];
        if status == Some("done") {
            assert_eq!(progress.state, NodeState::Succeeded);
            assert_eq!(
                progress.attempts[0].outcome,
                Some(AttemptOutcome::Succeeded)
            );
            assert!(run.checkpoint.pause.is_none());
        } else {
            assert_eq!(
                progress.attempts[0].outcome,
                Some(if status.is_some() {
                    AttemptOutcome::DeclaredStop
                } else {
                    AttemptOutcome::StoppedNoWork
                })
            );
            assert!(run.checkpoint.pause.is_some());
            if status == Some("blocked") {
                assert!(progress.blocked);
                assert_eq!(progress.attempts[0].stop, Some(StopCause::Blocked));
                assert!(
                    progress
                        .advance_error
                        .as_deref()
                        .unwrap()
                        .contains("missing capability")
                );
            }
        }
        assert_eq!(owner.routes.len(), 1);
    }
}

// Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
#[test]
fn retry_of_a_declared_stop_preserves_a_valid_checkpoint() {
    for status in ["blocked", "stopped_early"] {
        let document = document("worktree_write", json!({"stopped_early":"chair"}));
        let text = format!(
            "```handoff\n{}\n```",
            json!({
                "schema_version": 2, "status": status, "note": "Work remains",
                "remaining": ["Finish"], "decisions": [], "verify": []
            })
        );
        let mut owner = Owner {
            observations: [terminal(
                "a/1",
                AttemptOutcome::Succeeded,
                None,
                Some(&text),
            )]
            .into(),
            ..Default::default()
        };
        let mut journal = journal(&document);
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        let mut checkpoint = run.checkpoint;
        assert_eq!(
            checkpoint.nodes[0].attempts[0].state,
            AttemptState::Terminal
        );
        patch::retry_node(
            &mut patch::LiveGraph {
                document: &document,
                checkpoint: &mut checkpoint,
            },
            &document.nodes[0].id,
            None,
            &patch::PatchContext {
                catalogue: None,
                library: &template::TemplateLibrary::bundled(),
                resolvers: &Default::default(),

                journal: None,
                limits: &plans::generous(),
                worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
            },
        )
        .unwrap();
        assert!(checkpoint.pause.is_none());
        journal.saved = Some(checkpoint);
        resume(&document, &mut Owner::default(), &mut journal).unwrap();
    }
}

/// Small: a settled review-required lane remains visible as such while its
/// dependent starts, without an acknowledgement or a synthetic retry.
#[test]
fn settled_review_required_proceeds_without_acknowledgement() {
    let mut document = document("read_only", serde_json::Value::Null);
    let mut dependent = document.nodes[0].clone();
    dependent.id = "dependent".into();
    dependent.depends_on = vec!["work".into()];
    document.nodes.push(dependent);
    let mut owner = Owner {
        observations: [
            terminal("a/1", AttemptOutcome::ReviewRequired, None, None),
            done("b/1"),
        ]
        .into(),
        ..Default::default()
    };
    let mut journal = journal(&document);
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(run.status, GraphStatus::Complete);
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .all(|node| node.state == NodeState::Succeeded)
    );
    assert_eq!(
        run.checkpoint.nodes[0].attempts[0].outcome,
        Some(AttemptOutcome::ReviewRequired)
    );
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        run.checkpoint
    );
}

/// Small: a provider that never started can consume another route only after
/// the owner proves custody release; its typed cause is never task failure.
#[test]
fn never_spawned_attempt_retries_elsewhere_only_with_released_custody() {
    for released in [false, true] {
        let document = document("worktree_write", serde_json::Value::Null);
        let mut failed = terminal(
            "a/1",
            AttemptOutcome::PreparationRefused(provenant_records::run::CauseKind::SpawnFailure),
            Some(StopCause::SpawnFailure),
            None,
        );
        if let AttemptObservation::Terminal { launched, .. } = &mut failed {
            *launched = false;
        }
        let mut owner = Owner {
            observations: [failed, done("b/1")].into(),
            spawn_custody_released: Some(released),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
        assert_eq!(
            adapters(&owner),
            if released {
                vec!["first", "second"]
            } else {
                vec!["first"]
            }
        );
        let progress = &run.checkpoint.nodes[0];
        assert_eq!(
            progress.state,
            if released {
                NodeState::Succeeded
            } else {
                NodeState::Failed
            }
        );
        assert_eq!(progress.attempts[0].stop, Some(StopCause::SpawnFailure));
        assert!(
            !progress
                .attempts
                .iter()
                .any(|attempt| attempt.outcome == Some(AttemptOutcome::TaskFailed))
        );
    }
}

/// Invariant: only an explicit retry-policy stall consumes the next route;
/// the fail-policy outcome stays terminal after the same launched stall.
#[test]
fn stalled_provider_retry_advances_but_fail_policy_does_not() {
    for (outcome, expected_routes, expected_state) in [
        (
            AttemptOutcome::ProviderStalledRetry,
            vec!["first", "second"],
            NodeState::Succeeded,
        ),
        (AttemptOutcome::TaskFailed, vec!["first"], NodeState::Failed),
    ] {
        let document = document("worktree_write", serde_json::Value::Null);
        let mut owner = Owner {
            observations: [terminal("a/1", outcome, None, None), done("b/1")].into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
        assert_eq!(adapters(&owner), expected_routes);
        assert_eq!(run.checkpoint.nodes[0].state, expected_state);
        if outcome == AttemptOutcome::ProviderStalledRetry {
            assert_eq!(owner.worktrees[0], owner.worktrees[1]);
        }
    }
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
                terminal("a/1", AttemptOutcome::OtherFailure, Some(cause), Some("```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"Work remains\",\"remaining\":[\"Finish work\"],\"decisions\":[],\"verify\":[]}\n```")),
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
fn a_stopped_writer_restarts_when_handoff_holds_a_known_credential() {
    let secret = "plain-registered-restart-credential-3391";
    let _scope = provenant_redact::publication_scope([secret.as_bytes().to_vec()]).unwrap();
    let document = document("worktree_write", serde_json::Value::Null);
    let handoff = format!(
        "```handoff\n{{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"Renderer half done with {secret}\",\"remaining\":[\"Finish renderer\"],\"decisions\":[],\"verify\":[]}}\n```"
    );
    let mut owner = Owner {
        observations: [
            terminal(
                "a/1",
                AttemptOutcome::TaskFailed,
                Some(StopCause::LaneTokenLost),
                Some(&handoff),
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
    assert!(owner.prompts[1].contains(secret));
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
    }
}

// Spec: docs/specs/fabric.md#one-model-spelling-per-route
#[test]
fn a_route_equal_to_the_handoff_appears_once_but_its_account_and_effort_variants_stay() {
    let full = "opencode/opencode/deepseek-v4.1-flash-free";
    let document = document("worktree_write", serde_json::Value::Null);
    let plan = validate_flat_plan(&document, &Default::default())
        .unwrap()
        .0;
    let mut node = plan.nodes[0].clone();
    // The ladder hands off to `full` on the ready "spare" account. The host
    // entry point already fixed the spelling, so a declared route equal to
    // that target (canonical id, account and effort) is already offered and
    // must appear once. A route that differs only by account or by effort is
    // a real alternative and stays beside the hand-off.
    node.routes = vec![
        serde_json::from_value(json!({"adapter":"opencode","model":full,"account":"spare"}))
            .unwrap(),
        serde_json::from_value(
            json!({"adapter":"opencode","model":full,"account":"spare","effort":"high"}),
        )
        .unwrap(),
        serde_json::from_value(json!({"adapter":"opencode","model":full,"account":"other"}))
            .unwrap(),
        serde_json::from_value(json!({"adapter":"second","model":"second/model"})).unwrap(),
    ];
    let progress: NodeProgress = serde_json::from_value(json!({
        "id": "work",
        "state": "failed",
        "attempts": [{
            "launched": true,
            "key": {"graph_id":"g-next","node_id":"work","attempt":1,"route_index":0},
            "route": {"adapter":"opencode","model":full},
            "state": "terminal",
            "attempt_ref": "a/1",
            "outcome": "other_failure",
            "launch_actor": "chair"
        }]
    }))
    .unwrap();
    let model = provenant_routing::ModelId::parse(full).unwrap();
    let mut facts = ladder_facts();
    facts.stopped.model = model.clone();
    for fact in &mut facts.readiness {
        fact.model = model.clone();
    }
    let successor = next_step::depletion_successor(
        &node,
        &progress,
        StopCause::UsageLimited,
        &mut move |_| -> Result<Option<DepletionFacts>, String> { Ok(Some(facts.clone())) },
    );
    assert_eq!(
        successor
            .routes
            .iter()
            .map(|route| (
                route.model.as_deref(),
                route.account.as_deref(),
                route.effort.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            (Some(full), Some("spare"), None),
            (Some(full), Some("spare"), Some("high")),
            (Some(full), Some("other"), None),
            (Some("second/model"), None, None),
        ],
        "a route equal to the hand-off target is offered once; account and effort variants stay"
    );
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

fn single_route_document(on_stop: serde_json::Value) -> GraphDocument {
    let mut document = document("worktree_write", on_stop);
    document.nodes[0].routes = Some(vec![serde_json::from_value(route("first")).unwrap()]);
    document
}

fn stopped_early_handoff(remaining: &[&str]) -> String {
    format!(
        "```handoff\n{}\n```",
        json!({
            "schema_version": 2, "status": "stopped_early", "note": "Work remains",
            "remaining": remaining, "decisions": [], "verify": []
        })
    )
}

/// A writer round that declares `stopped_early` with the given head.
fn stopped_round(reference: &str, remaining: &[&str], commit: char) -> AttemptObservation {
    let handoff = stopped_early_handoff(remaining);
    let mut observation = terminal(reference, AttemptOutcome::Succeeded, None, Some(&handoff));
    if let AttemptObservation::Terminal { commit: head, .. } = &mut observation {
        *head = Some(commit.to_string().repeat(40));
    }
    observation
}

fn restart_reasons(checkpoint: &GraphCheckpoint) -> Vec<String> {
    checkpoint
        .expansions
        .iter()
        .filter_map(|expansion| {
            let patch = expansion.patch.as_ref()?;
            patch.restart.as_ref()?;
            patch.reason.clone()
        })
        .collect()
}

fn pause_reason(checkpoint: &GraphCheckpoint) -> &str {
    checkpoint.nodes[0].expansion_pause.as_deref().unwrap()
}

/// Invariant: a recoverable stop on a node with no alternative route continues
/// on the same route; it does not pause.
#[test]
fn a_recoverable_stop_on_a_single_route_continues_on_the_same_route() {
    for cause in [
        StopCause::StoppedEarly,
        StopCause::StoppedNoWork,
        StopCause::LaneTokenLost,
        StopCause::ContextHandoff,
    ] {
        let document = single_route_document(serde_json::Value::Null);
        let handoff = stopped_early_handoff(&["Finish the renderer"]);
        let stopped = if cause == StopCause::StoppedEarly {
            terminal("a/1", AttemptOutcome::Succeeded, None, Some(&handoff))
        } else {
            terminal(
                "a/1",
                AttemptOutcome::TaskFailed,
                Some(cause),
                Some(&handoff),
            )
        };
        let mut owner = Owner {
            observations: [stopped, done("a/2")].into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
        assert!(run.checkpoint.successful(), "{cause}");
        assert_eq!(adapters(&owner), ["first", "first"], "{cause}");
        assert_eq!(owner.routes[0], owner.routes[1], "{cause}");
        assert_eq!(owner.worktrees[0], owner.worktrees[1], "{cause}");
        assert!(owner.prompts[1].contains("Finish the renderer"), "{cause}");
        assert_eq!(
            restarts(&run.checkpoint),
            [(
                cause,
                NextStep::SameRoute {
                    continuation: provenant_records::Continuation::Fresh
                }
            )],
            "{cause}"
        );
        let reasons = restart_reasons(&run.checkpoint);
        assert!(
            reasons[0].contains("continuation 1 of 2")
                && reasons[0].contains("limits.continuations=2"),
            "{reasons:?}"
        );
    }
}

/// Invariant: a different route is still preferred when one exists.
#[test]
fn a_stopped_early_writer_still_prefers_a_different_route() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut owner = Owner {
        observations: [stopped_round("a/1", &["Finish"], '1'), done("b/1")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(adapters(&owner), ["first", "second"]);
    assert_eq!(
        restarts(&run.checkpoint),
        [(StopCause::StoppedEarly, NextStep::NewRoute)]
    );
}

/// Invariant: rounds are bounded by a configurable limit, and the reason shows
/// the limit, its value and where the value comes from.
#[test]
fn same_route_continuations_stop_at_the_continuation_limit_and_name_it() {
    for (configured, rounds, source) in [(None, 3, "default"), (Some(1), 2, "configured")] {
        let document = single_route_document(serde_json::Value::Null);
        let mut limits = crate::config::GraphLimits::default();
        if let Some(continuations) = configured {
            limits.continuations = continuations;
        }
        let observations: VecDeque<_> = ["a", "b", "c"]
            .iter()
            .take(rounds)
            .enumerate()
            .map(|(index, item)| {
                stopped_round(
                    &format!("a/{index}"),
                    &[*item],
                    char::from(b'1' + index as u8),
                )
            })
            .collect();
        let mut owner = Owner {
            observations,
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
        assert_eq!(restarts(&run.checkpoint).len(), rounds - 1, "{source}");
        let reason = pause_reason(&run.checkpoint);
        assert!(
            reason.contains(&format!("limits.continuations={}", limits.continuations))
                && reason.contains(source),
            "{reason}"
        );
    }
}

/// Invariant: no continuation loop without progress. The first round has
/// nothing to compare with; each later round must add a commit or change the
/// handoff's remaining items.
#[test]
fn a_same_route_round_without_progress_pauses_for_the_chair() {
    for (second, continues) in [
        (stopped_round("a/2", &["Finish"], '1'), false),
        (stopped_round("a/2", &["Finish"], '2'), true),
        (stopped_round("a/2", &["Finish", "Polish"], '1'), true),
    ] {
        let document = single_route_document(serde_json::Value::Null);
        let mut owner = Owner {
            observations: [stopped_round("a/1", &["Finish"], '1'), second, done("a/3")].into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
        if continues {
            assert!(run.checkpoint.successful());
            assert_eq!(restarts(&run.checkpoint).len(), 2);
        } else {
            assert_eq!(restarts(&run.checkpoint).len(), 1);
            assert_eq!(adapters(&owner), ["first", "first"]);
            assert!(
                pause_reason(&run.checkpoint).contains("without progress"),
                "{}",
                pause_reason(&run.checkpoint)
            );
        }
    }
}

/// Invariant: explicit graph policy beats the default.
#[test]
fn on_stop_chair_beats_the_single_route_continuation() {
    let document = single_route_document(json!({"stopped_early": "chair"}));
    let mut owner = Owner {
        observations: [stopped_round("a/1", &["Finish"], '1'), done("a/2")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert_eq!(adapters(&owner), ["first"]);
    assert!(restarts(&run.checkpoint).is_empty());
    assert!(pause_reason(&run.checkpoint).contains("return to the chair"));
}

/// Invariant: an explicit `new_route` is the node's choice; the same-route
/// fallback applies only to the table default.
#[test]
fn an_explicit_on_stop_new_route_pauses_on_a_single_route_instead_of_continuing() {
    let document = single_route_document(json!({"stopped_early": "new_route"}));
    let mut owner = Owner {
        observations: [stopped_round("a/1", &["Finish"], '1'), done("a/2")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert_eq!(adapters(&owner), ["first"]);
    assert!(restarts(&run.checkpoint).is_empty());
    assert!(
        pause_reason(&run.checkpoint).contains("no different route is available"),
        "{}",
        pause_reason(&run.checkpoint)
    );
}

fn blocked_handoff(blocked_on: Option<&str>) -> String {
    let mut handoff = json!({
        "schema_version": 2, "status": "blocked", "note": "the shared cargo lock",
        "remaining": ["Finish"], "decisions": [], "verify": []
    });
    if let Some(kind) = blocked_on {
        handoff["blocked_on"] = kind.into();
    }
    format!("```handoff\n{handoff}\n```")
}

fn blocked_round(reference: &str, blocked_on: Option<&str>) -> AttemptObservation {
    terminal(
        reference,
        AttemptOutcome::Succeeded,
        None,
        Some(&blocked_handoff(blocked_on)),
    )
}

/// Invariant: environment blocks are retried before anyone is paged.
#[test]
fn an_environment_block_retries_once_on_the_same_route_then_pauses_as_environment() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut owner = Owner {
        observations: [
            blocked_round("a/1", Some("environment")),
            blocked_round("a/2", Some("environment")),
            done("a/3"),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert_eq!(adapters(&owner), ["first", "first"]);
    assert_eq!(owner.worktrees[0], owner.worktrees[1]);
    assert_eq!(
        restarts(&run.checkpoint),
        [(
            StopCause::BlockedEnvironment,
            NextStep::SameRoute {
                continuation: provenant_records::Continuation::Fresh
            }
        )]
    );
    let reason = pause_reason(&run.checkpoint);
    assert!(
        reason.contains("environment problem") && reason.contains("the shared cargo lock"),
        "{reason}"
    );
    assert!(reason.contains("retried once"), "{reason}");
    assert!(!reason.contains("owner decision"), "{reason}");
}

/// An environment block is not pinned: a node can send it to the chair at once.
#[test]
fn on_stop_sends_an_environment_block_straight_to_the_chair() {
    let document = document("worktree_write", json!({"blocked_environment": "chair"}));
    let mut owner = Owner {
        observations: [blocked_round("a/1", Some("environment")), done("a/2")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert_eq!(adapters(&owner), ["first"]);
    assert!(restarts(&run.checkpoint).is_empty());
    let reason = pause_reason(&run.checkpoint);
    assert!(reason.contains("environment problem"), "{reason}");
    assert!(!reason.contains("retried"), "{reason}");
}

/// Invariant: only owner_decision or capability blocks say "owner decision".
#[test]
fn a_dependency_block_pauses_as_a_wait_without_claiming_an_owner_decision() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut owner = Owner {
        observations: [blocked_round("a/1", Some("dependency")), done("a/2")].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert_eq!(adapters(&owner), ["first"]);
    assert!(restarts(&run.checkpoint).is_empty());
    let reason = pause_reason(&run.checkpoint);
    assert!(
        reason.contains("is waiting on the shared cargo lock"),
        "{reason}"
    );
    assert!(!reason.contains("owner decision"), "{reason}");
}

/// Invariant: an owner boundary stays pinned, whatever the node's `on_stop`.
#[test]
fn owner_decision_and_capability_blocks_stay_pinned_to_the_chair() {
    for (kind, wording) in [
        ("owner_decision", "owner decision"),
        ("capability", "missing capability"),
    ] {
        let document = document("worktree_write", json!({"blocked": "new_route"}));
        let mut owner = Owner {
            observations: [blocked_round("a/1", Some(kind)), done("b/1")].into(),
            ..Default::default()
        };
        let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
        assert_eq!(adapters(&owner), ["first"], "{kind}");
        assert!(restarts(&run.checkpoint).is_empty(), "{kind}");
        let reason = pause_reason(&run.checkpoint);
        assert!(reason.contains(wording), "{kind}: {reason}");
    }
}

/// Invariant: no kind given means no claim about the kind.
#[test]
fn a_blocked_handoff_without_a_kind_gets_neutral_wording() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut owner = Owner {
        observations: [blocked_round("a/1", None)].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert!(restarts(&run.checkpoint).is_empty());
    let reason = pause_reason(&run.checkpoint);
    assert!(
        reason.contains("is blocked: the shared cargo lock")
            && reason.contains("blocked_on not given"),
        "{reason}"
    );
    assert!(!reason.contains("owner decision"), "{reason}");
    assert!(!reason.contains("capability the brief"), "{reason}");
}

/// An unrecognised kind is reported as unrecognised, not as missing.
#[test]
fn an_unrecognised_blocked_on_is_named_not_reported_missing() {
    let document = document("worktree_write", serde_json::Value::Null);
    let mut owner = Owner {
        observations: [blocked_round("a/1", Some("weather"))].into(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    let reason = pause_reason(&run.checkpoint);
    assert!(
        reason.contains("is blocked: the shared cargo lock"),
        "{reason}"
    );
    assert!(reason.contains("weather"), "{reason}");
    assert!(!reason.contains("not given"), "{reason}");
    assert!(!reason.contains("owner decision"), "{reason}");
}

/// Host/project defaults are resolved once at admission; the node's own entry
/// wins where both name the same cause.
#[test]
fn stopped_no_work_default_pauses_and_the_node_overrides_it() {
    let defaults =
        provenant_records::OnStop::new([(StopCause::StoppedNoWork, NextStep::Chair)].into());
    let handoff = stopped_early_handoff(&["Finish the renderer"]);
    let stopped = || {
        terminal(
            "a/1",
            AttemptOutcome::StoppedNoWork,
            Some(StopCause::StoppedNoWork),
            Some(&handoff),
        )
    };

    let document = single_route_document(serde_json::Value::Null);
    let mut owner = Owner {
        observations: [stopped()].into(),
        on_stop_defaults: defaults.clone(),
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert!(pause_reason(&run.checkpoint).contains("return to the chair"));
    assert!(restarts(&run.checkpoint).is_empty());

    let document = single_route_document(json!({"stopped_no_work": "same_route"}));
    let mut owner = Owner {
        observations: [stopped(), done("a/2")].into(),
        on_stop_defaults: defaults,
        ..Default::default()
    };
    let run = advance(&document, &mut owner, &mut journal(&document)).unwrap();
    assert!(run.checkpoint.successful());
    assert_eq!(
        restarts(&run.checkpoint),
        [(
            StopCause::StoppedNoWork,
            NextStep::SameRoute {
                continuation: provenant_records::Continuation::Fresh
            }
        )]
    );
}
