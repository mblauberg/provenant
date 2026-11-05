// The bundled `deliver` preset: write, CI gate, (repair, CI gate)*, review,
// land, run through the real scheduler with a scripted CI host.
#[allow(unused_imports)] // The worktree driver includes this module without libtest.
use super::preset::{Layers, expand};
#[allow(unused_imports)] // The worktree driver includes this module without libtest.
use provenant_fabric_runtime::graph::{
    AccessMode, AskReply, AskScope, AttemptKey, AttemptObservation, AttemptOutcome, CiFailedCheck,
    CiHostStamp, CiObservation, CiState, CiStatus, GraphCheckpoint, GraphDocument, GraphJournal,
    GraphRoute, GraphRun, GraphStatus, LandObservation, LandRequest, NodeInput, NodeProgress,
    NodeState, Replay, advance_with_budget,
};
#[allow(unused_imports)] // The worktree driver includes this module without libtest.
use serde_json::{Value, json};
#[allow(unused_imports)] // The worktree driver includes this module without libtest.
use std::collections::VecDeque;

fn advance(
    document: &GraphDocument,
    host: &mut Host,
    journal: &mut Journal,
) -> Result<GraphRun, String> {
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    advance_with_budget(document, host, journal, &config.limits, &config.budget)
        .map_err(|error| error.to_string())
}

const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const WRITER_REPORT: &str = "Implemented the parser.\nRemaining: wire the docs table.\n";

#[derive(Clone)]
enum Ci {
    Pass,
    Pending,
    Fail(&'static str, &'static str),
}

#[derive(Default)]
struct Journal(Option<GraphCheckpoint>);

impl GraphJournal for Journal {
    type Error = std::convert::Infallible;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.0.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.0 = Some(checkpoint.clone());
        Ok(())
    }
}

#[derive(Default)]
struct Host {
    /// Scripted CI answers in order; passing once the script runs out.
    ci: VecDeque<Ci>,
    /// The newest commit pushed to the pull request branch.
    head: Option<String>,
    commits: usize,
    launched: Vec<String>,
    prompts: Vec<(String, String)>,
    sources: Vec<(String, Option<String>)>,
    /// Branch and head of every land request.
    lands: Vec<(String, String)>,
    asks: Vec<(String, String)>,
    ci_polls: usize,
    /// The writer's final report; `WRITER_REPORT` when unset.
    writer_report: Option<String>,
    /// The attempts each launch was asked to route away from.
    distinct: Vec<(String, Vec<String>)>,
    routes: Vec<(String, GraphRoute)>,
    host_native_unavailable: bool,
    /// Give the fake routes distinct attempt identities in one model family.
    one_family: bool,
    /// Head of the integration branch; a writer that integrates moves it.
    integration: Option<String>,
    /// Read-only nodes whose id contains one of these fail their verdict.
    failing: Vec<String>,
    /// Delivery mode and fast-forward restriction each launch was given.
    deliveries: Vec<(String, Option<provenant_records::run::DeliveryMode>, bool)>,
}

impl CiStatus for Host {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: "boot".into(),
            host_monotonic_micros: 1 + self.ci_polls as u64 * 30_000_000,
        })
    }
    fn observe_ci(&mut self, target: &str, _: &[String], _: usize, _: usize) -> CiObservation {
        assert_eq!(target, "refs/heads/lane/write");
        self.ci_polls += 1;
        let (state, failed_checks) = match self.ci.pop_front().unwrap_or(Ci::Pass) {
            Ci::Pass => (CiState::Passed, Vec::new()),
            Ci::Pending => (CiState::Pending, Vec::new()),
            Ci::Fail(name, excerpt) => (
                CiState::Failed,
                vec![CiFailedCheck {
                    extra: Default::default(),
                    name: name.into(),
                    url: None,
                    excerpt: excerpt.into(),
                }],
            ),
        };
        CiObservation {
            extra: Default::default(),
            state,
            summary: "scripted".into(),
            head_sha: self.head.clone(),
            failed_checks,
            check_names: vec!["build".into(), "tests".into()],
            truncated: false,
        }
    }
}

impl provenant_fabric_runtime::graph::AttemptOwner for Host {
    type Error = std::convert::Infallible;
    fn actor(&self) -> String {
        "deliver-test".into()
    }
    fn preparation_fallback_evidence(
        &mut self,
        key: &AttemptKey,
        reference: &str,
    ) -> Result<Option<provenant_records::run::PreparationFailure>, Self::Error> {
        let refused_locally = self.host_native_unavailable
            && key.route_index == 0
            && reference
                == if self.one_family {
                    format!("fixture/{}/{}", key.node_id, key.route_index)
                } else {
                    format!("fixture/{}", key.node_id)
                }
            && self.routes.iter().any(|(id, route)| {
                id == &key.node_id && route.adapter.as_deref() == Some("host-native")
            });
        Ok(refused_locally
            .then_some(provenant_records::run::PreparationFailure::AdapterUnavailable))
    }
    fn attempt_families(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        Ok(if self.one_family && !attempts.is_empty() {
            vec!["one-family".into()]
        } else {
            Vec::new()
        })
    }
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(self.integration.clone())
    }
    fn ci_status(&mut self) -> Option<&mut dyn CiStatus> {
        Some(self)
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        provenant_core::WallTime::from_micros(1 + self.ci_polls as i64 * 30_000_000).ok()
    }
    fn node_ask(&mut self, scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        self.asks
            .push((scope.ask.ask_id.clone(), scope.ask.session.clone()));
        Ok(None)
    }
    fn land(&mut self, request: LandRequest<'_>) -> Result<LandObservation, Self::Error> {
        // A tier that asks only observes; the chair has not merged by hand.
        if request.authority.is_none() {
            return Ok(LandObservation::Pending {
                reason: "awaiting the chair".into(),
            });
        }
        self.lands
            .push((request.branch.into(), request.head.into()));
        Ok(LandObservation::Merged {
            pull_request: Some(7),
            note: None,
        })
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        node: NodeInput<'_>,
        route: &GraphRoute,
        access: AccessMode,
        _: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        let reference = if self.one_family {
            format!("fixture/{}/{}", key.node_id, key.route_index)
        } else {
            format!("fixture/{}", key.node_id)
        };
        self.launched.push(key.node_id.clone());
        self.deliveries.push((
            key.node_id.clone(),
            replay.delivery.cloned(),
            replay.fast_forward_only,
        ));
        self.routes.push((key.node_id.clone(), route.clone()));
        if self.host_native_unavailable && route.adapter.as_deref() == Some("host-native") {
            return Ok(AttemptObservation::Terminal {
                attempt_ref: Some(reference),
                outcome: AttemptOutcome::AdapterUnavailable,
                launched: false,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
            });
        }
        self.prompts
            .push((key.node_id.clone(), node.prompt.to_owned()));
        self.sources
            .push((key.node_id.clone(), node.source_commit.map(str::to_owned)));
        self.distinct
            .push((key.node_id.clone(), node.distinct_from_attempts.to_vec()));
        let writer = access == AccessMode::WorktreeWrite;
        let text = if key.node_id.ends_with("write") {
            self.writer_report
                .clone()
                .unwrap_or_else(|| WRITER_REPORT.to_owned())
        } else if writer {
            format!(
                "Repaired in {0}.\n```handoff\n{{\"schema_version\":2,\"status\":\"done\",\"note\":\"Repaired.\",\"remaining\":[\"Left by {0}.\"],\"decisions\":[],\"verify\":[]}}\n```\n",
                key.node_id
            )
        } else {
            let verdict = if self.failing.iter().any(|part| key.node_id.contains(part)) {
                "FAIL"
            } else {
                "PASS"
            };
            format!(
                "Checked {}.\n```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: {verdict}\n",
                key.node_id
            )
        };
        let mut result =
            provenant_records::result_capture::ResultCapture::new(node.required_verdict);
        result.push(text.as_bytes());
        let commit = if writer {
            self.commits += 1;
            let commit = format!("{:040x}", self.commits);
            self.head = Some(commit.clone());
            if matches!(
                replay.delivery,
                Some(provenant_records::run::DeliveryMode::Integrate(_))
            ) {
                self.integration = Some(commit.clone());
            }
            Some(commit)
        } else {
            node.source_commit.map(str::to_owned)
        };
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(reference),
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: Some(Box::new(result.finish())),
            result_warning: None,
            result_path: None,
            branch: writer.then(|| {
                replay
                    .branch
                    .map_or_else(|| "lane/write".to_owned(), str::to_owned)
            }),
            commit,
        })
    }
}

fn deliver(overrides: &[&str]) -> GraphDocument {
    let mut overrides: Vec<String> = overrides.iter().map(|value| (*value).into()).collect();
    overrides.push("parameters.worktree='/repo/lane'".into());
    overrides.push(format!("parameters.subject.commit='{BASE}'"));
    let expanded = expand(&Layers::empty(), "deliver", "approved brief", &overrides).unwrap();
    serde_json::from_value(expanded.document).unwrap()
}

fn drive(document: &GraphDocument, host: &mut Host, journal: &mut Journal) -> GraphRun {
    let mut last: Option<GraphRun> = None;
    for _ in 0..24 {
        let run = advance(document, host, journal).unwrap();
        if last
            .as_ref()
            .is_some_and(|last| last.checkpoint == run.checkpoint)
        {
            return run;
        }
        last = Some(run);
    }
    last.unwrap()
}

fn progress<'a>(checkpoint: &'a GraphCheckpoint, id: &str) -> &'a NodeProgress {
    checkpoint
        .nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("no node {id}"))
}

fn commit(n: usize) -> String {
    format!("{n:040x}")
}

const HOST_REPAIR: &str =
    "parameters.repair_tier=[{adapter='host-native',name='ci-fixer'},{alias='senior'}]";

#[test]
fn repair_tiers_keep_role_effort_and_lists_require_per_route_effort() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "deliver",
            "[roles.repairer]\ntier='junior'\neffort='high'\n",
        )
        .unwrap();
    let args = ["parameters.worktree='/repo/lane'".into()];
    let expanded = expand(&layers, "deliver", "approved brief", &args).unwrap();
    let fix = &expanded.document["nodes"][1]["composite"]["nodes"][1]["loop"]["body"][0];
    assert_eq!(fix["route"], json!({"alias":"junior","effort":"high"}));
    let args = [
        args[0].clone(),
        "roles.repairer.tier=[{adapter='host-native',name='ci-fixer'},{alias='senior'}]".into(),
    ];
    let error = expand(&layers, "deliver", "approved brief", &args).unwrap_err();
    assert!(
        error.contains("route lists cannot be combined with effort"),
        "{error}"
    );
}

#[test]
fn deliver_falls_back_when_the_host_repairer_is_absent() {
    let document = deliver(&[HOST_REPAIR]);
    let mut host = Host {
        ci: VecDeque::from([Ci::Fail("tests", "test failure")]),
        host_native_unavailable: true,
        ..Default::default()
    };
    let run = drive(&document, &mut host, &mut Journal::default());
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    let fixes: Vec<_> = host
        .routes
        .iter()
        .filter(|(id, _)| id.contains("fix"))
        .map(|(_, route)| route)
        .collect();
    assert_eq!(fixes.len(), 2);
    assert_eq!(fixes[0].adapter.as_deref(), Some("host-native"));
    assert_eq!(fixes[1].alias.as_deref(), Some("senior"));
    assert_eq!(host.lands, [("lane/write".into(), commit(2))]);
}

#[test]
fn deliver_substitutes_a_verifier_after_a_verified_preparation_failure() {
    fn set_verifier(nodes: &mut [provenant_fabric_runtime::graph::GraphNode]) -> usize {
        let mut found = 0;
        for node in nodes {
            if node.role.as_deref() == Some("review.verify") {
                node.route = None;
                node.routes = Some(vec![
                    serde_json::from_value(json!({"adapter":"host-native","name":"verify"}))
                        .unwrap(),
                    serde_json::from_value(json!({"alias":"senior"})).unwrap(),
                ]);
                found += 1;
            }
            if let Some(composite) = &mut node.composite {
                found += set_verifier(&mut composite.nodes);
            }
        }
        found
    }
    let mut document = deliver(&[]);
    assert_eq!(set_verifier(&mut document.nodes), 1);
    let mut host = Host {
        host_native_unavailable: true,
        one_family: true,
        ..Default::default()
    };
    let run = drive(&document, &mut host, &mut Journal::default());
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    let verifier = run
        .checkpoint
        .nodes
        .iter()
        .find(|node| node.id.ends_with("--verify"))
        .unwrap();
    assert_eq!(verifier.state, NodeState::Succeeded);
    assert_eq!(verifier.attempts.len(), 2);
    assert!(verifier.attempts[1].review_substitution);
    assert_ne!(
        verifier.attempts[0].attempt_ref,
        verifier.attempts[1].attempt_ref
    );
    assert_eq!(
        verifier.separation.as_ref().unwrap().outcome,
        provenant_fabric_runtime::graph::SeparationOutcome::Unavailable
    );
    assert!(
        run.checkpoint
            .nodes
            .iter()
            .any(|node| node.id.ends_with("--synthesis") && node.state == NodeState::Succeeded)
    );
    assert_eq!(host.lands, [("lane/write".into(), commit(1))]);
}

#[test]
fn a_successful_host_repair_still_waits_for_ci_on_its_pushed_head() {
    let document = deliver(&[HOST_REPAIR, "parameters.max_iterations=1"]);
    let mut host = Host {
        ci: VecDeque::from([
            Ci::Fail("tests", "before repair"),
            Ci::Fail("tests", "after repair"),
        ]),
        ..Default::default()
    };
    let run = drive(&document, &mut host, &mut Journal::default());
    assert_eq!(run.status, GraphStatus::Paused, "{:?}", run.checkpoint);
    let repair = progress(&run.checkpoint, "ci--repair--r1--fix");
    assert_eq!(repair.state, NodeState::Succeeded);
    assert_eq!(
        repair.attempts[0].route.adapter.as_deref(),
        Some("host-native")
    );
    let gate = progress(&run.checkpoint, "ci--repair--r1--check")
        .ci
        .as_ref()
        .unwrap();
    assert_eq!(gate.head_sha.as_deref(), Some(commit(2).as_str()));
    assert_eq!(gate.state, CiState::Failed);
    assert!(host.lands.is_empty());
    assert!(!host.launched.iter().any(|id| id.starts_with("review--")));
}

#[test]
fn deliver_lowers_to_write_gate_repair_loop_review_and_land() {
    let document = expand(
        &Layers::empty(),
        "deliver",
        "approved brief",
        &["parameters.worktree='/repo/lane'".into()],
    )
    .unwrap()
    .document;
    let ids: Vec<&str> = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["write", "ci", "review", "accept", "land"]);
    let nodes = &document["nodes"];
    assert_eq!(nodes[0]["delivery"], "pr");
    let ci = &nodes[1]["composite"]["nodes"];
    assert_eq!(ci[0]["condition"]["type"], "ci");
    assert_eq!(ci[0]["condition"]["target"], json!({"node":"write"}));
    assert_eq!(ci[0]["on_fail"], "repair");
    assert_eq!(ci[1]["loop"]["max_rounds"], 3);
    assert_eq!(ci[1]["loop"]["until"], "check");
    assert!(ci[1]["loop"].get("decision_session").is_none());
    let fix = &ci[1]["loop"]["body"][0];
    assert_eq!(fix["continues"], "write");
    assert_eq!(fix["access"], "worktree_write");
    assert_eq!(fix["worktree"], "/repo/lane");
    assert_eq!(nodes[2]["kind"], "composite");
    assert_eq!(
        nodes[4]["land"],
        json!({"node":"write","tier":"substantial"})
    );
    assert!(nodes[4].get("prompt").is_none() && nodes[4].get("read_roots").is_none());
    assert!(document.get("decision_session").is_none());

    // Knobs: the loop bound, the escalation session, the land tier and land off.
    let tuned = expand(
        &Layers::empty(),
        "deliver",
        "approved brief",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.max_iterations=5".into(),
            "parameters.decision_session='chair'".into(),
            "parameters.land_tier='crucial'".into(),
            "parameters.checks=['build']".into(),
        ],
    )
    .unwrap()
    .document;
    let ci = &tuned["nodes"][1]["composite"]["nodes"];
    assert_eq!(ci[1]["loop"]["max_rounds"], 5);
    assert_eq!(ci[1]["loop"]["decision_session"], "chair");
    assert_eq!(ci[0]["condition"]["checks"], json!(["build"]));
    assert_eq!(tuned["decision_session"], "chair");
    assert_eq!(tuned["nodes"][4]["land"]["tier"], "crucial");
    assert_eq!(tuned["nodes"][4]["land"]["session"], "chair");
    let parked = expand(
        &Layers::empty(),
        "deliver",
        "approved brief",
        &[
            "parameters.worktree='/repo/lane'".into(),
            "parameters.land=0".into(),
        ],
    )
    .unwrap()
    .document;
    assert_eq!(parked["nodes"].as_array().unwrap().len(), 4);
    for refused in [
        "parameters.land=2",
        "parameters.max_iterations=0",
        "parameters.land_tier='bold'",
    ] {
        assert!(
            expand(
                &Layers::empty(),
                "deliver",
                "approved brief",
                &["parameters.worktree='/repo/lane'".into(), refused.into()],
            )
            .is_err(),
            "{refused}"
        );
    }
    let config = provenant_fabric_runtime::config::GraphConfig::default();
    let inspection = deliver(&[]).inspect(&config.limits).unwrap();
    assert!(
        inspection.limit_reason.is_none(),
        "{:?}",
        inspection.limit_reason
    );
}

#[test]
fn green_ci_skips_the_repairs_and_lands_the_reviewed_writer_head() {
    let document = deliver(&[]);
    let mut host = Host::default();
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    assert!(!host.launched.iter().any(|id| id.contains("fix")));
    assert_eq!(
        progress(&run.checkpoint, "ci--repair").state,
        NodeState::Skipped
    );
    assert_eq!(progress(&run.checkpoint, "ci").state, NodeState::Succeeded);
    assert_eq!(host.lands, [("lane/write".into(), commit(1))]);
    let reviewed = host
        .sources
        .iter()
        .filter(|(id, _)| id.starts_with("review--"))
        .collect::<Vec<_>>();
    assert!(!reviewed.is_empty());
    assert!(
        reviewed
            .iter()
            .all(|(_, source)| source.as_deref() == Some(commit(1).as_str()))
    );
}

#[test]
fn a_red_gate_repairs_from_the_failure_excerpt_and_the_repaired_head_is_reviewed_and_landed() {
    let document = deliver(&[]);
    let mut host = Host {
        ci: VecDeque::from([Ci::Fail("tests", "assertion failed: parser::rejects_tabs")]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    let fixes: Vec<&String> = host
        .launched
        .iter()
        .filter(|id| id.contains("fix"))
        .collect();
    assert_eq!(fixes, ["ci--repair--r1--fix"]);
    let (_, prompt) = host
        .prompts
        .iter()
        .find(|(id, _)| id == "ci--repair--r1--fix")
        .unwrap();
    assert!(
        prompt.contains("tests: assertion failed: parser::rejects_tabs"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Remaining: wire the docs table."),
        "{prompt}"
    );
    // A fresh agent: the writer's brief is not carried, only its final report.
    let (_, writer_brief) = host.prompts.iter().find(|(id, _)| id == "write").unwrap();
    assert!(writer_brief.contains("Implement one bounded slice"));
    assert!(!prompt.contains("Implement one bounded slice"), "{prompt}");
    assert!(prompt.contains("no earlier transcript"), "{prompt}");
    // The repair continues the writer's branch; review and land pin its head.
    let repaired = commit(2);
    assert_eq!(
        progress(&run.checkpoint, "ci--repair--r1--check")
            .ci
            .as_ref()
            .unwrap()
            .head_sha
            .as_deref(),
        Some(repaired.as_str())
    );
    let reviewed = host
        .sources
        .iter()
        .filter(|(id, _)| id.starts_with("review--"))
        .collect::<Vec<_>>();
    assert!(!reviewed.is_empty());
    assert!(
        reviewed
            .iter()
            .all(|(_, source)| source.as_deref() == Some(repaired.as_str()))
    );
    assert_eq!(host.lands, [("lane/write".into(), repaired)]);
}

#[test]
fn pending_ci_runs_no_model_and_status_shows_the_waiting_gate() {
    let document = deliver(&[]);
    let mut host = Host {
        ci: VecDeque::from([Ci::Pending, Ci::Pending, Ci::Pending]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let first = advance(&document, &mut host, &mut journal).unwrap();
    assert_eq!(host.launched, ["write"]);
    let mut waiting = first;
    for _ in 0..2 {
        waiting = advance(&document, &mut host, &mut journal).unwrap();
    }
    assert_eq!(
        host.launched,
        ["write"],
        "no attempt starts while CI is pending"
    );
    assert_eq!(waiting.status, GraphStatus::Pending);
    let gate = progress(&waiting.checkpoint, "ci--check");
    assert_eq!(gate.state, NodeState::Running);
    assert!(gate.attempts.is_empty());
    let human = crate::fabric::graph_human(&waiting);
    assert!(
        human.contains("CI waiting elapsed=") && human.contains("checks=build, tests"),
        "{human}"
    );
}

#[test]
fn an_exhausted_repair_loop_asks_the_decision_session_or_pauses() {
    let failing = || {
        VecDeque::from([
            Ci::Fail("tests", "still red"),
            Ci::Fail("tests", "still red"),
            Ci::Fail("tests", "still red"),
        ])
    };
    let document = deliver(&[
        "parameters.max_iterations=2",
        "parameters.decision_session='chair'",
    ]);
    let mut host = Host {
        ci: failing(),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    let fixes: Vec<&String> = host
        .launched
        .iter()
        .filter(|id| id.contains("fix"))
        .collect();
    assert_eq!(fixes, ["ci--repair--r1--fix", "ci--repair--r2--fix"]);
    assert!(
        host.asks
            .iter()
            .any(|(ask, session)| ask.ends_with("ci--repair.rounds-2") && session == "chair"),
        "{:?}",
        host.asks
    );
    assert!(host.lands.is_empty());
    assert!(!host.launched.iter().any(|id| id.starts_with("review--")));
    assert_ne!(run.status, GraphStatus::Complete);

    let document = deliver(&["parameters.max_iterations=2"]);
    let mut host = Host {
        ci: failing(),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert!(host.asks.is_empty());
    let paused = progress(&run.checkpoint, "ci--repair");
    assert!(
        paused
            .loop_pause
            .as_deref()
            .is_some_and(|reason| reason.contains("exhausted after 2 rounds")),
        "{:?}",
        paused
    );
    assert!(host.lands.is_empty());
}

#[test]
fn deliver_nests_inside_another_preset() {
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "ship",
            "stages = [{ id = \"ship\", preset = \"deliver\", parameters = { worktree = \"/repo/lane\" } }]\n",
        )
        .unwrap();
    let expanded = expand(
        &layers,
        "ship",
        "approved brief",
        &[format!("ship.subject.commit='{BASE}'")],
    )
    .unwrap();
    let document: GraphDocument = serde_json::from_value(expanded.document).unwrap();
    let mut host = Host::default();
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    assert_eq!(host.lands.len(), 1);

    // The child's decision session reaches its land node, so a crucial tier
    // asks the wrapper's chair instead of failing for want of a session, and
    // waits for the chair to land by hand.
    let mut layers = Layers::empty();
    layers
        .insert(
            "repo",
            "ship",
            "stages = [{ id = \"ship\", preset = \"deliver\", parameters = { worktree = \"/repo/lane\", decision_session = \"chair\", land_tier = \"crucial\" } }]\n",
        )
        .unwrap();
    let expanded = expand(
        &layers,
        "ship",
        "approved brief",
        &[format!("ship.subject.commit='{BASE}'")],
    )
    .unwrap();
    let document: GraphDocument = serde_json::from_value(expanded.document).unwrap();
    let mut host = Host::default();
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    let land = progress(&run.checkpoint, "ship--land");
    assert_eq!(land.state, NodeState::Running, "{:?}", land.advance_error);
    assert!(host.lands.is_empty());
    assert!(land.land.as_ref().unwrap().authorised_by.is_none());
    assert!(
        host.asks
            .iter()
            .any(|(ask, session)| ask == "ship--land.ask-1" && session == "chair"),
        "{:?}",
        host.asks
    );
}

#[test]
fn a_council_pinned_to_the_writer_commit_never_lands_the_repaired_head() {
    // The council reviews the writer's commit, then a repair moves the head:
    // the passing verdict judged the old head, so land refuses the new one.
    fn pin_to_writer(value: &mut Value) {
        if value == &json!({"type":"head", "node":"write"}) {
            *value = json!({"type":"node", "node":"write"});
        }
        match value {
            Value::Array(items) => items.iter_mut().for_each(pin_to_writer),
            Value::Object(map) => map.values_mut().for_each(pin_to_writer),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(deliver(&[])).unwrap();
    pin_to_writer(&mut value);
    let document: GraphDocument = serde_json::from_value(value).unwrap();
    let mut host = Host {
        ci: VecDeque::from([Ci::Fail("tests", "assertion failed")]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    let reviewed: Vec<_> = host
        .sources
        .iter()
        .filter(|(id, _)| id.starts_with("review--"))
        .collect();
    assert!(!reviewed.is_empty());
    assert!(
        reviewed
            .iter()
            .all(|(_, source)| source.as_deref() == Some(commit(1).as_str())),
        "{reviewed:?}"
    );
    assert!(host.lands.is_empty(), "{:?}", host.lands);
    let land = progress(&run.checkpoint, "land");
    assert_eq!(land.state, NodeState::Failed);
    let error = land.advance_error.as_deref().unwrap();
    assert!(
        error.contains(&commit(2)) && error.contains(&format!("accept passed {}", commit(1))),
        "{error}"
    );
}

#[test]
fn reviewers_route_away_from_the_writer_and_every_repair_author() {
    let document = deliver(&[]);
    let mut host = Host {
        ci: VecDeque::from([Ci::Fail("tests", "assertion failed")]),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    let separated: Vec<_> = host
        .distinct
        .iter()
        .filter(|(id, _)| id.contains("correctness-spec"))
        .collect();
    assert!(!separated.is_empty(), "{:?}", host.distinct);
    for (id, attempts) in separated {
        assert_eq!(
            attempts,
            &["fixture/ci--repair--r1--fix", "fixture/write"],
            "{id}"
        );
    }
}

#[test]
fn remaining_work_at_the_end_of_a_long_report_reaches_every_repair_round() {
    let mut report = String::from("Implemented the parser.\n");
    while report.len() < 3 * 4096 {
        report.push_str("Notes: the tokenizer keeps byte offsets for every span.\n");
    }
    report.push_str(
        "```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"Parser landed.\",\"remaining\":[\"Wire the docs table.\"],\"decisions\":[],\"verify\":[]}\n```\n",
    );
    let document = deliver(&[]);
    let mut host = Host {
        ci: VecDeque::from([
            Ci::Fail("tests", "first red"),
            Ci::Fail("tests", "second red"),
        ]),
        writer_report: Some(report),
        ..Default::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    let brief = |id: &str| {
        host.prompts
            .iter()
            .find(|(launched, _)| launched == id)
            .map(|(_, prompt)| prompt.clone())
            .unwrap_or_else(|| panic!("no {id}"))
    };
    for round in ["ci--repair--r1--fix", "ci--repair--r2--fix"] {
        let prompt = brief(round);
        assert!(
            prompt.contains("write remaining: [\"Wire the docs table.\"]"),
            "{round}: {prompt}"
        );
    }
    let second = brief("ci--repair--r2--fix");
    assert!(
        second.contains("ci--repair--r1--fix remaining: [\"Left by ci--repair--r1--fix.\"]"),
        "{second}"
    );
    assert!(second.contains("second red"), "{second}");
    assert!(
        second.contains("Repaired in ci--repair--r1--fix."),
        "{second}"
    );
}

#[test]
fn repository_layers_never_lower_the_land_tier_or_add_a_land() {
    let temp = tempfile::tempdir().unwrap();
    let (user, repo) = (temp.path().join("user"), temp.path().join("repo"));
    std::fs::create_dir(&user).unwrap();
    std::fs::create_dir(&repo).unwrap();
    let tier = |overrides: &[&str]| {
        let mut all = vec!["parameters.worktree='/repo/lane'".to_owned()];
        all.extend(overrides.iter().map(|item| (*item).to_owned()));
        let layers = Layers::load(&user, &repo).unwrap();
        let expanded = expand(&layers, "deliver", "approved brief", &all).unwrap();
        let nodes = expanded.document["nodes"].as_array().unwrap().clone();
        nodes
            .iter()
            .find(|node| node["id"] == "land")
            .map(|node| node["land"]["tier"].as_str().unwrap().to_owned())
    };
    std::fs::write(
        user.join("deliver.toml"),
        "[parameters]\nland_tier = 'crucial'\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("deliver.toml"),
        "[parameters]\nland_tier = 'routine'\n",
    )
    .unwrap();
    assert_eq!(tier(&[]).as_deref(), Some("crucial"));
    // An explicit command-line value still applies.
    assert_eq!(
        tier(&["parameters.land_tier='routine'"]).as_deref(),
        Some("routine")
    );
    // A land the user configuration turned off takes the strictest tier.
    std::fs::write(user.join("deliver.toml"), "[parameters]\nland = 0\n").unwrap();
    std::fs::write(repo.join("deliver.toml"), "[parameters]\nland = 1\n").unwrap();
    assert_eq!(tier(&[]).as_deref(), Some("terminal"));
    // A stricter repository tier stands.
    std::fs::remove_file(user.join("deliver.toml")).unwrap();
    std::fs::write(
        repo.join("deliver.toml"),
        "[parameters]\nland_tier = 'crucial'\n",
    )
    .unwrap();
    assert_eq!(tier(&[]).as_deref(), Some("crucial"));
}

#[test]
fn repository_roles_never_lower_a_custom_land_role() {
    let temp = tempfile::tempdir().unwrap();
    let (user, repo) = (temp.path().join("user"), temp.path().join("repo"));
    std::fs::create_dir_all(user.join("presets")).unwrap();
    std::fs::create_dir_all(repo.join("presets")).unwrap();
    std::fs::write(
        user.join("presets/ship-flow.toml"),
        "stages = [\n  { id = \"write\", tier = \"senior\", access = \"worktree_write\", worktree = \"/repo/lane\", delivery = \"pr\", prompt = \"${prompt}\" },\n  { id = \"ship\", role = \"ship\", depends_on = [\"write\"] },\n]\n",
    )
    .unwrap();
    std::fs::write(
        user.join("roles.toml"),
        "[roles.ship]\nkind = \"land\"\nland = { node = \"write\", tier = \"crucial\" }\n",
    )
    .unwrap();
    let tier = || {
        let layers = Layers::load(&user.join("presets"), &repo.join("presets")).unwrap();
        let expanded = expand(&layers, "ship-flow", "approved brief", &[]).unwrap();
        expanded.document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "ship")
            .map(|node| node["land"]["tier"].as_str().unwrap().to_owned())
    };
    assert_eq!(tier().as_deref(), Some("crucial"));
    // A repository role override, with no repository preset, cannot lower it.
    std::fs::write(
        repo.join("roles.toml"),
        "[roles.ship]\nland = { node = \"write\", tier = \"routine\" }\n",
    )
    .unwrap();
    assert_eq!(tier().as_deref(), Some("crucial"));
}

#[test]
fn big_batch_lanes_deliver_then_the_council_gates_the_principal_and_repairer() {
    use provenant_records::run::DeliveryMode;
    let integrate = DeliveryMode::Integrate("batch/graph".into());
    let lanes = "parameters.lanes=[{name='a',worktree='/repo/a',scope=['x'],spec='#1'},{name='b',worktree='/repo/b',scope=['y'],spec='#2'}]";
    let expanded = expand(
        &Layers::empty(),
        "big-batch",
        "the batch",
        &[
            lanes.into(),
            "parameters.batch_worktree='/repo/batch'".into(),
            "parameters.external=0".into(),
            "parameters.width=1".into(),
            format!("parameters.base='{BASE}'"),
        ],
    )
    .unwrap();
    // Writers start from the commit the host resolves `HEAD` to.
    fn pin(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if map.get("type") == Some(&json!("commit"))
                    && map.get("commit") == Some(&json!("HEAD"))
                {
                    map.insert("commit".into(), json!(BASE));
                }
                map.values_mut().for_each(pin);
            }
            Value::Array(values) => values.iter_mut().for_each(pin),
            _ => {}
        }
    }
    let mut value = expanded.document;
    pin(&mut value);
    let document: GraphDocument = serde_json::from_value(value).unwrap();
    let mut host = Host {
        integration: Some(BASE.into()),
        // Round one's review rejects, so a second repair round runs.
        failing: vec!["repair--r1--review".into()],
        ..Host::default()
    };
    let mut journal = Journal::default();
    let run = drive(&document, &mut host, &mut journal);
    assert_eq!(run.status, GraphStatus::Complete, "{:?}", run.checkpoint);
    let position = |id: &str| {
        host.launched
            .iter()
            .position(|launched| launched == id)
            .unwrap_or_else(|| panic!("{id} never launched: {:?}", host.launched))
    };
    let delivery = |id: &str| {
        host.deliveries
            .iter()
            .find(|(node, ..)| node == id)
            .unwrap_or_else(|| panic!("no launch of {id}"))
            .clone()
    };
    // Lanes deliver into the integration branch and may merge there.
    let lane_writers: Vec<&str> = host
        .launched
        .iter()
        .map(String::as_str)
        .filter(|id| id.starts_with("lanes") && id.ends_with("--implement"))
        .collect();
    assert_eq!(lane_writers.len(), 2, "{:?}", host.launched);
    for writer in &lane_writers {
        let (_, mode, fast_forward) = delivery(writer);
        assert_eq!(
            (mode, fast_forward),
            (Some(integrate.clone()), false),
            "{writer}"
        );
    }
    // The council reviews only after every lane has integrated; the principal
    // follows the council and the repair rounds follow the principal.
    let council: Vec<usize> = host
        .launched
        .iter()
        .enumerate()
        .filter(|(_, id)| id.starts_with("council"))
        .map(|(index, _)| index)
        .collect();
    assert!(!council.is_empty(), "{:?}", host.launched);
    let last_lane = lane_writers.iter().map(|id| position(id)).max().unwrap();
    assert!(council.iter().all(|index| *index > last_lane));
    let (refactor, first, second) = (
        position("refactor"),
        position("repair--r1--fix"),
        position("repair--r2--fix"),
    );
    assert!(council.iter().all(|index| *index < refactor));
    assert!(refactor < first && first < second, "{:?}", host.launched);
    let (_, mode, fast_forward) = delivery("refactor");
    assert_eq!((mode, fast_forward), (Some(integrate.clone()), false));
    // Repair rounds integrate by fast-forward only.
    for round in ["repair--r1--fix", "repair--r2--fix"] {
        let (_, mode, fast_forward) = delivery(round);
        assert_eq!(
            (mode, fast_forward),
            (Some(integrate.clone()), true),
            "{round}"
        );
    }
    // The principal starts from the council's pinned combined head.
    let source = |id: &str| {
        host.sources
            .iter()
            .find(|(node, _)| node == id)
            .and_then(|(_, source)| source.clone())
    };
    let combined = progress(&run.checkpoint, "refactor").source_commit.clone();
    assert_eq!(source("refactor"), combined);
    assert!(combined.is_some());
    // Each repair round continues the worktree and descends from the head the
    // previous delivery left on the integration branch, never a stale pin.
    assert_eq!(source("repair--r1--fix"), None);
    assert_eq!(source("repair--r2--fix"), None);
    let head = |id: &str| {
        progress(&run.checkpoint, id)
            .outputs
            .as_ref()
            .unwrap()
            .commit
            .clone()
    };
    let (principal, round_one, round_two) = (
        head("refactor"),
        head("repair--r1--fix"),
        head("repair--r2--fix"),
    );
    assert!(principal.is_some() && round_one.is_some() && round_two.is_some());
    assert_ne!(principal, round_one);
    assert_ne!(round_one, round_two);
    assert_eq!(host.integration, round_two);
}
