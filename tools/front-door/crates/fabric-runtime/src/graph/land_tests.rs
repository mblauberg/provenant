use super::*;
use crate::config::{LandChoice, LandPolicy, LandSource};
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;

const WRITE_HEAD: &str = "1111111111111111111111111111111111111111";
const REPAIR_HEAD: &str = "2222222222222222222222222222222222222222";

#[derive(Default)]
struct Journal(Option<GraphCheckpoint>);
impl GraphJournal for Journal {
    type Error = Infallible;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.0.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.0 = Some(checkpoint.clone());
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct Launch {
    node: String,
    worktree: Option<String>,
    delivery: Option<provenant_records::run::DeliveryMode>,
    branch: Option<String>,
}

#[derive(Default)]
struct Owner {
    launches: Vec<Launch>,
    lands: Vec<(String, String, LandAuthority)>,
    outcomes: VecDeque<LandObservation>,
    policy: Option<LandChoice>,
    reply: Option<String>,
    asked: usize,
    writer_branch: Option<String>,
    /// The newest commit a writer delivered, which CI reports as the head.
    head: Option<String>,
}
impl CiStatus for Owner {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: "boot".into(),
            host_monotonic_micros: 1,
        })
    }
    fn observe_ci(&mut self, _: &str, _: &[String], _: usize, _: usize) -> CiObservation {
        CiObservation {
            extra: Default::default(),
            state: CiState::Passed,
            summary: "checks passed".into(),
            head_sha: self.head.clone(),
            failed_checks: Vec::new(),
            check_names: Vec::new(),
            truncated: false,
        }
    }
}
impl AttemptOwner for Owner {
    type Error = Infallible;
    fn actor(&self) -> String {
        "fixture".into()
    }
    fn ci_status(&mut self) -> Option<&mut dyn CiStatus> {
        Some(self)
    }
    fn node_ask(&mut self, _: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        self.asked += 1;
        Ok(self.reply.clone().map(|option| AskReply {
            option: Some(option),
            text: None,
            actor: "chair".into(),
            at: None,
        }))
    }
    fn land_policy(&self, tier: provenant_records::workflow::RiskTier) -> LandChoice {
        self.policy
            .unwrap_or_else(|| crate::config::GraphLand::default().choose(None, tier))
    }
    fn land(&mut self, request: LandRequest<'_>) -> Result<LandObservation, Self::Error> {
        self.lands.push((
            request.branch.into(),
            request.head.into(),
            request.authority.clone(),
        ));
        Ok(self
            .outcomes
            .pop_front()
            .unwrap_or(LandObservation::Merged {
                pull_request: Some(7),
                note: None,
            }))
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        _: NodeInput<'_>,
        _: &GraphRoute,
        _: AccessMode,
        worktree: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.launches.push(Launch {
            node: key.node_id.clone(),
            worktree: worktree.map(str::to_owned),
            delivery: replay.delivery.cloned(),
            branch: replay.branch.map(str::to_owned),
        });
        let (result, branch, commit) = match key.node_id.as_str() {
            "write" => (None, self.writer_branch.clone(), Some(WRITE_HEAD)),
            "repair" => (None, replay.branch.map(str::to_owned), Some(REPAIR_HEAD)),
            _ => {
                let mut capture = crate::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
                capture.push(b"looks right\nVERDICT: PASS\n");
                (Some(capture.finish()), None, None)
            }
        };
        if let Some(commit) = commit {
            self.head = Some(commit.into());
        }
        Ok(AttemptObservation::Terminal {
            attempt_ref: None,
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result,
            result_warning: None,
            result_path: None,
            branch,
            commit: commit.map(str::to_owned),
        })
    }
}

fn owner() -> Owner {
    Owner {
        writer_branch: Some("feature/x".into()),
        ..Default::default()
    }
}

fn nodes(tier: &str) -> serde_json::Value {
    json!([
        {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
         "prompt":"write it", "route":{"alias":"senior"}},
        {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["write"],
         "condition":{"type":"ci", "target":"feature/x"}},
        {"id":"repair", "access":"worktree_write", "worktree":"/wt/feature", "continues":"write",
         "prompt":"fix the findings", "depends_on":["ci"], "route":{"alias":"senior"}},
        {"id":"review", "access":"read_only", "prompt":"review", "depends_on":["repair"],
         "route":{"alias":"senior"}},
        {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["review"],
         "inputs":[{"node":"review", "outputs":["verdict"]}],
         "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}},
        {"id":"land", "kind":"land", "access":"read_only", "depends_on":["accepted"],
         "land":{"node":"write", "tier":tier}}
    ])
}

fn document(tier: &str, session: Option<&str>) -> GraphDocument {
    let mut value = json!({"schema_version":1, "graph_id":"g-land", "nodes":nodes(tier)});
    if let Some(session) = session {
        value["decision_session"] = session.into();
    }
    serde_json::from_value(value).unwrap()
}

fn run(document: &GraphDocument, owner: &mut Owner, journal: &mut Journal) -> GraphCheckpoint {
    let mut last = None;
    for _ in 0..12 {
        let checkpoint = advance(document, owner, journal).unwrap().checkpoint;
        if last.as_ref() == Some(&checkpoint) {
            break;
        }
        last = Some(checkpoint);
    }
    last.unwrap()
}

fn node<'a>(checkpoint: &'a GraphCheckpoint, id: &str) -> &'a NodeProgress {
    checkpoint.nodes.iter().find(|node| node.id == id).unwrap()
}

#[test]
fn repair_reuses_the_writer_worktree_and_pushes_only_to_its_branch() {
    let mut owner = owner();
    let checkpoint = run(
        &document("routine", None),
        &mut owner,
        &mut Journal::default(),
    );
    use provenant_records::run::DeliveryMode;
    assert_eq!(
        owner.launches[0],
        Launch {
            node: "write".into(),
            worktree: Some("/wt/feature".into()),
            delivery: Some(DeliveryMode::Pr),
            branch: None,
        }
    );
    assert_eq!(
        owner.launches[1],
        Launch {
            node: "repair".into(),
            worktree: Some("/wt/feature".into()),
            delivery: Some(DeliveryMode::Push),
            branch: Some("feature/x".into()),
        }
    );
    let repair = node(&checkpoint, "repair").outputs.as_ref().unwrap();
    assert_eq!(repair.branch.as_deref(), Some("feature/x"));
    assert_eq!(node(&checkpoint, "land").state, NodeState::Succeeded);
}

#[test]
fn a_repair_without_a_delivered_branch_fails_before_launch() {
    let mut owner = Owner::default();
    let checkpoint = run(
        &document("routine", None),
        &mut owner,
        &mut Journal::default(),
    );
    let repair = node(&checkpoint, "repair");
    assert_eq!(repair.state, NodeState::Failed);
    assert!(repair.attempts.is_empty());
    assert!(
        repair
            .advance_error
            .as_deref()
            .unwrap()
            .contains("recorded no delivery branch")
    );
    assert_eq!(owner.launches.len(), 1);
}

#[test]
fn an_auto_tier_lands_the_newest_delivered_head_through_the_host() {
    let mut owner = owner();
    let checkpoint = run(
        &document("substantial", None),
        &mut owner,
        &mut Journal::default(),
    );
    assert_eq!(
        owner.lands,
        [(
            "feature/x".to_owned(),
            REPAIR_HEAD.to_owned(),
            LandAuthority::Policy
        )]
    );
    // The gate bound the writer's head; the repair is newer, so the host's
    // own land path checks CI again at the pinned head.
    let ci = node(&checkpoint, "ci").ci.as_ref().unwrap();
    assert_eq!(ci.head_sha.as_deref(), Some(WRITE_HEAD));
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Succeeded);
    let record = land.land.as_ref().unwrap();
    assert_eq!(
        (record.policy, record.policy_source, record.pull_request),
        (LandPolicy::Auto, LandSource::Default, Some(7))
    );
    let outputs = land.outputs.as_ref().unwrap();
    assert_eq!(outputs.verdict.as_deref(), Some(land::LANDED));
    assert_eq!(outputs.commit.as_deref(), Some(REPAIR_HEAD));
    let excerpt = &outputs.result.as_ref().unwrap().excerpt;
    assert!(excerpt.contains("#7") && excerpt.contains("policy is auto (default)"));
    assert!(checkpoint.asks.is_empty());
}

#[test]
fn a_crucial_tier_raises_a_land_ask_and_lands_on_its_answer() {
    let document = document("crucial", Some("chair"));
    let mut owner = owner();
    let mut journal = Journal::default();
    let checkpoint = run(&document, &mut owner, &mut journal);
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Running);
    assert!(owner.lands.is_empty());
    let ask = &checkpoint.asks[0];
    assert_eq!(
        (ask.ask_id.as_str(), ask.kind, ask.session.as_str()),
        ("land.ask-1", AskKind::Land, "chair")
    );
    let options: Vec<_> = ask.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(options, ["land", "hold"]);
    assert!(ask.body.contains(REPAIR_HEAD) && ask.body.contains("crucial tier asks first"));

    owner.reply = Some("land".into());
    let checkpoint = run(&document, &mut owner, &mut journal);
    let authority = LandAuthority::Ask {
        ask_id: "land.ask-1".into(),
        actor: "chair".into(),
    };
    assert_eq!(
        owner.lands,
        [(
            "feature/x".to_owned(),
            REPAIR_HEAD.to_owned(),
            authority.clone()
        )]
    );
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Succeeded);
    assert_eq!(land.land.as_ref().unwrap().authorised_by, Some(authority));
    assert!(
        land.outputs
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .excerpt
            .contains("chair answered land to land.ask-1")
    );
    assert!(checkpoint.asks[0].resolved);
}

#[test]
fn a_hold_answer_fails_the_land_without_merging() {
    let mut owner = owner();
    owner.reply = Some("hold".into());
    let checkpoint = run(
        &document("crucial", Some("chair")),
        &mut owner,
        &mut Journal::default(),
    );
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Failed);
    assert!(
        land.advance_error
            .as_deref()
            .unwrap()
            .contains("held by chair")
    );
    assert!(owner.lands.is_empty());
}

#[test]
fn a_configured_policy_overrides_the_default_and_records_its_source() {
    let mut owner = owner();
    owner.policy = Some(LandChoice {
        policy: LandPolicy::Ask,
        source: LandSource::Project,
    });
    let checkpoint = run(
        &document("routine", Some("chair")),
        &mut owner,
        &mut Journal::default(),
    );
    let record = node(&checkpoint, "land").land.as_ref().unwrap();
    assert_eq!(
        (
            record.policy,
            record.policy_source,
            record.authorised_by.as_ref()
        ),
        (LandPolicy::Ask, LandSource::Project, None)
    );
    assert!(checkpoint.asks[0].body.contains("project config"));
    assert!(owner.lands.is_empty());
}

#[test]
fn an_ask_tier_without_a_session_fails_with_guidance() {
    let mut owner = owner();
    let checkpoint = run(
        &document("crucial", None),
        &mut owner,
        &mut Journal::default(),
    );
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Failed);
    let error = land.advance_error.as_deref().unwrap();
    assert!(error.contains("decision session") && error.contains("provenant land"));
    assert!(owner.lands.is_empty() && checkpoint.asks.is_empty());
}

#[test]
fn a_pending_land_is_observed_again_and_a_refusal_fails_the_node() {
    let mut owner = owner();
    owner.outcomes = VecDeque::from([
        LandObservation::Pending {
            reason: "checks running".into(),
        },
        LandObservation::Refused {
            reason: "HEAD_MOVED".into(),
        },
    ]);
    let document = document("routine", None);
    let mut journal = Journal::default();
    let mut checkpoint = run(&document, &mut owner, &mut journal);
    for _ in 0..4 {
        if node(&checkpoint, "land").state != NodeState::Running {
            break;
        }
        assert_eq!(
            node(&checkpoint, "land")
                .land
                .as_ref()
                .unwrap()
                .note
                .as_deref(),
            Some("checks running")
        );
        checkpoint = advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint;
    }
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Failed);
    assert!(
        land.advance_error
            .as_deref()
            .unwrap()
            .contains("land refused: HEAD_MOVED")
    );
    assert_eq!(owner.lands.len(), 2);
    assert!(owner.lands.iter().all(|(_, head, _)| head == REPAIR_HEAD));
}

fn refusal(value: serde_json::Value) -> String {
    let document: GraphDocument = serde_json::from_value(value).unwrap();
    match advance(&document, &mut owner(), &mut Journal::default()) {
        Err(GraphError::Invalid(reason)) => reason,
        other => panic!("expected refusal, got {:?}", other.map(|run| run.status)),
    }
}

#[test]
fn continuation_and_land_wiring_is_checked_before_launch() {
    for (edit, reason) in [
        (
            Box::new(|nodes: &mut serde_json::Value| nodes[2]["worktree"] = "/wt/other".into())
                as Box<dyn FnOnce(&mut serde_json::Value)>,
            "same worktree",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| nodes[2]["delivery"] = "push".into()),
            "no delivery of its own",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| {
                nodes[2]["depends_on"] = json!([]);
            }),
            "upstream writer",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| nodes[0]["delivery"] = "none".into()),
            "upstream writer",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| nodes[5]["land"]["node"] = "repair".into()),
            "writer that delivers pr",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| nodes[5]["depends_on"] = json!(["ci"])),
            "verdict gate and a CI gate",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| nodes[5]["prompt"] = "merge".into()),
            "no agent configuration",
        ),
        (
            Box::new(|nodes: &mut serde_json::Value| {
                nodes[5]["land"]["tier"] = "urgent".into();
            }),
            "",
        ),
    ] {
        let mut value = json!({"schema_version":1, "graph_id":"g-land", "nodes":nodes("routine")});
        edit(&mut value["nodes"]);
        if reason.is_empty() {
            assert!(serde_json::from_value::<GraphDocument>(value).is_err());
            continue;
        }
        let refused = refusal(value);
        assert!(refused.contains(reason), "{refused} lacks {reason}");
    }
}

#[test]
fn a_loop_body_cannot_land() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1, "graph_id":"g-land", "nodes":[
            {"id":"rounds", "kind":"loop", "access":"read_only",
             "loop":{"max_rounds":2, "until":"done", "body":[
                {"id":"land", "kind":"land", "access":"read_only",
                 "land":{"node":"write", "tier":"routine"}},
                {"id":"done", "kind":"gate", "access":"read_only", "depends_on":["land"],
                 "condition":{"type":"budget"}}
             ]}}
        ]
    }))
    .unwrap();
    let reason = refusal(serde_json::to_value(document).unwrap());
    assert!(reason.contains("cannot land"), "{reason}");
}

#[test]
fn land_records_keep_unknown_fields() {
    let value = json!({"tier":"crucial", "policy":"ask", "policy_source":"project",
        "branch":"feature/x", "head":REPAIR_HEAD,
        "authorised_by":{"type":"ask", "ask_id":"land.ask-1", "actor":"chair"},
        "receipt":"future"});
    let record: LandRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), value);
}
