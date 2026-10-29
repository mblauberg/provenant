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
        _: &GraphRoute,
        access: AccessMode,
        _: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.launched.push(key.node_id.clone());
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
            format!(
                "Checked {}.\n```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\nVERDICT: PASS\n",
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
            Some(commit)
        } else {
            node.source_commit.map(str::to_owned)
        };
        Ok(AttemptObservation::Terminal {
            attempt_ref: Some(format!("fixture/{}", key.node_id)),
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
    // asks the wrapper's chair instead of failing for want of a session.
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
