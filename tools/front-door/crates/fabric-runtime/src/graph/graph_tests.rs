use super::*;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::fmt;

#[derive(Debug)]
struct NoObservation;

impl fmt::Display for NoObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("no fake observation remains")
    }
}
impl Error for NoObservation {}

#[derive(Default)]
struct MemoryJournal(Option<GraphCheckpoint>);

impl GraphJournal for MemoryJournal {
    type Error = Infallible;

    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        Ok(self
            .0
            .as_ref()
            .filter(|checkpoint| checkpoint.graph_id == graph_id)
            .cloned())
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        self.0 = Some(checkpoint.clone());
        Ok(())
    }
}

#[derive(Default)]
struct FakeOwner {
    quota_redraw: bool,
    retry_limit: Option<u8>,
    input_cap_source: Option<InputCapSource>,
    retry_budgets: Vec<Option<u8>>,
    deliveries: usize,
    delivery_effects: usize,
    delivery_created: bool,
    delivery_identity: Option<Option<String>>,
    cancelled_attempt_settled: bool,
    cancellation_stops: usize,
    delivery_modes: Vec<Option<provenant_records::run::DeliveryMode>>,
    fast_forward_only: Vec<bool>,
    observations: VecDeque<AttemptObservation>,
    calls: Vec<(AttemptKey, GraphRoute)>,
    attempt_numbers: Vec<Option<u32>>,
    titles: Vec<String>,
    prompts: Vec<String>,
    permissions: Vec<(AccessMode, Option<String>)>,
    stored_refs: Vec<Option<String>>,
    actor: String,
    launch_actors: Vec<String>,
    usage: AttemptUsage,
    usage_fails: bool,
    refuse_expansion_target: bool,
    expansion_validations: usize,
    integration_heads: VecDeque<String>,
    graph_bases: (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ),
    implicit_head: Option<String>,
    implicit_bindings: usize,
    provisions: Vec<(String, String, String)>,
    provisioned: Vec<(String, String)>,
    provision_fail_once: bool,
    source_commits: Vec<Option<String>>,
    resolved_review_base: Option<String>,
    review_base_resolutions: usize,
    review_base_worktrees: Vec<Option<String>>,
    observed_review_bases: Vec<Option<String>>,
    equal_range: bool,
    no_review_base: bool,
    read_roots: Vec<Vec<String>>,
    attachments: Vec<Vec<String>>,
    capabilities: Vec<Option<Vec<String>>>,
    attempt_specs: Vec<Option<AttemptSpec>>,
    distinct_from: Vec<Vec<String>>,
    family_peers: Vec<Vec<String>>,
    family_pool: Vec<String>,
    prior_attempts: Vec<Vec<String>>,
    models: std::collections::BTreeMap<String, String>,
    repair_panel: Option<Vec<String>>,
    repair_panel_calls: usize,
    repair_panel_frame: bool,
    repair_panel_verifiers: usize,
    repair_panel_synthesis: bool,
    repair_growth: bool,
    repair_growth_calls: usize,
    require_distinct: Vec<bool>,
    skill_requirements: Vec<bool>,
    deadlines: Vec<Option<u64>>,
    families: std::collections::BTreeMap<String, String>,
    answers: std::collections::BTreeMap<String, AskReply>,
    now: Option<provenant_core::WallTime>,
    asked: Vec<String>,
    /// Every request filed, in order; a repeat files nothing.
    filed: Vec<AskRequest>,
    closed_asks: Vec<String>,
    owner_notifications: Vec<String>,
    file_requests: Vec<Vec<InputRequest>>,
    file_caps: Vec<usize>,
    materialise: Option<fn(&[InputRequest]) -> MaterialisedInputs>,
    fallback_evidence:
        std::collections::BTreeMap<String, provenant_records::run::PreparationFailure>,
    omit_fallback_evidence: bool,
    fail_fallback_evidence: bool,
    fallback_cause: Option<provenant_records::RouteExclusionReason>,
}

/// An inbox reply from `chair`, choosing `option` or sending `text`.
fn reply(option: Option<&str>, text: Option<&str>) -> AskReply {
    AskReply {
        option: option.map(str::to_owned),
        text: text.map(str::to_owned),
        actor: "chair".into(),
        at: None,
    }
}

impl AttemptOwner for FakeOwner {
    type Error = NoObservation;
    fn detect_repair_growth(&mut self, _: &str, _: &str) -> Result<bool, String> {
        self.repair_growth_calls += 1;
        Ok(self.repair_growth)
    }
    fn resolve_repair_panel(
        &mut self,
        _: &GraphDocument,
        _: &str,
    ) -> Result<RepairPanelResolution, String> {
        self.repair_panel_calls += 1;
        self.repair_panel
            .clone()
            .ok_or_else(|| "fixture has no panel".into())
            .map(|lenses| RepairPanelResolution {
                frame: self.repair_panel_frame, verifiers: self.repair_panel_verifiers,
                synthesis: self.repair_panel_synthesis, advice: Default::default(),
                slots: lenses.into_iter().enumerate().map(|(index, lens)| RepairPanelSlot {
                lens, route: serde_json::json!({"route":{"alias":if index == 0 {"senior"} else {"principal"}}}),
                family_group: Some("group-0".into()),
            }).collect() })
    }
    fn input_cap_source(&self) -> InputCapSource {
        self.input_cap_source
            .unwrap_or(InputCapSource::EffectiveGraphLimits)
    }

    fn retry_allowance(
        &self,
        _: &provenant_records::ExtraFields,
        _: &provenant_records::ExtraFields,
    ) -> Result<Option<(u8, String)>, Self::Error> {
        Ok(self.retry_limit.map(|limit| (limit, "host_config".into())))
    }
    fn quota_alias_redraw(&mut self, _: &AttemptKey, _: &str) -> Result<bool, Self::Error> {
        Ok(self.quota_redraw)
    }

    fn preparation_fallback_evidence(
        &mut self,
        _key: &AttemptKey,
        reference: &str,
    ) -> Result<Option<PreparationFallbackEvidence>, Self::Error> {
        if self.fail_fallback_evidence {
            return Err(NoObservation);
        }
        Ok(self
            .fallback_evidence
            .get(reference)
            .copied()
            .map(|failure| PreparationFallbackEvidence {
                failure,
                handoff: provenant_routing::FallbackHandoff::Local,
                custody_released: true,
            }))
    }

    fn resolve_review_base(
        &mut self,
        reference: &str,
        worktree: Option<&str>,
    ) -> Result<Option<String>, Self::Error> {
        self.review_base_resolutions += 1;
        self.review_base_worktrees.push(worktree.map(str::to_owned));
        Ok(self.resolved_review_base.clone().or_else(|| {
            reference
                .parse::<provenant_core::GitOid>()
                .ok()
                .map(|oid| oid.to_string())
        }))
    }

    fn preparation_refusal_cause(
        &mut self,
        _: &AttemptKey,
        _: &str,
    ) -> Result<Option<provenant_records::RouteExclusionReason>, Self::Error> {
        Ok(self.fallback_cause.clone())
    }

    fn bind_writer_worktree(
        &mut self,
        worktree: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        self.implicit_bindings += 1;
        Ok(Some(WriterWorktreeBinding {
            extra: Default::default(),
            worktree: if std::path::Path::new(worktree).is_absolute() {
                worktree.to_owned()
            } else {
                format!("/fixture/{worktree}")
            },
            head: self.implicit_head.clone().unwrap_or_else(|| "a".repeat(40)),
        }))
    }
    fn provision_writer_worktree(
        &mut self,
        path: &str,
        branch: &str,
        base: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        self.provisions
            .push((path.into(), branch.into(), base.into()));
        self.implicit_head = Some(base.into());
        self.bind_writer_worktree(path)
    }
    fn provision_child_worktree(&mut self, path: &str, source: &str) -> Result<bool, Self::Error> {
        self.provisioned.push((path.into(), source.into()));
        if std::mem::take(&mut self.provision_fail_once) {
            return Err(NoObservation);
        }
        Ok(true)
    }

    fn bound_graph_bases(
        &self,
    ) -> (
        Option<provenant_core::GitOid>,
        Option<provenant_core::GitOid>,
    ) {
        self.graph_bases
    }
    fn applied_route(&mut self, attempt: &str) -> Result<Option<GraphRoute>, Self::Error> {
        Ok(self.models.get(attempt).map(|model| GraphRoute {
            extra: Default::default(),
            adapter: Some("fixture".into()),
            model: Some(model.clone()),
            name: None,
            alias: None,
            effort: None,
            account: None,
            tier: None,
        }))
    }
    fn attempt_models(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        Ok(attempts
            .iter()
            .filter_map(|attempt| self.models.get(attempt).cloned())
            .collect())
    }
    fn stop_cancelled_attempt(&mut self, _attempt: &GraphAttempt) -> Result<(), Self::Error> {
        self.cancellation_stops += 1;
        Ok(())
    }
    fn cancelled_attempt_settled(&mut self, _attempt: &GraphAttempt) -> Result<bool, Self::Error> {
        Ok(self.cancelled_attempt_settled)
    }
    fn actor(&self) -> String {
        self.actor.clone()
    }
    fn file_ask(&mut self, filing: AskFiling<'_>) -> Result<AskFiled, Self::Error> {
        let id = &filing.ask.ask_id;
        let first = self.filed.iter().find(|ask| &ask.ask_id == id);
        let deadline = first.map_or(filing.ask.wake_at, |ask| ask.wake_at);
        if first.is_none() {
            self.filed.push(filing.ask.clone());
        }
        Ok(AskFiled {
            request: id.clone(),
            deadline,
            closed: self.closed_asks.contains(id),
        })
    }
    fn node_ask(&mut self, scope: AskScope<'_>) -> Result<Option<AskReply>, Self::Error> {
        self.asked.push(scope.ask.ask_id.clone());
        Ok(self.answers.get(&scope.ask.ask_id).cloned())
    }
    fn notify_owner(&mut self, filing: AskFiling<'_>) -> Result<bool, Self::Error> {
        self.owner_notifications.push(filing.ask.ask_id.clone());
        Ok(true)
    }
    fn close_node_ask(&mut self, scope: AskScope<'_>) -> Result<(), Self::Error> {
        self.closed_asks.push(scope.ask.ask_id.clone());
        Ok(())
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.now
    }
    fn materialise_inputs(
        &mut self,
        _key: &AttemptKey,
        requests: &[InputRequest],
        cap: usize,
    ) -> Option<MaterialisedInputs> {
        self.file_requests.push(requests.to_vec());
        self.file_caps.push(cap);
        self.materialise.map(|materialise| materialise(requests))
    }
    fn integration_head(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(self.integration_heads.pop_front())
    }
    fn validate_expansion(
        &mut self,
        document: &GraphDocument,
        _checkpoint: &GraphCheckpoint,
    ) -> Result<(), Self::Error> {
        self.expansion_validations += 1;
        if self.refuse_expansion_target
            && document.nodes.iter().any(|node| {
                matches!(
                    &node.delivery,
                    Some(provenant_records::run::DeliveryMode::Integrate(_))
                )
            })
        {
            return Err(NoObservation);
        }
        Ok(())
    }
    fn attempt_families(&mut self, attempts: &[String]) -> Result<Vec<String>, Self::Error> {
        let mut families = Vec::new();
        for family in attempts
            .iter()
            .filter_map(|attempt| self.families.get(attempt))
        {
            if !families.contains(family) {
                families.push(family.clone());
            }
        }
        Ok(families)
    }
    fn usage(&mut self, _attempt_ref: &str) -> Result<AttemptUsage, Self::Error> {
        if self.usage_fails {
            Err(NoObservation)
        } else {
            Ok(self.usage.clone())
        }
    }
    fn deliver_graph(
        &mut self,
        _document: &GraphDocument,
        _checkpoint: &GraphCheckpoint,
    ) -> Result<Option<String>, Self::Error> {
        self.deliveries += 1;
        if !self.delivery_created {
            self.delivery_created = true;
            self.delivery_effects += 1;
        }
        Ok(self
            .delivery_identity
            .clone()
            .unwrap_or_else(|| Some("https://example.invalid/pr/1".into())))
    }

    fn review_range(&mut self, _: &str, _: bool) -> Result<ReviewRange, Self::Error> {
        Ok(if self.equal_range {
            ReviewRange::Resolved {
                base: "1".repeat(40),
                head: "1".repeat(40),
            }
        } else if self.no_review_base {
            ReviewRange::NoBase
        } else {
            ReviewRange::Unknown
        })
    }

    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        node: NodeInput<'_>,
        route: &GraphRoute,
        access: AccessMode,
        worktree: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.source_commits
            .push(node.source_commit.map(str::to_owned));
        self.observed_review_bases
            .push(node.review_base.map(str::to_owned));
        self.read_roots.push(node.read_roots.to_vec());
        self.attachments.push(node.attachments.to_vec());
        self.capabilities
            .push(node.capabilities.map(<[String]>::to_vec));
        self.attempt_specs.push(node.attempt.cloned());
        self.distinct_from
            .push(node.distinct_from_attempts.to_vec());
        self.family_peers.push(node.family_peers.to_vec());
        self.prior_attempts.push(node.prior_attempts.to_vec());
        self.require_distinct.push(node.require_distinct);
        self.skill_requirements.push(node.require_skill_location);
        self.deadlines.push(node.deadline_seconds);
        self.titles.push(node.title.to_owned());
        self.prompts.push(node.prompt.to_owned());
        self.delivery_modes.push(replay.delivery.cloned());
        self.fast_forward_only.push(replay.fast_forward_only);
        self.stored_refs.push(replay.stored_ref.map(str::to_owned));
        self.retry_budgets.push(node.settlement_retries);
        self.launch_actors.push(replay.launch_actor.to_owned());
        self.attempt_numbers.push(node.context.attempt);
        self.calls.push((key.clone(), route.clone()));
        self.permissions.push((access, worktree.map(str::to_owned)));
        if !self.family_pool.is_empty() {
            let family = self
                .family_pool
                .iter()
                .find(|family| !node.family_peers.contains(family))
                .unwrap_or(&self.family_pool[0])
                .clone();
            let reference = format!("h/{}", self.calls.len());
            self.families.insert(reference.clone(), family);
            return Ok(done_as(&reference));
        }
        let observation = self.observations.pop_front().ok_or(NoObservation)?;
        if let AttemptObservation::Terminal {
            attempt_ref: Some(reference),
            outcome,
            launched: false,
            ..
        } = &observation
        {
            use provenant_records::run::PreparationFailure;
            let evidence = match outcome {
                AttemptOutcome::AuthRequired => Some(PreparationFailure::AuthRequired),
                AttemptOutcome::UsageLimited => Some(PreparationFailure::UsageLimited),
                AttemptOutcome::AdapterUnavailable => Some(PreparationFailure::AdapterUnavailable),
                _ => None,
            };
            if let Some(evidence) = evidence
                && !self.omit_fallback_evidence
            {
                self.fallback_evidence.insert(reference.clone(), evidence);
            }
        }
        Ok(observation)
    }
}

fn route(adapter: &str) -> GraphRoute {
    GraphRoute {
        extra: Default::default(),
        account: None,
        adapter: Some(adapter.to_owned()),
        name: None,
        model: Some(format!("{adapter}/model")),
        alias: None,
        effort: None,
        tier: None,
    }
}

fn pool_route(alias: &str) -> GraphRoute {
    GraphRoute {
        alias: Some(alias.into()),
        adapter: None,
        model: None,
        ..route("fixture")
    }
}

fn node(id: &str, route: GraphRoute) -> GraphNode {
    GraphNode {
        on_stop: Default::default(),
        attempt: None,
        attachments: Vec::new(),
        family_group: None,
        context: None,
        extra: Default::default(),
        composite_members: Vec::new(),
        when: Vec::new(),
        topology_present: Default::default(),
        distinct_from: None,
        distinct_strength: None,
        dispositions: Vec::new(),
        composite: None,
        role: None,
        read_roots: Vec::new(),
        capabilities: None,
        source: None,
        after_expansion: Vec::new(),
        plan: None,
        write_scope: Vec::new(),
        acceptance_checks: Vec::new(),
        decide: None,
        continues: None,
        land: None,
        loop_body: None,
        round: None,
        kind: None,
        condition: None,
        on_fail: None,
        delivery: None,
        require_skill_location: false,
        access: AccessMode::ReadOnly,
        worktree: None,
        id: id.to_owned(),
        title: None,
        deadline_seconds: None,
        prompt: format!("work for {id}"),
        depends_on: Vec::new(),
        advisory: false,
        inputs: Vec::new(),
        route: Some(route),
        routes: None,
        required_verdict: None,
        require_findings: false,
        skip_without_findings: None,
    }
}

fn graph(nodes: Vec<GraphNode>) -> GraphDocument {
    GraphDocument {
        review_base: None,
        context: None,
        extra: Default::default(),
        decision_session: None,
        integration_branch: None,
        delivery: None,
        limits: None,
        schema_version: 1,
        graph_id: "g-test".to_owned(),
        nodes,
    }
}

fn lens_then_verify(strength: Option<FamilyStrength>) -> GraphDocument {
    let mut verify = node("verify", route("first"));
    verify.distinct_from = Some(DistinctFrom::Dependencies);
    verify.distinct_strength = strength;
    verify.depends_on = vec!["mid".into()];
    let mut mid = node("mid", route("first"));
    mid.depends_on = vec!["lens-a".into(), "lens-b".into()];
    graph(vec![
        node("lens-a", route("first")),
        node("lens-b", route("first")),
        mid,
        verify,
    ])
}

fn done_as(reference: &str) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: Some(reference.into()),
        outcome: AttemptOutcome::Succeeded,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    }
}

fn separation_run(
    strength: Option<FamilyStrength>,
    verifier_family: &str,
) -> (FakeOwner, GraphCheckpoint) {
    let mut owner = FakeOwner {
        observations: [
            done_as("h/1"),
            done_as("h/2"),
            done_as("h/3"),
            done_as("h/4"),
        ]
        .into(),
        families: [
            ("h/1".to_owned(), "anthropic".to_owned()),
            ("h/2".to_owned(), "anthropic".to_owned()),
            ("h/3".to_owned(), "anthropic".to_owned()),
            ("h/4".to_owned(), verifier_family.to_owned()),
        ]
        .into(),
        ..Default::default()
    };
    let run = advance(
        &lens_then_verify(strength),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap();
    (owner, run.checkpoint)
}

const DISPOSITIONS: [&str; 3] = ["accepted", "rejected", "deferred"];

fn synthesis_run(text: &str) -> NodeProgress {
    let mut synthesis = node("synthesis", route("first"));
    synthesis.dispositions = DISPOSITIONS.map(String::from).to_vec();
    let mut owner = FakeOwner {
        observations: [terminal_with_result(AttemptOutcome::Succeeded, text)].into(),
        ..Default::default()
    };
    advance(
        &graph(vec![synthesis]),
        &mut owner,
        &mut MemoryJournal::default(),
    )
    .unwrap()
    .checkpoint
    .nodes
    .remove(0)
}

fn findings_text(entries: &str) -> String {
    format!("```findings\n{{\"schema_version\":1,\"findings\":[{entries}]}}\n```\nVERDICT: PASS\n")
}

fn entry(extra: &str) -> String {
    format!(
        "{{\"id\":\"F1\",\"severity\":\"high\",\"file\":\"a.rs\",\"line\":null,\"summary\":\"s\",\"status\":\"open\"{extra}}}"
    )
}

/// Three lenses: `lens-a` succeeds, `lens-b` returns no findings block and
/// `lens-c` is unavailable. `verify` reads the lens group.
fn partial_review(minimum: usize) -> (GraphDocument, FakeOwner) {
    let ok = "LENS-A-OK\n```findings\n{\"schema_version\":1,\"findings\":[{\"id\":\"F1\",\"severity\":\"low\",\"file\":\"a.rs\",\"line\":1,\"summary\":\"note\",\"status\":\"open\"}]}\n```";
    // A fixed route makes unavailable members final; an alias authorises
    // stop-policy redraw and would consume a consumer's fixture observation.
    let lens = |id: &str| {
        serde_json::json!({"id":id,"access":"read_only","prompt":id,
            "require_findings":true,"route":{"adapter":"fixture","model":"fixture/lens"}})
    };
    let exports: Vec<_> = ["lens-a", "lens-b", "lens-c"]
        .map(|id| serde_json::json!({"node":id,"outputs":["result","findings"]}))
        .into();
    let document = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "graph_id": "partial-review", "nodes": [
            {"id":"lenses","kind":"composite","access":"read_only",
             "composite":{"min_successful":minimum,
                "nodes":[lens("lens-a"),lens("lens-b"),lens("lens-c")],"exports":exports}},
            {"id":"verify","access":"read_only","prompt":"verify","route":{"alias":"fixture"},
             "depends_on":["lenses"],"inputs":[{"node":"lenses","outputs":["result","findings"]}]}
        ]
    }))
    .unwrap();
    let owner = FakeOwner {
        observations: VecDeque::from([
            terminal_with_result(AttemptOutcome::Succeeded, ok),
            terminal_with_result(AttemptOutcome::Succeeded, "LENS-B-NO-FINDINGS"),
            terminal(AttemptOutcome::AdapterUnavailable),
            terminal_with_result(AttemptOutcome::Succeeded, "verified"),
        ]),
        ..Default::default()
    };
    (document, owner)
}

fn gate_node(condition: GateCondition) -> GraphNode {
    let mut gate = node("gate", route("unused"));
    gate.kind = Some(NodeKind::Gate);
    gate.condition = Some(condition);
    gate.prompt.clear();
    gate.route = None;
    gate
}

fn verdict_fixture(
    threshold: VerdictThreshold,
    verdicts: &[Option<&str>],
) -> (GraphDocument, FakeOwner) {
    let mut gate = gate_node(GateCondition::Verdict {
        extra: Default::default(),
        expected: "VERDICT: PASS".into(),
        threshold,
    });
    let mut nodes = Vec::new();
    let mut owner = FakeOwner::default();
    for (i, verdict) in verdicts.iter().enumerate() {
        let review = node(&format!("review-{i}"), route("fixture"));
        gate.depends_on.push(review.id.clone());
        gate.inputs.push(GraphInput {
            required_outputs: Vec::new(),
            extra: Default::default(),
            node: review.id.clone(),
            outputs: vec![OutputKind::Verdict],
        });
        let mut capture = crate::result_capture::ResultCapture::new(None);
        let text = verdict.map_or_else(
            || "no verdict".to_owned(),
            |line| format!("VERDICT: {line}"),
        );
        capture.push(text.as_bytes());
        let mut observation = terminal(AttemptOutcome::Succeeded);
        if let AttemptObservation::Terminal { result, .. } = &mut observation {
            *result = Some(Box::new(capture.finish()));
        }
        owner.observations.push_back(observation);
        nodes.push(review);
    }
    nodes.push(gate);
    (graph(nodes), owner)
}

fn terminal(outcome: AttemptOutcome) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: None,
        outcome,
        launched: true,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    }
}

fn preparation_refusal(reference: &str, outcome: AttemptOutcome) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: Some(reference.into()),
        outcome,
        launched: false,
        result: None,
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    }
}

fn terminal_with_result(outcome: AttemptOutcome, result: &str) -> AttemptObservation {
    AttemptObservation::Terminal {
        attempt_ref: None,
        outcome,
        launched: true,
        result: Some(Box::new(capture(result))),
        result_warning: None,
        result_path: None,
        branch: None,
        commit: None,
        stop: None,
    }
}

fn terminal_from(result: &str, attempt_ref: &str) -> AttemptObservation {
    let mut observation = terminal_with_result(AttemptOutcome::Succeeded, result);
    if let AttemptObservation::Terminal {
        attempt_ref: slot, ..
    } = &mut observation
    {
        *slot = Some(attempt_ref.to_owned());
    }
    observation
}

/// `parent` feeds `child`'s prompt; the owner records what `child` asked for.
fn parent_and_child(
    materialise: Option<fn(&[InputRequest]) -> MaterialisedInputs>,
) -> (GraphDocument, FakeOwner) {
    let mut child = node("child", route("fixture"));
    child.depends_on.push("parent".into());
    child.inputs.push(GraphInput {
        required_outputs: Vec::new(),
        extra: Default::default(),
        node: "parent".into(),
        outputs: vec![OutputKind::Result],
    });
    let owner = FakeOwner {
        observations: VecDeque::from([
            terminal_from("short excerpt", "host-a/attempt-1"),
            terminal(AttemptOutcome::Succeeded),
        ]),
        materialise,
        ..Default::default()
    };
    (graph(vec![child, node("parent", route("fixture"))]), owner)
}

fn capture(text: &str) -> crate::result_capture::CapturedResult {
    let mut capture = crate::result_capture::ResultCapture::new(Some("VERDICT: PASS"));
    capture.push(text.as_bytes());
    // The sealed handoff a settled attempt carries, as the fixture text declares it.
    let mut result = capture.finish();
    result.handoff = provenant_records::handoff::Handoff::from_turns(text, &[]);
    result
}

fn check_fallback(
    case: &str,
    access: AccessMode,
    launched: bool,
    outcome: AttemptOutcome,
    expected: bool,
) {
    let mut task = node("node", route("first"));
    task.access = access;
    task.worktree = (access == AccessMode::WorktreeWrite).then(|| "/private/tmp/writer".into());
    task.route = None;
    task.routes = Some(vec![route("first"), route("second")]);
    let document = graph(vec![task]);
    let mut owner = FakeOwner {
        observations: VecDeque::from([
            AttemptObservation::Terminal {
                attempt_ref: Some("host/attempt".into()),
                outcome,
                launched,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                stop: None,
            },
            terminal(AttemptOutcome::Succeeded),
        ]),
        ..FakeOwner::default()
    };
    let mut journal = MemoryJournal::default();
    let run = advance(&document, &mut owner, &mut journal).unwrap();
    // A launched node changes route through a graph stop-policy restart,
    // keeping the settled attempt in history rather than inline fallback.
    let restarted = !expected
        && launched
        && matches!(
            outcome,
            AttemptOutcome::AuthRequired
                | AttemptOutcome::UsageLimited
                | AttemptOutcome::AdapterUnavailable
        );
    assert_eq!(
        owner.calls.len(),
        if expected || restarted { 2 } else { 1 },
        "{case}"
    );
    assert_eq!(
        run.checkpoint.nodes[0].attempts.len(),
        if expected { 2 } else { 1 },
        "{case}"
    );
    assert!(
        owner
            .permissions
            .iter()
            .all(|(mode, worktree)| *mode == access
                && worktree.as_deref()
                    == (access == AccessMode::WorktreeWrite).then_some("/private/tmp/writer")),
        "{case}"
    );

    assert_eq!(
        run.checkpoint.nodes[0].state,
        if expected || restarted {
            NodeState::Succeeded
        } else {
            NodeState::Failed
        },
        "{case}"
    );
    assert_eq!(
        run.checkpoint
            .expansions
            .iter()
            .filter(|expansion| expansion
                .patch
                .as_ref()
                .is_some_and(|record| record.restart.is_some()))
            .count(),
        usize::from(restarted),
        "{case}"
    );
    assert_eq!(
        run.checkpoint.nodes[0].worktree.as_deref(),
        (access == AccessMode::WorktreeWrite).then_some("/private/tmp/writer"),
        "{case}"
    );
    // Reopening the checkpoint must make the same safety decision.
    assert_eq!(
        advance(&document, &mut owner, &mut journal).unwrap(),
        run,
        "{case}"
    );
}

mod loop_tests {
    include!("loop_tests.rs");
}
mod plan_tests {
    include!("plan_tests.rs");
    include!("planner_implement_invariants.rs");
}
mod relay_tests {
    include!("relay_tests.rs");
    include!("relay_implement_invariants.rs");
}
mod envelope_tests {
    include!("envelope_tests.rs");
}
mod ask_tests {
    include!("ask_tests.rs");
}
fn at(seconds: i64) -> provenant_core::WallTime {
    provenant_core::WallTime::from_micros(1_800_000_000_000_000 + seconds * 1_000_000).unwrap()
}

/// An owner whose launch fails with typed facade kinds, for the persisted
/// launch dispositions. Errors are keyed by call number, first call is 1.
#[derive(Default)]
struct ErrorLauncher {
    errors: std::collections::BTreeMap<usize, crate::coordination::facade::FacadeError>,
    binding_error: Option<crate::coordination::facade::FacadeError>,
    observations: VecDeque<AttemptObservation>,
    calls: Vec<AttemptKey>,
    stored_refs: Vec<Option<String>>,
    now: Option<provenant_core::WallTime>,
    now_after_error: Option<provenant_core::WallTime>,
    admission_seconds: Option<u64>,
    launch_policy: Option<LaunchRetryPolicy>,
    accepted: bool,
    acceptance_lookup_error: bool,
}

impl AttemptOwner for ErrorLauncher {
    type Error = crate::coordination::facade::FacadeError;

    fn actor(&self) -> String {
        "actor".into()
    }
    fn bind_writer_worktree(
        &mut self,
        _path: &str,
    ) -> Result<Option<WriterWorktreeBinding>, Self::Error> {
        match self.binding_error.take() {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }
    fn wall_now(&self) -> Option<provenant_core::WallTime> {
        self.now
    }
    fn admission_timeout_seconds(&self) -> Option<u64> {
        Some(self.admission_seconds.unwrap_or(3600))
    }
    fn launch_retry_policy(&self) -> LaunchRetryPolicy {
        self.launch_policy
            .clone()
            .unwrap_or_else(|| LaunchRetryPolicy {
                limit: 20,
                ..Default::default()
            })
    }
    fn launch_accepted(&mut self, _attempt: &GraphAttempt) -> Result<bool, Self::Error> {
        Ok(self.accepted)
    }
    fn launch_refused_before_acceptance(
        &mut self,
        attempt: &GraphAttempt,
    ) -> Result<bool, Self::Error> {
        if self.acceptance_lookup_error {
            return Err(Self::Error::StateCorrupt);
        }
        self.launch_accepted(attempt).map(|accepted| !accepted)
    }
    fn observe_or_start(
        &mut self,
        key: &AttemptKey,
        _node: NodeInput<'_>,
        _route: &GraphRoute,
        _access: AccessMode,
        _worktree: Option<&str>,
        replay: Replay<'_>,
    ) -> Result<AttemptObservation, Self::Error> {
        self.calls.push(key.clone());
        self.stored_refs.push(replay.stored_ref.map(str::to_owned));
        let error = self.errors.remove(&self.calls.len());
        if let Some(error) = error {
            if self.now_after_error.is_some() {
                self.now = self.now_after_error;
            }
            return Err(error);
        }
        self.observations
            .pop_front()
            .ok_or(Self::Error::OwnerUnavailable)
    }
}

fn busy_times(
    count: usize,
) -> std::collections::BTreeMap<usize, crate::coordination::facade::FacadeError> {
    (1..=count)
        .map(|round| (round, crate::coordination::facade::FacadeError::Busy))
        .collect()
}

fn retry_of(checkpoint: &GraphCheckpoint) -> &LaunchRetry {
    checkpoint.nodes[0]
        .launch_retry
        .as_ref()
        .expect("a pending launch retry")
}

mod retry_tests {
    include!("retry_tests.rs");
}

mod launch_and_separation_tests {
    include!("graph_launch_and_separation_tests.rs");
}

mod synthesis_findings_tests {
    include!("graph_synthesis_findings_tests.rs");
}

mod finding_propagation_tests {
    include!("graph_finding_propagation_tests.rs");
}

mod sources_and_composites_tests {
    include!("graph_sources_and_composites_tests.rs");
}

mod composite_review_tests {
    include!("graph_composite_review_tests.rs");
}

mod plan_and_gates_tests {
    include!("graph_plan_and_gates_tests.rs");
}

mod limits_and_budget_tests {
    include!("graph_limits_and_budget_tests.rs");
}

mod upstream_materialisation_tests {
    include!("graph_upstream_materialisation_tests.rs");
}

mod delivery_and_routes_tests {
    include!("graph_delivery_and_routes_tests.rs");
}

mod retry_and_substitution_tests {
    include!("graph_retry_and_substitution_tests.rs");
}

mod inputs_and_outputs_tests {
    include!("graph_inputs_and_outputs_tests.rs");
}

mod launch_retry_tests {
    include!("graph_launch_retry_tests.rs");
}

mod composite_and_allowance_tests {
    include!("graph_composite_and_allowance_tests.rs");
}

mod launch_refusals_tests {
    include!("graph_launch_refusals_tests.rs");
}

mod quota_and_review_tests {
    include!("graph_quota_and_review_tests.rs");
}

mod review_contract_tests {
    include!("graph_review_contract_tests.rs");
}
