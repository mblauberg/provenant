use super::*;

fn publication_branch() -> String {
    format!("provenant/graph/{}", "a".repeat(64))
}
use crate::config::{LandChoice, LandPolicy};
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;

const WRITE_HEAD: &str = "1111111111111111111111111111111111111111";
const REPAIR_HEAD: &str = "2222222222222222222222222222222222222222";
const REBASE_HEAD: &str = "5555555555555555555555555555555555555555";
const REPUBLISHED_HEAD: &str = "6666666666666666666666666666666666666666";
/// The commit the review read the writer's head against.
const REVIEW_BASE: &str = "3333333333333333333333333333333333333333";
/// The fetched land target, one commit past `REVIEW_BASE`.
const TARGET: &str = "4444444444444444444444444444444444444444";

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

#[derive(Clone, Debug, PartialEq)]
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
    land_requests: Vec<(String, String, Option<u64>)>,
    /// Observe-only requests while a tier asks: branch and head.
    observed: Vec<(String, String)>,
    /// Whether the chair has merged by hand, which observation reports.
    chair_merged: bool,
    /// External merge state survives a failed graph checkpoint save.
    merged_by_policy: bool,
    merge_requests: usize,
    outcomes: VecDeque<LandObservation>,
    policy: Option<LandChoice>,
    reply: Option<String>,
    asked: usize,
    /// Every request filed, in order.
    filed: Vec<AskRequest>,
    /// Writer attempt references excluded from each launched node.
    distinctions: Vec<(String, Vec<String>)>,
    writer_branch: Option<String>,
    /// The newest commit a writer delivered, which CI reports as the head.
    head: Option<String>,
    /// The verdict line reviewers end with; PASS when unset.
    review_verdict: Option<&'static str>,
    equal_review_range: bool,
    pending_writer: bool,
    wall_seconds: i64,
    no_wall_clock: bool,
    tick: bool,
    backoff: CiPollBackoff,
    repair_fixture: bool,
    repair_prompts: Vec<String>,
    repair_families: std::collections::BTreeMap<String, String>,
    repair_alternative: bool,
    /// The repository history that proves review carry-over against `TARGET`.
    history: Option<provenant_core::review_cover::history::History>,
    integration_base: Option<provenant_core::GitOid>,
    conflict_state: Option<ConflictState>,
    conflict_head: Option<String>,
    conflict_states: VecDeque<ConflictState>,
    conflict_heads: VecDeque<Option<String>>,
    ci_head: Option<String>,
    ci_states: VecDeque<CiState>,
    hygiene_after_heads: VecDeque<String>,
    hygiene_observations: Vec<(String, String, String)>,
    publication_heads: Vec<String>,
    gate_observations: Vec<(String, String)>,
}
impl CiStatus for Owner {
    fn host_stamp(&self) -> Option<CiHostStamp> {
        Some(CiHostStamp {
            extra: Default::default(),
            boot_identity: "boot".into(),
            host_monotonic_micros: 1,
        })
    }
    fn observe_ci(
        &mut self,
        target: &str,
        _: &[String],
        _: bool,
        _: usize,
        _: usize,
    ) -> CiObservation {
        self.gate_observations.push(("ci".into(), target.into()));
        CiObservation {
            quota_reset_at: None,
            extra: Default::default(),
            state: self.ci_states.pop_front().unwrap_or(CiState::Passed),
            summary: "checks passed".into(),
            head_sha: self.ci_head.clone().or_else(|| self.head.clone()),
            failed_checks: Vec::new(),
            check_names: vec!["build".into()],
            truncated: false,
        }
    }
    fn observe_conflict(&mut self, target: &str) -> ConflictObservation {
        self.gate_observations
            .push(("conflict".into(), target.into()));
        ConflictObservation {
            state: self
                .conflict_states
                .pop_front()
                .or(self.conflict_state)
                .unwrap_or(ConflictState::Clear),
            head_sha: self
                .conflict_heads
                .pop_front()
                .unwrap_or_else(|| self.conflict_head.clone().or_else(|| self.head.clone())),
            summary: "published PR conflict state".into(),
        }
    }
}
impl AttemptOwner for Owner {
    type Error = Infallible;
    fn worktree_layout(&self) -> provenant_config::WorktreeLayoutPort {
        provenant_config::PortTable::builtin().worktree_layout
    }
    fn attempt_families(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        let mut families = Vec::new();
        for family in attempts
            .iter()
            .filter_map(|attempt| self.repair_families.get(attempt))
        {
            if !families.contains(family) {
                families.push(family.clone());
            }
        }
        Ok(families)
    }
    fn review_range(&mut self, _: &str, _: bool) -> Result<ReviewRange, Self::Error> {
        Ok(if self.equal_review_range {
            ReviewRange::Resolved {
                base: WRITE_HEAD.into(),
                head: WRITE_HEAD.into(),
            }
        } else {
            ReviewRange::Unknown
        })
    }
    fn actor(&self) -> String {
        "fixture".into()
    }
    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        let base = self.history.as_ref().map(|_| REVIEW_BASE.parse().unwrap());
        (base, self.integration_base)
    }
    fn bind_writer_worktree(
        &mut self,
        worktree: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        Ok(Some(WriterWorktreeBinding {
            extra: Default::default(),
            source_directory_identity: None,
            shared_worktree: None,
            worktree: worktree.to_owned(),
            head: self.head.clone().unwrap_or_else(|| WRITE_HEAD.into()),
        }))
    }
    fn run_hygiene(
        &mut self,
        request: HygieneRequest<'_>,
    ) -> Result<HygieneExecution, Self::Error> {
        let after_head = self
            .hygiene_after_heads
            .pop_front()
            .unwrap_or_else(|| REPAIR_HEAD.into());
        self.hygiene_observations.push((
            request.node_id.to_owned(),
            request.input_head.to_owned(),
            after_head.clone(),
        ));
        Ok(HygieneExecution {
            outcome: provenant_records::graph::HygieneOutcome {
                kind: provenant_records::graph::HygieneOutcomeKind::Clean,
                before_head: Some(request.input_head.to_owned()),
                after_head: Some(after_head),
                handoff: Some("formatter completed".into()),
            },
            branch: self.writer_branch.clone(),
            formatter_source: Some("fixture".into()),
            diagnostic: None,
        })
    }
    fn publish_delivery(
        &mut self,
        request: PublicationRequest<'_>,
    ) -> Result<Option<DeliveryPublication>, Self::Error> {
        self.head = Some(request.expected_head.to_owned());
        self.publication_heads
            .push(request.expected_head.to_owned());
        Ok(Some(DeliveryPublication {
            branch: publication_branch(),
            head: request.expected_head.to_owned(),
            pull_request: "https://example.invalid/pr/7".into(),
        }))
    }
    fn review_coverage(
        &mut self,
        _branch: &str,
        reviewed: provenant_core::review_cover::Reviewed<'_>,
        head: &str,
    ) -> Result<Option<provenant_core::review_cover::Coverage>, Self::Error> {
        Ok(self.history.as_mut().map(|history| {
            let Ok(coverage) =
                provenant_core::review_cover::coverage(history, reviewed, head, TARGET);
            coverage
        }))
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        if self.no_wall_clock {
            None
        } else {
            provenant_core::WallTime::from_micros(self.wall_seconds.saturating_mul(1_000_000)).ok()
        }
    }
    fn respect_ci_wake(&self) -> bool {
        self.tick
    }
    fn ci_poll_backoff(&self, _: &str) -> CiPollBackoff {
        self.backoff
    }
    fn ci_status(&mut self) -> Option<&mut dyn CiStatus> {
        Some(self)
    }
    fn file_ask(&mut self, filing: AskFiling<'_>) -> Result<AskFiled, Self::Error> {
        if !self.filed.iter().any(|ask| ask.ask_id == filing.ask.ask_id) {
            self.filed.push(filing.ask.clone());
        }
        Ok(AskFiled {
            request: filing.ask.ask_id.clone(),
            deadline: filing.ask.wake_at,
            closed: false,
        })
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
        self.land_requests.push((
            request.branch.to_owned(),
            request.head.to_owned(),
            request.pull_request,
        ));
        if request.pull_request.is_none() {
            return Ok(LandObservation::Identified { pull_request: 7 });
        }
        assert_eq!(request.pull_request, Some(7));
        let Some(authority) = request.authority else {
            self.observed
                .push((request.branch.into(), request.head.into()));
            return Ok(if self.chair_merged {
                LandObservation::Merged {
                    pull_request: Some(7),
                    note: None,
                }
            } else {
                LandObservation::Pending {
                    reason: "not merged yet".into(),
                }
            });
        };
        self.lands.push((
            request.branch.into(),
            request.head.into(),
            authority.clone(),
        ));
        if self.merged_by_policy {
            return Ok(LandObservation::Merged {
                pull_request: Some(7),
                note: None,
            });
        }
        self.merge_requests += 1;
        let observation = self
            .outcomes
            .pop_front()
            .unwrap_or(LandObservation::Merged {
                pull_request: Some(7),
                note: None,
            });
        if matches!(observation, LandObservation::Merged { .. }) {
            self.merged_by_policy = true;
        }
        Ok(observation)
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        input: NodeInput<'_>,
        _: &GraphRoute,
        access: AccessMode,
        worktree: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        if self.repair_fixture {
            self.repair_prompts.push(input.prompt.to_owned());
        }
        self.distinctions
            .push((key.node_id.clone(), input.distinct_from_attempts.to_vec()));
        if self.pending_writer && key.node_id == "write" {
            return Ok(AttemptObservation::Pending {
                attempt_ref: Some("pending-writer".into()),
            });
        }
        self.launches.push(Launch {
            node: key.node_id.clone(),
            worktree: worktree.map(str::to_owned),
            delivery: replay.delivery.cloned(),
            branch: replay.branch.map(str::to_owned),
        });
        let (result, branch, commit) = match key.node_id.as_str() {
            "write" => (None, self.writer_branch.clone(), Some(WRITE_HEAD)),
            "rebase" | "rebase--r1--fix" => {
                (None, replay.branch.map(str::to_owned), Some(REBASE_HEAD))
            }
            "repair" => (None, replay.branch.map(str::to_owned), Some(REPAIR_HEAD)),
            id if self.repair_fixture && id.ends_with("--fix") => {
                let mut capture = crate::result_capture::ResultCapture::new(None);
                capture.push(b"```findings\n{\"schema_version\":1,\"findings\":[],\"repair_items\":[{\"id\":\"input handling\",\"status\":\"fixed\",\"reason\":\"Removed the observed defect\"}]}\n```\n");
                (
                    Some(capture.finish()),
                    replay.branch.map(str::to_owned),
                    Some(REPAIR_HEAD),
                )
            }
            _ if self.repair_fixture
                && access == AccessMode::WorktreeWrite
                && replay.branch.is_none() =>
            {
                (None, self.writer_branch.clone(), Some(WRITE_HEAD))
            }
            _ => {
                let verdict = if self.repair_fixture
                    && (key.node_id == "initial--lens" || key.node_id == "delivery--review")
                {
                    "```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"a.rs\",\"line\":1,\"summary\":\"defect\",\"status\":\"open\",\"evidence\":\"counterexample\",\"proposed_fix\":\"repair\",\"trigger\":{\"sequence\":\"Default invocation\",\"reach\":\"default_path\",\"observed\":true},\"triage\":{\"decision\":\"confirmed\",\"reason\":\"Observed at the checked caller\",\"worth_fixing\":true,\"root_cause\":\"input handling\",\"blocking_class\":\"default_path_defect\"}}]}\n```\nVERDICT: FAIL"
                } else if self.repair_fixture
                    && (key.node_id == "initial--synthesis"
                        || key.node_id == "delivery--review--synthesis")
                {
                    "```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"disposition\":\"accepted\",\"reason\":\"confirmed\"}]}\n```\nVERDICT: FAIL"
                } else if self.repair_fixture
                    && key.node_id.starts_with("cycles--r")
                    && key.node_id.ends_with("--synthesis")
                {
                    "```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"a.rs\",\"line\":1,\"summary\":\"defect\",\"status\":\"fixed\",\"evidence\":\"counterexample\",\"proposed_fix\":\"repair\",\"trigger\":{\"sequence\":\"Default invocation\",\"reach\":\"default_path\",\"observed\":true},\"disposition\":\"rejected\",\"reason\":\"The accepted repair removes the reported defect from the reviewed code\",\"triage\":{\"decision\":\"confirmed\",\"reason\":\"The original defect was real and the checked repair closes it\",\"worth_fixing\":false,\"root_cause\":\"input handling\",\"blocking_class\":\"none\",\"shared_flaw\":false},\"design_defect\":false}]}\n```\nVERDICT: PASS"
                } else if self.repair_fixture {
                    "```findings\n{\"schema_version\":1,\"risk_tier\":\"substantial\",\"findings\":[]}\n```\nVERDICT: PASS"
                } else {
                    self.review_verdict.unwrap_or("VERDICT: PASS")
                };
                let mut capture = crate::result_capture::ResultCapture::new(None);
                capture.push(format!("looks right\n{verdict}\n").as_bytes());
                (Some(capture.finish()), None, None)
            }
        };
        if let Some(commit) = commit {
            self.head = Some(commit.into());
        }
        let attempt_ref = if self.repair_fixture {
            let reference = format!("fixture/{}", self.launches.len());
            let family = if self.repair_alternative
                && input.distinct_from_attempts.iter().any(|reference| {
                    self.repair_families
                        .get(reference)
                        .is_some_and(|family| family == "reviewer")
                }) {
                "writer"
            } else {
                "reviewer"
            };
            self.repair_families
                .insert(reference.clone(), family.into());
            Some(reference)
        } else {
            self.equal_review_range.then(|| "fixture-attempt".into())
        };
        Ok(AttemptObservation::Terminal {
            attempt_ref,
            outcome: AttemptOutcome::Succeeded,
            launched: true,
            result: result.map(Box::new),
            result_warning: None,
            result_path: None,
            branch,
            commit: commit.map(str::to_owned),
            settlement_reason: Some(provenant_records::run::SettlementReason::Completed),
            stop: None,
        })
    }
}

fn owner() -> Owner {
    Owner {
        writer_branch: Some("feature/x".into()),
        ..Default::default()
    }
}

#[test]
fn findings_panels_feed_repair_and_land_the_reviewed_head() {
    for (model_synthesis, alternative) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let library = template::TemplateLibrary::bundled();
        let repair = template::Compiler::new(&library).compile(&json!({
            "id":"cycles","use":"repair","after":["initial"],
            "inputs":[{"node":"initial","outputs":["findings","verdict"]}],
            "repeat":{"until":"accepted","max_rounds":3,"review_repair":true},
            "with":{"worktree":"/wt/feature","write_scope":["src"],"writer":"write","lenses":["correctness"]}
        })).unwrap();
        let mut members = vec![
            json!({"id":"lens","template":"review.correctness","review_lens":"correctness",
            "access":"read_only","prompt":"review","route":{"alias":"senior"},"require_findings":true}),
        ];
        let exported = if model_synthesis {
            members.push(json!({"id":"synthesis","kind":"synthesise","access":"read_only","prompt":"synthesise",
                "route":{"alias":"senior"},"depends_on":["lens"],"require_findings":true,
                "inputs":[{"node":"lens","outputs":["findings"]}]}));
            "synthesis"
        } else {
            "lens"
        };
        let initial: GraphNode = serde_json::from_value(
            json!({"id":"initial","kind":"composite","access":"read_only",
            "depends_on":["write"],"composite":{"nodes":members,"reduce_verdict":!model_synthesis,
            "exports":[{"node":exported,"outputs":["findings","verdict"]}]}}),
        )
        .unwrap();
        let mut document: GraphDocument = serde_json::from_value(json!({"schema_version":1,"graph_id":"repair-land",
            "context":{"risk_tier":"substantial"},"nodes":[
            {"id":"write","access":"worktree_write","worktree":"/wt/feature","delivery":"pr","prompt":"write","route":{"alias":"senior"}},
            {"id":"ci","kind":"gate","access":"read_only","depends_on":["cycles"],"condition":{"type":"ci","target":{"node":"write"}}},
            {"id":"review","access":"read_only","depends_on":["ci"],"source":{"type":"head","node":"write"},"prompt":"review","route":{"alias":"senior"}},
            {"id":"accept","kind":"gate","access":"read_only","depends_on":["review"],"inputs":[{"node":"review","outputs":["verdict"]}],"condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}},
            {"id":"land","kind":"land","access":"read_only","depends_on":["accept"],"land":{"node":"write","tier":"routine"}}
        ]})).unwrap();
        document.nodes.insert(1, initial);
        document.nodes.insert(2, repair);
        let mut owner = owner();
        owner.repair_fixture = true;
        owner.repair_alternative = alternative;
        let mut journal = Journal::default();
        let run = advance(&document, &mut owner, &mut journal).unwrap();
        assert!(
            run.checkpoint.successful(),
            "model={model_synthesis}: {:?}",
            run.checkpoint
                .nodes
                .iter()
                .map(|n| (&n.id, n.state, &n.advance_error))
                .collect::<Vec<_>>()
        );
        let fix = owner
            .launches
            .iter()
            .position(|launch| launch.node.ends_with("--fix"))
            .unwrap();
        let prompt = &owner.repair_prompts[fix];
        let repair_node = run
            .checkpoint
            .nodes
            .iter()
            .find(|n| n.id.ends_with("--fix"))
            .unwrap();
        let separation = repair_node.separation.as_ref().unwrap();
        assert_eq!(separation.excluded, ["reviewer"]);
        assert_eq!(
            separation.outcome,
            if alternative {
                SeparationOutcome::Separated
            } else {
                SeparationOutcome::Unavailable
            }
        );
        let warnings = &repair_node.outputs.as_ref().unwrap().warnings;
        assert_eq!(
            warnings.iter().any(|warning| warning
                .contains("No separate model family was available for the repair writer")),
            !alternative
        );
        for field in [
            "F1",
            "\"lens\":\"correctness\"",
            "\"severity\":\"high\"",
            "\"file\":\"a.rs\"",
            "\"line\":1",
            "\"evidence\":\"counterexample\"",
            "\"proposed_fix\":\"repair\"",
        ] {
            assert!(prompt.contains(field), "missing {field}: {prompt}");
        }
        assert!(
            owner
                .launches
                .iter()
                .any(|launch| launch.node.contains("--check--correctness"))
        );
        assert_eq!(owner.lands[0].1, REPAIR_HEAD);
        assert_eq!(
            node(&run.checkpoint, "review").source_commit.as_deref(),
            Some(REPAIR_HEAD)
        );
        assert_eq!(
            advance(&document, &mut owner, &mut journal)
                .unwrap()
                .checkpoint,
            run.checkpoint
        );
    }
}

#[test]
fn bundled_deliver_repairs_findings_before_landing() {
    let library = template::TemplateLibrary::bundled();
    let resolvers = std::collections::BTreeMap::from([
        ("review-frame".to_owned(), vec!["false".to_owned()]),
        ("review-verify".to_owned(), vec!["false".to_owned()]),
        ("review-synthesis".to_owned(), vec!["false".to_owned()]),
        ("review-verifiers".to_owned(), vec!["1".to_owned()]),
        ("review-panel".to_owned(), vec!["correctness".to_owned()]),
        ("review-advice-correctness".to_owned(), vec![String::new()]),
    ]);
    let delivery = template::Compiler {
        library: &library,
        resolvers: &resolvers,
    }
    .compile(&json!({"id":"delivery","use":"deliver",
            "with":{"worktree":"/wt/feature","write_scope":["src"]}}))
    .unwrap();
    let mut document: GraphDocument =
        serde_json::from_value(json!({"schema_version":2,"graph_id":"bundled-deliver",
        "context":{"risk_tier":"substantial"},"nodes":[]}))
        .unwrap();
    document.nodes.push(delivery);
    let mut owner = owner();
    owner.repair_fixture = true;
    owner.repair_alternative = true;
    owner.policy = Some(LandChoice {
        policy: LandPolicy::Auto,
        source: Layer::Default,
    });
    let mut journal = Journal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    assert!(
        run.checkpoint.successful(),
        "{:?}",
        run.checkpoint
            .nodes
            .iter()
            .map(|n| (&n.id, n.state, &n.advance_error))
            .collect::<Vec<_>>()
    );
    let fix = owner
        .launches
        .iter()
        .position(|launch| launch.node.ends_with("--fix"))
        .unwrap();
    for field in [
        "F1",
        "\"lens\":\"correctness\"",
        "\"severity\":\"high\"",
        "\"file\":\"a.rs\"",
        "\"line\":1",
        "\"evidence\":\"counterexample\"",
        "\"proposed_fix\":\"repair\"",
    ] {
        assert!(
            owner.repair_prompts[fix].contains(field),
            "missing {field} in {}",
            owner.repair_prompts[fix]
        );
    }
    assert!(
        owner
            .launches
            .iter()
            .any(|launch| launch.node.contains("--check--correctness"))
    );
    assert_eq!(owner.lands[0].1, REPAIR_HEAD);
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

fn queue_publish_document(tier: &str) -> GraphDocument {
    // The recovery loop is the join for the clean publication path and its
    // retry path. Landing waits on that settled result, not the entry verdict,
    // which may fail to activate the loop.
    let recovery_body = json!([
       {"id":"fix", "access":"worktree_write", "worktree":"/wt/feature",
        "prompt":"rebase the published branch", "route":{"alias":"senior"},
        "source":{"type":"node","node":"previous.verified_clean"},
        "inputs":[{"node":"previous.refresh","outputs":["branch","commit"]}]},
       {"id":"verified_clean", "kind":"hygiene", "access":"worktree_write",
        "worktree":"/wt/feature", "depends_on":["fix"],
        "source":{"type":"node","node":"fix"}},
       {"id":"refresh", "kind":"publish", "access":"worktree_write",
        "worktree":"/wt/feature", "depends_on":["verified_clean"],
        "source":{"type":"node","node":"verified_clean"}},
       {"id":"fresh_conflict", "kind":"gate", "access":"read_only",
        "depends_on":["refresh"],
        "condition":{"type":"conflict","target":{"node":"refresh"}}},
       {"id":"fresh_ci", "kind":"gate", "access":"read_only",
        "depends_on":["fresh_conflict"],
        "condition":{"type":"ci","target":{"node":"refresh"}}},
       {"id":"fresh_review", "access":"read_only",
        "prompt":"review the recovery publication",
        "source":{"type":"head","node":"refresh"},
        "depends_on":["refresh","fresh_conflict","fresh_ci"],
        "route":{"alias":"senior"}},
       {"id":"accepted", "kind":"gate", "access":"read_only",
        "depends_on":["fresh_review","fresh_ci"],
        "inputs":[{"node":"fresh_review","outputs":["verdict"]}],
        "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
    ]);
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":"g-land", "integration_branch":"delivery/item",
        "delivery":"pr", "nodes":[
            {"id":"write", "access":"worktree_write", "worktree":"/wt/feature",
             "prompt":"write it", "route":{"alias":"senior"}},
            {"id":"hygiene", "kind":"hygiene", "access":"worktree_write",
             "worktree":"/wt/feature", "depends_on":["write"],
             "source":{"type":"node","node":"write"}},
            {"id":"publish", "kind":"publish", "access":"worktree_write",
             "worktree":"/wt/feature", "depends_on":["hygiene"],
             "source":{"type":"node","node":"hygiene"}},
            {"id":"conflict", "kind":"gate", "access":"read_only",
             "depends_on":["publish"], "on_fail":"rebase",
             "condition":{"type":"conflict","target":{"node":"publish"}}},
            {"id":"ci", "kind":"gate", "access":"read_only",
             "depends_on":["conflict"], "on_fail":"rebase",
             "condition":{"type":"ci","target":{"node":"publish"}}},
            {"id":"rebase", "kind":"loop", "access":"read_only",
             "depends_on":["conflict","ci","publish","hygiene","accepted"],
             "loop":{"max_rounds":2,"until":"accepted",
              "retry_on":["verified_clean","fresh_conflict","fresh_ci","accepted"],
              "selected_publication":"refresh","initial_publication":"publish",
              "previous_bindings":{"refresh":"publish","verified_clean":"hygiene"},
              "body":recovery_body}},
            {"id":"review", "access":"read_only", "prompt":"review the published head",
             "review_base":REVIEW_BASE, "source":{"type":"head","node":"publish"},
             "distinct_from":"writer",
             "depends_on":["ci","publish"], "route":{"alias":"senior"}},
            {"id":"accepted", "kind":"gate", "access":"read_only",
             "depends_on":["review"], "on_fail":"rebase", "inputs":[{"node":"review","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}},
            {"id":"land", "kind":"land", "access":"read_only",
             "depends_on":["rebase"], "land":{"node":"rebase","tier":tier}}
        ]
    }))
    .unwrap();
    document.queue = Some(QueueKnobs::default());
    document
}

fn queue_conflict_recovery_document(recovery: serde_json::Value) -> GraphDocument {
    let mut document: GraphDocument = serde_json::from_value(json!({
        "schema_version":2, "graph_id":"g-conflict-recovery",
        "integration_branch":"delivery/item", "nodes":[
            {"id":"write", "access":"worktree_write", "worktree":"/wt/feature",
             "prompt":"write it", "write_scope":["src"], "route":{"alias":"senior"}},
            {"id":"hygiene", "kind":"hygiene", "access":"worktree_write",
             "worktree":"/wt/feature", "depends_on":["write"],
             "source":{"type":"node","node":"write"}},
            {"id":"publish", "kind":"publish", "access":"worktree_write",
             "worktree":"/wt/feature", "depends_on":["hygiene"],
             "source":{"type":"node","node":"hygiene"}},
            {"id":"conflict", "kind":"gate", "access":"read_only",
             "depends_on":["publish"], "on_fail":"rebase",
             "condition":{"type":"conflict","target":{"node":"publish"}}},
            recovery
        ]
    }))
    .unwrap();
    document.queue = Some(QueueKnobs::default());
    document
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
fn queue_publication_without_writer_delivery_supports_head_review_and_landing() {
    let mut document = queue_publish_document("routine");
    document.delivery = None;
    assert!(
        document
            .nodes
            .iter()
            .find(|node| node.id == "write")
            .unwrap()
            .delivery
            .is_none()
    );
    assert!(validate_plan(&document).is_ok());
    let mut owner = owner();
    owner.repair_fixture = true;
    owner.policy = Some(LandChoice {
        policy: LandPolicy::Auto,
        source: Layer::Default,
    });
    owner.integration_base = Some(TARGET.parse().unwrap());
    let checkpoint = run(&document, &mut owner, &mut Journal::default());

    assert!(checkpoint.successful(), "{:?}", checkpoint.nodes);
    assert!(checkpoint.delivered);
    assert_eq!(
        checkpoint.pull_request.as_deref(),
        Some("https://example.invalid/pr/7")
    );
    let publication = node(&checkpoint, "publish");
    let receipt = publication
        .outputs
        .as_ref()
        .unwrap()
        .publication
        .as_ref()
        .unwrap();
    assert_eq!(receipt.branch, publication_branch());
    assert_eq!(receipt.head, REPAIR_HEAD);
    let conflict = node(&checkpoint, "conflict").conflict.as_ref().unwrap();
    assert_eq!(conflict.state, ConflictState::Clear);
    assert_eq!(conflict.head_sha.as_deref(), Some(REPAIR_HEAD));
    let ci = node(&checkpoint, "ci");
    assert_eq!(
        ci.ci.as_ref().unwrap().head_sha.as_deref(),
        Some(REPAIR_HEAD)
    );
    assert_eq!(
        ci.outputs.as_ref().unwrap().commit.as_deref(),
        Some(REPAIR_HEAD)
    );
    assert_eq!(
        node(&checkpoint, "review").source_commit.as_deref(),
        Some(REPAIR_HEAD)
    );
    let review = node(&checkpoint, "review");
    assert_eq!(
        review.attempts[0]
            .input_manifest
            .as_ref()
            .unwrap()
            .source_producer
            .as_ref()
            .unwrap()
            .node_id,
        "write"
    );
    assert_eq!(
        review.attempts[0]
            .input_manifest
            .as_ref()
            .unwrap()
            .source_producer
            .as_ref(),
        node(&checkpoint, "publish")
            .delivery_publication
            .as_ref()
            .map(|publication| &publication.source_key)
    );
    assert_eq!(
        owner
            .launches
            .iter()
            .find(|launch| launch.node == "review")
            .unwrap()
            .worktree
            .as_deref(),
        Some("/wt/feature")
    );
    assert_eq!(
        owner
            .distinctions
            .iter()
            .find(|(node, _)| node == "review")
            .unwrap()
            .1,
        vec!["fixture/1".to_owned()]
    );

    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Succeeded);
    let record = land.land.as_ref().unwrap();
    assert_eq!(record.branch, receipt.branch);
    assert_eq!(record.head, receipt.head);
    assert_eq!(record.pull_request, Some(7));
    assert_eq!(
        owner.land_requests[0],
        (receipt.branch.clone(), receipt.head.clone(), None)
    );
    assert_eq!(
        owner.land_requests[1],
        (receipt.branch.clone(), receipt.head.clone(), Some(7))
    );
    assert_eq!(owner.lands[0].1, REPAIR_HEAD);
}

#[test]
fn queue_repair_writer_may_bind_the_published_head() {
    let mut document = queue_publish_document("routine");
    document.delivery = None;
    assert!(validate_plan(&document).is_ok());
    let mut head_bound = document.clone();
    head_bound
        .nodes
        .iter_mut()
        .find(|node| node.id == "rebase")
        .unwrap()
        .loop_body
        .as_mut()
        .unwrap()
        .body[0]
        .source = Some(SourceBinding::Head {
        node: "previous.refresh".into(),
    });
    head_bound
        .nodes
        .iter_mut()
        .find(|node| node.id == "rebase")
        .unwrap()
        .loop_body
        .as_mut()
        .unwrap()
        .previous_bindings
        .remove("verified_clean");
    assert!(validate_plan(&head_bound).is_ok());

    let mut wrong_worktree = document.clone();
    wrong_worktree
        .nodes
        .iter_mut()
        .find(|node| node.id == "rebase")
        .unwrap()
        .loop_body
        .as_mut()
        .unwrap()
        .body[0]
        .worktree = Some("/wt/unretained".into());
    assert!(validate_plan(&wrong_worktree).is_err());

    let mut legacy_writer_source = document;
    legacy_writer_source
        .nodes
        .iter_mut()
        .find(|node| node.id == "review")
        .unwrap()
        .source = Some(SourceBinding::Head {
        node: "write".into(),
    });
    assert!(validate_plan(&legacy_writer_source).is_err());
}

#[test]
fn queue_conflict_recovery_requires_the_published_head_and_retained_worktree() {
    let recovery_loop = |worktree: &str| {
        json!({
            "id":"rebase", "kind":"loop", "access":"read_only",
            "depends_on":["conflict","publish","hygiene"],
            "loop":{"max_rounds":2,"until":"accepted",
              "retry_on":["verified_clean","refresh_conflict","refresh_ci","accepted"],
              "selected_publication":"refresh","initial_publication":"publish",
              "previous_bindings":{"refresh":"publish","verified_clean":"hygiene"},
              "body":[
                {"id":"fix", "access":"worktree_write", "worktree":worktree,
                 "prompt":"rebase the retained published branch", "write_scope":["src"],
                 "route":{"alias":"senior"},
                 "source":{"type":"node","node":"previous.verified_clean"},
                 "inputs":[{"node":"previous.refresh","outputs":["branch","commit"]}]},
                {"id":"verified_clean", "kind":"hygiene", "access":"worktree_write",
                 "worktree":worktree, "depends_on":["fix"],
                 "source":{"type":"node","node":"fix"}},
                {"id":"refresh", "kind":"publish", "access":"worktree_write",
                 "worktree":worktree, "depends_on":["verified_clean"],
                 "source":{"type":"node","node":"verified_clean"}},
                {"id":"refresh_conflict", "kind":"gate", "access":"read_only",
                 "depends_on":["refresh"],
                 "condition":{"type":"conflict","target":{"node":"refresh"}}},
                {"id":"refresh_ci", "kind":"gate", "access":"read_only",
                 "depends_on":["refresh_conflict"],
                 "condition":{"type":"ci","target":{"node":"refresh"}}},
                {"id":"fresh_review", "access":"read_only", "prompt":"review recovery head",
                 "source":{"type":"head","node":"refresh"},
                 "depends_on":["refresh","refresh_conflict","refresh_ci"],
                 "route":{"alias":"senior"}},
                {"id":"accepted", "kind":"gate", "access":"read_only",
                 "depends_on":["fresh_review","refresh_ci"],
                 "inputs":[{"node":"fresh_review","outputs":["verdict"]}],
                 "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
              ]}
        })
    };

    let valid = queue_conflict_recovery_document(recovery_loop("/wt/feature"));
    assert!(validate_plan(&valid).is_ok());
    let expanded = loops::expand(&valid, &[], &crate::config::GraphLimits::default()).unwrap();
    let (plan, positions) =
        validate_flat_plan(&expanded, &crate::config::GraphLimits::default()).unwrap();
    let rebase_writer = &plan.nodes[positions["rebase--r1--fix"]];
    assert!(matches!(
        &rebase_writer.source,
        Some(SourceBinding::Node { node }) if node == "hygiene"
    ));
    assert!(rebase_writer.inputs.iter().any(|input| {
        input.node == "publish"
            && input.outputs.contains(&OutputKind::Branch)
            && input.outputs.contains(&OutputKind::Commit)
    }));

    let different_worktree = queue_conflict_recovery_document(recovery_loop("/wt/unretained"));
    assert!(validate_plan(&different_worktree).is_err());

    let composite = queue_conflict_recovery_document(json!({
        "id":"rebase", "kind":"loop", "access":"read_only",
        "depends_on":["conflict","publish","hygiene"],
        "loop":{"max_rounds":2,"until":"accepted",
          "retry_on":["verified_clean","refresh_conflict","refresh_ci","accepted"],
          "selected_publication":"refresh","initial_publication":"publish",
          "previous_bindings":{"refresh":"publish","verified_clean":"hygiene"},
          "body":[
            {"id":"writer_bundle", "kind":"composite", "access":"read_only",
             "composite":{"nodes":[
                {"id":"fix", "access":"worktree_write", "worktree":"/wt/feature",
                 "prompt":"rebase the retained published branch", "write_scope":["src"],
                 "route":{"alias":"senior"},
                 "source":{"type":"node","node":"previous.verified_clean"},
                 "inputs":[{"node":"previous.refresh","outputs":["branch","commit"]}]},
                {"id":"followup", "access":"worktree_write", "worktree":"/wt/feature",
                 "prompt":"finish the rebase", "write_scope":["src"],
                 "route":{"alias":"senior"}, "depends_on":["fix"],
                 "source":{"type":"node","node":"fix"}}
             ], "exports":[{"node":"followup","outputs":["branch","commit","result"]}]}},
            {"id":"verified_clean", "kind":"hygiene", "access":"worktree_write",
             "worktree":"/wt/feature", "depends_on":["writer_bundle"],
             "source":{"type":"node","node":"writer_bundle"}},
            {"id":"refresh", "kind":"publish", "access":"worktree_write",
             "worktree":"/wt/feature", "depends_on":["verified_clean"],
             "source":{"type":"node","node":"verified_clean"}},
            {"id":"refresh_conflict", "kind":"gate", "access":"read_only",
             "depends_on":["refresh"],
             "condition":{"type":"conflict","target":{"node":"refresh"}}},
            {"id":"refresh_ci", "kind":"gate", "access":"read_only",
             "depends_on":["refresh_conflict"],
             "condition":{"type":"ci","target":{"node":"refresh"}}},
            {"id":"fresh_review", "access":"read_only", "prompt":"review recovery head",
             "source":{"type":"head","node":"refresh"},
             "depends_on":["refresh","refresh_conflict","refresh_ci"],
             "route":{"alias":"senior"}},
            {"id":"accepted", "kind":"gate", "access":"read_only",
             "depends_on":["fresh_review","refresh_ci"],
             "inputs":[{"node":"fresh_review","outputs":["verdict"]}],
             "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
          ]}
    }));
    assert!(validate_plan(&composite).is_ok());

    let unrelated_writer = queue_conflict_recovery_document(json!({
        "id":"rebase", "kind":"composite", "access":"read_only",
        "depends_on":["conflict"], "composite":{"nodes":[
            {"id":"fix", "access":"worktree_write", "worktree":"/wt/feature",
             "prompt":"rebase the retained published branch", "write_scope":["src"],
             "route":{"alias":"senior"}, "source":{"type":"head","node":"publish"}},
            {"id":"unbound", "access":"worktree_write", "worktree":"/wt/feature",
             "prompt":"write without the published head", "write_scope":["src"],
             "route":{"alias":"senior"}, "depends_on":["fix"]}
        ], "exports":[{"node":"unbound","outputs":["result"]}]}
    }));
    assert!(validate_plan(&unrelated_writer).is_err());

    let mut missing_recovery = valid;
    missing_recovery
        .nodes
        .iter_mut()
        .find(|node| node.id == "conflict")
        .unwrap()
        .on_fail = None;
    // Loop admission catches the missing initial conflict entry before the
    // later queue-repair relation check runs.
    assert_eq!(
        validate_plan(&missing_recovery).unwrap_err(),
        "loop rebase initial_publication must be its directly bound publication seed and have an entry failure gate"
    );
}

#[test]
fn queue_ci_repair_must_republish_and_recheck_the_repaired_head() {
    let ci_repair_document = |reenter: bool| {
        let mut document = queue_publish_document("routine");
        document.nodes.retain(|node| node.id != "land");
        document
            .nodes
            .iter_mut()
            .find(|node| node.id == "ci")
            .unwrap()
            .on_fail = Some("ci_repair".into());
        let recovery = if reenter {
            json!({
                "id":"ci_repair", "kind":"loop", "access":"read_only",
                "depends_on":["ci","publish","hygiene"],
                "loop":{"max_rounds":2,"until":"accepted",
                  "retry_on":["verified_clean","fresh_conflict","fresh_ci","accepted"],
                  "selected_publication":"refresh","initial_publication":"publish",
                  "previous_bindings":{"refresh":"publish","verified_clean":"hygiene"},
                  "body":[
                    {"id":"fix", "access":"worktree_write", "worktree":"/wt/feature",
                     "prompt":"repair the published CI findings", "write_scope":["src"],
                     "source":{"type":"node","node":"previous.verified_clean"},
                     "inputs":[{"node":"previous.refresh","outputs":["branch","commit"]}],
                     "route":{"alias":"senior"}},
                    {"id":"verified_clean", "kind":"hygiene", "access":"worktree_write",
                     "worktree":"/wt/feature", "depends_on":["fix"],
                     "source":{"type":"node","node":"fix"}},
                    {"id":"refresh", "kind":"publish", "access":"worktree_write",
                     "worktree":"/wt/feature", "depends_on":["verified_clean"],
                     "source":{"type":"node","node":"verified_clean"}},
                    {"id":"fresh_conflict", "kind":"gate", "access":"read_only",
                     "depends_on":["refresh"],
                     "condition":{"type":"conflict","target":{"node":"refresh"}}},
                    {"id":"fresh_ci", "kind":"gate", "access":"read_only",
                     "depends_on":["fresh_conflict"],
                     "condition":{"type":"ci","target":{"node":"refresh"}}},
                    {"id":"fresh_review", "access":"read_only", "prompt":"review recovery head",
                     "source":{"type":"head","node":"refresh"},
                     "depends_on":["refresh","fresh_conflict","fresh_ci"],
                     "route":{"alias":"senior"}},
                    {"id":"accepted", "kind":"gate", "access":"read_only",
                     "depends_on":["fresh_review","fresh_ci"],
                     "inputs":[{"node":"fresh_review","outputs":["verdict"]}],
                     "condition":{"type":"verdict","expected":"VERDICT: PASS","threshold":"all"}}
                  ]}
            })
        } else {
            json!({
                "id":"ci_repair", "kind":"loop", "access":"read_only",
                "depends_on":["ci","publish"],
                "loop":{"max_rounds":1,"until":"accepted","body":[
                    {"id":"fix", "access":"worktree_write", "worktree":"/wt/feature",
                     "prompt":"repair the published CI findings", "write_scope":["src"],
                     "source":{"type":"head","node":"publish"},
                     "route":{"alias":"senior"}}
                ]}
            })
        };
        document
            .nodes
            .push(serde_json::from_value(recovery).unwrap());
        document
    };

    assert!(validate_plan(&ci_repair_document(true)).is_ok());
    assert!(validate_plan(&ci_repair_document(false)).is_err());
}

#[test]
fn queue_land_does_not_run_without_clear_conflict_and_green_ci_on_publish_head() {
    for mismatch_conflict in [true, false] {
        let document = queue_publish_document("routine");
        let mut owner = owner();
        owner.repair_fixture = true;
        owner.integration_base = Some(TARGET.parse().unwrap());
        if mismatch_conflict {
            owner.conflict_head = Some(WRITE_HEAD.into());
        } else {
            owner.ci_head = Some(WRITE_HEAD.into());
        }
        let checkpoint = run(&document, &mut owner, &mut Journal::default());

        assert!(owner.land_requests.is_empty());
        assert!(node(&checkpoint, "land").land.is_none());
        if mismatch_conflict {
            assert_eq!(
                node(&checkpoint, "conflict")
                    .conflict
                    .as_ref()
                    .unwrap()
                    .state,
                ConflictState::Pending
            );
        } else {
            assert_eq!(
                node(&checkpoint, "conflict")
                    .conflict
                    .as_ref()
                    .unwrap()
                    .state,
                ConflictState::Clear
            );
            assert_eq!(
                node(&checkpoint, "ci").ci.as_ref().unwrap().state,
                CiState::Pending
            );
        }
    }
}

#[test]
fn queue_land_admission_requires_conflict_ci_and_verdict_after_publish() {
    let mut document = queue_publish_document("routine");
    assert!(validate_plan(&document).is_ok());

    document.nodes.retain(|node| node.id != "conflict");
    document
        .nodes
        .iter_mut()
        .find(|node| node.id == "ci")
        .unwrap()
        .depends_on = vec!["publish".into()];
    document
        .nodes
        .iter_mut()
        .find(|node| node.id == "rebase")
        .unwrap()
        .depends_on
        .retain(|dependency| dependency != "conflict");

    assert!(
        validate_plan(&document)
            .err()
            .is_some_and(|error| error.contains("conflict, CI and verdict gates"))
    );
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
    assert_eq!(
        repair.advance_error_code.as_deref(),
        Some("continuation_delivery_missing")
    );
    assert_eq!(owner.launches.len(), 1);
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Blocked);
    assert_eq!(land.advance_error, None);
}

#[test]
fn land_scheduler_resolves_land_execution_with_graph_advance_in_scope() {
    // The graph-level advance is also in scope during tests; the land entry
    // point must still resolve to the scheduler's eight-argument operation.
    let _land_advance = land::advance::<Owner, Journal>;
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
        (LandPolicy::Auto, Layer::Default, Some(7))
    );
    let outputs = land.outputs.as_ref().unwrap();
    assert_eq!(outputs.verdict.as_deref(), Some(land::LANDED));
    assert_eq!(outputs.commit.as_deref(), Some(REPAIR_HEAD));
    let excerpt = &outputs.result.as_ref().unwrap().excerpt;
    assert!(excerpt.contains("#7") && excerpt.contains("policy is auto (default)"));
    assert!(checkpoint.asks.is_empty());
}

#[test]
fn land_identity_save_failure_prevents_merge() {
    #[derive(Default)]
    struct FailingJournal(Option<GraphCheckpoint>);
    impl GraphJournal for FailingJournal {
        type Error = std::io::Error;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self.0.clone())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if node(checkpoint, "land")
                .land
                .as_ref()
                .and_then(|land| land.pull_request)
                .is_some()
            {
                return Err(std::io::Error::other("injected identity save failure"));
            }
            self.0 = Some(checkpoint.clone());
            Ok(())
        }
    }
    let mut owner = owner();
    let mut journal = FailingJournal::default();
    let error = advance(&document("routine", None), &mut owner, &mut journal).unwrap_err();
    assert!(matches!(error, GraphError::Journal(ref e) if e.to_string().contains("injected")));
    assert_eq!(owner.merge_requests, 0);
    let land = node(journal.0.as_ref().unwrap(), "land")
        .land
        .as_ref()
        .unwrap();
    assert_eq!(land.pull_request, None);
}

#[test]
fn land_rejects_a_merge_observation_for_another_pr() {
    let mut owner = owner();
    owner.outcomes.push_back(LandObservation::Merged {
        pull_request: Some(8),
        note: None,
    });
    let mut journal = Journal::default();
    let error = advance(&document("routine", None), &mut owner, &mut journal).unwrap_err();
    assert!(
        matches!(error, GraphError::Refused(ref reason) if reason.contains("saved PR identity"))
    );
    let saved = journal.0.as_ref().unwrap();
    assert_eq!(node(saved, "land").state, NodeState::Running);
    assert_eq!(
        node(saved, "land").land.as_ref().unwrap().pull_request,
        Some(7)
    );
}

#[test]
fn merged_land_is_reobserved_after_its_checkpoint_save_fails() {
    #[derive(Default)]
    struct FailingJournal {
        saved: Option<GraphCheckpoint>,
        failed: bool,
    }
    impl GraphJournal for FailingJournal {
        type Error = std::io::Error;
        fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
            Ok(self.saved.clone())
        }
        fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
            if !self.failed && node(checkpoint, "land").state == NodeState::Succeeded {
                self.failed = true;
                return Err(std::io::Error::other("injected checkpoint failure"));
            }
            self.saved = Some(checkpoint.clone());
            Ok(())
        }
    }
    let document = document("routine", None);
    let mut owner = owner();
    let mut journal = FailingJournal::default();
    let error = advance(&document, &mut owner, &mut journal).unwrap_err();
    assert!(
        matches!(error, GraphError::Journal(ref error) if error.to_string().contains("injected"))
    );
    let saved = journal.saved.clone().unwrap();
    assert_eq!(node(&saved, "land").state, NodeState::Running);
    assert_eq!(owner.merge_requests, 1);
    let recovered = advance(&document, &mut owner, &mut journal)
        .unwrap()
        .checkpoint;
    assert_eq!(node(&recovered, "land").state, NodeState::Succeeded);
    assert_eq!(owner.merge_requests, 1);
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        recovered
    );
    assert_eq!(owner.merge_requests, 1);
}

#[test]
fn pending_land_without_a_wall_clock_keeps_the_prior_checkpoint() {
    let document = document("routine", None);
    let mut owner = owner();
    owner.no_wall_clock = true;
    owner.outcomes.push_back(LandObservation::Pending {
        reason: "checks pending".into(),
    });
    let mut journal = Journal::default();
    let error = advance(&document, &mut owner, &mut journal).unwrap_err();
    assert!(matches!(error, GraphError::Refused(ref reason) if reason.contains("no wall clock")));
    let saved = journal.0.as_ref().unwrap();
    assert_eq!(node(saved, "land").state, NodeState::Running);
    let record = node(saved, "land").land.as_ref().unwrap();
    assert_eq!(record.pending_observations, 0);
    assert!(record.note.is_none());
    assert!(record.wake_at.is_none());
}

#[test]
fn a_crucial_tier_asks_and_succeeds_only_once_the_chair_merges_by_hand() {
    let document = document("crucial", Some("chair"));
    let mut owner = owner();
    let mut journal = Journal::default();
    let checkpoint = run(&document, &mut owner, &mut journal);
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Running);
    assert!(owner.lands.is_empty());
    assert!(
        owner
            .observed
            .iter()
            .all(|observed| *observed == ("feature/x".to_owned(), REPAIR_HEAD.to_owned()))
    );
    let ask = &checkpoint.asks[0];
    assert_eq!(
        (ask.ask_id.as_str(), ask.kind, ask.session.as_str()),
        ("land.ask-1", AskKind::Land, "chair")
    );
    let filed = &owner.filed[0];
    let options: Vec<_> = filed.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(options, ["hold"]);
    assert!(filed.body.contains(REPAIR_HEAD) && filed.body.contains("crucial tier asks first"));
    assert!(
        filed
            .body
            .contains(&format!("provenant land N --head {REPAIR_HEAD}"))
    );

    owner.chair_merged = true;
    let checkpoint = run(&document, &mut owner, &mut journal);
    assert!(owner.lands.is_empty(), "the graph never merges an ask tier");
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Succeeded);
    let authority = LandAuthority::Chair {
        ask_id: "land.ask-1".into(),
    };
    assert_eq!(land.land.as_ref().unwrap().authorised_by, Some(authority));
    assert!(
        land.outputs
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .excerpt
            .contains("the chair landed it by hand after land.ask-1")
    );
    assert!(checkpoint.asks.is_empty());
}

#[test]
fn an_inbox_answer_of_land_never_approves_a_land() {
    let mut owner = owner();
    owner.reply = Some("land".into());
    let checkpoint = run(
        &document("crucial", Some("chair")),
        &mut owner,
        &mut Journal::default(),
    );
    let land = node(&checkpoint, "land");
    assert_eq!(land.state, NodeState::Failed);
    assert!(owner.lands.is_empty(), "{:?}", owner.lands);
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_answer_cannot_approve")
    );
    assert!(land.land.as_ref().unwrap().authorised_by.is_none());
}

#[test]
fn an_answer_after_a_pending_observation_clears_the_land_deadline() {
    let document = document("crucial", Some("chair"));
    let mut owner = owner();
    let mut journal = Journal::default();
    let pending = run(&document, &mut owner, &mut journal);
    let land = node(&pending, "land");
    assert_eq!(land.state, NodeState::Running);
    assert!(land.land.as_ref().unwrap().wake_at.is_some());

    owner.reply = Some("land".into());
    let answered = advance(&document, &mut owner, &mut journal).unwrap();
    let land = node(&answered.checkpoint, "land");
    assert_eq!(land.state, NodeState::Failed);
    assert!(land.land.as_ref().unwrap().wake_at.is_none());
    // The failed checkpoint stays valid for the next advance.
    let again = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(node(&again.checkpoint, "land").state, NodeState::Failed);
    assert!(owner.lands.is_empty());
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
    assert_eq!(land.advance_error_code.as_deref(), Some("land_held"));
    assert!(owner.lands.is_empty());
}

#[test]
fn a_configured_policy_overrides_the_default_and_records_its_source() {
    let mut owner = owner();
    owner.policy = Some(LandChoice {
        policy: LandPolicy::Ask,
        source: Layer::Project,
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
        (LandPolicy::Ask, Layer::Project, None)
    );
    assert!(owner.filed[0].body.contains("project"));
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
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_session_missing")
    );
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
    assert_eq!(land.advance_error_code.as_deref(), Some("land_refused"));
    assert_eq!(owner.lands.len(), 2);
    assert!(owner.lands.iter().all(|(_, head, _)| head == REPAIR_HEAD));
}

#[test]
fn a_pending_land_persists_a_retry_deadline_that_survives_restart() {
    let mut owner = owner();
    owner.tick = true;
    owner.backoff = CiPollBackoff {
        initial_seconds: 60,
        max_seconds: 240,
    };
    owner.outcomes = VecDeque::from([LandObservation::Pending {
        reason: "CI_PENDING".into(),
    }]);
    let document = document("routine", None);
    let mut journal = Journal::default();
    let pending = run(&document, &mut owner, &mut journal);
    let record = node(&pending, "land").land.as_ref().unwrap();
    assert_eq!(record.wake_at.unwrap().as_micros(), 60_000_000);
    assert_eq!(record.pending_observations, 1);
    // Tick wake discovery reads the checkpoint deadline.
    assert_eq!(pending.wake_at, record.wake_at);
    assert_eq!(owner.lands.len(), 1);

    journal.0 = Some(serde_json::from_slice(&serde_json::to_vec(&pending).unwrap()).unwrap());
    owner.wall_seconds = 59;
    let early = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(early.checkpoint, pending);
    assert_eq!(owner.lands.len(), 1);

    owner.wall_seconds = 60;
    let landed = advance(&document, &mut owner, &mut journal).unwrap();
    assert_eq!(landed.status, GraphStatus::Complete);
    let land = node(&landed.checkpoint, "land");
    assert_eq!(land.state, NodeState::Succeeded);
    assert!(land.land.as_ref().unwrap().wake_at.is_none());
    assert!(landed.checkpoint.wake_at.is_none());
    assert_eq!(owner.lands.len(), 2);
}

/// Run `nodes` with reviewers ending in `verdict`, returning the land node.
fn land_outcome(nodes: serde_json::Value, verdict: &'static str) -> (Owner, NodeProgress) {
    let document: GraphDocument =
        serde_json::from_value(json!({"schema_version":1, "graph_id":"g-land", "nodes":nodes}))
            .unwrap();
    let mut owner = owner();
    owner.review_verdict = Some(verdict);
    let checkpoint = run(&document, &mut owner, &mut Journal::default());
    let land = node(&checkpoint, "land").clone();
    (owner, land)
}

#[test]
fn a_failed_review_routed_to_land_never_calls_the_host() {
    let (owner, land) = land_outcome(
        json!([
            {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
             "prompt":"write it", "route":{"alias":"senior"}},
            {"id":"review", "access":"read_only", "prompt":"review", "depends_on":["write"],
             "route":{"alias":"senior"}},
            {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["review"],
             "inputs":[{"node":"review", "outputs":["verdict"]}], "on_fail":"land",
             "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}},
            {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["write"],
             "condition":{"type":"ci", "target":"feature/x"}},
            {"id":"land", "kind":"land", "access":"read_only", "depends_on":["accepted", "ci"],
             "land":{"node":"write", "tier":"routine"}}
        ]),
        "VERDICT: REPAIR",
    );
    assert!(owner.lands.is_empty());
    assert_eq!(land.state, NodeState::Failed);
    assert!(land.land.is_none());
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_review_required")
    );
}

#[test]
fn empty_review_range_cannot_pass_deliver_gate_or_land() {
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version": 1, "graph_id": "empty-review-land", "nodes": [
            {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
             "prompt":"write it", "route":{"alias":"senior"}},
            {"id":"review", "role":"lens.correctness-spec", "access":"read_only", "prompt":"review",
             "depends_on":["write"], "route":{"alias":"senior"}},
            {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["review"],
             "inputs":[{"node":"review", "outputs":["verdict"]}],
             "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}},
            {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["write"],
             "condition":{"type":"ci", "target":"feature/x"}},
            {"id":"land", "kind":"land", "access":"read_only", "depends_on":["accepted", "ci"],
             "land":{"node":"write", "tier":"routine"}}
        ]
    }))
    .unwrap();
    let mut owner = owner();
    owner.equal_review_range = true;
    let checkpoint = run(&document, &mut owner, &mut Journal::default());
    let review = node(&checkpoint, "review").outputs.as_ref().unwrap();
    assert_eq!(review.verdict.as_deref(), Some("VERDICT: INCONCLUSIVE"));
    assert!(!node(&checkpoint, "accepted").gate.as_ref().unwrap().passed);
    assert!(owner.lands.is_empty());
    assert!(node(&checkpoint, "land").land.is_none());
}

#[test]
fn reduced_review_requires_every_counted_member_to_judge_the_land_head() {
    for (other_head, allowed) in [(WRITE_HEAD, true), (REPAIR_HEAD, false)] {
        let (owner, land) = land_outcome(
            json!([
                {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
                 "prompt":"write it", "route":{"alias":"senior"}},
                {"id":"reviews", "kind":"composite", "access":"read_only", "depends_on":["write"],
                 "composite":{"reduce_verdict":true,"min_successful":1,
                    "nodes":[
                        {"id":"a","access":"read_only","prompt":"review","route":{"alias":"senior"},"source":{"type":"commit","commit":WRITE_HEAD}},
                        {"id":"b","access":"read_only","prompt":"review","route":{"alias":"senior"},"source":{"type":"commit","commit":other_head}}
                    ],"exports":[{"node":"a","outputs":["result"]},{"node":"b","outputs":["result"]}]}},
                {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["reviews"],
                 "inputs":[{"node":"reviews", "outputs":["verdict"]}],
                 "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}},
                {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["write"],
                 "condition":{"type":"ci", "target":"feature/x"}},
                {"id":"land", "kind":"land", "access":"read_only", "depends_on":["accepted", "ci"],
                 "land":{"node":"write", "tier":"routine"}}
            ]),
            "VERDICT: PASS",
        );
        assert_eq!(!owner.lands.is_empty(), allowed);
        if !allowed {
            assert_eq!(
                land.advance_error_code.as_deref(),
                Some("land_review_required")
            );
        }
    }
}

#[test]
fn ci_output_never_substitutes_for_review() {
    // write, CI, a verdict gate on CI's own PASS, land: no reviewer ran.
    let (owner, land) = land_outcome(
        json!([
            {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
             "prompt":"write it", "route":{"alias":"senior"}},
            {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["write"],
             "condition":{"type":"ci", "target":"feature/x"}},
            {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["ci"],
             "inputs":[{"node":"ci", "outputs":["verdict"]}],
             "condition":{"type":"verdict", "expected":"PASS", "threshold":"all"}},
            {"id":"land", "kind":"land", "access":"read_only", "depends_on":["accepted"],
             "land":{"node":"write", "tier":"substantial"}}
        ]),
        "VERDICT: PASS",
    );
    assert!(owner.lands.is_empty(), "{:?}", owner.lands);
    assert_eq!(land.state, NodeState::Failed);
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_review_required")
    );
}

#[test]
fn a_carried_over_head_lands_and_a_changed_delta_does_not() {
    // write H1, review H1 against the bound REVIEW_BASE, PASS, then the continuation
    // brings in the target (H2), CI, land H2.
    let graph = json!([
        {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
         "prompt":"write it", "route":{"alias":"senior"}},
        {"id":"review", "access":"read_only", "prompt":"review", "depends_on":["write"],
         "route":{"alias":"senior"}},
        {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["review"],
         "inputs":[{"node":"review", "outputs":["verdict"]}],
         "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}},
        {"id":"repair", "access":"worktree_write", "worktree":"/wt/feature", "continues":"write",
         "prompt":"bring in main", "depends_on":["accepted"], "route":{"alias":"senior"}},
        {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["repair"],
         "condition":{"type":"ci", "target":"feature/x"}},
        {"id":"land", "kind":"land", "access":"read_only", "depends_on":["ci"],
         "land":{"node":"write", "tier":"routine"}}
    ]);
    let document: GraphDocument =
        serde_json::from_value(json!({"schema_version":1, "graph_id":"g-land", "nodes":graph}))
            .unwrap();
    for (extra, lands) in [(&[][..], true), (&["one-more-line"][..], false)] {
        let mut history = provenant_core::review_cover::history::History::default();
        history.commit(REVIEW_BASE, &[], &["base"]);
        history.commit(TARGET, &[REVIEW_BASE], &["main-change"]);
        history.commit(WRITE_HEAD, &[REVIEW_BASE], &["feature"]);
        history.commit(REPAIR_HEAD, &[WRITE_HEAD, TARGET], extra);
        let mut owner = owner();
        owner.history = Some(history);
        let checkpoint = run(&document, &mut owner, &mut Journal::default());
        let land = node(&checkpoint, "land");
        if lands {
            assert_eq!(land.state, NodeState::Succeeded, "{:?}", land.advance_error);
            assert_eq!(owner.lands.len(), 1);
            assert_eq!(owner.lands[0].1, REPAIR_HEAD);
        } else {
            assert!(owner.lands.is_empty(), "{:?}", owner.lands);
            assert_eq!(land.state, NodeState::Failed);
            assert_eq!(
                land.advance_error_code.as_deref(),
                Some("land_review_required")
            );
        }
    }
}

#[test]
fn a_review_of_an_earlier_head_never_lands_a_continuation() {
    // write H1, review H1, PASS, repair H2, CI, land: H2 was never reviewed.
    let (owner, land) = land_outcome(
        json!([
            {"id":"write", "access":"worktree_write", "worktree":"/wt/feature", "delivery":"pr",
             "prompt":"write it", "route":{"alias":"senior"}},
            {"id":"review", "access":"read_only", "prompt":"review", "depends_on":["write"],
             "route":{"alias":"senior"}},
            {"id":"accepted", "kind":"gate", "access":"read_only", "depends_on":["review"],
             "inputs":[{"node":"review", "outputs":["verdict"]}],
             "condition":{"type":"verdict", "expected":"VERDICT: PASS", "threshold":"all"}},
            {"id":"repair", "access":"worktree_write", "worktree":"/wt/feature", "continues":"write",
             "prompt":"polish", "depends_on":["accepted"], "route":{"alias":"senior"}},
            {"id":"ci", "kind":"gate", "access":"read_only", "depends_on":["repair"],
             "condition":{"type":"ci", "target":"feature/x"}},
            {"id":"land", "kind":"land", "access":"read_only", "depends_on":["ci"],
             "land":{"node":"write", "tier":"routine"}}
        ]),
        "VERDICT: PASS",
    );
    assert!(owner.lands.is_empty());
    assert_eq!(land.state, NodeState::Failed);
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_review_required")
    );

    // A reviewer pinned to the writer's commit judged H1 even when it ran
    // after the repair.
    let mut nodes = nodes("routine");
    nodes[3]["source"] = json!({"type":"node", "node":"write"});
    nodes[3]["depends_on"] = json!(["repair", "write"]);
    let (owner, land) = land_outcome(nodes, "VERDICT: PASS");
    assert!(owner.lands.is_empty());
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_review_required")
    );
}

fn refusal(value: serde_json::Value) -> &'static str {
    let document: GraphDocument = serde_json::from_value(value).unwrap();
    match advance(&document, &mut owner(), &mut Journal::default()) {
        Err(error @ (GraphError::Invalid(_) | GraphError::Admission(_))) => error.code(),
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
        assert_eq!(refused, "invalid_graph");
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
    assert_eq!(reason, "invalid_graph");
}

#[test]
fn land_records_keep_unknown_fields() {
    let value = json!({"tier":"crucial", "policy":"ask", "policy_source":"project",
        "branch":"feature/x", "head":REPAIR_HEAD,
        "authorised_by":{"type":"chair", "ask_id":"land.ask-1"},
        "receipt":"future"});
    let record: LandRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(record).unwrap(), value);
}

#[test]
fn a_head_source_reviews_the_newest_continuation_and_lands_it() {
    // write H1, CI, repair H2, review bound to the writer chain's head, land.
    let mut nodes = nodes("routine");
    nodes[3]["source"] = json!({"type":"head", "node":"write"});
    let (landed, land) = land_outcome(nodes.clone(), "VERDICT: PASS");
    assert_eq!(land.state, NodeState::Succeeded, "{:?}", land.advance_error);
    assert_eq!(
        landed.lands,
        [(
            "feature/x".to_owned(),
            REPAIR_HEAD.to_owned(),
            LandAuthority::Policy
        )]
    );
    let document: GraphDocument =
        serde_json::from_value(json!({"schema_version":1, "graph_id":"g-land", "nodes":nodes}))
            .unwrap();
    let mut owner = owner();
    let mut journal = Journal::default();
    let checkpoint = run(&document, &mut owner, &mut journal);
    assert_eq!(
        node(&checkpoint, "review").source_commit.as_deref(),
        Some(REPAIR_HEAD)
    );
    assert_eq!(
        node(&checkpoint, "review").attempts[0]
            .input_manifest
            .as_ref()
            .unwrap()
            .source_producer
            .as_ref()
            .unwrap()
            .node_id,
        "repair"
    );
    // The pinned head replays unchanged.
    assert_eq!(
        advance(&document, &mut owner, &mut journal)
            .unwrap()
            .checkpoint,
        checkpoint
    );
}

#[test]
fn a_head_source_inside_a_composite_names_an_outer_writer() {
    let mut nodes = nodes("routine");
    let mut review = nodes[3].clone();
    review["source"] = json!({"type":"head", "node":"write"});
    review["depends_on"] = json!([]);
    nodes[3] = json!({"id":"review", "kind":"composite", "access":"read_only",
        "depends_on":["repair"],
        "composite":{"nodes":[review], "exports":[{"node":"review", "outputs":["verdict"]}]}});
    let (owner, land) = land_outcome(nodes, "VERDICT: PASS");
    assert_eq!(land.state, NodeState::Succeeded, "{:?}", land.advance_error);
    assert_eq!(owner.lands[0].1, REPAIR_HEAD);
}

#[test]
fn a_head_source_must_name_an_upstream_writer() {
    for (target, depends_on) in [("write", json!([])), ("accepted", json!(["accepted"]))] {
        let mut nodes = nodes("routine");
        nodes
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"late", "access":"read_only",
            "prompt":"look", "depends_on":depends_on, "route":{"alias":"senior"},
            "source":{"type":"head", "node":target}}));
        let refused = refusal(json!({"schema_version":1, "graph_id":"g-land", "nodes":nodes}));
        assert_eq!(refused, "invalid_graph", "{target}");
    }
}

#[test]
fn a_composite_verdict_binds_to_the_head_its_producer_reviewed() {
    // write H1, CI, repair H2, then a council pinned to H1: its boundary
    // follows the repair, but the exported verdict judged H1, so H2 never lands.
    let mut nodes = nodes("routine");
    let mut review = nodes[3].clone();
    review["source"] = json!({"type":"node", "node":"write"});
    review["depends_on"] = json!([]);
    nodes[3] = json!({"id":"review", "kind":"composite", "access":"read_only",
        "depends_on":["repair", "write"],
        "composite":{"nodes":[review], "exports":[{"node":"review", "outputs":["verdict"]}]}});
    let (owner, land) = land_outcome(nodes, "VERDICT: PASS");
    assert!(owner.lands.is_empty(), "{:?}", owner.lands);
    assert_eq!(land.state, NodeState::Failed);
    assert_eq!(
        land.advance_error_code.as_deref(),
        Some("land_review_required")
    );
}

#[test]
fn a_replaced_gate_accepting_rejection_never_lands() {
    let document = document("routine", None);
    let mut owner = owner();
    owner.pending_writer = true;
    owner.review_verdict = Some("VERDICT: FAIL");
    let mut journal = Journal::default();
    let mut checkpoint = advance(&document, &mut owner, &mut journal)
        .unwrap()
        .checkpoint;
    assert_eq!(node(&checkpoint, "accepted").state, NodeState::Pending);
    let mut replacement = nodes("routine")[4].clone();
    replacement["condition"]["expected"] = json!("VERDICT: FAIL");
    let library = template::TemplateLibrary::bundled();
    let resolvers = Default::default();
    let limits = plans::generous();
    patch::admit_patch(
        &mut patch::LiveGraph {
            document: &document,
            checkpoint: &mut checkpoint,
        },
        None,
        &patch::Patch {
            schema_version: 1,
            replace: vec![replacement],
            ..Default::default()
        },
        &patch::PatchContext {
            catalogue: None,
            library: &library,
            resolvers: &resolvers,

            journal: None,
            limits: &limits,

            worktree_layout: provenant_config::PortTable::builtin().worktree_layout,
        },
    )
    .unwrap();
    journal.0 = Some(checkpoint);
    owner.pending_writer = false;
    let checkpoint = run(&document, &mut owner, &mut journal);
    assert!(node(&checkpoint, "accepted").gate.as_ref().unwrap().passed);
    assert_eq!(node(&checkpoint, "land").state, NodeState::Failed);
    assert_eq!(
        node(&checkpoint, "land").advance_error_code.as_deref(),
        Some("land_review_required")
    );
    assert!(owner.lands.is_empty());
}

#[test]
fn a_land_ask_id_carries_the_sequence_and_the_retry_generation() {
    for (sequence, generation) in [(1, 0), (2, 0), (1, 3), (7, 12)] {
        let id = land::ask_id("land", sequence, generation);
        assert_eq!(land::parse(&id, "land"), Some((sequence, generation)));
    }
    assert_ne!(land::ask_id("land", 1, 0), land::ask_id("land", 1, 1));
    assert_eq!(land::parse("land.ask-1.retry-0", "land"), None);
    assert_eq!(land::parse("land.ask-0", "land"), None);
    assert_eq!(land::parse("other.ask-1", "land"), None);
}

#[derive(Default)]
struct ReplayJournal {
    checkpoint: Option<GraphCheckpoint>,
    saves: Vec<GraphCheckpoint>,
    failed_after_conflict_reset: bool,
}
impl GraphJournal for ReplayJournal {
    type Error = std::io::Error;
    fn load(&mut self, _: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self.checkpoint.clone())
    }
    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.checkpoint = Some(checkpoint.clone());
        self.saves.push(checkpoint.clone());
        let conflict = checkpoint.nodes.iter().find(|node| node.id == "conflict");
        let ci = checkpoint.nodes.iter().find(|node| node.id == "ci");
        let reset_checkpoint = conflict.is_some_and(|node| {
            node.state == NodeState::Pending && node.gate.is_none() && node.conflict.is_none()
        }) && ci.is_some_and(|node| {
            node.state == NodeState::Pending && node.gate.is_none() && node.ci.is_none()
        }) && checkpoint
            .nodes
            .iter()
            .find(|node| node.id == "publish")
            .and_then(|node| node.outputs.as_ref())
            .and_then(|outputs| outputs.commit.as_deref())
            == Some(REPAIR_HEAD)
            && checkpoint
                .nodes
                .iter()
                .find(|node| node.id == "rebase")
                .is_some_and(|node| node.state == NodeState::Pending);
        if reset_checkpoint && !self.failed_after_conflict_reset {
            self.failed_after_conflict_reset = true;
            return Err(std::io::Error::other(
                "simulated lost acknowledgement after durable conflict reset",
            ));
        }
        Ok(())
    }
}

#[test]
fn queue_ci_conflict_after_clear_rebases_republishes_and_rechecks_after_replay() {
    let mut document = queue_publish_document("routine");
    document.nodes.retain(|node| node.id != "land");
    assert!(validate_plan(&document).is_ok());

    let mut first_owner = owner();
    first_owner.repair_fixture = true;
    first_owner.integration_base = Some(TARGET.parse().unwrap());
    first_owner.hygiene_after_heads = VecDeque::from([REPAIR_HEAD.into(), REPUBLISHED_HEAD.into()]);
    first_owner.conflict_states = VecDeque::from([
        ConflictState::Clear,
        ConflictState::Conflicting,
        ConflictState::Clear,
    ]);
    first_owner.ci_states = VecDeque::from([CiState::Conflict, CiState::Passed]);
    let mut first_journal = ReplayJournal::default();
    assert!(matches!(
        advance(&document, &mut first_owner, &mut first_journal),
        Err(GraphError::Journal(_))
    ));
    assert_eq!(first_owner.launches.len(), 1);
    assert_eq!(first_owner.launches[0].node, "write");
    assert_eq!(first_owner.hygiene_observations.len(), 1);
    assert_eq!(first_owner.publication_heads, vec![REPAIR_HEAD.to_owned()]);
    assert_eq!(first_owner.gate_observations.len(), 2);

    let reset = first_journal
        .saves
        .iter()
        .find(|checkpoint| {
            let conflict = node(checkpoint, "conflict");
            let ci = node(checkpoint, "ci");
            conflict.state == NodeState::Pending
                && conflict.gate.is_none()
                && conflict.conflict.is_none()
                && ci.state == NodeState::Pending
                && ci.gate.is_none()
                && ci.ci.is_none()
                && node(checkpoint, "publish")
                    .outputs
                    .as_ref()
                    .and_then(|outputs| outputs.commit.as_deref())
                    == Some(REPAIR_HEAD)
                && node(checkpoint, "rebase").state == NodeState::Pending
                && node(checkpoint, "rebase--r1--fix").state == NodeState::Pending
                && node(checkpoint, "rebase--r1--verified_clean").state == NodeState::Pending
        })
        .expect("CI conflict reset must be durable before retrying conflict observation");
    let replay_checkpoint: GraphCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(reset).unwrap()).unwrap();

    let mut replay_owner = owner();
    replay_owner.repair_fixture = true;
    replay_owner.integration_base = Some(TARGET.parse().unwrap());
    replay_owner.hygiene_after_heads = VecDeque::from([REPUBLISHED_HEAD.into()]);
    replay_owner.conflict_states =
        VecDeque::from([ConflictState::Conflicting, ConflictState::Clear]);
    replay_owner.conflict_heads =
        VecDeque::from([Some(REPAIR_HEAD.into()), Some(REPUBLISHED_HEAD.into())]);
    replay_owner.ci_states = VecDeque::from([CiState::Passed]);
    let mut replay_journal = ReplayJournal {
        checkpoint: Some(replay_checkpoint),
        failed_after_conflict_reset: true,
        ..Default::default()
    };
    let replayed = advance(&document, &mut replay_owner, &mut replay_journal).unwrap();

    assert_eq!(
        node(&replayed.checkpoint, "conflict").state,
        NodeState::Failed
    );
    assert_eq!(
        node(&replayed.checkpoint, "rebase").state,
        NodeState::Succeeded
    );
    assert!(node(&replayed.checkpoint, "ci").gate.is_none());
    assert_ne!(node(&replayed.checkpoint, "ci").state, NodeState::Failed);
    assert!(
        !replay_owner
            .launches
            .iter()
            .any(|launch| launch.node == "rebase--r1--ci_repair")
    );
    assert_eq!(
        node(&replayed.checkpoint, "rebase--r1--verified_clean").state,
        NodeState::Succeeded
    );
    assert_eq!(
        node(&replayed.checkpoint, "rebase--r1--refresh").state,
        NodeState::Succeeded
    );
    let receipt = node(&replayed.checkpoint, "rebase--r1--refresh")
        .outputs
        .as_ref()
        .unwrap()
        .publication
        .as_ref()
        .unwrap();
    assert_eq!(receipt.head, REPUBLISHED_HEAD);
    let conflict = node(&replayed.checkpoint, "rebase--r1--fresh_conflict")
        .conflict
        .as_ref()
        .unwrap();
    assert_eq!(conflict.state, ConflictState::Clear);
    assert_eq!(conflict.head_sha.as_deref(), Some(REPUBLISHED_HEAD));
    let ci = node(&replayed.checkpoint, "rebase--r1--fresh_ci");
    assert_eq!(ci.ci.as_ref().unwrap().state, CiState::Passed);
    assert_eq!(
        ci.ci.as_ref().unwrap().head_sha.as_deref(),
        Some(REPUBLISHED_HEAD)
    );
    assert_eq!(
        replay_owner
            .launches
            .iter()
            .filter(|launch| launch.node == "rebase--r1--fix")
            .count(),
        1
    );
    assert_eq!(
        replay_owner.hygiene_observations,
        vec![(
            "rebase--r1--verified_clean".into(),
            REBASE_HEAD.into(),
            REPUBLISHED_HEAD.into()
        )]
    );
    assert_eq!(
        replay_owner.publication_heads,
        vec![REPUBLISHED_HEAD.to_owned()]
    );

    let launches = replay_owner.launches.clone();
    let hygiene = replay_owner.hygiene_observations.clone();
    let publications = replay_owner.publication_heads.clone();
    let gate_observations = replay_owner.gate_observations.clone();
    let replayed_again = advance(&document, &mut replay_owner, &mut replay_journal).unwrap();
    assert_eq!(replayed_again.checkpoint, replayed.checkpoint);
    assert_eq!(replay_owner.launches, launches);
    assert_eq!(replay_owner.hygiene_observations, hygiene);
    assert_eq!(replay_owner.publication_heads, publications);
    assert_eq!(replay_owner.gate_observations, gate_observations);
}
